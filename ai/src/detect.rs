//! Finding local model servers and Claude Code.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

use crate::ProviderConfig;
use crate::providers::claude_aliases;

/// How long a local server gets to answer before we call the port empty.
const PROBE_TIMEOUT: Duration = Duration::from_millis(400);

/// The model the UI suggests for a new Anthropic setup.
const DEFAULT_ANTHROPIC_MODEL: &str = "claude-opus-5";

/// A provider found on this machine, ready to use.
#[derive(Debug, Clone, PartialEq)]
pub struct Detected {
    /// For example "LM Studio on port 1234".
    pub label: String,
    pub config: ProviderConfig,
    pub models: Vec<String>,
}

/// A local server to look for: its name and its OpenAI-style base URL.
#[derive(Debug, Clone)]
pub(crate) struct Probe {
    pub name: &'static str,
    pub base_url: String,
}

/// The usual ports. Unsloth Studio's docs give 8888 as its API port; it also
/// asks for a key on every request, so it often shows up as needing one.
fn default_probes() -> Vec<Probe> {
    [
        ("LM Studio", 1234),
        ("Ollama", 11434),
        ("llama.cpp", 8080),
        ("vLLM", 8000),
        ("Unsloth Studio", 8888),
    ]
    .into_iter()
    .map(|(name, port)| Probe {
        name,
        base_url: format!("http://127.0.0.1:{port}/v1"),
    })
    .collect()
}

/// Looks for LM Studio, Ollama, llama.cpp and similar servers on their
/// usual local ports, and for the Claude Code CLI.
pub async fn detect() -> Vec<Detected> {
    let mut found = probe_servers(&default_probes()).await;
    if let Some(command) = find_claude() {
        found.push(Detected {
            label: "Claude Code, with your Claude subscription".to_string(),
            config: ProviderConfig::ClaudeCode {
                command,
                model: None,
            },
            models: claude_aliases(),
        });
    }
    if let Some(api_key) = std::env::var("ANTHROPIC_API_KEY")
        .ok()
        .filter(|k| !k.trim().is_empty())
    {
        found.push(Detected {
            label: "Anthropic API key from ANTHROPIC_API_KEY".to_string(),
            config: ProviderConfig::Anthropic {
                api_key,
                model: DEFAULT_ANTHROPIC_MODEL.to_string(),
            },
            models: vec![DEFAULT_ANTHROPIC_MODEL.to_string()],
        });
    }
    found
}

/// Asks each server for its models at once and keeps the ones that answer.
pub(crate) async fn probe_servers(probes: &[Probe]) -> Vec<Detected> {
    let client = match reqwest::Client::builder().timeout(PROBE_TIMEOUT).build() {
        Ok(client) => client,
        Err(e) => {
            tracing::warn!("could not build the probe client: {e}");
            return Vec::new();
        }
    };
    let checks = probes.iter().map(|probe| probe_one(&client, probe));
    futures::future::join_all(checks)
        .await
        .into_iter()
        .flatten()
        .collect()
}

async fn probe_one(client: &reqwest::Client, probe: &Probe) -> Option<Detected> {
    let response = client
        .get(format!("{}/models", probe.base_url))
        .send()
        .await
        .ok()?;
    let status = response.status().as_u16();
    let port = reqwest::Url::parse(&probe.base_url)
        .ok()
        .and_then(|url| url.port_or_known_default())
        .map(|port| format!(" on port {port}"))
        .unwrap_or_default();
    let (label, models) = match status {
        200..=299 => {
            let body: Value = response.json().await.ok()?;
            let models = crate::providers::model_ids(&body);
            (format!("{}{port}", probe.name), models)
        }
        401 | 403 => (
            format!("{}{port} (needs an API key)", probe.name),
            Vec::new(),
        ),
        _ => return None,
    };
    tracing::debug!(%label, models = models.len(), "found a local model server");
    let model = models.first().cloned().unwrap_or_default();
    Some(Detected {
        label,
        config: ProviderConfig::OpenAiCompatible {
            base_url: probe.base_url.clone(),
            api_key: None,
            model,
        },
        models,
    })
}

/// The Claude Code CLI on `PATH`, or where its installers put it.
fn find_claude() -> Option<PathBuf> {
    let on_path = std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .map(|dir| dir.join("claude"));
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let installed = home.into_iter().flat_map(|home| {
        [
            home.join(".local/bin/claude"),
            home.join(".claude/local/claude"),
        ]
    });
    on_path.chain(installed).find(|p| is_executable(p))
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}
