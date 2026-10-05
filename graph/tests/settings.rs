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

fn away() -> mailrs_graph::AutomaticReplies {
    mailrs_graph::AutomaticReplies {
        status: "alwaysEnabled".into(),
        external_audience: "all".into(),
        internal_reply_message: "<p>Away</p>".into(),
        external_reply_message: "<p>Away</p>".into(),
        ..Default::default()
    }
}

#[tokio::test]
async fn the_automatic_reply_goes_inside_mailbox_settings() {
    let server = MockServer::start().await;
    common::token_endpoint(&server, "r").await;
    Mock::given(method("PATCH"))
        .and(path("/v1.0/me/mailboxSettings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "automaticRepliesSetting": {"status": "alwaysEnabled", "externalAudience": "all"},
        })))
        .mount(&server)
        .await;
    common::graph(&server).set_automatic_replies(&away()).await.unwrap();
    let asked = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&asked.last().unwrap().body).unwrap();
    assert_eq!(
        body,
        json!({"automaticRepliesSetting": {
            "status": "alwaysEnabled",
            "externalAudience": "all",
            "internalReplyMessage": "<p>Away</p>",
            "externalReplyMessage": "<p>Away</p>",
        }}),
        "the documented request, with no schedule for a reply that is always on"
    );
}

#[tokio::test]
async fn a_save_answers_what_graph_kept() {
    let server = MockServer::start().await;
    common::token_endpoint(&server, "r").await;
    // Outlook.com answers 200 to an always-on reply and leaves it off.
    Mock::given(method("PATCH"))
        .and(path("/v1.0/me/mailboxSettings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "automaticRepliesSetting": {
                "status": "disabled",
                "externalAudience": "none",
                "internalReplyMessage": "",
                "externalReplyMessage": "",
            },
        })))
        .mount(&server)
        .await;
    let kept = common::graph(&server).set_automatic_replies(&away()).await.unwrap();
    assert_eq!(kept.status, "disabled");
}

#[tokio::test]
async fn a_save_whose_answer_leaves_out_the_reply_reads_it_back() {
    let server = MockServer::start().await;
    common::token_endpoint(&server, "r").await;
    Mock::given(method("PATCH"))
        .and(path("/v1.0/me/mailboxSettings"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me/mailboxSettings/automaticRepliesSetting"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "alwaysEnabled",
            "externalAudience": "all",
            "internalReplyMessage": "<html><body><p>Away</p></body></html>",
            "externalReplyMessage": "<html><body><p>Away</p></body></html>",
        })))
        .mount(&server)
        .await;
    let kept = common::graph(&server).set_automatic_replies(&away()).await.unwrap();
    assert_eq!(kept.status, "alwaysEnabled");
    assert!(kept.internal_reply_message.contains("Away"));
}
