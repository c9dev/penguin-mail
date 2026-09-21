//! Provider state and the agent loop.

mod anthropic;
mod claude_code;
mod openai;
mod think;

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use crate::{AgentEvent, AiError, ModelList, ProviderConfig, ToolHost, ToolOutcome};

pub(crate) use anthropic::AnthropicChat;
#[cfg(test)]
pub(crate) use anthropic::list_models as anthropic_models;
#[cfg(test)]
pub(crate) use anthropic::thinking_request;
#[cfg(test)]
pub(crate) use claude_code::catalog_models;
pub(crate) use claude_code::{ClaudeCodeChat, claude_aliases};
pub(crate) use openai::{OpenAiChat, model_ids};
#[cfg(test)]
pub(crate) use think::{Piece, ThinkTags};

/// Model requests per user message before the loop gives up.
pub(crate) const MAX_ROUNDS: usize = 25;

/// Longest tool preview shown in the UI, in characters.
const PREVIEW_CHARS: usize = 160;

pub(crate) enum State {
    OpenAi(OpenAiChat),
    Anthropic(AnthropicChat),
    ClaudeCode(ClaudeCodeChat),
}

impl State {
    pub(crate) fn new(config: ProviderConfig, system_prompt: String) -> State {
        match config {
            ProviderConfig::OpenAiCompatible {
                base_url,
                api_key,
                model,
            } => State::OpenAi(OpenAiChat::new(&base_url, api_key, model, system_prompt)),
            ProviderConfig::Anthropic { api_key, model } => State::Anthropic(AnthropicChat::new(
                anthropic::default_base(),
                api_key,
                model,
                system_prompt,
            )),
            ProviderConfig::ClaudeCode { command, model } => {
                State::ClaudeCode(ClaudeCodeChat::new(command, model, system_prompt))
            }
        }
    }

    /// Only Anthropic's API takes a request for thinking. Claude Code thinks
    /// as its own settings say, and local servers think when their model
    /// does, sending it back as `reasoning_content` or `<think>` spans.
    pub(crate) fn ask_for_thinking(&mut self) {
        if let State::Anthropic(chat) = self {
            chat.think = true;
        }
    }

    /// Anthropic's API and Claude Code bring their own web tools. A local
    /// server has none, and the app offers its own through the tool host.
    pub(crate) fn set_web(&mut self, on: bool) {
        match self {
            State::Anthropic(chat) => chat.web = on,
            State::ClaudeCode(chat) => chat.web = on,
            State::OpenAi(_) => {}
        }
    }

    pub(crate) async fn send(
        &mut self,
        text: String,
        host: Arc<dyn ToolHost>,
        events: async_channel::Sender<AgentEvent>,
    ) -> Result<String, AiError> {
        match self {
            State::OpenAi(chat) => chat.send(text, host, &events).await,
            State::Anthropic(chat) => chat.send(text, host, &events).await,
            State::ClaudeCode(chat) => chat.send(text, host, &events).await,
        }
    }
}

pub(crate) async fn list_models(config: &ProviderConfig) -> Result<ModelList, AiError> {
    match config {
        ProviderConfig::OpenAiCompatible {
            base_url, api_key, ..
        } => openai::list_models(&openai::trim_base(base_url), api_key.as_deref()).await,
        ProviderConfig::Anthropic { api_key, .. } => {
            anthropic::list_models(&anthropic::default_base(), api_key).await
        }
        ProviderConfig::ClaudeCode { command, .. } => claude_code::list_models(command).await,
    }
}

pub(crate) async fn test(config: &ProviderConfig) -> Result<String, AiError> {
    match config {
        ProviderConfig::OpenAiCompatible { .. } | ProviderConfig::Anthropic { .. } => {
            let models = list_models(config).await?.models;
            Ok(match models.len() {
                1 => "Connected, 1 model available".to_string(),
                n => format!("Connected, {n} models available"),
            })
        }
        ProviderConfig::ClaudeCode { command, .. } => claude_code::test(command).await,
    }
}

/// An HTTP client for streaming model replies. It has no overall timeout,
/// because a reply with tool calls can take minutes.
pub(crate) fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .read_timeout(Duration::from_secs(300))
        .build()
        .unwrap_or_default()
}

