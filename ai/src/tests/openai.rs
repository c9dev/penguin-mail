use std::sync::Arc;

use serde_json::{Value, json};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::{FakeHost, Sequence, drain};
use crate::providers::OpenAiChat;
use crate::{AgentEvent, ProviderConfig, list_models};

/// An OpenAI-style stream: `data:` lines ending in `[DONE]`.
fn stream(chunks: &[Value]) -> ResponseTemplate {
    let mut body: String = chunks.iter().map(|c| format!("data: {c}\n\n")).collect();
    body.push_str("data: [DONE]\n\n");
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(body)
}

fn delta(delta: Value) -> Value {
    json!({"choices": [{"index": 0, "delta": delta, "finish_reason": null}]})
}

#[tokio::test]
async fn streams_text_and_runs_a_tool_round_trip() {
    let server = MockServer::start().await;
    let first = stream(&[
        delta(json!({"role": "assistant", "content": "Let me look."})),
        delta(
            json!({"tool_calls": [{"index": 0, "id": "call_1", "type": "function",
            "function": {"name": "search_mail", "arguments": "{\"que"}}]}),
        ),
        delta(
            json!({"tool_calls": [{"index": 0, "function": {"arguments": "ry\":\"invoice\"}"}}]}),
        ),
        json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]}),
    ]);
    let second = stream(&[
        delta(json!({"content": "Found "})),
        delta(json!({"content": "2."})),
    ]);
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer sk-local"))
        .respond_with(Sequence::new(vec![first, second]))
        .expect(2)
        .mount(&server)
        .await;

    let host = Arc::new(FakeHost::default());
    let (tx, rx) = async_channel::unbounded();
    let mut chat = OpenAiChat::new(
        &format!("{}/v1/", server.uri()),
        Some("sk-local".into()),
        "qwen3".into(),
        "You sort mail.".into(),
    );
    let reply = chat
        .send("Find invoices".into(), host.clone(), &tx)
        .await
        .unwrap();

    assert_eq!(reply, "Found 2.");
    assert_eq!(
        host.calls(),
        vec![("search_mail".into(), json!({"query": "invoice"}))]
    );
    assert_eq!(
        drain(&rx),
        vec![
            AgentEvent::Text("Let me look.".into()),
            AgentEvent::ToolStarted {
                name: "search_mail".into(),
                input: json!({"query": "invoice"}),
            },
            AgentEvent::ToolFinished {
                name: "search_mail".into(),
                ok: true,
                preview: r#"{"hits":2}"#.into(),
            },
            AgentEvent::Text("Found ".into()),
            AgentEvent::Text("2.".into()),
        ]
    );

    let requests = server.received_requests().await.unwrap();
    let first: Value = requests[0].body_json().unwrap();
    assert_eq!(first["stream"], json!(true));
    assert_eq!(first["model"], json!("qwen3"));
    assert_eq!(first["tools"][0]["type"], json!("function"));
    assert_eq!(first["tools"][0]["function"]["name"], json!("search_mail"));
    assert_eq!(
        first["tools"][0]["function"]["parameters"]["required"],
        json!(["query"])
    );
    let second: Value = requests[1].body_json().unwrap();
    assert_eq!(
        second["messages"],
        json!([
            {"role": "system", "content": "You sort mail."},
            {"role": "user", "content": "Find invoices"},
            {"role": "assistant", "content": "Let me look.", "tool_calls": [{
                "id": "call_1",
                "type": "function",
                "function": {"name": "search_mail", "arguments": "{\"query\":\"invoice\"}"},
            }]},
            {"role": "tool", "tool_call_id": "call_1", "content": "{\"hits\":2}"},
        ])
    );
}

