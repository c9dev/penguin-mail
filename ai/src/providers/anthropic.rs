//! Anthropic's Messages API over raw HTTP, with the user's own API key.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{Value, json};

use super::{
    MAX_ROUNDS, emit, error_text, http_client, network, outcome_text, refuse_tool, run_tool,
    too_many_rounds,
};
use crate::sse::SseReader;
use crate::{AgentEvent, AiError, Model, ModelList, ToolHost, ToolOutcome, ToolSpec};

pub(crate) const ANTHROPIC_BASE: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2023-06-01";
const MAX_TOKENS: u32 = 64_000;
/// Tokens a model with a fixed thinking budget may spend thinking in one
/// request. It has to stay below `MAX_TOKENS`, which covers thinking and
/// reply together.
const THINKING_BUDGET: u32 = 16_000;
/// Models asked for per page of `/v1/models`, the most the API allows.
const MODEL_PAGE: u32 = 1000;
/// Pages read before the list stops, so a runaway cursor cannot loop.
const MAX_MODEL_PAGES: usize = 20;

/// The API base, or `PENGUIN_MAIL_ANTHROPIC_BASE` when set.
pub(crate) fn default_base() -> String {
    std::env::var("PENGUIN_MAIL_ANTHROPIC_BASE")
        .ok()
        .filter(|b| !b.is_empty())
        .map(|b| b.trim_end_matches('/').to_string())
        .unwrap_or_else(|| ANTHROPIC_BASE.to_string())
}

pub(crate) struct AnthropicChat {
    base_url: String,
    api_key: String,
    model: String,
    system_prompt: String,
    history: Vec<Value>,
    client: reqwest::Client,
    /// Ask the model to think, when it can.
    pub(crate) think: bool,
}

/// A content block being assembled from stream events.
struct Block {
    value: Value,
    /// `input_json_delta` fragments for a `tool_use` block.
    json: String,
}

struct Reply {
    content: Vec<Value>,
    stop_reason: String,
    /// Tool input that failed to parse, by `tool_use` id.
    bad_input: BTreeMap<String, String>,
}

impl AnthropicChat {
    pub(crate) fn new(
        base_url: String,
        api_key: String,
        model: String,
        system_prompt: String,
    ) -> AnthropicChat {
        AnthropicChat {
            base_url,
            api_key,
            model,
            system_prompt,
            history: Vec::new(),
            client: http_client(),
            think: false,
        }
    }

    pub(crate) async fn send(
        &mut self,
        text: String,
        host: Arc<dyn ToolHost>,
        events: &async_channel::Sender<AgentEvent>,
    ) -> Result<String, AiError> {
        crate::history::trim(&mut self.history, crate::history::BUDGET);
        let saved = self.history.len();
        self.history.push(json!({"role": "user", "content": text}));
        let result = self.run(&host, events).await;
        if result.is_err() {
            self.history.truncate(saved);
        }
        result
    }

    async fn run(
        &mut self,
        host: &Arc<dyn ToolHost>,
        events: &async_channel::Sender<AgentEvent>,
    ) -> Result<String, AiError> {
        let specs = host.specs();
        for _ in 0..MAX_ROUNDS {
            let reply = self.round(&specs, events).await?;
            match reply.stop_reason.as_str() {
                "refusal" => {
                    return Err(AiError::Api(
                        "Claude declined this request. Rephrase it or start a new chat."
                            .to_string(),
                    ));
                }
                "max_tokens" => {
                    return Err(AiError::Api(format!(
                        "the reply hit the {MAX_TOKENS}-token limit before it finished"
                    )));
                }
                _ => {}
            }
            self.history
                .push(json!({"role": "assistant", "content": reply.content}));
            match reply.stop_reason.as_str() {
                "tool_use" => {
                    let results = run_tools(host, events, &reply).await;
                    self.history
                        .push(json!({"role": "user", "content": results}));
                }
                // The server paused a long turn; sending the history again resumes it.
                "pause_turn" => {}
                _ => return Ok(reply_text(&reply.content)),
            }
        }
        Err(too_many_rounds())
    }