pub(crate) fn network(e: reqwest::Error) -> AiError {
    AiError::Network(e.to_string())
}

/// Turns a failed response into an error message, preferring the JSON
/// `error.message` field that OpenAI and Anthropic both send.
pub(crate) async fn error_text(response: reqwest::Response) -> (u16, String) {
    let status = response.status().as_u16();
    let body = response.text().await.unwrap_or_default();
    let parsed: Option<Value> = serde_json::from_str(&body).ok();
    let message = parsed.as_ref().and_then(|v| {
        v["error"]["message"]
            .as_str()
            .or_else(|| v["error"].as_str())
            .or_else(|| v["message"].as_str())
            .map(str::to_string)
    });
    let message = message.unwrap_or_else(|| truncate(body.trim(), 500));
    (status, message)
}

pub(crate) fn too_many_rounds() -> AiError {
    AiError::Other(format!(
        "stopped after {MAX_ROUNDS} model requests without a final answer"
    ))
}

/// Sends an event to the UI. A closed channel means nobody is watching,
/// and the turn carries on.
pub(crate) async fn emit(events: &async_channel::Sender<AgentEvent>, event: AgentEvent) {
    let _ = events.send(event).await;
}

/// Runs one tool through the host and reports it to the UI.
pub(crate) async fn run_tool(
    host: &Arc<dyn ToolHost>,
    events: &async_channel::Sender<AgentEvent>,
    id: &str,
    name: &str,
    input: Value,
) -> ToolOutcome {
    emit(
        events,
        AgentEvent::ToolStarted {
            id: id.to_string(),
            name: name.to_string(),
            input: input.clone(),
        },
    )
    .await;
    tracing::debug!(tool = name, "running tool");
    let outcome = host.call(name.to_string(), input).await;
    finish_tool(events, id, name, &outcome).await;
    outcome
}

/// Reports a call that never reached the host, because its input was not
/// JSON the tool could take. The UI shows the raw text it came as.
pub(crate) async fn refuse_tool(
    events: &async_channel::Sender<AgentEvent>,
    id: &str,
    name: &str,
    raw: &str,
    outcome: &ToolOutcome,
) {
    emit(
        events,
        AgentEvent::ToolStarted {
            id: id.to_string(),
            name: name.to_string(),
            input: Value::String(raw.to_string()),
        },
    )
    .await;
    finish_tool(events, id, name, outcome).await;
}

pub(crate) async fn finish_tool(
    events: &async_channel::Sender<AgentEvent>,
    id: &str,
    name: &str,
    outcome: &ToolOutcome,
) {
    let ok = matches!(outcome, ToolOutcome::Ok(_));
    let output = outcome_text(outcome);
    emit(
        events,
        AgentEvent::ToolFinished {
            id: id.to_string(),
            name: name.to_string(),
            ok,
            preview: preview(&output),
            output,
        },
    )
    .await;
}

/// What the model reads as the tool's result.
pub(crate) fn outcome_text(outcome: &ToolOutcome) -> String {
    match outcome {
        ToolOutcome::Ok(Value::String(text)) => text.clone(),
        ToolOutcome::Ok(value) => value.to_string(),
        ToolOutcome::Err(message) => message.clone(),
    }
}

/// The first line of `text`, cut to fit a tool row in the UI.
pub(crate) fn preview(text: &str) -> String {
    let mut lines = text.trim().lines();
    let cut = truncate(lines.next().unwrap_or(""), PREVIEW_CHARS);
    if lines.next().is_some() && !cut.ends_with('…') {
        format!("{cut}…")
    } else {
        cut
    }
}

pub(crate) fn truncate(text: &str, max_chars: usize) -> String {
    match text.char_indices().nth(max_chars) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_string(),
    }
}

/// Parses a tool's accumulated argument string. Empty means no arguments.
pub(crate) fn parse_tool_input(raw: &str) -> Result<Value, String> {
    if raw.trim().is_empty() {
        return Ok(Value::Object(Default::default()));
    }
    match serde_json::from_str::<Value>(raw) {
        Ok(value @ Value::Object(_)) => Ok(value),
        Ok(_) => Err(format!("tool input must be a JSON object, got: {raw}")),
        Err(e) => Err(format!("tool input is not valid JSON ({e}): {raw}")),
    }
}
