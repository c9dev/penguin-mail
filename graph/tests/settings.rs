mod common;

use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn a_rule_keeps_what_the_app_has_no_word_for() {
    let server = MockServer::start().await;
    common::token_endpoint(&server, "r").await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me/mailFolders/inbox/messageRules"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": [{
            "id": "r1", "displayName": "Boss", "sequence": 1, "isEnabled": true,
            "conditions": {"fromAddresses": [{"emailAddress": {"address": "boss@contoso.com"}}], "importance": "high"},
            "actions": {"markImportance": "high", "stopProcessingRules": true},
        }]})))
        .mount(&server)
        .await;
    let rules = common::graph(&server).rules().await.unwrap();
    let rule = &rules[0];
    assert_eq!(rule.conditions.as_ref().unwrap().from_addresses.len(), 1);
    assert!(
        rule.conditions
            .as_ref()
            .unwrap()
            .other
            .contains_key("importance")
    );
    assert!(
        rule.actions
            .as_ref()
            .unwrap()
            .other
            .contains_key("markImportance")
    );
}

#[tokio::test]
async fn the_automatic_reply_goes_inside_mailbox_settings() {
    let server = MockServer::start().await;
    common::token_endpoint(&server, "r").await;
    Mock::given(method("PATCH"))
        .and(path("/v1.0/me/mailboxSettings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&server)
        .await;
    let replies = mailrs_graph::AutomaticReplies {
        status: "alwaysEnabled".into(),
        external_audience: "all".into(),
        internal_reply_message: "<p>Away</p>".into(),
        external_reply_message: "<p>Away</p>".into(),
        ..Default::default()
    };
    common::graph(&server)
        .set_automatic_replies(&replies)
        .await
        .unwrap();
    let asked = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&asked.last().unwrap().body).unwrap();
    assert_eq!(body["automaticRepliesSetting"]["status"], "alwaysEnabled");
    assert!(
        body["automaticRepliesSetting"]
            .get("scheduledStartDateTime")
            .is_none()
    );
}
