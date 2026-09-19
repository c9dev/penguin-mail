//! The user's Claude subscription, through the Claude Code CLI in headless
//! mode. Each turn runs `claude -p` once; `--resume` carries the chat over.
//! The app's tools reach Claude Code through the MCP bridge in
//! [`crate::bridge`].

use std::collections::HashMap;
use std::ffi::OsString;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

use super::{emit, preview, truncate};
use crate::bridge::{self, SERVER_NAME};
use crate::{AgentEvent, AiError, ToolHost};

/// Model aliases Claude Code accepts for `--model`.
const MODELS: [&str; 4] = ["opus", "sonnet", "haiku", "fable"];

pub(crate) fn claude_models() -> Vec<String> {
    MODELS.iter().map(|m| m.to_string()).collect()
}

/// The directory Claude Code runs in. It stays the same across turns, since
/// Claude Code files sessions by working directory and `--resume` looks there.
fn default_work_dir() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .unwrap_or_else(std::env::temp_dir);
    base.join("penguin-mail")
}

/// The program Claude Code starts as the MCP server: this app, unless
/// `PENGUIN_MAIL_BRIDGE_COMMAND` names another.
fn default_bridge_command() -> std::io::Result<PathBuf> {
    match std::env::var_os("PENGUIN_MAIL_BRIDGE_COMMAND") {
        Some(command) if !command.is_empty() => Ok(PathBuf::from(command)),
        _ => std::env::current_exe(),
    }
}

pub(crate) struct ClaudeCodeChat {
    command: PathBuf,
    model: Option<String>,
    system_prompt: String,
    session_id: Option<String>,
    work_dir: PathBuf,
    bridge_command: Option<PathBuf>,
}

impl ClaudeCodeChat {
    pub(crate) fn new(
        command: PathBuf,
        model: Option<String>,
        system_prompt: String,
    ) -> ClaudeCodeChat {
        ClaudeCodeChat {
            command,
            model: model.filter(|m| !m.is_empty()),
            system_prompt,
            session_id: None,
            work_dir: default_work_dir(),
            bridge_command: None,
        }
    }

    /// Replaces the working directory and bridge command. Tests use this.
    #[cfg(test)]
    pub(crate) fn with_paths(mut self, work_dir: PathBuf, bridge_command: PathBuf) -> Self {
        self.work_dir = work_dir;
        self.bridge_command = Some(bridge_command);
        self
    }

    pub(crate) async fn send(
        &mut self,
        text: String,
        host: Arc<dyn ToolHost>,
        events: &async_channel::Sender<AgentEvent>,
    ) -> Result<String, AiError> {
        let io = |what: &str, e: std::io::Error| AiError::Other(format!("{what}: {e}"));
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.work_dir)
            .map_err(|e| io("could not create Claude Code's working directory", e))?;
        let bridge_command = match &self.bridge_command {
            Some(command) => command.clone(),
            None => default_bridge_command()
                .map_err(|e| io("could not find the app binary for the tool bridge", e))?,
        };
        let bridge = bridge::serve(&self.work_dir, host)
            .await
            .map_err(|e| io("could not start the tool bridge", e))?;
        let config = mcp_config(&bridge_command, &bridge.socket);

        tracing::debug!(command = %self.command.display(), resume = self.session_id.is_some(), "starting Claude Code");
        let mut child = Command::new(&self.command)
            .args(self.args(&config))
            .current_dir(&self.work_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| io(&format!("could not run {}", self.command.display()), e))?;

        // The prompt goes in on stdin, so text that starts with a dash never
        // reads as a flag.
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes()).await;
        }
        let stderr = child.stderr.take().map(|mut stderr| {
            tokio::spawn(async move {
                let mut buf = String::new();
                let _ = stderr.read_to_string(&mut buf).await;
                buf
            })
        });
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| AiError::Other("Claude Code gave no output stream".to_string()))?;

        let mut turn = Turn::default();
        let mut lines = BufReader::new(stdout).lines();
        while let Some(line) = lines
            .next_line()
            .await
            .map_err(|e| io("could not read Claude Code's output", e))?
        {
            turn.handle(&line, events).await;
        }
        let status = child
            .wait()
            .await
            .map_err(|e| io("Claude Code did not exit cleanly", e))?;
        drop(bridge);
        let stderr = match stderr {
            Some(task) => task.await.unwrap_or_default(),
            None => String::new(),
        };

        if let Some(session) = &turn.session_id {
            self.session_id = Some(session.clone());
        }
        match turn.result {
            Some(Ok(text)) => Ok(text),
            Some(Err(message)) => Err(AiError::Api(message)),
            None => {
                let detail = truncate(stderr.trim(), 400);
                Err(AiError::Other(format!(
                    "Claude Code stopped without an answer ({status}): {detail}"
                )))
            }
        }
    }

    fn args(&self, mcp_config: &str) -> Vec<OsString> {
        let mut args: Vec<OsString> = [
            "-p",
            "--output-format",
            "stream-json",
            "--verbose",
            "--strict-mcp-config",
            "--mcp-config",
            mcp_config,
            // No built-in tools: no shell, no file access, no web.
            "--tools",
            "",
            "--allowedTools",
            &format!("mcp__{SERVER_NAME}"),
            // Anything not allowed above is refused without a prompt.
            "--permission-mode",
            "dontAsk",
            "--system-prompt",
            &self.system_prompt,
        ]
        .iter()
        .map(OsString::from)
        .collect();
        if let Some(model) = &self.model {
            args.extend(["--model", model].map(OsString::from));
        }
        if let Some(session) = &self.session_id {
            args.extend(["--resume", session].map(OsString::from));
        }
        args
    }
}

