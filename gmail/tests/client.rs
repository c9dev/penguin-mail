use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use mailrs_gmail::{
    GMAIL_SCOPE, GmailClient, GmailError, Granted, HistoryChange, OAuthClient, SETTINGS_SCOPE,
    authorize,
};
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
    GmailClient::new(oauth(server), "rt".into())
        .with_base_url(format!("{}{API}", server.uri()))
        .with_people_url(format!("{}/v1", server.uri()))
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
async fn a_refresh_that_grants_less_tells_the_hook() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "at-1", "expires_in": 3600,
            "scope": format!("{GMAIL_SCOPE} {SETTINGS_SCOPE}"),
        })))
        .expect(1)
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
    let told: Arc<Mutex<Option<Granted>>> = Arc::new(Mutex::new(None));
    let seen = Arc::clone(&told);
    let client = client(&server)
        .with_granted(Some(Granted::parse(
            "https://www.googleapis.com/auth/gmail.modify \
             https://www.googleapis.com/auth/gmail.settings.basic \
             https://www.googleapis.com/auth/calendar.events",
        )))
        .on_granted(move |granted| {
            *seen.lock().unwrap() = Some(granted.clone());
        });
    client.profile().await.unwrap();
    let smaller = Granted::parse(&format!("{GMAIL_SCOPE} {SETTINGS_SCOPE}"));
    assert_eq!(*told.lock().unwrap(), Some(smaller.clone()));
    assert_eq!(client.granted(), Some(smaller));
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
async fn a_labelled_listing_names_the_label_by_id() {
    let server = MockServer::start().await;
    mount_token(&server, 1).await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/messages")))
        .and(query_param("q", "newer_than:30d"))
        .and(query_param("labelIds", "Label_7"))
        .and(query_param("maxResults", "500"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "messages": [{"id": "a", "threadId": "t"}]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let page = client(&server)
        .list_labelled("Label_7", "newer_than:30d", None, 500)
        .await
        .unwrap();
    assert_eq!(page.messages.len(), 1);
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
async fn a_metadata_fetch_asks_for_the_unsubscribe_headers() {
    let server = MockServer::start().await;
    mount_token(&server, 1).await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/messages/m1")))
        .and(query_param("format", "metadata"))
        .and(query_param("metadataHeaders", "List-Unsubscribe"))
        .and(query_param("metadataHeaders", "List-Unsubscribe-Post"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "m1",
            "threadId": "t1",
            "payload": {"headers": [
                {"name": "List-Unsubscribe", "value": "<https://news.example/u/1>"},
                {"name": "List-Unsubscribe-Post", "value": "List-Unsubscribe=One-Click"},
            ]},
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/threads/t1")))
        .and(query_param("metadataHeaders", "List-Unsubscribe"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "t1", "messages": []})))
        .expect(1)
        .mount(&server)
        .await;

    let client = client(&server);
    let message = client.message_metadata("m1").await.unwrap();
    let headers = &message.payload.unwrap().headers;
    assert_eq!(headers.len(), 2, "both headers came back: {headers:?}");
    assert!(
        client
            .thread_metadata("t1")
            .await
            .unwrap()
            .messages
            .is_empty()
    );
}

/// A 200 whose body is not what Gmail documents is a decode error, which
/// sync retries, never an empty page. An answer without `historyId` read
/// as an empty page would move the sync cursor back to 0.
#[tokio::test]
async fn a_malformed_answer_is_a_decode_error() {
    let server = MockServer::start().await;
    mount_token(&server, 1).await;
    for (start, body) in [
        ("10", r#"{"history": [{"id": "11", "#),
        ("20", ""),
        ("30", r#"{"history": []}"#),
        ("40", r#"{"historyId": "not a number"}"#),
    ] {
        Mock::given(method("GET"))
            .and(path(format!("{API}/history")))
            .and(query_param("startHistoryId", start))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path(format!("{API}/messages/x")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "x"})))
        .mount(&server)
        .await;

    let client = client(&server);
    for start in [10, 20, 30, 40] {
        let answer = client.history(start, None).await;
        assert!(
            matches!(answer, Err(GmailError::Decode(_))),
            "start {start}: {answer:?}"
        );
    }
    let answer = client.message_metadata("x").await;
    assert!(matches!(answer, Err(GmailError::Decode(_))), "{answer:?}");
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
async fn a_batch_changes_every_message_in_one_post() {
    let server = MockServer::start().await;
    mount_token(&server, 1).await;
    Mock::given(method("POST"))
        .and(path(format!("{API}/messages/batchModify")))
        .and(body_json(json!({
            "ids": ["m1", "m2", "m3"],
            "addLabelIds": ["TRASH"],
            "removeLabelIds": ["INBOX"],
        })))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("{API}/messages/batchDelete")))
        .and(body_json(json!({"ids": ["m1", "m2"]})))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let client = client(&server);
    let ids = ["m1".to_string(), "m2".to_string(), "m3".to_string()];

    client
        .batch_modify(&ids, &["TRASH".into()], &["INBOX".into()])
        .await
        .unwrap();
    client.batch_delete(&ids[..2]).await.unwrap();
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
    let authorized = authorize(&oauth(&server), &api, |consent_url| {
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
        mailrs_mime::html::html_to_text(&default.signature),
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
async fn a_one_click_refusal_names_the_lists_server_and_its_answer() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let err = mailrs_gmail::one_click_unsubscribe(&format!("{}/u/1", server.uri()))
        .await
        .unwrap_err();
    assert_eq!(
        err,
        mailrs_gmail::OneClickError::Refused {
            host: "127.0.0.1".to_string(),
            status: 404,
        }
    );
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

#[tokio::test]
async fn contacts_page_through_and_hand_back_a_sync_token() {
    let server = MockServer::start().await;
    mount_token(&server, 1).await;
    let first = include_str!("people/connections-page-1.json");
    let last = include_str!("people/connections-page-2.json");
    Mock::given(method("GET"))
        .and(path("/v1/people/me/connections"))
        .and(header("authorization", "Bearer at-1"))
        .and(query_param(
            "personFields",
            mailrs_gmail::people::PERSON_FIELDS,
        ))
        .and(query_param("requestSyncToken", "true"))
        .and(query_param("pageToken", "page-2"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(last, "application/json"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/people/me/connections"))
        .and(query_param("syncToken", "sync-token-1"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({"error": {
            "code": 400,
            "status": "FAILED_PRECONDITION",
            "details": [{"reason": "EXPIRED_SYNC_TOKEN"}],
        }})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/people/me/connections"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(first, "application/json"))
        .mount(&server)
        .await;

    let client = client(&server);
    let page = client.connections(None, None).await.unwrap();
    assert_eq!(page.people.len(), 2);
    assert_eq!(page.next_page_token.as_deref(), Some("page-2"));

    let page = client.connections(Some("page-2"), None).await.unwrap();
    assert_eq!(page.next_sync_token.as_deref(), Some("sync-token-1"));

    assert!(matches!(
        client.connections(None, Some("sync-token-1")).await,
        Err(GmailError::ExpiredSyncToken)
    ));
}

#[tokio::test]
async fn a_contact_photo_arrives_as_plain_bytes() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/photos/mara"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(b"jpeg-bytes".to_vec(), "image/jpeg"))
        .mount(&server)
        .await;
    let photo = client(&server)
        .contact_photo(&format!("{}/photos/mara", server.uri()))
        .await
        .unwrap();
    assert_eq!(photo, b"jpeg-bytes");
}

#[tokio::test]
async fn a_label_says_how_many_conversations_carry_it() {
    let server = MockServer::start().await;
    mount_token(&server, 1).await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/labels/Label_7")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "Label_7", "name": "Kites", "messagesTotal": 40, "threadsTotal": 31,
        })))
        .mount(&server)
        .await;
    assert_eq!(client(&server).label_threads("Label_7").await.unwrap(), 31);
}

#[tokio::test]
async fn a_new_contact_posts_the_fields_it_names() {
    let server = MockServer::start().await;
    mount_token(&server, 1).await;
    Mock::given(method("POST"))
        .and(path("/v1/people:createContact"))
        .and(header("authorization", "Bearer at-1"))
        .and(body_json(json!({
            "names": [{"unstructuredName": "Priya Shah"}],
            "emailAddresses": [{"value": "priya@example.org"}],
            "organizations": [{"name": "Fernwood"}],
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "resourceName": "people/c9",
            "etag": "%Ej4",
            "names": [{"displayName": "Priya Shah"}],
            "emailAddresses": [{"value": "priya@example.org"}],
            "organizations": [{"name": "Fernwood"}],
        })))
        .expect(1)
        .mount(&server)
        .await;
    let made = client(&server)
        .create_contact(&mailrs_gmail::ContactFields {
            name: Some("Priya Shah".into()),
            emails: Some(vec!["priya@example.org".into()]),
            organization: Some("Fernwood".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(made.resource, "people/c9");
    assert_eq!(made.organization.as_deref(), Some("Fernwood"));
}

/// Google refuses a change that does not carry the contact's etag, so the
/// client reads it first and hands it back with the fields that change.
#[tokio::test]
async fn a_contact_change_hands_back_the_etag_and_names_its_fields() {
    let server = MockServer::start().await;
    mount_token(&server, 1).await;
    Mock::given(method("GET"))
        .and(path("/v1/people/c9"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"resourceName": "people/c9", "etag": "%Ej4", "metadata":{"sources":[{"type":"CONTACT","id":"c9","etag":"source-v1"}]}})),
        )
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/v1/people/c9:updateContact"))
        .and(query_param("updatePersonFields", "phoneNumbers"))
        .and(body_json(json!({
            "etag": "%Ej4",
            "metadata":{"sources":[{"type":"CONTACT","id":"c9","etag":"source-v1"}]},
            "phoneNumbers": [{"value": "+351 21 000 0000"}],
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "resourceName": "people/c9",
            "names": [{"displayName": "Priya Shah"}],
            "emailAddresses": [{"value": "priya@example.org"}],
            "phoneNumbers": [{"value": "+351 21 000 0000"}],
        })))
        .expect(1)
        .mount(&server)
        .await;
    let changed = client(&server)
        .update_contact(
            "people/c9",
            &mailrs_gmail::ContactFields {
                phones: Some(vec!["+351 21 000 0000".into()]),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(changed.phone.as_deref(), Some("+351 21 000 0000"));
    assert_eq!(changed.emails, ["priya@example.org"]);
}

#[tokio::test]
async fn writing_a_contact_without_the_permission_says_so() {
    let server = MockServer::start().await;
    mount_token(&server, 1).await;
    Mock::given(method("POST"))
        .and(path("/v1/people:createContact"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({"error": {
            "code": 403,
            "status": "PERMISSION_DENIED",
            "message": "Request had insufficient authentication scopes.",
            "details": [{"reason": "ACCESS_TOKEN_SCOPE_INSUFFICIENT"}],
        }})))
        .mount(&server)
        .await;
    let refused = client(&server)
        .create_contact(&mailrs_gmail::ContactFields {
            name: Some("Priya".into()),
            ..Default::default()
        })
        .await;
    assert!(
        matches!(refused, Err(GmailError::MissingScope)),
        "{refused:?}"
    );
}

/// Gmail answers the filter list of an account that has none with an empty
/// body, not `{}`. The Rules dialog showed a decode error for it.
#[tokio::test]
async fn an_account_with_no_filters_lists_none() {
    let server = MockServer::start().await;
    mount_token(&server, 1).await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/settings/filters")))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    assert_eq!(client(&server).filters().await.unwrap(), vec![]);
}

/// A Google Cloud project without the People API switched on refuses before
/// any question of permission, so asking the person to grant access would
/// change nothing. The error names the API and the page that turns it on.
#[tokio::test]
async fn an_api_switched_off_in_the_project_says_which_and_where() {
    let server = MockServer::start().await;
    mount_token(&server, 1).await;
    let url =
        "https://console.developers.google.com/apis/api/people.googleapis.com/overview?project=7";
    Mock::given(method("GET"))
        .and(path("/v1/people/me/connections"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({"error": {
            "code": 403,
            "message": "People API has not been used in project 7 before or it is disabled.",
            "status": "PERMISSION_DENIED",
            "details": [{
                "@type": "type.googleapis.com/google.rpc.ErrorInfo",
                "reason": "SERVICE_DISABLED",
                "metadata": {"serviceTitle": "People API", "activationUrl": url}
            }]
        }})))
        .mount(&server)
        .await;
    match client(&server).connections(None, None).await {
        Err(GmailError::ApiDisabled {
            service,
            enable_url,
        }) => {
            assert_eq!(service, "People API");
            assert_eq!(enable_url, url);
        }
        other => panic!("unexpected result: {other:?}"),
    }
}

#[tokio::test]
async fn an_answer_over_the_json_cap_is_refused_not_read_whole() {
    let server = MockServer::start().await;
    mount_token(&server, 1).await;
    let padding = "x".repeat(17 << 20);
    Mock::given(method("GET"))
        .and(path(format!("{API}/profile")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"emailAddress": padding, "historyId": "1"})),
        )
        .mount(&server)
        .await;
    let err = client(&server).profile().await.unwrap_err();
    assert!(matches!(err, GmailError::TooLarge { .. }), "{err:?}");
}

#[tokio::test]
async fn a_contact_photo_over_the_cap_is_refused() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/photo"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![0u8; 9 << 20]))
        .mount(&server)
        .await;
    let url = format!("{}/photo", server.uri());
    let err = client(&server).contact_photo(&url).await.unwrap_err();
    assert!(matches!(err, GmailError::TooLarge { .. }), "{err:?}");
}

#[tokio::test]
async fn a_photo_under_the_cap_comes_back_whole() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/photo"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![7u8; 100_000]))
        .mount(&server)
        .await;
    let url = format!("{}/photo", server.uri());
    assert_eq!(
        client(&server).contact_photo(&url).await.unwrap().len(),
        100_000
    );
}

#[tokio::test]
async fn a_trash_listing_explicitly_includes_deleted_mail() {
    let server = MockServer::start().await;
    mount_token(&server, 1).await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/messages")))
        .and(query_param("labelIds", "TRASH"))
        .and(query_param("includeSpamTrash", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "messages": [{"id": "a", "threadId": "t"}]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let page = client(&server).list_labelled("TRASH", "", None, 500).await;
    let urls: Vec<_> = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.url.to_string())
        .collect();
    assert!(page.is_ok(), "{page:?}: {urls:?}");
    assert_eq!(page.unwrap().messages.len(), 1);
}

#[tokio::test]
async fn expanded_google_contacts_keep_all_fields_and_resolve_group_names() {
    let server = MockServer::start().await;
    mount_token(&server, 1).await;
    Mock::given(method("GET")).and(path("/v1/contactGroups"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"contactGroups":[
            {"resourceName":"contactGroups/g1","name":"Friends","groupType":"USER_CONTACT_GROUP"},
            {"resourceName":"contactGroups/g2","name":"Work","groupType":"USER_CONTACT_GROUP"},
            {"resourceName":"contactGroups/myContacts","name":"My Contacts","groupType":"SYSTEM_CONTACT_GROUP"}
        ]}))).mount(&server).await;
    Mock::given(method("GET")).and(path("/v1/people/me/connections"))
        .and(query_param("personFields", mailrs_gmail::people::PERSON_FIELDS))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"connections":[{
            "resourceName":"people/c1", "etag":"v1", "names":[{"displayName":"Demo Contact"}],
            "phoneNumbers":[{"value":"+1 202 555 0101"},{"value":"+1 202 555 0102"}],
            "birthdays":[{"date":{"month":4,"day":23}}],
            "biographies":[{"value":"Demo notes"}],
            "addresses":[{"streetAddress":"Example Street","city":"Example City"}],
            "urls":[{"value":"https://example.org"}],
            "events":[{"type":"anniversary","date":{"year":2020,"month":6,"day":8}}],
            "userDefined":[{"key":"Customer ID","value":"demo"}],
            "memberships":[{"contactGroupMembership":{"contactGroupResourceName":"contactGroups/g1"}},
              {"contactGroupMembership":{"contactGroupResourceName":"contactGroups/g2"}},
              {"contactGroupMembership":{"contactGroupResourceName":"contactGroups/myContacts"}}]
        }]}))).mount(&server).await;
    let p = client(&server)
        .connections(None, None)
        .await
        .unwrap()
        .people
        .remove(0);
    assert!(p.emails.is_empty());
    assert_eq!(p.details.groups, ["Friends", "Work"]);
    assert_eq!(p.details.phones.len(), 2);
    assert_eq!(p.details.birthday, "--04-23");
    assert_eq!(p.details.dates[0].value, "2020-06-08");
    assert_eq!(p.details.custom[0].label, "Customer ID");
    assert_eq!(p.details.revision, "v1");
    assert_eq!(p.details.urls, ["https://example.org"]);
    assert!(p.details.addresses[0].contains("Example City"));
}

