mod common;

use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn shared_mail_targets_the_mailbox_while_identity_stays_with_the_owner() {
    let server = MockServer::start().await;
    common::token_endpoint(&server, "rotated").await;
    Mock::given(method("GET"))
        .and(path("/v1.0/users/sales@contoso.com/mailFolders"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": []})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1.0/users/sales@contoso.com/sendMail"))
        .respond_with(ResponseTemplate::new(202))
        .expect(1)
        .mount(&server)
        .await;
    let owner = common::graph(&server);
    let shared = owner.shared_mailbox("sales@contoso.com").unwrap();
    Mock::given(method("GET"))
        .and(path("/v1.0/me/mailFolders"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": []})))
        .expect(1)
        .mount(&server)
        .await;
    shared.folders(None, None).await.unwrap();
    owner.folders(None, None).await.unwrap();
    shared
        .send_mime(b"From: sales@contoso.com\r\n\r\nHello")
        .await
        .unwrap();
    assert!(shared.categories().await.unwrap().is_empty());
    assert!(owner.shared_mailbox("../me").is_err());
    assert!(owner.shared_mailbox("sales@contoso.com/../me").is_err());
}

#[tokio::test]
async fn shared_writes_in_batches_never_use_the_owners_mailbox() {
    let server = MockServer::start().await;
    common::token_endpoint(&server, "rotated").await;
    Mock::given(method("POST"))
        .and(path("/v1.0/$batch"))
        .respond_with(|request: &wiremock::Request| {
            let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
            let calls = body["requests"].as_array().unwrap();
            assert_eq!(calls[0]["url"], "/users/sales@contoso.com/messages/m1/move");
            ResponseTemplate::new(200).set_body_json(json!({"responses": [
                {"id": calls[0]["id"], "status": 200, "body": {}}
            ]}))
        })
        .expect(1)
        .mount(&server)
        .await;
    let shared = common::graph(&server)
        .shared_mailbox("sales@contoso.com")
        .unwrap();
    shared
        .apply(&[mailrs_graph::Write::Move {
            id: "m1".into(),
            folder: "inbox".into(),
        }])
        .await
        .unwrap()[0]
        .as_ref()
        .unwrap();
}
