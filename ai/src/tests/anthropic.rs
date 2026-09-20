use std::sync::Arc;

use serde_json::{Value, json};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::{FakeHost, Sequence, drain, sse};
use crate::providers::AnthropicChat;
use crate::{AgentEvent, AiError};

fn start(index: u64, block: Value) -> (Option<&'static str>, Value) {
    (
        Some("content_block_start"),
        json!({"type": "content_block_start", "index": index, "content_block": block}),
    )
}

fn delta(index: u64, delta: Value) -> (Option<&'static str>, Value) {
    (
        Some("content_block_delta"),
        json!({"type": "content_block_delta", "index": index, "delta": delta}),
    )
}

fn stop(index: u64) -> (Option<&'static str>, Value) {
    (
        Some("content_block_stop"),
        json!({"type": "content_block_stop", "index": index}),
    )
}

fn message_start() -> (Option<&'static str>, Value) {
    (
        Some("message_start"),
        json!({"type": "message_start", "message": {"id": "msg_1", "type": "message",
            "role": "assistant", "content": [], "stop_reason": null}}),
    )
}

fn message_end(stop_reason: &str) -> Vec<(Option<&'static str>, Value)> {
    vec![
        (
            Some("message_delta"),
            json!({"type": "message_delta", "delta": {"stop_reason": stop_reason},
                "usage": {"output_tokens": 10}}),
        ),
        (Some("message_stop"), json!({"type": "message_stop"})),
    ]
}

fn text_reply(text: &str) -> ResponseTemplate {
    let mut events = vec![
        message_start(),
        start(0, json!({"type": "text", "text": ""})),
        delta(0, json!({"type": "text_delta", "text": text})),
        stop(0),
    ];
    events.extend(message_end("end_turn"));
    sse(&events)
}

fn chat(server: &MockServer) -> AnthropicChat {
    AnthropicChat::new(
        server.uri(),
        "sk-ant-test".into(),
        "claude-opus-5".into(),
        "You sort mail.".into(),
    )
}