    async fn round(
        &self,
        specs: &[ToolSpec],
        events: &async_channel::Sender<AgentEvent>,
    ) -> Result<Reply, AiError> {
        let mut body = json!({
            "model": self.model,
            "max_tokens": MAX_TOKENS,
            "stream": true,
            "messages": self.history,
        });
        if !self.system_prompt.is_empty() {
            body["system"] = json!(self.system_prompt);
        }
        if self.think
            && let Some(thinking) = thinking_request(&self.model)
        {
            body["thinking"] = thinking;
        }
        if !specs.is_empty() {
            body["tools"] = specs
                .iter()
                .map(|s| {
                    json!({
                        "name": s.name,
                        "description": s.description,
                        "input_schema": s.input_schema,
                    })
                })
                .collect();
        }
        tracing::debug!(model = %self.model, "messages request");
        let response = self
            .client
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
            .json(&body)
            .send()
            .await
            .map_err(network)?;
        if !response.status().is_success() {
            return Err(status_error(response).await);
        }
        read_stream(SseReader::new(response), events).await
    }
}

async fn status_error(response: reqwest::Response) -> AiError {
    let (status, message) = error_text(response).await;
    match status {
        401 => AiError::Api(format!("Anthropic rejected the API key: {message}")),
        _ => AiError::Api(format!("HTTP {status}: {message}")),
    }
}

/// Runs every `tool_use` block and returns all results for one user message.
async fn run_tools(
    host: &Arc<dyn ToolHost>,
    events: &async_channel::Sender<AgentEvent>,
    reply: &Reply,
) -> Vec<Value> {
    let mut results = Vec::new();
    for block in reply.content.iter().filter(|b| b["type"] == "tool_use") {
        let id = block["id"].as_str().unwrap_or_default();
        let name = block["name"].as_str().unwrap_or_default();
        let outcome = match reply.bad_input.get(id) {
            Some(raw) => {
                let outcome = ToolOutcome::Err(json!({"INVALID_JSON": raw}).to_string());
                refuse_tool(events, id, name, raw, &outcome).await;
                outcome
            }
            None => run_tool(host, events, id, name, block["input"].clone()).await,
        };
        let mut result = json!({
            "type": "tool_result",
            "tool_use_id": id,
            "content": outcome_text(&outcome),
        });
        if matches!(outcome, ToolOutcome::Err(_)) {
            result["is_error"] = json!(true);
        }
        results.push(result);
    }
    results
}

/// The `thinking` field for a model, or `None` for one that cannot think.
///
/// Anthropic changed how a request asks. Models from Claude 3.7 to the 4.5
/// family take a fixed budget. From 4.6 on they choose how long to think
/// (`adaptive`), and from 4.7 on they reject a budget with an error. Those
/// newer models also hide the thinking unless asked for a summary, and a
/// pane that shows nothing while the model thinks looks stuck. An unknown
/// name gets no field, since a model without thinking rejects one.
pub(crate) fn thinking_request(model: &str) -> Option<Value> {
    let model = model.trim().to_lowercase();
    let rest = model.strip_prefix("claude-")?;
    let parts: Vec<&str> = rest.split('-').collect();
    // A version number, as opposed to a date suffix such as 20250929.
    let number = |part: Option<&&str>| {
        part.filter(|p| p.len() <= 2)
            .and_then(|p| p.parse::<u32>().ok())
    };
    let version = match parts.first().copied() {
        Some("fable" | "mythos") => return Some(adaptive(true)),
        Some("opus" | "sonnet" | "haiku") => {
            (number(parts.get(1))?, number(parts.get(2)).unwrap_or(0))
        }
        // The older naming, as in claude-3-7-sonnet-20250219.
        _ => (number(parts.first())?, number(parts.get(1)).unwrap_or(0)),
    };
    match version {
        v if v < (3, 7) => None,
        v if v < (4, 6) => Some(json!({"type": "enabled", "budget_tokens": THINKING_BUDGET})),
        (4, 6) => Some(adaptive(false)),
        _ => Some(adaptive(true)),
    }
}

/// Adaptive thinking; `summarized` asks for readable text where the model
/// would otherwise send it empty.
fn adaptive(summarized: bool) -> Value {
    if summarized {
        json!({"type": "adaptive", "display": "summarized"})
    } else {
        json!({"type": "adaptive"})
    }
}

fn reply_text(content: &[Value]) -> String {
    content
        .iter()
        .filter(|b| b["type"] == "text")
        .filter_map(|b| b["text"].as_str())
        .collect()
}

