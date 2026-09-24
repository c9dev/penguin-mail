//! MCP servers the person adds on the AI page, each one a [`Source`].
//!
//! A server is saved as an [`McpServer`] in the settings, with its bearer
//! token in the keyring. At run time it lives in the [`registry`], which
//! starts it the first time a turn needs it, keeps it for later turns, and
//! stops it when the settings drop it or the app quits. The model sees each
//! tool as `<server>__<tool>`, and every call asks first unless the person
//! answered Always Allow for that tool.

mod command;
mod registry;

use std::sync::Arc;

use mailrs_ai::mcp::McpTool;
use mailrs_ai::{BoxFuture, ToolOutcome, ToolSpec};
use serde_json::Value;

pub use command::{join_command_line, split_command_line};
pub use crate::settings::mcp::{McpServer, McpTransport};
pub use registry::{Status, registry};

use super::Source;
use mailrs_domain::translate::{fill, gettext};

/// The longest tool name the models take: Anthropic's and OpenAI's limit.
const NAME_LIMIT: usize = 64;
/// How much of a call's input the question shows.
const SUMMARY_CHARS: usize = 240;

/// One line saying how the app reaches `server`: its command line or its
/// URL.
pub fn summary(server: &McpServer) -> String {
    match &server.transport {
        McpTransport::Stdio { command, args, env } => join_command_line(env, command, args),
        McpTransport::Http { url } => url.clone(),
    }
}

/// Whether a name can be a server's: letters, digits, `-` and `_`, since it
/// becomes part of every tool name and of the key an Always answer is
/// stored under.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// The sources for the enabled servers, sharing the servers already
/// running.
pub fn sources(servers: &[McpServer]) -> Vec<Arc<dyn Source>> {
    registry()
        .sources(servers)
        .into_iter()
        .map(|source| source as Arc<dyn Source>)
        .collect()
}

/// The names the model sees for one server's tools, each with the tool it
/// stands for. A name the model would refuse is cleaned up, and two tools
/// that clean up the same way get a number, so each name stays unique.
pub fn tool_names(server: &str, tools: &[McpTool]) -> Vec<(String, McpTool)> {
    let prefix = format!("{}__", clean(server));
    let mut named: Vec<(String, McpTool)> = Vec::with_capacity(tools.len());
    for tool in tools {
        let base = cut(&format!("{prefix}{}", clean(&tool.name)), NAME_LIMIT);
        let mut name = base.clone();
        let mut n = 2;
        while named.iter().any(|(taken, _)| *taken == name) {
            let suffix = format!("_{n}");
            name = format!("{}{suffix}", cut(&base, NAME_LIMIT - suffix.len()));
            n += 1;
        }
        named.push((name, tool.clone()));
    }
    named
}

/// Keeps the characters a tool name may hold and turns the rest into `_`.
fn clean(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// A cleaned name is ASCII, so any byte is a character boundary.
fn cut(name: &str, limit: usize) -> String {
    name[..name.len().min(limit)].to_string()
}

/// What the question shows of a call's input: the JSON as sent, cut short.
pub fn input_summary(input: &Value) -> String {
    let text = match input {
        Value::Object(fields) if fields.is_empty() => return String::new(),
        Value::Null => return String::new(),
        other => other.to_string(),
    };
    match text.char_indices().nth(SUMMARY_CHARS) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text,
    }
}

/// One running server, offered as a source for a turn.
pub struct McpSource {
    server: Arc<registry::Running>,
}

impl McpSource {
    fn name(&self) -> &str {
        &self.server.config.name
    }

    /// The model's name for each tool the server offers now.
    fn named(&self) -> Vec<(String, McpTool)> {
        tool_names(self.name(), &self.server.tools())
    }
}

impl Source for McpSource {
    fn id(&self) -> String {
        format!("mcp:{}", self.name())
    }

    fn specs(&self) -> Vec<ToolSpec> {
        self.named()
            .into_iter()
            .map(|(name, tool)| ToolSpec {
                name,
                description: match (&tool.title, tool.description.trim()) {
                    (Some(title), "") => title.clone(),
                    (_, description) => description.to_string(),
                },
                input_schema: tool.input_schema,
            })
            .collect()
    }

    fn ask(&self, name: &str, input: &Value) -> Option<String> {
        let tool = self
            .named()
            .into_iter()
            .find(|(named, _)| named == name)
            .map_or_else(|| name.to_string(), |(_, tool)| tool.name);
        let question = fill(
            &gettext("Run {tool} on the MCP server {server}?"),
            &[("tool", &tool), ("server", self.name())],
        );
        let summary = input_summary(input);
        Some(if summary.is_empty() {
            question
        } else {
            format!("{question}\n{summary}")
        })
    }

    fn call(&self, name: String, input: Value) -> BoxFuture<ToolOutcome> {
        let tool = self
            .named()
            .into_iter()
            .find(|(named, _)| *named == name)
            .map(|(_, tool)| tool.name);
        let server = Arc::clone(&self.server);
        Box::pin(async move {
            let Some(tool) = tool else {
                return ToolOutcome::Err(format!("The server has no tool called {name}."));
            };
            server.call(&tool, input).await
        })
    }

    fn prepare(&self) -> BoxFuture<()> {
        let server = Arc::clone(&self.server);
        Box::pin(async move { server.start().await })
    }
}

#[cfg(test)]
mod tests;
