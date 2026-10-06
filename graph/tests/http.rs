use std::sync::{Arc, Mutex};
use std::time::Duration;

use mailrs_graph::{BatchRequest, Graph, GraphError, Method, MicrosoftClient, Page, Session};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

mod common;

use common::{CLIENT, graph, token_endpoint};

#[tokio::test]
async fn every_call_asks_for_immutable_ids() {
    let server = MockServer::start().await;
    token_endpoint(&server, "refresh-0").await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"mail": "a@outlook.com"})))
        .mount(&server)
        .await;
    let _: Value = graph(&server).get("me", &[]).await.unwrap();
    let asked: Vec<Request> = server.received_requests().await.unwrap();
    let me = asked.iter().find(|r| r.url.path() == "/v1.0/me").unwrap();
    let prefer: Vec<String> = me
        .headers
        .get_all("prefer")
        .iter()
        .map(|v| v.to_str().unwrap().to_string())
        .collect();
    assert!(
        prefer.iter().any(|p| p == "IdType=\"ImmutableId\""),
        "{prefer:?}"
    );
    assert_eq!(me.headers.get("authorization").unwrap(), "Bearer access-1");
}

#[tokio::test]
async fn a_link_to_another_host_is_refused_without_the_token() {
    let server = MockServer::start().await;
    token_endpoint(&server, "refresh-0").await;
    let graph = graph(&server);
    let off = graph
        .follow::<Page<Value>>("https://evil.example/v1.0/me/messages?$skiptoken=x", &[])
        .await;
    assert!(matches!(off, Err(GraphError::OffHost(host)) if host == "evil.example"));
    // Same host, other scheme: refused as well.
    let plain = graph
        .follow::<Page<Value>>(&server.uri().replace("http://", "ftp://"), &[])
        .await;
    assert!(matches!(plain, Err(GraphError::OffHost(_))));
    // Not even the token endpoint was asked: nothing went anywhere.
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_short_retry_after_is_waited_out_once() {
    let server = MockServer::start().await;
    token_endpoint(&server, "refresh-0").await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "0"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"mail": "a@outlook.com"})))
        .mount(&server)
        .await;
    let me: Value = graph(&server).get("me", &[]).await.unwrap();
    assert_eq!(me["mail"], "a@outlook.com");
}

#[tokio::test]
async fn a_long_retry_after_answers_throttled() {
    let server = MockServer::start().await;
    token_endpoint(&server, "refresh-0").await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me"))
        .respond_with(ResponseTemplate::new(503).insert_header("Retry-After", "120"))
        .mount(&server)
        .await;
    let answer = graph(&server).get::<Value>("me", &[]).await;
    assert!(matches!(
        answer,
        Err(GraphError::Throttled { retry_after: Some(d) }) if d == Duration::from_secs(120)
    ));
}

#[tokio::test]
async fn a_refused_access_token_refreshes_once_then_retries() {
    let server = MockServer::start().await;
    token_endpoint(&server, "refresh-0").await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me"))
        .respond_with(ResponseTemplate::new(401).set_body_json(
            json!({"error": {"code": "InvalidAuthenticationToken", "message": "expired"}}),
        ))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&server)
        .await;
    let _: Value = graph(&server).get("me", &[]).await.unwrap();
    let tokens = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.url.path() == "/token")
        .count();
    assert_eq!(tokens, 2, "one refresh to start, one forced by the 401");
}

#[tokio::test]
async fn a_rotated_refresh_token_reaches_the_callback() {
    let server = MockServer::start().await;
    token_endpoint(&server, "refresh-1").await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&server)
        .await;
    let saved = Arc::new(Mutex::new(Vec::new()));
    let kept = Arc::clone(&saved);
    let client = MicrosoftClient::new(CLIENT).with_endpoints(
        format!("{}/authorize", server.uri()),
        format!("{}/token", server.uri()),
    );
    let session =
        Session::new(client, "refresh-0").on_rotated(move |token| kept.lock().unwrap().push(token));
    let graph = Graph::with_base(Arc::new(session), &format!("{}/v1.0/", server.uri())).unwrap();
    let _: Value = graph.get("me", &[]).await.unwrap();
    assert_eq!(*saved.lock().unwrap(), ["refresh-1"]);
}

#[tokio::test]
async fn a_body_over_the_limit_is_refused() {
    let server = MockServer::start().await;
    token_endpoint(&server, "refresh-0").await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me/messages/m1/$value"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![b'x'; 11]))
        .mount(&server)
        .await;
    let answer = graph(&server).get_bytes("me/messages/m1/$value", 10).await;
    assert!(matches!(answer, Err(GraphError::TooLarge { limit: 10 })));
}