#[tokio::test]
async fn runs_tools_and_echoes_thinking_back_unchanged() {
    let server = MockServer::start().await;
    let mut events = vec![
        message_start(),
        start(
            0,
            json!({"type": "thinking", "thinking": "", "signature": ""}),
        ),
        delta(0, json!({"type": "thinking_delta", "thinking": "Need to "})),
        delta(0, json!({"type": "thinking_delta", "thinking": "search."})),
        delta(
            0,
            json!({"type": "signature_delta", "signature": "EqQBCgIYAhIM"}),
        ),
        stop(0),
        start(1, json!({"type": "text", "text": ""})),
        delta(1, json!({"type": "text_delta", "text": "Checking."})),
        stop(1),
        start(
            2,
            json!({"type": "tool_use", "id": "toolu_1", "name": "search_mail", "input": {}}),
        ),
        delta(
            2,
            json!({"type": "input_json_delta", "partial_json": "{\"query\": "}),
        ),
        delta(
            2,
            json!({"type": "input_json_delta", "partial_json": "\"invoice\"}"}),
        ),
        stop(2),
        start(
            3,
            json!({"type": "tool_use", "id": "toolu_2", "name": "fail", "input": {}}),
        ),
        stop(3),
        start(
            4,
            json!({"type": "tool_use", "id": "toolu_3", "name": "search_mail", "input": {}}),
        ),
        delta(
            4,
            json!({"type": "input_json_delta", "partial_json": "{\"query\": \"x"}),
        ),
        stop(4),
    ];
    events.extend(message_end("tool_use"));
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "sk-ant-test"))
        .and(header("anthropic-version", "2023-06-01"))
        .respond_with(Sequence::new(vec![
            sse(&events),
            text_reply("Two invoices."),
        ]))
        .expect(2)
        .mount(&server)
        .await;

    let host = Arc::new(FakeHost::default());
    let (tx, rx) = async_channel::unbounded();
    let mut chat = chat(&server);
    let reply = chat
        .send("Find invoices".into(), host.clone(), &tx)
        .await
        .unwrap();
    assert_eq!(reply, "Two invoices.");

    // The block with broken JSON never reached the host.
    assert_eq!(
        host.calls(),
        vec![
            ("search_mail".into(), json!({"query": "invoice"})),
            ("fail".into(), json!({})),
        ]
    );
    let seen = drain(&rx);
    assert_eq!(seen[0], AgentEvent::Text("Checking.".into()));
    assert_eq!(
        seen[1],
        AgentEvent::ToolStarted {
            name: "search_mail".into(),
            input: json!({"query": "invoice"}),
        }
    );
    assert_eq!(seen.last(), Some(&AgentEvent::Text("Two invoices.".into())));

    let requests = server.received_requests().await.unwrap();
    let first: Value = requests[0].body_json().unwrap();
    assert_eq!(first["max_tokens"], json!(64000));
    assert_eq!(first["stream"], json!(true));
    assert_eq!(first["system"], json!("You sort mail."));
    assert!(first.get("thinking").is_none());
    assert_eq!(first["tools"][0]["name"], json!("search_mail"));
    assert_eq!(
        first["tools"][0]["input_schema"]["required"],
        json!(["query"])
    );

    let second: Value = requests[1].body_json().unwrap();
    let messages = second["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 3);
    assert_eq!(
        messages[1],
        json!({"role": "assistant", "content": [
            {"type": "thinking", "thinking": "Need to search.", "signature": "EqQBCgIYAhIM"},
            {"type": "text", "text": "Checking."},
            {"type": "tool_use", "id": "toolu_1", "name": "search_mail", "input": {"query": "invoice"}},
            {"type": "tool_use", "id": "toolu_2", "name": "fail", "input": {}},
            {"type": "tool_use", "id": "toolu_3", "name": "search_mail", "input": {}},
        ]})
    );
    // Every result goes back in one user message.
    let results = &messages[2];
    assert_eq!(results["role"], json!("user"));
    let results = results["content"].as_array().unwrap();
    assert_eq!(results.len(), 3);
    assert_eq!(
        results[0],
        json!({"type": "tool_result", "tool_use_id": "toolu_1", "content": "{\"hits\":2}"})
    );
    assert_eq!(
        results[1],
        json!({"type": "tool_result", "tool_use_id": "toolu_2", "content": "no such label", "is_error": true})
    );
    assert_eq!(results[2]["tool_use_id"], json!("toolu_3"));
    assert_eq!(results[2]["is_error"], json!(true));
    let invalid: Value = serde_json::from_str(results[2]["content"].as_str().unwrap()).unwrap();
    assert_eq!(invalid, json!({"INVALID_JSON": "{\"query\": \"x"}));
}

#[tokio::test]
async fn refusal_is_an_error_and_leaves_history_clean() {
    let server = MockServer::start().await;
    let mut refused = vec![
        message_start(),
        start(
            0,
            json!({"type": "tool_use", "id": "toolu_1", "name": "search_mail", "input": {}}),
        ),
        delta(
            0,
            json!({"type": "input_json_delta", "partial_json": "{\"query\": \"a\"}"}),
        ),
        stop(0),
    ];
    refused.extend(message_end("refusal"));
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(Sequence::new(vec![sse(&refused), text_reply("Hello.")]))
        .mount(&server)
        .await;

    let host = Arc::new(FakeHost::default());
    let (tx, _rx) = async_channel::unbounded();
    let mut chat = chat(&server);
    let err = chat
        .send("Bad".into(), host.clone(), &tx)
        .await
        .unwrap_err();
    assert!(
        matches!(err, AiError::Api(ref m) if m.contains("declined")),
        "{err}"
    );
    assert!(host.calls().is_empty(), "a refused turn runs no tools");

    assert_eq!(chat.send("Hi".into(), host, &tx).await.unwrap(), "Hello.");
    let requests = server.received_requests().await.unwrap();
    let second: Value = requests[1].body_json().unwrap();
    assert_eq!(
        second["messages"],
        json!([{"role": "user", "content": "Hi"}])
    );
}

