use super::*;
use crate::settings::{AiProvider, WebSearch};
use serde_json::Value;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn settings(server: &MockServer) -> Settings {
    let mut settings = Settings::default();
    settings.ai.provider = AiProvider::Local;
    settings.ai.local_model = "first-model".into();
    settings.ai.base_url = format!("{}/v1", server.uri());
    settings.ai.web_search = WebSearch::Off;
    settings
}

fn chat() -> Chat {
    let (requests, _) = async_channel::unbounded();
    let (approvals, _) = async_channel::unbounded();
    Chat::new(requests, approvals)
}

async fn reply(server: &MockServer) {
    Mock::given(method("POST")).and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_string("data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Kept the context.\"},\"finish_reason\":null}]}\n\ndata: [DONE]\n\n"))
        .mount(server).await;
}

#[tokio::test]
async fn each_question_uses_its_model_and_tool_instructions_without_losing_history() {
    let server = MockServer::start().await;
    reply(&server).await;
    let mut settings = settings(&server);
    let mut chat = chat();
    let first = chat
        .ask(
            settings.clone(),
            Tools {
                mcp: true,
                skills: true,
            },
            "Remember the invoice".into(),
            Arc::default(),
        )
        .unwrap();
    assert_eq!(first.answer.await.unwrap(), "Kept the context.");
    settings.ai.local_model = "second-model".into();
    settings.ai.language = "Portuguese".into();
    let second = chat
        .ask(
            settings,
            Tools {
                mcp: false,
                skills: false,
            },
            "What about it?".into(),
            Arc::default(),
        )
        .unwrap();
    second.answer.await.unwrap();
    let requests = server.received_requests().await.unwrap();
    let first: Value = requests[0].body_json().unwrap();
    let second: Value = requests[1].body_json().unwrap();
    assert_eq!(first["model"], "first-model");
    assert_eq!(second["model"], "second-model");
    let messages = second["messages"].as_array().unwrap();
    assert!(
        messages
            .iter()
            .any(|message| message["content"] == "Remember the invoice")
    );
    assert!(
        messages
            .iter()
            .any(|message| message["content"] == "Kept the context.")
    );
    assert_eq!(messages.last().unwrap()["content"], "What about it?");
    let prompt = messages[0]["content"].as_str().unwrap();
    assert!(prompt.contains("Portuguese"));
    assert!(prompt.contains("Skills are disabled"));
    assert!(prompt.contains("MCP servers are disabled"));
    assert!(
        !first["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("Skills are disabled")
    );
    assert!(
        second["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["function"]["name"] == "send_email")
    );
    assert!(
        !second["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["function"]["name"] == "fetch_page")
    );
}

#[tokio::test]
async fn stopping_at_approval_keeps_the_previous_turn_and_drops_the_unfinished_one() {
    let server = MockServer::start().await;
    reply(&server).await;
    let mut settings = settings(&server);
    settings.ai.web_search = WebSearch::Claude;
    let (requests, _) = async_channel::unbounded();
    let (approvals, asked) = async_channel::unbounded();
    let mut chat = Chat::new(requests, approvals);
    let first = chat
        .ask(
            settings.clone(),
            Tools {
                mcp: true,
                skills: true,
            },
            "Keep this".into(),
            Arc::default(),
        )
        .unwrap();
    first.answer.await.unwrap();
    server.reset().await;
    let tool = serde_json::json!({"choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "id": "page1", "type": "function", "function": {"name": "fetch_page", "arguments": "{\"url\":\"https://example.com/page\"}"}}]}, "finish_reason": "tool_calls"}]});
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(format!("data: {tool}\n\ndata: [DONE]\n\n")),
        )
        .mount(&server)
        .await;
    let question = chat
        .ask(
            settings.clone(),
            Tools {
                mcp: true,
                skills: true,
            },
            "Cancel this".into(),
            Arc::default(),
        )
        .unwrap();
    let answer = tokio::spawn(question.answer);
    let approval = tokio::time::timeout(std::time::Duration::from_secs(5), asked.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(approval.question.contains("https://example.com/page"));
    question.stop.send(()).await.unwrap();
    assert!(answer.await.unwrap().is_err());
    assert!(
        approval.reply.is_closed(),
        "stopping must release the pending approval"
    );
    let mut saw_tool = false;
    while let Ok(event) = question.events.try_recv() {
        saw_tool |= matches!(event, AgentEvent::ToolStarted { name, .. } if name == "fetch_page");
    }
    assert!(saw_tool);
    server.reset().await;
    reply(&server).await;
    let next = chat
        .ask(
            settings,
            Tools {
                mcp: false,
                skills: false,
            },
            "Continue".into(),
            Arc::default(),
        )
        .unwrap();
    next.answer.await.unwrap();
    let requests = server.received_requests().await.unwrap();
    let body: Value = requests[0].body_json().unwrap();
    let text = body["messages"].to_string();
    assert!(text.contains("Keep this"));
    assert!(!text.contains("Cancel this"));
    assert!(!text.contains("page1"));
}

