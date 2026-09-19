use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use mailrs_gmail::{GmailClient, OAuthClient};
use serde_json::json;
use wiremock::matchers::{body_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const API: &str = "/gmail/v1/users/me";

async fn setup() -> (MockServer, GmailClient) {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"access_token": "at", "expires_in": 3600})),
        )
        .mount(&server)
        .await;
    let oauth = OAuthClient::new("cid", "secret").with_endpoints(
        format!("{}/auth", server.uri()),
        format!("{}/token", server.uri()),
    );
    let client =
        GmailClient::new(oauth, "rt".into()).with_base_url(format!("{}{API}", server.uri()));
    (server, client)
}

fn draft_json(id: &str, message: &str) -> serde_json::Value {
    json!({"id": id, "message": {"id": message, "threadId": "t1", "labelIds": ["DRAFT"]}})
}

#[tokio::test]
async fn send_posts_base64url_mime_with_the_thread() {
    let (server, client) = setup().await;
    let raw = b"Subject: hi\r\n\r\nbody?>";
    Mock::given(method("POST"))
        .and(path(format!("{API}/messages/send")))
        .and(body_json(
            json!({"raw": URL_SAFE_NO_PAD.encode(raw), "threadId": "t1"}),
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"id": "m9", "threadId": "t1"})),
        )
        .expect(1)
        .mount(&server)
        .await;
    assert_eq!(client.send(raw, Some("t1")).await.unwrap().id, "m9");
}

#[tokio::test]
async fn drafts_are_created_updated_listed_and_deleted() {
    let (server, client) = setup().await;
    let raw = URL_SAFE_NO_PAD.encode(b"Subject: draft\r\n\r\nhello");
    Mock::given(method("POST"))
        .and(path(format!("{API}/drafts")))
        .and(body_json(json!({"message": {"raw": raw}})))
        .respond_with(ResponseTemplate::new(200).set_body_json(draft_json("d1", "m1")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path(format!("{API}/drafts/d1")))
        .and(body_json(
            json!({"id": "d1", "message": {"raw": raw, "threadId": "t1"}}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(draft_json("d1", "m2")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/drafts")))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"drafts": [draft_json("d1", "m2")]})),
        )
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path(format!("{API}/drafts/d1")))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let bytes = b"Subject: draft\r\n\r\nhello";
    let created = client.create_draft(bytes, None).await.unwrap();
    assert_eq!(
        (created.id.as_str(), created.message.id.as_str()),
        ("d1", "m1")
    );
    let updated = client.update_draft("d1", bytes, Some("t1")).await.unwrap();
    assert_eq!(updated.message.id, "m2");
    assert_eq!(client.list_drafts().await.unwrap()[0].message.id, "m2");
    client.delete_draft("d1").await.unwrap();
}

#[tokio::test]
async fn send_as_reports_the_default_identity() {
    let (server, client) = setup().await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/settings/sendAs")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"sendAs": [
            {"sendAsEmail": "alias@example.com", "displayName": "Alias"},
            {"sendAsEmail": "me@example.com", "displayName": "Me Myself", "isDefault": true, "isPrimary": true}
        ]})))
        .mount(&server)
        .await;
    let identities = client.send_as().await.unwrap();
    let default = identities.iter().find(|s| s.is_default).unwrap();
    assert_eq!(default.display_name, "Me Myself");
}

#[tokio::test]
async fn attachments_are_decoded() {
    let (server, client) = setup().await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/messages/m1/attachments/a1")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "size": 3, "data": URL_SAFE_NO_PAD.encode([0xff, 0x00, 0x7f])
        })))
        .mount(&server)
        .await;
    assert_eq!(
        client.attachment("m1", "a1").await.unwrap(),
        vec![0xff, 0x00, 0x7f]
    );
}