async fn read_stream(
    mut sse: SseReader,
    events: &async_channel::Sender<AgentEvent>,
) -> Result<Reply, AiError> {
    let mut blocks: BTreeMap<u64, Block> = BTreeMap::new();
    let mut stop_reason = String::new();
    let mut stopped = false;
    while let Some(event) = sse.next().await? {
        let data: Value = serde_json::from_str(&event.data)
            .map_err(|e| AiError::Api(format!("unreadable stream event: {e}")))?;
        let index = data["index"].as_u64().unwrap_or(0);
        match data["type"].as_str().unwrap_or(&event.event) {
            "content_block_start" => {
                let mut value = data["content_block"].clone();
                if value["type"] == "tool_use" {
                    value["input"] = json!({});
                }
                blocks.insert(
                    index,
                    Block {
                        value,
                        json: String::new(),
                    },
                );
            }
            "content_block_delta" => {
                let Some(block) = blocks.get_mut(&index) else {
                    continue;
                };
                let delta = &data["delta"];
                match delta["type"].as_str().unwrap_or_default() {
                    "text_delta" => {
                        let text = delta["text"].as_str().unwrap_or_default();
                        append(&mut block.value, "text", text);
                        if !text.is_empty() {
                            emit(events, AgentEvent::Text(text.to_string())).await;
                        }
                    }
                    "thinking_delta" => {
                        let thinking = delta["thinking"].as_str().unwrap_or_default();
                        append(&mut block.value, "thinking", thinking);
                        if !thinking.is_empty() {
                            emit(events, AgentEvent::Thinking(thinking.to_string())).await;
                        }
                    }
                    "signature_delta" => {
                        append(
                            &mut block.value,
                            "signature",
                            delta["signature"].as_str().unwrap_or_default(),
                        );
                    }
                    "input_json_delta" => {
                        block
                            .json
                            .push_str(delta["partial_json"].as_str().unwrap_or_default());
                    }
                    other => tracing::debug!(delta = other, "ignoring content delta"),
                }
            }
            "message_delta" => {
                if let Some(reason) = data["delta"]["stop_reason"].as_str() {
                    stop_reason = reason.to_string();
                }
            }
            "message_stop" => {
                stopped = true;
                break;
            }
            "error" => {
                let message = data["error"]["message"].as_str().unwrap_or("unknown error");
                return Err(AiError::Api(message.to_string()));
            }
            // message_start, content_block_stop and ping carry nothing we keep.
            _ => {}
        }
    }
    if !stopped {
        return Err(AiError::Network(
            "the connection closed before the reply finished".to_string(),
        ));
    }
    let mut content = Vec::with_capacity(blocks.len());
    let mut bad_input = BTreeMap::new();
    for block in blocks.into_values() {
        let mut value = block.value;
        if value["type"] == "tool_use" && !block.json.trim().is_empty() {
            match serde_json::from_str::<Value>(&block.json) {
                Ok(input @ Value::Object(_)) => value["input"] = input,
                _ => {
                    let id = value["id"].as_str().unwrap_or_default().to_string();
                    bad_input.insert(id, block.json);
                }
            }
        }
        content.push(value);
    }
    Ok(Reply {
        content,
        stop_reason,
        bad_input,
    })
}

fn append(block: &mut Value, field: &str, text: &str) {
    match &mut block[field] {
        Value::String(current) => current.push_str(text),
        other => *other = Value::String(text.to_string()),
    }
}

/// Every model the key may use, newest first, following the pages Anthropic
/// hands back through `has_more` and `last_id`.
pub(crate) async fn list_models(base_url: &str, api_key: &str) -> Result<ModelList, AiError> {
    if api_key.trim().is_empty() {
        return Err(AiError::Api(
            "Add an Anthropic API key to see the models it can use.".to_string(),
        ));
    }
    let client = http_client();
    let mut models = Vec::new();
    let mut after: Option<String> = None;
    for _ in 0..MAX_MODEL_PAGES {
        let mut url = format!("{base_url}/v1/models?limit={MODEL_PAGE}");
        if let Some(id) = &after {
            url.push_str(&format!("&after_id={id}"));
        }
        let response = client
            .get(url)
            .header("x-api-key", api_key)
            .header("anthropic-version", API_VERSION)
            .send()
            .await
            .map_err(network)?;
        if !response.status().is_success() {
            return Err(status_error(response).await);
        }
        let body: Value = response.json().await.map_err(network)?;
        models.extend(parse_models(&body));
        if !body["has_more"].as_bool().unwrap_or(false) {
            break;
        }
        match body["last_id"].as_str() {
            Some(id) => after = Some(id.to_string()),
            None => break,
        }
    }
    Ok(ModelList::new(models))
}

/// One page of `/v1/models`. Anthropic sends a `display_name` such as
/// "Claude Opus 5" beside the dated id, and the picker shows both.
pub(crate) fn parse_models(body: &Value) -> Vec<Model> {
    body["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let id = m["id"].as_str()?;
            Some(Model::named(
                id,
                m["display_name"].as_str().unwrap_or_default(),
            ))
        })
        .collect()
}