/// The `--mcp-config` JSON that registers the bridge as a stdio server.
fn mcp_config(bridge_command: &Path, socket: &Path) -> String {
    json!({
        "mcpServers": {
            SERVER_NAME: {
                "type": "stdio",
                "command": bridge_command,
                "args": ["--mcp-bridge", socket],
            }
        }
    })
    .to_string()
}

/// What one run of Claude Code has reported so far.
#[derive(Default)]
struct Turn {
    session_id: Option<String>,
    /// Tool names by `tool_use` id, to label results.
    tools: HashMap<String, String>,
    result: Option<Result<String, String>>,
    wrote_text: bool,
}

impl Turn {
    async fn handle(&mut self, line: &str, events: &async_channel::Sender<AgentEvent>) {
        let Ok(message) = serde_json::from_str::<Value>(line) else {
            tracing::debug!("skipping a line of Claude Code output that is not JSON");
            return;
        };
        if let Some(session) = message["session_id"].as_str() {
            self.session_id = Some(session.to_string());
        }
        match message["type"].as_str().unwrap_or_default() {
            "assistant" => self.assistant(&message, events).await,
            "user" => self.tool_results(&message, events).await,
            "result" => self.result = Some(final_result(&message)),
            _ => {}
        }
    }

    async fn assistant(&mut self, message: &Value, events: &async_channel::Sender<AgentEvent>) {
        for block in content_blocks(message) {
            match block["type"].as_str().unwrap_or_default() {
                "text" => {
                    let text = block["text"].as_str().unwrap_or_default();
                    if text.is_empty() {
                        continue;
                    }
                    // Separate replies from different model rounds.
                    let text = if self.wrote_text {
                        format!("\n\n{text}")
                    } else {
                        text.to_string()
                    };
                    self.wrote_text = true;
                    emit(events, AgentEvent::Text(text)).await;
                }
                "tool_use" => {
                    let name = tool_name(block["name"].as_str().unwrap_or_default());
                    if let Some(id) = block["id"].as_str() {
                        self.tools.insert(id.to_string(), name.clone());
                    }
                    emit(
                        events,
                        AgentEvent::ToolStarted {
                            name,
                            input: block["input"].clone(),
                        },
                    )
                    .await;
                }
                _ => {}
            }
        }
    }

    async fn tool_results(&mut self, message: &Value, events: &async_channel::Sender<AgentEvent>) {
        for block in content_blocks(message) {
            if block["type"] != "tool_result" {
                continue;
            }
            let id = block["tool_use_id"].as_str().unwrap_or_default();
            let name = self
                .tools
                .get(id)
                .cloned()
                .unwrap_or_else(|| id.to_string());
            let text = match &block["content"] {
                Value::String(text) => text.clone(),
                Value::Array(parts) => parts
                    .iter()
                    .filter_map(|p| p["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n"),
                _ => String::new(),
            };
            emit(
                events,
                AgentEvent::ToolFinished {
                    name,
                    ok: !block["is_error"].as_bool().unwrap_or(false),
                    preview: preview(&text),
                },
            )
            .await;
        }
    }
}

fn content_blocks(message: &Value) -> impl Iterator<Item = &Value> {
    message["message"]["content"]
        .as_array()
        .into_iter()
        .flatten()
}

/// The app's tool name, without Claude Code's `mcp__penguin-mail__` prefix.
fn tool_name(name: &str) -> String {
    let prefix = format!("mcp__{SERVER_NAME}__");
    name.strip_prefix(&prefix).unwrap_or(name).to_string()
}

fn final_result(message: &Value) -> Result<String, String> {
    let text = message["result"].as_str().unwrap_or_default().to_string();
    if !message["is_error"].as_bool().unwrap_or(false) {
        return Ok(text);
    }
    if !text.is_empty() {
        return Err(text);
    }
    let errors: Vec<&str> = message["errors"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    if !errors.is_empty() {
        return Err(errors.join("; "));
    }
    let subtype = message["subtype"].as_str().unwrap_or("unknown error");
    Err(format!("Claude Code failed: {subtype}"))
}

/// Checks that the CLI runs and that the user has signed in once.
pub(crate) async fn test(command: &Path) -> Result<String, AiError> {
    let run = Command::new(command)
        .arg("--version")
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(Duration::from_secs(10), run)
        .await
        .map_err(|_| AiError::Other(format!("{} --version took too long", command.display())))?
        .map_err(|e| AiError::Other(format!("could not run {}: {e}", command.display())))?;
    if !output.status.success() {
        return Err(AiError::Other(format!(
            "{} --version failed: {}",
            command.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let signed_in = std::env::var_os("HOME")
        .map(|home| Path::new(&home).join(".claude").is_dir())
        .unwrap_or(false);
    if !signed_in {
        return Err(AiError::Other(
            "Claude Code is installed but has never run. Run `claude` in a terminal and sign in."
                .to_string(),
        ));
    }
    // The CLI prints "2.1.278 (Claude Code)".
    let stdout = String::from_utf8_lossy(&output.stdout);
    let version = stdout.split_whitespace().next().unwrap_or_default();
    Ok(format!("Claude Code {version}"))
}
