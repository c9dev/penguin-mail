use std::collections::HashMap;
use std::time::Duration;

use mailrs_gmail::{GmailClient, GmailError, HistoryChange, OAuthClient, authorize};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use wiremock::matchers::{body_json, body_string_contains, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const API: &str = "/gmail/v1/users/me";

async fn mount_token(server: &MockServer, expected_calls: u64) {
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"access_token": "at-1", "expires_in": 3600})))
        .expect(expected_calls)
        .mount(server)
        .await;
}

fn oauth(server: &MockServer) -> OAuthClient {
    OAuthClient::new("cid", "secret")
        .with_endpoints(format!("{}/auth", server.uri()), format!("{}/token", server.uri()))
}

fn client(server: &MockServer) -> GmailClient {
    GmailClient::new(oauth(server), "rt".into()).with_base_url(format!("{}{API}", server.uri()))
}

fn message_json(id: &str) -> serde_json::Value {
    json!({"id": id, "threadId": format!("t-{id}"), "labelIds": ["INBOX"]})
}

#[tokio::test]
async fn profile_sends_the_bearer_token_and_caches_it() {
    let server = MockServer::start().await;
    mount_token(&server, 1).await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/profile")))
        .and(header("authorization", "Bearer at-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"emailAddress": "me@example.com", "historyId": "77"})))
        .expect(2)
        .mount(&server)
        .await;
    let client = client(&server);
    assert_eq!(client.profile().await.unwrap().history_id, 77);
    assert_eq!(client.profile().await.unwrap().email_address, "me@example.com");
}

#[tokio::test]
async fn a_401_refreshes_once_and_retries() {
    let server = MockServer::start().await;
    mount_token(&server, 2).await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/profile")))
        .respond_with(ResponseTemplate::new(401))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/profile")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"emailAddress": "me@example.com", "historyId": "1"})))
        .mount(&server)
        .await;
    assert!(client(&server).profile().await.is_ok());
}

#[tokio::test]
async fn errors_are_classified() {
    let server = MockServer::start().await;
    mount_token(&server, 1).await;
    let respond = |id: &str, template: ResponseTemplate| {
        Mock::given(method("GET")).and(path(format!("{API}/messages/{id}"))).respond_with(template)
    };
    respond("gone", ResponseTemplate::new(404)).mount(&server).await;
    respond("busy", ResponseTemplate::new(429).insert_header("retry-after", "7")).mount(&server).await;
    respond(
        "quota",
        ResponseTemplate::new(403).set_body_string(r#"{"error":{"errors":[{"reason":"userRateLimitExceeded"}]}}"#),
    )
    .mount(&server)
    .await;
    respond("boom", ResponseTemplate::new(500).set_body_string("oops")).mount(&server).await;

    let client = client(&server);
    assert!(matches!(client.message_metadata("gone").await, Err(GmailError::NotFound)));
    assert!(matches!(
        client.message_metadata("busy").await,
        Err(GmailError::RateLimited { retry_after: Some(d) }) if d == Duration::from_secs(7)
    ));
    assert!(matches!(
        client.message_metadata("quota").await,
        Err(GmailError::RateLimited { retry_after: None })
    ));
    match client.message_metadata("boom").await {
        Err(GmailError::Http { status: 500, body }) => assert_eq!(body, "oops"),
        other => panic!("unexpected result: {other:?}"),
    }
}

#[tokio::test]
async fn list_messages_sends_the_query_page_size_and_token() {
    let server = MockServer::start().await;
    mount_token(&server, 1).await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/messages")))
        .and(query_param("q", "{newer_than:30d in:inbox}"))
        .and(query_param("maxResults", "100"))
        .and(query_param("pageToken", "p2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "messages": [{"id": "a", "threadId": "t"}], "nextPageToken": "p3"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let page = client(&server)
        .list_messages("{newer_than:30d in:inbox}", Some("p2"), 100)
        .await
        .unwrap();
    assert_eq!(page.messages.len(), 1);
    assert_eq!(page.next_page_token.as_deref(), Some("p3"));
}

#[tokio::test]
async fn message_and_thread_fetches_request_the_right_formats() {
    let server = MockServer::start().await;
    mount_token(&server, 1).await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/messages/m1")))
        .and(query_param("format", "metadata"))
        .and(query_param("metadataHeaders", "Message-ID"))
        .respond_with(ResponseTemplate::new(200).set_body_json(message_json("m1")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/messages/m1")))
        .and(query_param("format", "full"))
        .respond_with(ResponseTemplate::new(200).set_body_json(message_json("m1")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/threads/t1")))
        .and(query_param("format", "metadata"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "t1", "messages": [message_json("m1"), message_json("m2")]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let client = client(&server);
    assert_eq!(client.message_metadata("m1").await.unwrap().id, "m1");
    assert_eq!(client.message_full("m1").await.unwrap().id, "m1");
    assert_eq!(client.thread_metadata("t1").await.unwrap().messages.len(), 2);
}

#[tokio::test]
async fn history_is_converted_to_changes() {
    let server = MockServer::start().await;
    mount_token(&server, 1).await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/history")))
        .and(query_param("startHistoryId", "10"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "history": [{"id": "11", "messagesAdded": [{"message": {"id": "a", "threadId": "ta"}}]}],
            "historyId": "11"
        })))
        .mount(&server)
        .await;
    let page = client(&server).history(10, None).await.unwrap();
    assert_eq!(page.history_id, 11);
    assert_eq!(page.changes, vec![HistoryChange::MessageAdded { id: "a".into(), thread_id: "ta".into() }]);
}

#[tokio::test]
async fn modify_and_trash_post_to_gmail() {
    let server = MockServer::start().await;
    mount_token(&server, 1).await;
    Mock::given(method("POST"))
        .and(path(format!("{API}/messages/m1/modify")))
        .and(body_json(json!({"addLabelIds": ["STARRED"], "removeLabelIds": ["INBOX"]})))
        .respond_with(ResponseTemplate::new(200).set_body_json(message_json("m1")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("{API}/messages/m1/trash")))
        .respond_with(ResponseTemplate::new(200).set_body_json(message_json("m1")))
        .expect(1)
        .mount(&server)
        .await;
    let client = client(&server);
    client.modify("m1", &["STARRED".into()], &["INBOX".into()]).await.unwrap();
    client.trash("m1").await.unwrap();
}

#[tokio::test]
async fn authorize_runs_the_consent_flow() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=authorization_code"))
        .and(body_string_contains("code=the-code"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "at-1", "expires_in": 3600, "refresh_token": "rt-new"
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/profile")))
        .and(header("authorization", "Bearer at-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"emailAddress": "me@example.com", "historyId": "5"})))
        .mount(&server)
        .await;

    let api = format!("{}{API}", server.uri());
    let authorized = authorize(&oauth(&server), &api, |consent_url| {
        let url = url::Url::parse(consent_url).unwrap();
        let q: HashMap<String, String> = url.query_pairs().into_owned().collect();
        let redirect = q["redirect_uri"].trim_start_matches("http://").to_string();
        let state = q["state"].clone();
        tokio::spawn(async move {
            let mut stream = tokio::net::TcpStream::connect(&redirect).await.unwrap();
            let request = format!("GET /?code=the-code&state={state} HTTP/1.1\r\nHost: {redirect}\r\n\r\n");
            stream.write_all(request.as_bytes()).await.unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).await.unwrap();
        });
    })
    .await
    .unwrap();
    assert_eq!(authorized.email, "me@example.com");
    assert_eq!(authorized.refresh_token, "rt-new");
}
