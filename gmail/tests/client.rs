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
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"access_token": "at-1", "expires_in": 3600})),
        )
        .expect(expected_calls)
        .mount(server)
        .await;
}

fn oauth(server: &MockServer) -> OAuthClient {
    OAuthClient::new("cid", "secret").with_endpoints(
        format!("{}/auth", server.uri()),
        format!("{}/token", server.uri()),
    )
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
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"emailAddress": "me@example.com", "historyId": "77"})),
        )
        .expect(2)
        .mount(&server)
        .await;
    let client = client(&server);
    assert_eq!(client.profile().await.unwrap().history_id, 77);
    assert_eq!(
        client.profile().await.unwrap().email_address,
        "me@example.com"
    );
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
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"emailAddress": "me@example.com", "historyId": "1"})),
        )
        .mount(&server)
        .await;
    assert!(client(&server).profile().await.is_ok());
}

#[tokio::test]
async fn errors_are_classified() {
    let server = MockServer::start().await;
    mount_token(&server, 1).await;
    let respond = |id: &str, template: ResponseTemplate| {
        Mock::given(method("GET"))
            .and(path(format!("{API}/messages/{id}")))
            .respond_with(template)
    };
    respond("gone", ResponseTemplate::new(404))
        .mount(&server)
        .await;
    respond(
        "busy",
        ResponseTemplate::new(429).insert_header("retry-after", "7"),
    )
    .mount(&server)
    .await;
    respond(
        "quota",
        ResponseTemplate::new(403)
            .set_body_string(r#"{"error":{"errors":[{"reason":"userRateLimitExceeded"}]}}"#),
    )
    .mount(&server)
    .await;
    respond("boom", ResponseTemplate::new(500).set_body_string("oops"))
        .mount(&server)
        .await;

    let client = client(&server);
    assert!(matches!(
        client.message_metadata("gone").await,
        Err(GmailError::NotFound)
    ));
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
    assert_eq!(
        client.thread_metadata("t1").await.unwrap().messages.len(),
        2
    );
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
    assert_eq!(
        page.changes,
        vec![HistoryChange::MessageAdded {
            id: "a".into(),
            thread_id: "ta".into()
        }]
    );
}

#[tokio::test]
async fn modify_and_trash_post_to_gmail() {
    let server = MockServer::start().await;
    mount_token(&server, 1).await;
    Mock::given(method("POST"))
        .and(path(format!("{API}/messages/m1/modify")))
        .and(body_json(
            json!({"addLabelIds": ["STARRED"], "removeLabelIds": ["INBOX"]}),
        ))
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
    client
        .modify("m1", &["STARRED".into()], &["INBOX".into()])
        .await
        .unwrap();
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
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"emailAddress": "me@example.com", "historyId": "5"})),
        )
        .mount(&server)
        .await;

    let api = format!("{}{API}", server.uri());
    let authorized = authorize(&oauth(&server), &api, &[], |consent_url| {
        let url = url::Url::parse(consent_url).unwrap();
        let q: HashMap<String, String> = url.query_pairs().into_owned().collect();
        let redirect = q["redirect_uri"].trim_start_matches("http://").to_string();
        let state = q["state"].clone();
        tokio::spawn(async move {
            let mut stream = tokio::net::TcpStream::connect(&redirect).await.unwrap();
            let request =
                format!("GET /?code=the-code&state={state} HTTP/1.1\r\nHost: {redirect}\r\n\r\n");
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

#[tokio::test]
async fn vacation_round_trips_through_gmail_settings() {
    let server = MockServer::start().await;
    mount_token(&server, 1).await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/settings/vacation")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "enableAutoReply": true,
            "responseSubject": "Away",
            "responseBodyHtml": "<div>Back on <b>Monday</b>.<br>Thanks &amp; bye</div>",
            "restrictToContacts": true,
            "startTime": "1700000000000"
        })))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path(format!("{API}/settings/vacation")))
        .and(body_json(json!({
            "enableAutoReply": false,
            "responseSubject": "Away",
            "responseBodyPlainText": "Back soon\n<ok>",
            "responseBodyHtml": "Back soon<br>&lt;ok&gt;",
            "restrictToContacts": false,
            "restrictToDomain": false,
            "endTime": "1800000000000"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    let client = client(&server);
    let vacation = client.vacation().await.unwrap();
    assert!(vacation.enabled && vacation.contacts_only);
    assert_eq!(vacation.subject, "Away");
    assert_eq!(vacation.body, "Back on Monday.\nThanks & bye");
    assert_eq!(vacation.start, Some(1_700_000_000_000));
    assert_eq!(vacation.end, None);

    let update = mailrs_domain::Vacation {
        enabled: false,
        subject: "Away".into(),
        body: "Back soon\n<ok>".into(),
        contacts_only: false,
        domain_only: false,
        start: None,
        end: Some(1_800_000_000_000),
    };
    client.set_vacation(&update).await.unwrap();
}

#[tokio::test]
async fn a_missing_scope_is_reported_as_such() {
    let server = MockServer::start().await;
    mount_token(&server, 1).await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/settings/vacation")))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "error": {"code": 403, "status": "PERMISSION_DENIED",
                      "details": [{"reason": "ACCESS_TOKEN_SCOPE_INSUFFICIENT"}]}
        })))
        .mount(&server)
        .await;
    assert!(matches!(
        client(&server).vacation().await,
        Err(GmailError::MissingScope)
    ));
}

