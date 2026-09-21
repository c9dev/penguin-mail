use std::sync::Arc;

use mailrs_ai::{BoxFuture, ToolHost, ToolOutcome, ToolSpec};
use serde_json::{Value, json};

use super::{ApprovalRequest, Source, Toolbox, Verdict};
use crate::assistant::{Host, ToolRequest};

/// A source with one tool that answers with its own id, and asks first
/// when `asks` is set.
struct Fake {
    id: &'static str,
    tool: &'static str,
    asks: bool,
}

impl Source for Fake {
    fn id(&self) -> String {
        self.id.into()
    }

    fn specs(&self) -> Vec<ToolSpec> {
        vec![ToolSpec {
            name: self.tool.into(),
            description: String::new(),
            input_schema: json!({"type": "object"}),
        }]
    }

    fn ask(&self, name: &str, _input: &Value) -> Option<String> {
        self.asks.then(|| format!("Run {name}?"))
    }

    fn call(&self, _name: String, _input: Value) -> BoxFuture<ToolOutcome> {
        let id = self.id.to_string();
        Box::pin(async move { ToolOutcome::Ok(json!(id)) })
    }
}

/// A mail host whose calls come back as "mail", and a toolbox over it with
/// a quiet web source and an MCP source that asks.
fn toolbox(allowed: &[&str]) -> (Toolbox, async_channel::Receiver<ApprovalRequest>) {
    let (requests, received) = async_channel::unbounded::<ToolRequest>();
    tokio::spawn(async move {
        while let Ok(request) = received.recv().await {
            let _ = request.reply.send(ToolOutcome::Ok(json!("mail"))).await;
        }
    });
    let mail = Host::new(
        vec![ToolSpec {
            name: "archive".into(),
            description: String::new(),
            input_schema: json!({"type": "object"}),
        }],
        requests,
    );
    let (approvals, asked) = async_channel::unbounded();
    let sources: Vec<Arc<dyn Source>> = vec![
        Arc::new(Fake {
            id: "web",
            tool: "web_search",
            asks: false,
        }),
        Arc::new(Fake {
            id: "mcp:files",
            tool: "files__read",
            asks: true,
        }),
    ];
    let toolbox = Toolbox::new(
        mail,
        sources,
        approvals,
        allowed.iter().map(|k| k.to_string()),
    );
    (toolbox, asked)
}

/// Answers every question the toolbox asks with `verdict`, and counts them.
fn answer(
    asked: async_channel::Receiver<ApprovalRequest>,
    verdict: Verdict,
) -> Arc<std::sync::atomic::AtomicUsize> {
    let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = Arc::clone(&count);
    tokio::spawn(async move {
        while let Ok(request) = asked.recv().await {
            assert_eq!(request.key, "mcp:files/files__read");
            counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let _ = request.reply.send(verdict).await;
        }
    });
    count
}

#[tokio::test]
async fn every_source_is_offered_and_each_call_reaches_its_owner() {
    let (toolbox, asked) = toolbox(&[]);
    let _count = answer(asked, Verdict::Once);
    let names: Vec<String> = toolbox.specs().into_iter().map(|s| s.name).collect();
    assert_eq!(names, ["archive", "web_search", "files__read"]);
    assert_eq!(
        toolbox.call("archive".into(), json!({})).await,
        ToolOutcome::Ok(json!("mail"))
    );
    assert_eq!(
        toolbox.call("web_search".into(), json!({})).await,
        ToolOutcome::Ok(json!("web"))
    );
    assert_eq!(
        toolbox.call("files__read".into(), json!({})).await,
        ToolOutcome::Ok(json!("mcp:files"))
    );
}

#[tokio::test]
async fn a_refused_call_does_not_run() {
    let (toolbox, asked) = toolbox(&[]);
    let count = answer(asked, Verdict::Deny);
    assert!(matches!(
        toolbox.call("files__read".into(), json!({})).await,
        ToolOutcome::Err(_)
    ));
    assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[tokio::test]
async fn always_stops_the_asking_for_that_tool() {
    let (toolbox, asked) = toolbox(&[]);
    let count = answer(asked, Verdict::Always);
    for _ in 0..3 {
        toolbox.call("files__read".into(), json!({})).await;
    }
    assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_tool_allowed_before_runs_without_asking() {
    let (toolbox, asked) = toolbox(&["mcp:files/files__read"]);
    let count = answer(asked, Verdict::Deny);
    assert_eq!(
        toolbox.call("files__read".into(), json!({})).await,
        ToolOutcome::Ok(json!("mcp:files"))
    );
    assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 0);
}
