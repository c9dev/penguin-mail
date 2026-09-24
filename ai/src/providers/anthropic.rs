//! Anthropic's Messages API over raw HTTP, with the user's own API key.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{Value, json};

use super::{
    MAX_ROUNDS, emit, error_text, finish_tool, http_client, network, outcome_text, refuse_tool,
    run_tool, too_many_rounds, truncate,
};
use crate::history::History;
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
/// Anthropic's own web search and page fetch, run on its servers. These
/// are the newest versions the docs list.
pub(crate) const WEB_SEARCH_TOOL: &str = "web_search_20260318";
pub(crate) const WEB_FETCH_TOOL: &str = "web_fetch_20260318";
/// Searches and fetches the model may run in one request.
const WEB_USES: u32 = 8;
/// Tokens of one fetched page that reach the model. A page goes back with
/// every later request of the chat, so a long one would crowd out the mail
/// the chat is about.
const FETCH_TOKENS: u32 = 20_000;
/// Characters of a fetched page the tool row shows.
const FETCH_SHOWN: usize = 2_000;

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
    history: History,
    client: reqwest::Client,
    /// Ask the model to think, when it can.
    pub(crate) think: bool,
    /// Offer Anthropic's web search and page fetch.
    pub(crate) web: bool,
    /// A JSON schema the reply must follow, where the model takes one.
    pub(crate) format: Option<Value>,
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
            history: History::default(),
            client: http_client(),
            think: false,
            web: false,
            format: None,
        }
    }

    pub(crate) async fn send(
        &mut self,
        text: String,
        host: Arc<dyn ToolHost>,
        events: &async_channel::Sender<AgentEvent>,
    ) -> Result<String, AiError> {
        self.history.begin(json!({"role": "user", "content": text}));
        let result = self.run(&host, events).await;
        match result {
            Ok(_) => self.history.commit(),
            Err(_) => self.history.rollback(),
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
            "messages": messages(&self.history),
        });
        if self.think
            && let Some(thinking) = thinking_request(&self.model)
        {
            body["thinking"] = thinking;
        }
        let mut output = serde_json::Map::new();
        if !self.think && thinks_unasked(&self.model) {
            // These models think whether or not the request asks, so a chat
            // that wants a quick answer, such as a translation, asks for the
            // least effort instead.
            output.insert("effort".into(), json!("low"));
        }
        if let Some(schema) = &self.format
            && takes_format(&self.model)
        {
            output.insert(
                "format".into(),
                json!({"type": "json_schema", "schema": schema}),
            );
        }
        if !output.is_empty() {
            body["output_config"] = Value::Object(output);
        }
        let mut tools: Vec<Value> = specs
            .iter()
            .map(|s| {
                json!({
                    "name": s.name,
                    "description": s.description,
                    "input_schema": s.input_schema,
                })
            })
            .collect();
        if self.web {
            tools.extend(web_tools());
        }
        // Tools render before the system prompt, so one breakpoint at the
        // end of whichever comes last caches both: 68 tools are about 12,000
        // tokens that every round would otherwise send at full price.
        if !self.system_prompt.is_empty() {
            body["system"] = json!([{
                "type": "text",
                "text": self.system_prompt,
                "cache_control": ephemeral(),
            }]);
        } else if let Some(last) = tools.last_mut() {
            last["cache_control"] = ephemeral();
        }
        if !tools.is_empty() {
            body["tools"] = json!(tools);
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

/// A cache breakpoint with the default five minutes: the rounds of one
/// turn follow each other within seconds, and a question asked more than
/// five minutes after the last would pay double to keep an hour's entry.
fn ephemeral() -> Value {
    json!({"type": "ephemeral"})
}

/// The chat as the request sends it, with a cache breakpoint on the last
/// block of the message the history names. The next round sends all of
/// this again and reads it from the cache. The history itself never holds
/// a breakpoint, since the next round moves it and the bytes before the
/// new one have to match the ones the cache holds.
fn messages(history: &History) -> Vec<Value> {
    let mut messages = history.messages();
    if let Some(at) = history.cache_point()
        && let Some(message) = messages.get_mut(at)
    {
        if let Value::String(text) = &message["content"] {
            message["content"] = json!([{"type": "text", "text": text}]);
        }
        if let Some(block) = message["content"].as_array_mut().and_then(|b| b.last_mut()) {
            block["cache_control"] = ephemeral();
        }
    }
    messages
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

/// Anthropic's web search and page fetch, as the request declares them.
///
/// Both run as direct calls. From their 2026 versions on they default to
/// running inside Anthropic's code execution, which filters what comes back
/// but refuses models older than Claude 4.6 and adds code blocks and a
/// container to the chat. Direct calls work on every model that has the
/// tools, and the page size cap does the filtering's job.
pub(crate) fn web_tools() -> Vec<Value> {
    vec![
        json!({
            "type": WEB_SEARCH_TOOL,
            "name": "web_search",
            "max_uses": WEB_USES,
            "allowed_callers": ["direct"],
        }),
        json!({
            "type": WEB_FETCH_TOOL,
            "name": "web_fetch",
            "max_uses": WEB_USES,
            "max_content_tokens": FETCH_TOKENS,
            "allowed_callers": ["direct"],
        }),
    ]
}

/// The tool row for a server tool's result block, such as
/// `web_search_tool_result`: the tool's name and what it found. The block
/// itself goes back to Anthropic unchanged; this is only what the pane
/// shows.
pub(crate) fn server_result(block: &Value) -> (String, ToolOutcome) {
    let kind = block["type"].as_str().unwrap_or_default();
    let name = kind
        .strip_suffix("_tool_result")
        .unwrap_or(kind)
        .to_string();
    let content = &block["content"];
    if let Some(code) = content["error_code"].as_str() {
        let problem = format!("{name} failed: {code}");
        return (name, ToolOutcome::Err(problem));
    }
    let text = match kind {
        "web_search_tool_result" => {
            let hits: Vec<String> = content
                .as_array()
                .into_iter()
                .flatten()
                .map(|hit| {
                    format!(
                        "{}\n{}",
                        hit["title"].as_str().unwrap_or_default(),
                        hit["url"].as_str().unwrap_or_default()
                    )
                })
                .collect();
            match hits.is_empty() {
                true => "No results.".to_string(),
                false => hits.join("\n\n"),
            }
        }
        "web_fetch_tool_result" => {
            let document = &content["content"];
            let url = content["url"].as_str().unwrap_or_default();
            let title = document["title"].as_str().unwrap_or(url);
            let source = &document["source"];
            let body = match source["type"].as_str() {
                Some("text") => truncate(source["data"].as_str().unwrap_or_default(), FETCH_SHOWN),
                _ => source["media_type"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            };
            format!("{title}\n{url}\n\n{body}")
        }
        _ => truncate(&content.to_string(), FETCH_SHOWN),
    };
    (
        name,
        ToolOutcome::Ok(Value::String(text.trim_end().to_string())),
    )
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

/// Whether a model thinks when the request leaves `thinking` out. Up to
/// Opus 4.8 and Sonnet 4.6, leaving it out meant no thinking; Fable,
/// Mythos, and Opus and Sonnet from 5 on think anyway.
pub(crate) fn thinks_unasked(model: &str) -> bool {
    let model = model.trim().to_lowercase();
    let Some(rest) = model.strip_prefix("claude-") else {
        return false;
    };
    let mut parts = rest.split('-');
    match parts.next() {
        Some("fable" | "mythos") => true,
        Some("opus" | "sonnet") => parts
            .next()
            .and_then(|major| major.parse::<u32>().ok())
            .is_some_and(|major| major >= 5),
        _ => false,
    }
}

/// Whether a model answers in a JSON schema the request gives: Fable,
/// Mythos, Haiku 4.5, Opus from 4.8 and Sonnet from 5. Anthropic's list
/// leaves out Opus 4.6 and 4.7 and Sonnet 4.6, and a request that sends
/// one a schema could fail, so they keep answering from the prompt alone.
pub(crate) fn takes_format(model: &str) -> bool {
    let model = model.trim().to_lowercase();
    let Some(rest) = model.strip_prefix("claude-") else {
        return false;
    };
    let parts: Vec<&str> = rest.split('-').collect();
    let number = |at: usize| {
        parts
            .get(at)
            .filter(|p| p.len() <= 2)
            .and_then(|p| p.parse::<u32>().ok())
    };
    match parts.first().copied() {
        Some("fable" | "mythos") => true,
        Some("opus") => number(1).is_some_and(|major| (major, number(2).unwrap_or(0)) >= (4, 8)),
        Some("sonnet") => number(1).is_some_and(|major| major >= 5),
        Some("haiku") => number(1).is_some_and(|major| (major, number(2).unwrap_or(0)) >= (4, 5)),
        _ => false,
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
                if value["type"] == "tool_use" || value["type"] == "server_tool_use" {
                    value["input"] = json!({});
                }
                // A server tool's result arrives whole, in this one event.
                if let Some(id) = value["tool_use_id"].as_str()
                    && value["type"] != "tool_result"
                {
                    let (name, outcome) = server_result(&value);
                    finish_tool(events, id, &name, &outcome).await;
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
                    // Web search cites its sources. Each citation carries an
                    // index Anthropic wants back on later requests.
                    "citations_delta" => match &mut block.value["citations"] {
                        Value::Array(citations) => citations.push(delta["citation"].clone()),
                        other => *other = json!([delta["citation"].clone()]),
                    },
                    other => tracing::debug!(delta = other, "ignoring content delta"),
                }
            }
            // A server tool runs on Anthropic's side as soon as its input is
            // complete, so its row starts here rather than in `run_tools`.
            "content_block_stop" => {
                let Some(block) = blocks.get_mut(&index) else {
                    continue;
                };
                if block.value["type"] != "server_tool_use" {
                    continue;
                }
                if let Ok(input @ Value::Object(_)) = serde_json::from_str::<Value>(&block.json) {
                    block.value["input"] = input;
                }
                block.json.clear();
                emit(
                    events,
                    AgentEvent::ToolStarted {
                        id: block.value["id"].as_str().unwrap_or_default().to_string(),
                        name: block.value["name"].as_str().unwrap_or_default().to_string(),
                        input: block.value["input"].clone(),
                    },
                )
                .await;
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
            // message_start and ping carry nothing we keep.
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
