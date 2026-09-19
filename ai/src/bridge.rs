//! Lets Claude Code call the app's tools. The app serves its [`ToolHost`]
//! on a private Unix socket; Claude Code starts the app binary as an MCP
//! server over stdio (`--mcp-bridge <socket>`), which relays each call.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::ToolHost;

/// A running socket server. Dropping it stops serving and removes the socket.
pub struct Bridge {
    pub socket: PathBuf,
}

/// Serves `host` on a new socket under `dir`, readable only by this user.
pub async fn serve(_dir: &Path, _host: Arc<dyn ToolHost>) -> std::io::Result<Bridge> {
    todo!("implemented by the ai crate work")
}

/// The MCP stdio server that Claude Code launches. Runs until stdin closes.
pub async fn run_mcp_stdio(_socket: &Path) -> std::io::Result<()> {
    todo!("implemented by the ai crate work")
}