#[tokio::test]
async fn a_question_carries_picker_grants_to_mail_tools_separately_from_model_input() {
    use mailrs_ai::ToolOutcome;
    use serde_json::json;

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(|request: &wiremock::Request| {
            let body: Value = request.body_json().unwrap();
            let finished = body["messages"].as_array().unwrap().last().unwrap()["role"] == "tool";
            let delta = if finished {
                json!({"content": "Draft ready."})
            } else {
                json!({"tool_calls": [{"index": 0, "id": "draft1", "type": "function", "function": {
                    "name": "send_email", "arguments": "{\"to\":\"ana@example.com\"}"
                }}]})
            };
            let chunk = json!({"choices": [{"index": 0, "delta": delta,
                "finish_reason": if finished { "stop" } else { "tool_calls" }}]});
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(format!("data: {chunk}\n\ndata: [DONE]\n\n"))
        })
        .mount(&server)
        .await;
    let (requests, received) = async_channel::unbounded();
    let (approvals, _) = async_channel::unbounded();
    let mut chat = Chat::new(requests, approvals);
    let picked: Arc<[PathBuf]> = vec![PathBuf::from("/run/user/1000/doc/grant/lease.pdf")].into();
    let question = chat
        .ask(
            settings(&server),
            Tools {
                mcp: false,
                skills: false,
            },
            "Draft the lease".into(),
            Arc::clone(&picked),
        )
        .unwrap();
    let answer = tokio::spawn(question.answer);
    let request = tokio::time::timeout(std::time::Duration::from_secs(5), received.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(request.name, "send_email");
    assert_eq!(request.input, json!({"to": "ana@example.com"}));
    assert_eq!(request.picked_files, picked);
    request
        .reply
        .send(ToolOutcome::Ok(json!("Draft ready")))
        .await
        .unwrap();
    assert_eq!(answer.await.unwrap().unwrap(), "Draft ready.");
}

#[tokio::test]
async fn reasoning_preferences_apply_to_each_question_without_losing_history() {
    let server = MockServer::start().await;
    reply(&server).await;
    let mut chat = chat();
    for (at, enabled) in [false, true, false].into_iter().enumerate() {
        let mut saved = serde_json::to_value(settings(&server)).unwrap();
        saved["ai"]["assistant_reasoning"] = enabled.into();
        let settings = serde_json::from_value(saved).unwrap();
        chat.ask(
            settings,
            Tools {
                mcp: false,
                skills: false,
            },
            format!("Question {at}"),
            Arc::default(),
        )
        .unwrap()
        .answer
        .await
        .unwrap();
        let requests = server.received_requests().await.unwrap();
        let body: Value = requests[at].body_json().unwrap();
        if enabled {
            assert!(body.get("reasoning_effort").is_none());
        } else {
            assert_eq!(body["reasoning_effort"], "none");
        }
        assert!(
            body["messages"]
                .as_array()
                .unwrap()
                .iter()
                .any(|message| message["content"] == "Question 0")
        );
    }
}

#[tokio::test]
async fn a_turn_gives_up_at_the_round_limit_preferences_set() {
    use mailrs_ai::ToolOutcome;
    use serde_json::json;

    let server = MockServer::start().await;
    // A model that answers every round with another tool call and never with
    // a reply, which is the case the limit exists for.
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_string("data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"again\",\"type\":\"function\",\"function\":{\"name\":\"search_mail\",\"arguments\":\"{\\\"query\\\":\\\"invoice\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\ndata: [DONE]\n\n"))
        .mount(&server).await;
    let (requests, received) = async_channel::unbounded();
    let (approvals, _) = async_channel::unbounded();
    let mut chat = Chat::new(requests, approvals);
    // Answer every call, so only the limit can end the turn.
    tokio::spawn(async move {
        while let Ok(request) = received.recv().await {
            let _ = request
                .reply
                .send(ToolOutcome::Ok(json!("nothing found")))
                .await;
        }
    });
    let mut saved = serde_json::to_value(settings(&server)).unwrap();
    saved["ai"]["max_rounds"] = 2.into();
    let settings: Settings = serde_json::from_value(saved).unwrap();

    let question = chat
        .ask(
            settings,
            Tools {
                mcp: false,
                skills: false,
            },
            "Find invoices".into(),
            Arc::default(),
        )
        .unwrap();
    let answer = question.answer.await;

    assert_eq!(
        answer.unwrap_err().to_string(),
        "stopped after 2 model requests without a final answer"
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}
