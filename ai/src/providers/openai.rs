//! Servers that speak OpenAI's chat completions API: LM Studio, Ollama,
//! llama.cpp, vLLM, Unsloth and OpenAI itself.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{Value, json};

use super::{
    MAX_ROUNDS, emit, error_text, finish_tool, http_client, network, outcome_text,
    parse_tool_input, run_tool, too_many_rounds,
};
use crate::sse::SseReader;
use crate::{AgentEvent, AiError, Model, ModelList, ToolHost, ToolOutcome, ToolSpec};

/// Added to the system prompt when the server refuses tool definitions.
const NO_TOOLS_NOTE: &str = "This model runs without tools, so you cannot read, \
change or send mail here. Answer from what the user tells you, and say so if \
they ask you to act on their mail.";

pub(crate) struct OpenAiChat {
    base_url: String,
    api_key: Option<String>,
    model: String,
    system_prompt: String,
    history: Vec<Value>,
    /// Set once the server rejects `tools`; later requests leave them out.
    tools_off: bool,
    client: reqwest::Client,
}

/// A tool call assembled from streamed fragments.
#[derive(Default)]
struct PendingCall {
    id: String,
    name: String,
    arguments: String,
}

struct Reply {
    content: String,
    calls: Vec<PendingCall>,
}

pub(crate) fn trim_base(base_url: &str) -> String {
    base_url.trim().trim_end_matches('/').to_string()
}

impl OpenAiChat {
    pub(crate) fn new(
        base_url: &str,
        api_key: Option<String>,
        model: String,
        system_prompt: String,
    ) -> OpenAiChat {
        OpenAiChat {
            base_url: trim_base(base_url),
            api_key: api_key.filter(|k| !k.is_empty()),
            model,
            system_prompt,
            history: Vec::new(),
            tools_off: false,
            client: http_client(),
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
        for round in 0..MAX_ROUNDS {
            let mut reply = self.round(&specs, events).await?;
            for (index, call) in reply.calls.iter_mut().enumerate() {
                if call.id.is_empty() {
                    call.id = format!("call_{round}_{index}");
                }
            }
            self.history.push(assistant_message(&reply));
            if reply.calls.is_empty() {
                return Ok(reply.content);
            }
            for call in reply.calls {
                let outcome = match parse_tool_input(&call.arguments) {
                    Ok(input) => run_tool(host, events, &call.name, input).await,
                    Err(message) => {
                        emit(
                            events,
                            AgentEvent::ToolStarted {
                                name: call.name.clone(),
                                input: Value::String(call.arguments.clone()),
                            },
                        )
                        .await;
                        let outcome = ToolOutcome::Err(message);
                        finish_tool(events, &call.name, &outcome).await;
                        outcome
                    }
                };
                let content = match &outcome {
                    ToolOutcome::Ok(_) => outcome_text(&outcome),
                    ToolOutcome::Err(message) => format!("Error: {message}"),
                };
                self.history.push(json!({
                    "role": "tool",
                    "tool_call_id": call.id,
                    "content": content,
                }));
            }
        }
        Err(too_many_rounds())
    }

    /// One streamed request. Retries once without tools when the server
    /// turns them down.
    async fn round(
        &mut self,
        specs: &[ToolSpec],
        events: &async_channel::Sender<AgentEvent>,
    ) -> Result<Reply, AiError> {
        let with_tools = !self.tools_off && !specs.is_empty();
        let mut response = self.post(specs, with_tools).await?;
        if !response.status().is_success() {
            let (status, message) = error_text(response).await;
            if with_tools && (400..500).contains(&status) && mentions_tools(&message) {
                tracing::warn!(status, "server rejected tools; continuing without them");
                self.tools_off = true;
                response = self.post(specs, false).await?;
                if !response.status().is_success() {
                    let (status, message) = error_text(response).await;
                    return Err(AiError::Api(format!("HTTP {status}: {message}")));
                }
            } else {
                return Err(AiError::Api(format!("HTTP {status}: {message}")));
            }
        }
        read_stream(SseReader::new(response), events).await
    }

    async fn post(
        &self,
        specs: &[ToolSpec],
        with_tools: bool,
    ) -> Result<reqwest::Response, AiError> {
        let mut messages = Vec::with_capacity(self.history.len() + 1);
        let system = match (self.system_prompt.is_empty(), self.tools_off) {
            (true, false) => None,
            (true, true) => Some(NO_TOOLS_NOTE.to_string()),
            (false, false) => Some(self.system_prompt.clone()),
            (false, true) => Some(format!("{}\n\n{NO_TOOLS_NOTE}", self.system_prompt)),
        };
        if let Some(system) = system {
            messages.push(json!({"role": "system", "content": system}));
        }
        messages.extend(self.history.iter().cloned());
        let mut body = json!({
            "model": self.model,
            "stream": true,
            "messages": messages,
        });
        if with_tools {
            body["tools"] = specs.iter().map(tool_json).collect();
        }
        tracing::debug!(url = %self.base_url, model = %self.model, with_tools, "chat completion request");
        let mut request = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .json(&body);
        if let Some(key) = &self.api_key {
            request = request.bearer_auth(key);
        }
        request.send().await.map_err(network)
    }
}

fn tool_json(spec: &ToolSpec) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": spec.name,
            "description": spec.description,
            "parameters": spec.input_schema,
        }
    })
}