#[tokio::test]
async fn max_tokens_and_stream_errors_are_reported() {
    let server = MockServer::start().await;
    let mut cut = vec![
        message_start(),
        start(0, json!({"type": "text", "text": ""})),
        delta(0, json!({"type": "text_delta", "text": "A long"})),
        stop(0),
    ];
    cut.extend(message_end("max_tokens"));
    let overloaded = sse(&[
        message_start(),
        (
            Some("error"),
            json!({"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}}),
        ),
    ]);
    let unauthorized = ResponseTemplate::new(401).set_body_json(json!({
        "type": "error", "error": {"type": "authentication_error", "message": "invalid x-api-key"},
    }));
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(Sequence::new(vec![sse(&cut), overloaded, unauthorized]))
        .mount(&server)
        .await;

    let host = Arc::new(FakeHost::default());
    let (tx, _rx) = async_channel::unbounded();
    let mut chat = chat(&server);
    let err = chat.send("1".into(), host.clone(), &tx).await.unwrap_err();
    assert!(err.to_string().contains("64000-token limit"), "{err}");
    let err = chat.send("2".into(), host.clone(), &tx).await.unwrap_err();
    assert!(err.to_string().contains("Overloaded"), "{err}");
    let err = chat.send("3".into(), host, &tx).await.unwrap_err();
    assert!(err.to_string().contains("rejected the API key"), "{err}");
}

#[tokio::test]
async fn lists_models_across_pages_with_their_names() {
    let server = MockServer::start().await;
    // Recorded from GET /v1/models, cut to two models a page.
    let first = ResponseTemplate::new(200).set_body_json(json!({
        "data": [
            {"type": "model", "id": "claude-opus-5", "display_name": "Claude Opus 5",
             "created_at": "2026-02-05T00:00:00Z"},
            {"type": "model", "id": "claude-sonnet-4-5-20250929",
             "display_name": "Claude Sonnet 4.5", "created_at": "2025-09-29T00:00:00Z"}
        ],
        "first_id": "claude-opus-5",
        "last_id": "claude-sonnet-4-5-20250929",
        "has_more": true,
    }));
    let second = ResponseTemplate::new(200).set_body_json(json!({
        "data": [
            {"type": "model", "id": "claude-haiku-4-5-20251001",
             "display_name": "Claude Haiku 4.5", "created_at": "2025-10-01T00:00:00Z"}
        ],
        "first_id": "claude-haiku-4-5-20251001",
        "last_id": "claude-haiku-4-5-20251001",
        "has_more": false,
    }));
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .and(header("x-api-key", "sk-ant-test"))
        .and(header("anthropic-version", "2023-06-01"))
        .respond_with(Sequence::new(vec![first, second]))
        .mount(&server)
        .await;
    let models = crate::providers::anthropic_models(&server.uri(), "sk-ant-test")
        .await
        .unwrap();
    assert_eq!(models.note, None);
    assert_eq!(
        models
            .models
            .iter()
            .map(|m| (m.id.as_str(), m.name.as_str(), m.alias))
            .collect::<Vec<_>>(),
        vec![
            ("claude-opus-5", "Claude Opus 5", false),
            ("claude-sonnet-4-5-20250929", "Claude Sonnet 4.5", false),
            ("claude-haiku-4-5-20251001", "Claude Haiku 4.5", false),
        ]
    );
    // The second page asks for what follows the last id of the first.
    let asked: Vec<String> = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|r| r.url.query().unwrap_or_default().to_string())
        .collect();
    assert_eq!(asked.len(), 2);
    assert!(
        asked[1].contains("after_id=claude-sonnet-4-5-20250929"),
        "{asked:?}"
    );
}

#[tokio::test]
async fn says_plainly_when_the_key_is_missing_or_refused() {
    let err = crate::providers::anthropic_models("http://127.0.0.1:1", "  ")
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("Add an Anthropic API key"),
        "{err}"
    );

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "type": "error",
            "error": {"type": "authentication_error", "message": "invalid x-api-key"},
        })))
        .mount(&server)
        .await;
    let err = crate::providers::anthropic_models(&server.uri(), "sk-ant-wrong")
        .await
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("rejected the API key: invalid x-api-key"),
        "{err}"
    );
}