#[tokio::test]
async fn failed_and_malformed_calls_go_back_as_errors() {
    let server = MockServer::start().await;
    let first = stream(&[delta(json!({"tool_calls": [
        {"index": 0, "id": "a", "function": {"name": "fail", "arguments": "{}"}},
        {"index": 1, "id": "b", "function": {"name": "search_mail", "arguments": "{\"query\":"}},
    ]}))]);
    let second = stream(&[delta(json!({"content": "Sorry."}))]);
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(Sequence::new(vec![first, second]))
        .mount(&server)
        .await;

    let host = Arc::new(FakeHost::default());
    let (tx, _rx) = async_channel::unbounded();
    let mut chat = OpenAiChat::new(
        &format!("{}/v1", server.uri()),
        None,
        "m".into(),
        String::new(),
    );
    assert_eq!(
        chat.send("Go".into(), host.clone(), &tx).await.unwrap(),
        "Sorry."
    );

    // Only the well-formed call reached the host.
    assert_eq!(host.calls(), vec![("fail".into(), json!({}))]);
    let requests = server.received_requests().await.unwrap();
    assert!(requests[0].headers.get("authorization").is_none());
    let second: Value = requests[1].body_json().unwrap();
    let messages = second["messages"].as_array().unwrap();
    assert_eq!(
        messages[0]["role"],
        json!("user"),
        "no system message when the prompt is empty"
    );
    assert_eq!(
        messages[2],
        json!({"role": "tool", "tool_call_id": "a", "content": "Error: no such label"})
    );
    assert_eq!(messages[3]["tool_call_id"], json!("b"));
    assert!(
        messages[3]["content"]
            .as_str()
            .unwrap()
            .contains("not valid JSON")
    );
}

#[tokio::test]
async fn retries_without_tools_when_the_server_rejects_them() {
    let server = MockServer::start().await;
    let rejected = ResponseTemplate::new(400)
        .set_body_json(json!({"error": {"message": "This model does not support tools"}}));
    let answer = stream(&[delta(json!({"content": "Hi."}))]);
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(Sequence::new(vec![rejected, answer]))
        .mount(&server)
        .await;

    let (tx, _rx) = async_channel::unbounded();
    let mut chat = OpenAiChat::new(
        &format!("{}/v1", server.uri()),
        None,
        "m".into(),
        "Be brief.".into(),
    );
    let reply = chat
        .send("Hello".into(), Arc::new(FakeHost::default()), &tx)
        .await
        .unwrap();
    assert_eq!(reply, "Hi.");

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    let retry: Value = requests[1].body_json().unwrap();
    assert!(retry.get("tools").is_none());
    let system = retry["messages"][0]["content"].as_str().unwrap();
    assert!(system.starts_with("Be brief."));
    assert!(system.contains("without tools"));
}

#[tokio::test]
async fn other_client_errors_do_not_retry_and_leave_history_clean() {
    let server = MockServer::start().await;
    let bad =
        ResponseTemplate::new(404).set_body_json(json!({"error": {"message": "model not found"}}));
    let answer = stream(&[delta(json!({"content": "Hi."}))]);
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(Sequence::new(vec![bad, answer]))
        .mount(&server)
        .await;

    let host = Arc::new(FakeHost::default());
    let (tx, _rx) = async_channel::unbounded();
    let mut chat = OpenAiChat::new(
        &format!("{}/v1", server.uri()),
        None,
        "m".into(),
        String::new(),
    );
    let err = chat
        .send("One".into(), host.clone(), &tx)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("model not found"), "{err}");
    chat.send("Two".into(), host, &tx).await.unwrap();

    let requests = server.received_requests().await.unwrap();
    let second: Value = requests[1].body_json().unwrap();
    assert_eq!(
        second["messages"],
        json!([{"role": "user", "content": "Two"}])
    );
    assert!(second.get("tools").is_some());
}

#[tokio::test]
async fn lists_models_from_the_models_endpoint() {
    let server = MockServer::start().await;
    // Recorded from LM Studio's /v1/models, whose ids carry the version and
    // the quantization.
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "object": "list",
            "data": [
                {"id": "qwen3-8b", "object": "model", "owned_by": "organization_owner"},
                {"id": "unsloth/gemma-3-27b-it-GGUF:Q4_K_M", "object": "model"},
                {"id": "Gemma-3-12b", "object": "model"},
                {"id": "qwen3-8b", "object": "model"}
            ],
        })))
        .mount(&server)
        .await;
    let config = ProviderConfig::OpenAiCompatible {
        base_url: format!("{}/v1/", server.uri()),
        api_key: None,
        model: String::new(),
    };
    // Sorted, with the repeat dropped and every id whole.
    assert_eq!(
        list_models(&config).await.unwrap().ids(),
        vec![
            "Gemma-3-12b",
            "qwen3-8b",
            "unsloth/gemma-3-27b-it-GGUF:Q4_K_M",
        ]
    );
    assert_eq!(
        crate::test(&config).await.unwrap(),
        "Connected, 3 models available"
    );
}
