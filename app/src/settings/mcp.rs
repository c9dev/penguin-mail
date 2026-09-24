//! The MCP servers the person adds on the AI page, as the settings file
//! keeps them. The assistant starts and talks to them
//! (`crate::assistant::sources::mcp`); this is only what is saved, so the
//! settings depend on nothing the assistant runs.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// One MCP server, as the settings file keeps it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServer {
    /// Unique among the servers. It prefixes every tool name the model
    /// sees and names the server's token in the keyring.
    pub name: String,
    pub enabled: bool,
    pub transport: McpTransport,
}

/// How the app reaches a server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum McpTransport {
    /// A command the app starts and talks to over stdin and stdout.
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        /// Variables added to the app's own environment.
        #[serde(default)]
        env: BTreeMap<String, String>,
    },
    /// A URL, with a bearer token from the keyring when one is saved.
    Http { url: String },
}

impl McpServer {
    /// The keyring entry that holds this server's bearer token.
    pub fn token_key(&self) -> String {
        format!("mcp:{}", self.name)
    }
}
