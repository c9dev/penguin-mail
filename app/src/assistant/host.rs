//! The model's tool calls arrive on a tokio thread; the window lives on the
//! GTK thread. [`Host`] is the mail tools' source: it carries each call
//! across and waits for the answer.

use mailrs_ai::{BoxFuture, ToolOutcome, ToolSpec};
use serde_json::Value;

use super::sources::Source;

/// One tool call for the GTK thread to run.
pub struct ToolRequest {
    pub name: String,
    pub input: Value,
    pub reply: async_channel::Sender<ToolOutcome>,
}

pub struct Host {
    specs: Vec<ToolSpec>,
    requests: async_channel::Sender<ToolRequest>,
}

impl Host {
    pub fn new(specs: Vec<ToolSpec>, requests: async_channel::Sender<ToolRequest>) -> Host {
        Host { specs, requests }
    }
}

impl Source for Host {
    fn id(&self) -> String {
        "mail".into()
    }

    fn specs(&self) -> Vec<ToolSpec> {
        self.specs.clone()
    }

    /// Never asks here. A mail tool's question names what the window and
    /// Gmail hold when the call runs, such as the message it would send or
    /// the automatic reply it would change, so the tool asks from inside
    /// the call, on the GTK thread. See `run::catalog`.
    fn ask(&self, _name: &str, _input: &Value) -> Option<String> {
        None
    }

    fn call(&self, name: String, input: Value) -> BoxFuture<ToolOutcome> {
        let requests = self.requests.clone();
        Box::pin(async move {
            let (reply, answer) = async_channel::bounded(1);
            if requests
                .send(ToolRequest { name, input, reply })
                .await
                .is_err()
            {
                return ToolOutcome::Err("The mail window closed.".into());
            }
            answer
                .recv()
                .await
                .unwrap_or_else(|_| ToolOutcome::Err("The mail window closed.".into()))
        })
    }
}
