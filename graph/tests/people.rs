mod common;

use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn a_contact_without_a_photo_answers_none() {
    let server = MockServer::start().await;
    common::token_endpoint(&server, "r").await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me/contacts/c1/photo/$value"))
        .respond_with(
            ResponseTemplate::new(404)
                .set_body_json(json!({"error": {"code": "ErrorItemNotFound"}})),
        )
        .mount(&server)
        .await;
    assert_eq!(
        common::graph(&server)
            .contact_photo("c1", 1 << 20)
            .await
            .unwrap(),
        None
    );
}
