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
                id: "call_1".into(),
                name: "search_mail".into(),
                input: json!({"query": "invoice"}),
            },
            AgentEvent::ToolFinished {
                id: "call_1".into(),
                name: "search_mail".into(),
                ok: true,
                preview: r#"{"hits":2}"#.into(),
                output: r#"{"hits":2}"#.into(),
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

fn read_thread_call() -> ResponseTemplate {
    stream(&[delta(json!({"tool_calls": [
        {"index": 0, "id": "call_read", "function": {"name": "read_thread", "arguments": "{}"}},
    ]}))])
}

fn local_chat(server: &MockServer) -> OpenAiChat {
    OpenAiChat::new(
        &format!("{}/v1", server.uri()),
        None,
        "m".into(),
        String::new(),
    )
}

#[tokio::test]
async fn a_huge_turn_leaves_room_for_the_next_question() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(Sequence::new(vec![
            read_thread_call(),
            stream(&[delta(json!({"content": "A long thread."}))]),
            stream(&[delta(json!({"content": "Hello."}))]),
        ]))
        .mount(&server)
        .await;
    let host = Arc::new(super::OneTool {
        answer: Some("body ".repeat(100_000)),
    });
    let (tx, _rx) = async_channel::unbounded();
    let mut chat = local_chat(&server);
    chat.send("Read it".into(), host.clone(), &tx)
        .await
        .unwrap();
    assert_eq!(chat.send("Hi".into(), host, &tx).await.unwrap(), "Hello.");

    let requests = server.received_requests().await.unwrap();
    let last: Value = requests[2].body_json().unwrap();
    let messages = last["messages"].as_array().unwrap();
    super::no_orphans(messages);
    assert_eq!(messages, &[json!({"role": "user", "content": "Hi"})]);
}

#[tokio::test]
async fn stop_in_the_middle_of_a_turn_leaves_a_chat_that_still_works() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(Sequence::new(vec![
            read_thread_call(),
            stream(&[delta(json!({"content": "Hello."}))]),
        ]))
        .mount(&server)
        .await;
    let (tx, _rx) = async_channel::unbounded();
    let mut chat = local_chat(&server);
    let stopped = tokio::time::timeout(
        std::time::Duration::from_millis(200),
        chat.send(
            "Read it".into(),
            Arc::new(super::OneTool { answer: None }),
            &tx,
        ),
    )
    .await;
    assert!(stopped.is_err(), "the turn should still be waiting");
    let host = Arc::new(FakeHost::default());
    assert_eq!(chat.send("Hi".into(), host, &tx).await.unwrap(), "Hello.");

    let requests = server.received_requests().await.unwrap();
    let last: Value = requests[1].body_json().unwrap();
    assert_eq!(last["messages"], json!([{"role": "user", "content": "Hi"}]));
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

#[tokio::test]
async fn reasoning_streams_as_thinking_and_stays_out_of_the_reply() {
    let server = MockServer::start().await;
    // LM Studio and vLLM put reasoning in `reasoning_content`, Ollama in
    // `reasoning`, and a server that parses nothing leaves `<think>` tags in
    // the content, cut wherever the chunks fall.
    let first = stream(&[
        delta(json!({"role": "assistant", "content": null, "reasoning_content": "The user "})),
        delta(json!({"reasoning": "wants mail."})),
        delta(
            json!({"content": "", "tool_calls": [{"index": 0, "id": "call_1", "type": "function",
            "function": {"name": "search_mail", "arguments": "{\"query\":\"a\"}"}}]}),
        ),
        delta(
            json!({"content": null, "tool_calls": [{"index": 1, "id": "call_2", "type": "function",
            "function": {"name": "search_mail", "arguments": "{\"query\":\"b\"}"}}]}),
        ),
    ]);
    let second = stream(&[
        delta(json!({"content": "<thi"})),
        delta(json!({"content": "nk>Two hits"})),
        delta(json!({"content": " each.</th"})),
        delta(json!({"content": "ink>\n\nFound 4."})),
    ]);
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(Sequence::new(vec![first, second]))
        .mount(&server)
        .await;

    let host = Arc::new(FakeHost::default());
    let (tx, rx) = async_channel::unbounded();
    let mut chat = OpenAiChat::new(
        &format!("{}/v1", server.uri()),
        None,
        "qwen3".into(),
        String::new(),
    );
    let reply = chat.send("Go".into(), host, &tx).await.unwrap();
    assert_eq!(reply, "Found 4.");

    let seen = drain(&rx);
    let started: Vec<&str> = seen
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolStarted { id, .. } => Some(id.as_str()),
            _ => None,
        })
        .collect();
    let finished: Vec<&str> = seen
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolFinished { id, .. } => Some(id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(started, ["call_1", "call_2"]);
    assert_eq!(finished, ["call_1", "call_2"]);
    let words: Vec<&AgentEvent> = seen
        .iter()
        .filter(|e| matches!(e, AgentEvent::Text(_) | AgentEvent::Thinking(_)))
        .collect();
    assert_eq!(
        words,
        [
            &AgentEvent::Thinking("The user ".into()),
            &AgentEvent::Thinking("wants mail.".into()),
            &AgentEvent::Thinking("Two hits".into()),
            &AgentEvent::Thinking(" each.".into()),
            &AgentEvent::Text("Found 4.".into()),
        ]
    );

    // The history keeps the reply and leaves the reasoning out.
    let requests = server.received_requests().await.unwrap();
    let second: Value = requests[1].body_json().unwrap();
    assert_eq!(second["messages"][1]["content"], json!(""));
}

#[test]
fn think_tags_split_wherever_the_chunks_fall() {
    use crate::providers::{Piece, ThinkTags};
    let whole = "Sure. <think>plan á</think>\n\nDone <b>.";
    // Every way of cutting the text in two gives the same pieces.
    for cut in (0..=whole.len()).filter(|&i| whole.is_char_boundary(i)) {
        let mut tags = ThinkTags::default();
        let mut pieces = tags.push(&whole[..cut]);
        pieces.extend(tags.push(&whole[cut..]));
        pieces.extend(tags.finish());
        let mut text = String::new();
        let mut thinking = String::new();
        for piece in pieces {
            match piece {
                Piece::Text(t) => text.push_str(&t),
                Piece::Thinking(t) => thinking.push_str(&t),
            }
        }
        assert_eq!(text, "Sure. Done <b>.", "cut at {cut}");
        assert_eq!(thinking, "plan á", "cut at {cut}");
    }
    // A stream that ends inside a span keeps what it thought.
    let mut tags = ThinkTags::default();
    assert_eq!(tags.push("<think>half"), [Piece::Thinking("half".into())]);
    assert_eq!(tags.push("</thi"), []);
    assert_eq!(tags.finish(), [Piece::Thinking("</thi".into())]);
}
