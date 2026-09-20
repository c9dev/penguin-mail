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
use crate::{AgentEvent, AiError, Model, ModelList, ToolHost};

/// Aliases Claude Code takes for `--model`. Each one follows the newest
/// version of that model. `claude --help` names fable, opus and sonnet;
/// haiku is the fourth short name in the CLI's own model catalog.
const ALIASES: [&str; 4] = ["opus", "sonnet", "haiku", "fable"];

/// Where Claude Code caches the model catalog it fetches, under its config
/// directory.
const CATALOG_DIR: &str = "cache/model-catalog";

/// The aliases alone, for the row that lists what is on this computer.
pub(crate) fn claude_aliases() -> Vec<String> {
    ALIASES.iter().map(|m| m.to_string()).collect()
}

fn alias_models() -> Vec<Model> {
    ALIASES
        .iter()
        .map(|alias| Model {
            id: alias.to_string(),
            name: format!("{}, newest version", title_case(alias)),
            alias: true,
        })
        .collect()
}

/// What the installed CLI can run: the aliases first, then every version in
/// the model catalog the CLI keeps. Without that catalog the aliases are all
/// we know, and the note says so.
pub(crate) async fn list_models(command: &Path) -> Result<ModelList, AiError> {
    let version = cli_version(command).await;
    let Some((path, body)) = read_catalog() else {
        return Ok(ModelList::with_note(
            alias_models(),
            "Claude Code has not saved its model catalog yet, so only the aliases are listed. \
             Run claude once in a terminal to fill it in.",
        ));
    };
    let models = catalog_models(&body, version.as_deref());
    if models.is_empty() {
        return Ok(ModelList::with_note(
            alias_models(),
            format!(
                "Claude Code's model catalog at {} lists no models, so only the aliases are listed.",
                path.display()
            ),
        ));
    }
    Ok(ModelList::new(models))
}

/// The models in one catalog file, as the picker shows them: an alias per
/// model, saying which version it points at now, then the versions to pin.
pub(crate) fn catalog_models(body: &Value, cli_version: Option<&str>) -> Vec<Model> {
    let mut aliases: Vec<Model> = Vec::new();
    let mut versions: Vec<Model> = Vec::new();
    for entry in body["catalog"]["config"]["models"]
        .as_array()
        .into_iter()
        .flatten()
    {
        let Some(id) = entry["id"].as_str() else {
            continue;
        };
        if !runs_here(entry, cli_version) {
            continue;
        }
        let name = entry["name"].as_str().unwrap_or(id);
        let short = entry["short_name"].as_str().unwrap_or_default();
        let alias = short.to_lowercase();
        if ALIASES.contains(&alias.as_str()) && !aliases.iter().any(|m| m.id == alias) {
            aliases.push(Model {
                id: alias,
                name: format!("{short}, newest version (now {name})"),
                alias: true,
            });
        }
        versions.push(Model::named(id, name));
    }
    aliases.into_iter().chain(versions).collect()
}

/// False when the catalog marks a model as needing a newer CLI than the one
/// installed. An unreadable version keeps the model, since guessing wrong
/// would hide a model that works.
fn runs_here(entry: &Value, cli_version: Option<&str>) -> bool {
    let (Some(needs), Some(have)) = (entry["min_claude_code_version"].as_str(), cli_version) else {
        return true;
    };
    parts(have) >= parts(needs)
}

/// A dotted version as numbers, so 2.1.278 sorts above 2.1.9.
fn parts(version: &str) -> Vec<u64> {
    version
        .split('.')
        .map(|part| {
            part.chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
                .parse()
                .unwrap_or(0)
        })
        .collect()
}

/// The newest catalog file Claude Code has written, with its contents.
fn read_catalog() -> Option<(PathBuf, Value)> {
    let dir = config_dir()?.join(CATALOG_DIR);
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|e| e == "json"))
        .collect();
    // Claude Code names its own catalog with a "-cc" suffix; other surfaces
    // describe models this CLI does not run.
    let is_cc = |path: &PathBuf| {
        path.file_stem()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.ends_with("-cc"))
    };
    if files.iter().any(is_cc) {
        files.retain(is_cc);
    }
    files.sort_by_key(modified);
    let path = files.pop()?;
    let body = serde_json::from_str(&std::fs::read_to_string(&path).ok()?).ok()?;
    Some((path, body))
}

fn modified(path: &PathBuf) -> std::time::SystemTime {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .unwrap_or(std::time::UNIX_EPOCH)
}

/// Claude Code's config directory: `CLAUDE_CONFIG_DIR`, or `~/.claude`.
fn config_dir() -> Option<PathBuf> {
    match std::env::var_os("CLAUDE_CONFIG_DIR").filter(|dir| !dir.is_empty()) {
        Some(dir) => Some(PathBuf::from(dir)),
        None => std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".claude")),
    }
}

fn title_case(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
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

/// The installed CLI's version, such as `2.1.278`, or an error saying why it
/// did not answer. The CLI prints "2.1.278 (Claude Code)".
async fn version_output(command: &Path) -> Result<String, AiError> {
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
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_string())
}

/// The installed CLI's version, or none when it does not say.
async fn cli_version(command: &Path) -> Option<String> {
    match version_output(command).await {
        Ok(version) if !version.is_empty() => Some(version),
        Ok(_) => None,
        Err(err) => {
            tracing::debug!(error = %err, "could not read Claude Code's version");
            None
        }
    }
}

/// Checks that the CLI runs and that the user has signed in once.
pub(crate) async fn test(command: &Path) -> Result<String, AiError> {
    let version = version_output(command).await?;
    let signed_in = std::env::var_os("HOME")
        .map(|home| Path::new(&home).join(".claude").is_dir())
        .unwrap_or(false);
    if !signed_in {
        return Err(AiError::Other(
            "Claude Code is installed but has never run. Run `claude` in a terminal and sign in."
                .to_string(),
        ));
    }
    Ok(format!("Claude Code {version}"))
}
