//! Finding local model servers and Claude Code.

use crate::ProviderConfig;

/// A provider found on this machine, ready to use.
#[derive(Debug, Clone, PartialEq)]
pub struct Detected {
    /// For example "LM Studio on port 1234".
    pub label: String,
    pub config: ProviderConfig,
    pub models: Vec<String>,
}

/// Looks for LM Studio, Ollama, llama.cpp and similar servers on their
/// usual local ports, and for the Claude Code CLI.
pub async fn detect() -> Vec<Detected> {
    todo!("implemented by the ai crate work")
}
