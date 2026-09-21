//! Tool sources beyond the mail tools, and the [`Toolbox`] that puts them
//! in front of the model together.
//!
//! The mail tools run on the GTK thread through [`Host`], because they read
//! the window. A source here needs no window: web search, MCP servers,
//! skills and the shell each live in a module of their own and run on the
//! async runtime. Every provider reaches them through the one toolbox, and
//! Claude Code through the bridge in front of it, so the asking below holds
//! whichever model runs.

pub mod mcp;
pub mod web;

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use mailrs_ai::{BoxFuture, ToolHost, ToolOutcome, ToolSpec};
use serde_json::Value;

use super::Host;
use crate::settings::Settings;

pub mod skills;

/// What the person answered when a source asked before a call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Once,
    /// Allow this call and every later one to the same tool.
    Always,
    Deny,
}

/// A question for the person, carried to the GTK thread.
pub struct ApprovalRequest {
    /// What the pane shows: which tool, from where, with what.
    pub question: String,
    /// `source/tool`, the key an Always answer is stored under.
    pub key: String,
    pub reply: async_channel::Sender<Verdict>,
}

/// One place tools come from.
pub trait Source: Send + Sync {
    /// Short and stable, such as `web` or `mcp:github`. It prefixes the
    /// key an Always answer is stored under.
    fn id(&self) -> String;
    /// The tools this source offers right now.
    fn specs(&self) -> Vec<ToolSpec>;
    /// The question to ask before running this call, or `None` when it
    /// runs without asking.
    fn ask(&self, name: &str, input: &Value) -> Option<String>;
    fn call(&self, name: String, input: Value) -> BoxFuture<ToolOutcome>;
    /// Gets the tools ready before a turn lists them, such as starting a
    /// server. Most sources have nothing to wait for.
    fn prepare(&self) -> BoxFuture<()> {
        Box::pin(async {})
    }
    /// What this source adds to the system prompt, such as the skills the
    /// model may use.
    fn prompt(&self) -> Option<String> {
        None
    }
}

/// The sources the settings turn on. Each later part adds its own here.
pub fn for_settings(settings: &Settings) -> Vec<Arc<dyn Source>> {
    let mut sources = Vec::new();
    sources.extend(web::for_settings(settings));
    sources.extend(mcp::sources(&settings.mcp_servers));
    sources.extend(skills::sources(settings));
    sources
}

/// The system prompt for a conversation over `sources`: `base`, then what
/// each source adds, a blank line apart.
pub fn system_prompt(base: &str, sources: &[Arc<dyn Source>]) -> String {
    let mut prompt = base.to_string();
    for text in sources.iter().filter_map(|source| source.prompt()) {
        prompt.push_str("\n\n");
        prompt.push_str(&text);
    }
    prompt
}

/// Everything the model can call: the mail tools and every source.
pub struct Toolbox {
    mail: Host,
    sources: Vec<Arc<dyn Source>>,
    approvals: async_channel::Sender<ApprovalRequest>,
    /// Keys the person answered Always for, including ones answered during
    /// this turn.
    allowed: Arc<Mutex<HashSet<String>>>,
}

impl Toolbox {
    pub fn new(
        mail: Host,
        sources: Vec<Arc<dyn Source>>,
        approvals: async_channel::Sender<ApprovalRequest>,
        allowed: impl IntoIterator<Item = String>,
    ) -> Toolbox {
        Toolbox {
            mail,
            sources,
            approvals,
            allowed: Arc::new(Mutex::new(allowed.into_iter().collect())),
        }
    }

    fn owner(&self, name: &str) -> Option<Arc<dyn Source>> {
        self.sources
            .iter()
            .find(|source| source.specs().iter().any(|spec| spec.name == name))
            .cloned()
    }
}

impl ToolHost for Toolbox {
    fn specs(&self) -> Vec<ToolSpec> {
        let mut specs = self.mail.specs();
        for source in &self.sources {
            specs.extend(source.specs());
        }
        specs
    }

    fn prepare(&self) -> BoxFuture<()> {
        let waits: Vec<_> = self.sources.iter().map(|source| source.prepare()).collect();
        Box::pin(async move {
            futures::future::join_all(waits).await;
        })
    }

    fn call(&self, name: String, input: Value) -> BoxFuture<ToolOutcome> {
        let Some(source) = self.owner(&name) else {
            return self.mail.call(name, input);
        };
        let question = source.ask(&name, &input);
        let key = format!("{}/{name}", source.id());
        let approvals = self.approvals.clone();
        let allowed = Arc::clone(&self.allowed);
        Box::pin(async move {
            let known = allowed.lock().is_ok_and(|keys| keys.contains(&key));
            if let (Some(question), false) = (question, known) {
                let (reply, answer) = async_channel::bounded(1);
                let asked = approvals
                    .send(ApprovalRequest {
                        question,
                        key: key.clone(),
                        reply,
                    })
                    .await;
                let verdict = match asked {
                    Ok(()) => answer.recv().await.unwrap_or(Verdict::Deny),
                    Err(_) => Verdict::Deny,
                };
                match verdict {
                    Verdict::Deny => return ToolOutcome::Err("The user declined.".into()),
                    Verdict::Always => {
                        if let Ok(mut keys) = allowed.lock() {
                            keys.insert(key);
                        }
                    }
                    Verdict::Once => {}
                }
            }
            source.call(name, input).await
        })
    }
}

#[cfg(test)]
mod tests;