#[tokio::test]
async fn batch_answers_in_request_order_and_splits_at_twenty() {
    let server = MockServer::start().await;
    token_endpoint(&server, "refresh-0").await;
    Mock::given(method("POST"))
        .and(path("/v1.0/$batch"))
        .respond_with(|request: &Request| {
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            // Answer in reverse, as Graph may.
            let mut answers: Vec<Value> = body["requests"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| json!({"id": r["id"], "status": 200, "body": {"url": r["url"]}}))
                .collect();
            answers.reverse();
            ResponseTemplate::new(200).set_body_json(json!({"responses": answers}))
        })
        .mount(&server)
        .await;
    let requests: Vec<BatchRequest> = (0..25)
        .map(|i| BatchRequest::get(format!("me/messages/m{i}")))
        .collect();
    let answers = graph(&server).batch(&requests).await.unwrap();
    assert_eq!(answers.len(), 25);
    for (i, answer) in answers.into_iter().enumerate() {
        let body: Value = answer.into_json().unwrap().unwrap();
        assert_eq!(body["url"], format!("/me/messages/m{i}"));
    }
    let posts = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.url.path() == "/v1.0/$batch")
        .count();
    assert_eq!(posts, 2);
}

#[tokio::test]
async fn a_failed_entry_in_a_batch_keeps_its_own_error() {
    let server = MockServer::start().await;
    token_endpoint(&server, "refresh-0").await;
    Mock::given(method("POST"))
        .and(path("/v1.0/$batch"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"responses": [
            {"id": "1", "status": 204},
            {"id": "2", "status": 404, "body": {"error": {"code": "ErrorItemNotFound", "message": ""}}},
        ]})))
        .mount(&server)
        .await;
    let requests = [
        BatchRequest::new(
            Method::Patch,
            "me/messages/a".into(),
            Some(json!({"isRead": true})),
        ),
        BatchRequest::new(
            Method::Patch,
            "me/messages/b".into(),
            Some(json!({"isRead": true})),
        ),
    ];
    let mut answers = graph(&server).batch(&requests).await.unwrap().into_iter();
    assert!(matches!(
        answers.next().unwrap().into_json::<Value>(),
        Ok(None)
    ));
    assert!(matches!(
        answers.next().unwrap().into_json::<Value>(),
        Err(GraphError::NotFound)
    ));
}

/// Answers `at` with one item, `first`, and a next link to `/next`, and
/// `/next` with one item, `second`.
async fn two_pages(server: &MockServer, at: &str, first: Value, second: Value) {
    let next = format!("{}/v1.0/next?$skiptoken=2", server.uri());
    Mock::given(method("GET"))
        .and(path(at.to_string()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": [first], "@odata.nextLink": next})))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1.0/next"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": [second]})))
        .mount(server)
        .await;
}

#[tokio::test]
async fn calendars_read_every_page() {
    let server = MockServer::start().await;
    token_endpoint(&server, "refresh-0").await;
    two_pages(&server, "/v1.0/me/calendars", json!({"id": "c1", "name": "One"}), json!({"id": "c2", "name": "Two"})).await;
    let calendars = graph(&server).calendars().await.unwrap();
    let ids: Vec<&str> = calendars.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, ["c1", "c2"]);
}

#[tokio::test]
async fn contact_folders_read_every_page() {
    let server = MockServer::start().await;
    token_endpoint(&server, "refresh-0").await;
    two_pages(&server, "/v1.0/me/contactFolders", json!({"id": "f1", "displayName": "A"}), json!({"id": "f2", "displayName": "B"})).await;
    let folders = graph(&server).contact_folders().await.unwrap();
    let ids: Vec<&str> = folders.iter().map(|f| f.id.as_str()).collect();
    assert_eq!(ids, ["f1", "f2"]);
}

#[tokio::test]
async fn rules_read_every_page() {
    let server = MockServer::start().await;
    token_endpoint(&server, "refresh-0").await;
    let rule = |id: &str| json!({"id": id, "displayName": id, "sequence": 1, "isEnabled": true});
    two_pages(&server, "/v1.0/me/mailFolders/inbox/messageRules", rule("r1"), rule("r2")).await;
    let rules = graph(&server).rules().await.unwrap();
    let ids: Vec<&str> = rules.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, ["r1", "r2"]);
}

#[tokio::test]
async fn every_batch_entry_asks_for_immutable_ids() {
    let server = MockServer::start().await;
    token_endpoint(&server, "refresh-0").await;
    Mock::given(method("POST"))
        .and(path("/v1.0/$batch"))
        .respond_with(|request: &Request| {
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            let answers: Vec<Value> = body["requests"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| json!({"id": r["id"], "status": 200, "body": {"headers": r["headers"]}}))
                .collect();
            ResponseTemplate::new(200).set_body_json(json!({"responses": answers}))
        })
        .mount(&server)
        .await;
    let requests = [
        BatchRequest::get("me/messages/a".into()),
        BatchRequest::get("me/events/b".into()).header("Prefer", "outlook.timezone=\"UTC\""),
    ];
    let answers = graph(&server).batch(&requests).await.unwrap();
    let prefers: Vec<String> = answers
        .into_iter()
        .map(|a| {
            let body: Value = a.into_json().unwrap().unwrap();
            body["headers"]["Prefer"].as_str().unwrap_or_default().to_string()
        })
        .collect();
    assert_eq!(prefers[0], "IdType=\"ImmutableId\"");
    assert_eq!(prefers[1], "IdType=\"ImmutableId\", outlook.timezone=\"UTC\"");
}
