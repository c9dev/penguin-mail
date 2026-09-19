//! The model's tool calls arrive on a tokio thread; the window lives on the
//! GTK thread. [`Host`] carries each call across and waits for the answer.

use mailrs_ai::{BoxFuture, ToolHost, ToolOutcome, ToolSpec};

/// One tool call for the GTK thread to run.
pub struct ToolRequest {
    pub name: String,
    pub input: serde_json::Value,
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

impl ToolHost for Host {
    fn specs(&self) -> Vec<ToolSpec> {
        self.specs.clone()
    }

    fn call(&self, name: String, input: serde_json::Value) -> BoxFuture<ToolOutcome> {
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
