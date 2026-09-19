mod anthropic;
mod bridge;
mod claude_code;
mod detect;
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