#[tokio::test]
async fn the_default_identity_signature_comes_from_send_as() {
    let server = MockServer::start().await;
    mount_token(&server, 1).await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/settings/sendAs")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"sendAs": [
            {"sendAsEmail": "alias@example.com", "signature": "alias"},
            {"sendAsEmail": "me@example.com", "isDefault": true,
             "signature": "<div>Ann Lee<br>Maple &amp; Finch</div>"}
        ]})))
        .mount(&server)
        .await;
    let identities = client(&server).send_as().await.unwrap();
    let default = identities.iter().find(|s| s.is_default).unwrap();
    assert_eq!(
        mailrs_gmail::html_to_text(&default.signature),
        "Ann Lee\nMaple & Finch"
    );
}

#[tokio::test]
async fn a_draft_is_sent_by_id() {
    let server = MockServer::start().await;
    mount_token(&server, 1).await;
    Mock::given(method("POST"))
        .and(path(format!("{API}/drafts/send")))
        .and(body_json(json!({"id": "d1"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(message_json("m9")))
        .expect(1)
        .mount(&server)
        .await;
    assert_eq!(client(&server).send_draft("d1").await.unwrap().id, "m9");
}

#[tokio::test]
async fn filters_are_listed_created_and_deleted() {
    let server = MockServer::start().await;
    mount_token(&server, 1).await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/settings/filters")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"filter": [
            {"id": "f1", "criteria": {"from": "a@x.com"}, "action": {"addLabelIds": ["TRASH"]}}
        ]})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("{API}/settings/filters")))
        .and(body_json(json!({
            "criteria": {"from": "spam@x.com"},
            "action": {"addLabelIds": ["TRASH"], "removeLabelIds": ["INBOX"]}
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "f2", "criteria": {"from": "spam@x.com"},
            "action": {"addLabelIds": ["TRASH"], "removeLabelIds": ["INBOX"]}
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path(format!("{API}/settings/filters/f1")))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let client = client(&server);
    let listed = client.filters().await.unwrap();
    assert_eq!(listed[0].criteria.from.as_deref(), Some("a@x.com"));
    let created = client
        .create_filter(&mailrs_domain::Filter::block("spam@x.com"))
        .await
        .unwrap();
    assert_eq!(created.id.as_deref(), Some("f2"));
    client.delete_filter("f1").await.unwrap();
}

#[tokio::test]
async fn one_click_unsubscribe_posts_the_form() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/u/1"))
        .and(body_string_contains("List-Unsubscribe=One-Click"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    mailrs_gmail::one_click_unsubscribe(&format!("{}/u/1", server.uri()))
        .await
        .unwrap();
}

#[tokio::test]
async fn the_raw_message_is_decoded() {
    let server = MockServer::start().await;
    mount_token(&server, 1).await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/messages/m1")))
        .and(query_param("format", "raw"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"id": "m1", "raw": "U3ViamVjdDogSGkNCg0KYm9keQ"})),
        )
        .mount(&server)
        .await;
    assert_eq!(
        client(&server).raw_message("m1").await.unwrap(),
        b"Subject: Hi\r\n\r\nbody"
    );
}