fn mentions_tools(message: &str) -> bool {
    let message = message.to_lowercase();
    message.contains("tool") || message.contains("function")
}

fn assistant_message(reply: &Reply) -> Value {
    let mut message = json!({"role": "assistant", "content": reply.content});
    if !reply.calls.is_empty() {
        message["tool_calls"] = reply
            .calls
            .iter()
            .map(|call| {
                json!({
                    "id": call.id,
                    "type": "function",
                    "function": {"name": call.name, "arguments": call.arguments},
                })
            })
            .collect();
    }
    message
}

async fn read_stream(
    mut sse: SseReader,
    events: &async_channel::Sender<AgentEvent>,
) -> Result<Reply, AiError> {
    let mut content = String::new();
    let mut calls: BTreeMap<u64, PendingCall> = BTreeMap::new();
    while let Some(event) = sse.next().await? {
        if event.data.trim() == "[DONE]" {
            break;
        }
        let chunk: Value = serde_json::from_str(&event.data)
            .map_err(|e| AiError::Api(format!("unreadable stream chunk: {e}")))?;
        if let Some(error) = chunk.get("error") {
            let message = error["message"].as_str().unwrap_or("unknown error");
            return Err(AiError::Api(message.to_string()));
        }
        let Some(choice) = chunk["choices"].get(0) else {
            continue;
        };
        let delta = &choice["delta"];
        if let Some(text) = delta["content"].as_str()
            && !text.is_empty()
        {
            content.push_str(text);
            emit(events, AgentEvent::Text(text.to_string())).await;
        }
        for fragment in delta["tool_calls"].as_array().into_iter().flatten() {
            let index = fragment["index"].as_u64().unwrap_or(0);
            let call = calls.entry(index).or_default();
            if let Some(id) = fragment["id"].as_str()
                && call.id.is_empty()
            {
                call.id = id.to_string();
            }
            if let Some(name) = fragment["function"]["name"].as_str()
                && call.name.is_empty()
            {
                call.name = name.to_string();
            }
            if let Some(arguments) = fragment["function"]["arguments"].as_str() {
                call.arguments.push_str(arguments);
            }
        }
    }
    Ok(Reply {
        content,
        calls: calls.into_values().collect(),
    })
}

pub(crate) async fn list_models(
    base_url: &str,
    api_key: Option<&str>,
) -> Result<ModelList, AiError> {
    let mut request = http_client().get(format!("{base_url}/models"));
    if let Some(key) = api_key.filter(|k| !k.is_empty()) {
        request = request.bearer_auth(key);
    }
    let response = request.send().await.map_err(network)?;
    if !response.status().is_success() {
        let (status, message) = error_text(response).await;
        return Err(AiError::Api(format!("HTTP {status}: {message}")));
    }
    let body: Value = response.json().await.map_err(network)?;
    Ok(ModelList::new(parse_models(&body)))
}

/// The models in an OpenAI-style `/models` answer, by id, sorted and with
/// repeats dropped. The id carries the version and any quantization suffix,
/// so it is kept whole.
pub(crate) fn parse_models(body: &Value) -> Vec<Model> {
    let mut ids = model_ids(body);
    ids.sort_by_key(|id| id.to_lowercase());
    ids.dedup();
    ids.into_iter().map(Model::new).collect()
}

/// The `data[].id` list both OpenAI and Anthropic return from `/models`.
pub(crate) fn model_ids(body: &Value) -> Vec<String> {
    body["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| m["id"].as_str().map(str::to_string))
        .collect()
}