#[tokio::test]
async fn google_edits_keep_source_etags_and_phone_labels_and_reject_stale_writes() {
    let server = MockServer::start().await;
    mount_token(&server, 1).await;
    Mock::given(method("GET")).and(path("/v1/people/c1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"resourceName":"people/c1","etag":"v2",
        "metadata":{"sources":[{"type":"CONTACT","id":"c1","etag":"source-v2"}]},
        "phoneNumbers":[{"value":"+1 202 555 0101","type":"work"},{"value":"+1 202 555 0102","type":"home"}]})))
        .mount(&server).await;
    Mock::given(method("PATCH")).and(path("/v1/people/c1:updateContact"))
        .and(body_json(json!({"etag":"v2","metadata":{"sources":[{"type":"CONTACT","id":"c1","etag":"source-v2"}]},
        "phoneNumbers":[{"value":"+1 202 555 0102","type":"home"}],"birthdays":[{"date":{"year":0,"month":4,"day":23}}],
        "biographies":[],"userDefined":[{"key":"Customer ID","value":"demo"}]})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"resourceName":"people/c1","etag":"v3"})))
        .expect(1).mount(&server).await;
    let client = client(&server);
    let mut fields = mailrs_gmail::ContactFields {
        expected_revision: Some("v1".into()),
        phones: Some(vec!["+1 202 555 0102".into()]),
        details: mailrs_domain::contact::Changes {
            birthday: Some("--04-23".into()),
            notes: Some(String::new()),
            custom: Some(vec![mailrs_domain::contact::Field {
                label: "Customer ID".into(),
                value: "demo".into(),
            }]),
            ..Default::default()
        },
        ..Default::default()
    };
    assert!(matches!(
        client.update_contact("people/c1", &fields).await,
        Err(GmailError::Changed)
    ));
    assert!(matches!(
        client.delete_contact("people/c1", Some("v1")).await,
        Err(GmailError::Changed)
    ));
    fields.expected_revision = Some("v2".into());
    assert_eq!(
        client
            .update_contact("people/c1", &fields)
            .await
            .unwrap()
            .details
            .revision,
        "v3"
    );
    Mock::given(method("DELETE"))
        .and(path("/v1/people/c1:deleteContact"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    client
        .delete_contact("people/c1", Some("v2"))
        .await
        .unwrap();
}

#[tokio::test]
async fn google_group_edits_create_labels_and_preserve_system_memberships() {
    let server = MockServer::start().await;
    mount_token(&server, 1).await;
    Mock::given(method("GET"))
        .and(path("/v1/people/c1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"resourceName":"people/c1","etag":"v1",
        "metadata":{"sources":[{"type":"CONTACT","etag":"s1"}]},
        "memberships":[{"contactGroupMembership":{"contactGroupResourceName":"contactGroups/old"}},
         {"contactGroupMembership":{"contactGroupResourceName":"contactGroups/starred"}}]}),
        ))
        .mount(&server)
        .await;
    Mock::given(method("GET")).and(path("/v1/contactGroups"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"contactGroups":[
        {"resourceName":"contactGroups/old","name":"Old","groupType":"USER_CONTACT_GROUP"},
        {"resourceName":"contactGroups/starred","name":"Starred","groupType":"SYSTEM_CONTACT_GROUP"}]}))).mount(&server).await;
    Mock::given(method("POST")).and(path("/v1/contactGroups"))
        .and(body_json(json!({"contactGroup":{"name":"New"}})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"resourceName":"contactGroups/new","name":"New","groupType":"USER_CONTACT_GROUP"})))
        .expect(1).mount(&server).await;
    Mock::given(method("PATCH")).and(path("/v1/people/c1:updateContact"))
        .and(query_param("updatePersonFields","memberships"))
        .and(body_json(json!({"etag":"v1","metadata":{"sources":[{"type":"CONTACT","etag":"s1"}]},"memberships":[
        {"contactGroupMembership":{"contactGroupResourceName":"contactGroups/starred"}},
        {"contactGroupMembership":{"contactGroupResourceName":"contactGroups/new"}}]})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"resourceName":"people/c1","etag":"v2"}))).expect(1).mount(&server).await;
    let client = client(&server);
    let mut fields = mailrs_gmail::ContactFields {
        expected_groups: Some(vec!["Different".into()]),
        details: mailrs_domain::contact::Changes {
            groups: Some(vec!["New".into()]),
            ..Default::default()
        },
        ..Default::default()
    };
    assert!(matches!(
        client.update_contact("people/c1", &fields).await,
        Err(GmailError::Changed)
    ));
    fields.expected_groups = Some(vec!["Old".into()]);
    client.update_contact("people/c1", &fields).await.unwrap();
}

