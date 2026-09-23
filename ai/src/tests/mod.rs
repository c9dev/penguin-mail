mod anthropic;
mod bridge;
mod claude_code;
mod detect;
mod mcp;
mod openai;

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{Value, json};
use wiremock::{Request, Respond, ResponseTemplate};

use crate::{AgentEvent, BoxFuture, ToolHost, ToolOutcome, ToolSpec};

/// Two tools: `search_mail` finds two messages, `fail` always fails.
#[derive(Default)]
pub(crate) struct FakeHost {
    pub calls: Mutex<Vec<(String, Value)>>,
}

impl FakeHost {
    pub fn calls(&self) -> Vec<(String, Value)> {
        self.calls.lock().unwrap().clone()
    }
}

impl ToolHost for FakeHost {
    fn specs(&self) -> Vec<ToolSpec> {
        vec![
            ToolSpec {
                name: "search_mail".into(),
                description: "Searches mail.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {"query": {"type": "string"}},
                    "required": ["query"],
                }),
            },
            ToolSpec {
                name: "fail".into(),
                description: "Always fails.".into(),
                input_schema: json!({"type": "object", "properties": {}}),
            },
        ]
    }

    fn call(&self, name: String, input: Value) -> BoxFuture<ToolOutcome> {
        self.calls.lock().unwrap().push((name.clone(), input));
        Box::pin(async move {
            match name.as_str() {
                "search_mail" => ToolOutcome::Ok(json!({"hits": 2})),
                "fail" => ToolOutcome::Err("no such label".into()),
                _ => ToolOutcome::Err(format!("unknown tool {name}")),
            }
        })
    }
}

/// Replies with each template in turn, repeating the last one.
pub(crate) struct Sequence {
    replies: Vec<ResponseTemplate>,
    next: AtomicUsize,
}

impl Sequence {
    pub fn new(replies: Vec<ResponseTemplate>) -> Sequence {
        Sequence {
            replies,
            next: AtomicUsize::new(0),
        }
    }
}

impl Respond for Sequence {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        let n = self.next.fetch_add(1, Ordering::SeqCst);
        self.replies[n.min(self.replies.len() - 1)].clone()
    }
}

/// A 200 response carrying server-sent events.
pub(crate) fn sse(events: &[(Option<&str>, Value)]) -> ResponseTemplate {
    let mut body = String::new();
    for (name, data) in events {
        if let Some(name) = name {
            body.push_str(&format!("event: {name}\n"));
        }
        body.push_str(&format!("data: {data}\n\n"));
    }
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(body)
}

/// Everything sent on `events` so far.
pub(crate) fn drain(events: &async_channel::Receiver<AgentEvent>) -> Vec<AgentEvent> {
    let mut all = Vec::new();
    while let Ok(event) = events.try_recv() {
        all.push(event);
    }
    all
}

/// One tool, `read_thread`, that answers with `answer`, or never answers
/// when it is `None`, the way a tool waiting on the person does while they
/// press Stop.
pub(crate) struct OneTool {
    pub answer: Option<String>,
}

impl ToolHost for OneTool {
    fn specs(&self) -> Vec<ToolSpec> {
        vec![ToolSpec {
            name: "read_thread".into(),
            description: "Reads a thread.".into(),
            input_schema: json!({"type": "object", "properties": {}}),
        }]
    }

    fn call(&self, _name: String, _input: Value) -> BoxFuture<ToolOutcome> {
        let answer = self.answer.clone();
        Box::pin(async move {
            match answer {
                Some(text) => ToolOutcome::Ok(Value::String(text)),
                None => std::future::pending().await,
            }
        })
    }
}

/// Checks a request's messages, in either API's shape: the chat opens
/// with a question, and every tool result answers a call before it.
pub(crate) fn no_orphans(messages: &[Value]) {
    let first = messages
        .iter()
        .find(|m| m["role"] != "system")
        .expect("a request with no messages");
    assert_eq!(first["role"], "user", "{first}");
    let question = match &first["content"] {
        Value::Array(blocks) => blocks.iter().all(|b| b["type"] == "text"),
        _ => true,
    };
    assert!(question, "the chat opens with {first}");
    let mut asked = Vec::new();
    for message in messages {
        for call in message["tool_calls"].as_array().into_iter().flatten() {
            asked.push(call["id"].clone());
        }
        if message["role"] == "tool" {
            assert!(asked.contains(&message["tool_call_id"]), "orphan {message}");
        }
        for block in message["content"].as_array().into_iter().flatten() {
            match block["type"].as_str() {
                Some("tool_use") => asked.push(block["id"].clone()),
                Some("tool_result") => {
                    assert!(asked.contains(&block["tool_use_id"]), "orphan {block}")
                }
                _ => {}
            }
        }
    }
}