#[tokio::test]
async fn a_google_photo_retry_does_not_create_a_second_contact() {
    let server = MockServer::start().await;
    mount_token(&server, 1).await;
    let saved = json!({"resourceName":"people/c1","etag":"v1","names":[{"displayName":"Demo"}]});
    Mock::given(method("POST"))
        .and(path("/v1/people:createContact"))
        .respond_with(ResponseTemplate::new(200).set_body_json(saved.clone()))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/people/c1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(saved.clone()))
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/v1/people/c1:updateContact"))
        .respond_with(ResponseTemplate::new(200).set_body_json(saved.clone()))
        .mount(&server)
        .await;
    let failed = Mock::given(method("PATCH"))
        .and(path("/v1/people/c1:updateContactPhoto"))
        .respond_with(ResponseTemplate::new(400))
        .mount_as_scoped(&server)
        .await;
    let client = client(&server);
    let fields = mailrs_gmail::ContactFields {
        name: Some("Demo".into()),
        creation_key: Some("editor-1".into()),
        photo: Some(b"\x89PNG\r\n\x1a\n".to_vec()),
        ..Default::default()
    };
    assert!(client.create_contact(&fields).await.is_err());
    drop(failed);
    Mock::given(method("PATCH"))
        .and(path("/v1/people/c1:updateContactPhoto"))
        .and(body_json(
            json!({"photoBytes":"iVBORw0KGgo=","personFields":mailrs_gmail::people::PERSON_FIELDS}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"person":saved})))
        .expect(1)
        .mount(&server)
        .await;
    assert_eq!(
        client.create_contact(&fields).await.unwrap().resource,
        "people/c1"
    );
    Mock::given(method("DELETE"))
        .and(path("/v1/people/c1:deleteContactPhoto"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"person":saved})))
        .expect(1)
        .mount(&server)
        .await;
    client
        .update_contact(
            "people/c1",
            &mailrs_gmail::ContactFields {
                photo: Some(vec![]),
                ..Default::default()
            },
        )
        .await
        .unwrap();
}
