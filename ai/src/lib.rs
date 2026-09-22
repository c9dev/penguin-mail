//! The AI assistant's engine: providers, the agent loop, and the bridge that
//! lets Claude Code call the app's tools. The app supplies tools through
//! [`ToolHost`]; this crate never touches GTK or mail storage.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

pub mod bridge;
mod detect;
mod history;
pub mod mcp;
mod providers;
mod sse;
#[cfg(test)]
mod tests;

pub use detect::{Detected, detect};

/// A boxed future, so [`ToolHost`] stays object-safe.
pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// One tool the model may call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema for the input object.
    pub input_schema: serde_json::Value,
}

/// What a tool call produced.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolOutcome {
    Ok(serde_json::Value),
    /// Shown to the model as a failed tool result.
    Err(String),
}

/// The app's side of the conversation: the tools and how to run them. Calls
/// may wait on the user, for example to approve sending mail.
pub trait ToolHost: Send + Sync + 'static {
    fn specs(&self) -> Vec<ToolSpec>;
    fn call(&self, name: String, input: serde_json::Value) -> BoxFuture<ToolOutcome>;
    /// Gets the tools ready before the model first sees them, such as
    /// starting the servers some of them come from. [`ToolHost::specs`]
    /// cannot wait, so a turn waits here first.
    fn prepare(&self) -> BoxFuture<()> {
        Box::pin(async {})
    }
}

/// A host with nothing to call, for the features that are words in and
/// words out. A translation and an unsubscribe page both want an answer
/// and nothing else, and neither should be able to reach the mailbox.
pub struct NoTools;

impl ToolHost for NoTools {
    fn specs(&self) -> Vec<ToolSpec> {
        Vec::new()
    }

    fn call(&self, name: String, _input: serde_json::Value) -> BoxFuture<ToolOutcome> {
        Box::pin(async move { ToolOutcome::Err(format!("there is no tool called {name}")) })
    }
}

/// Where the model runs. Chosen in Preferences.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ProviderConfig {
    /// Any server speaking OpenAI's chat completions API: LM Studio, Ollama,
    /// llama.cpp, vLLM, Unsloth, OpenAI itself.
    OpenAiCompatible {
        base_url: String,
        api_key: Option<String>,
        model: String,
    },
    /// Anthropic's Messages API with the user's own key.
    Anthropic { api_key: String, model: String },
    /// The user's Claude subscription, through the Claude Code CLI.
    ClaudeCode {
        command: PathBuf,
        model: Option<String>,
    },
}

/// Progress of one turn, streamed to the UI.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentEvent {
    /// More of the assistant's reply text.
    Text(String),
    /// More of what the model thought before answering. Providers that
    /// hide their reasoning send none.
    Thinking(String),
    ToolStarted {
        /// Matches the call to its [`AgentEvent::ToolFinished`], since two
        /// calls to the same tool can run in one turn.
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolFinished {
        id: String,
        name: String,
        ok: bool,
        /// A short, human-readable result.
        preview: String,
        /// The whole result, as the model reads it.
        output: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum AiError {
    #[error("could not reach the model: {0}")]
    Network(String),
    #[error("the model service said: {0}")]
    Api(String),
    #[error("{0}")]
    Other(String),
}

/// One chat with the assistant. Keeps whatever history its provider needs.
pub struct Conversation {
    inner: providers::State,
}

impl Conversation {
    pub fn new(config: ProviderConfig, system_prompt: String) -> Conversation {
        Conversation {
            inner: providers::State::new(config, system_prompt),
        }
    }

    /// Asks the model to think before it answers, where the provider lets
    /// a request ask. The assistant wants this; a translation does not,
    /// because thinking makes a short job slow and costs tokens.
    pub fn with_thinking(mut self) -> Conversation {
        self.inner.ask_for_thinking();
        self
    }

    /// Lets Claude search the web and read pages with Anthropic's own tools,
    /// through the API or Claude Code, from the next message on. A local
    /// model ignores this: its web tools come from the tool host.
    pub fn set_web(&mut self, on: bool) {
        self.inner.set_web(on);
    }

    /// Sends a user message and runs tools until the model answers. Streams
    /// progress to `events`; returns the final reply text.
    pub async fn send(
        &mut self,
        text: String,
        host: Arc<dyn ToolHost>,
        events: async_channel::Sender<AgentEvent>,
    ) -> Result<String, AiError> {
        host.prepare().await;
        self.inner.send(text, host, events).await
    }
}

/// One model a provider offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Model {
    /// What goes into the settings and into every request.
    pub id: String,
    /// The provider's name for it, such as "Claude Opus 5". Empty when the
    /// provider gives an id and nothing else.
    pub name: String,
    /// True when the id is an alias that follows the newest version, rather
    /// than one pinned version.
    pub alias: bool,
}

impl Model {
    /// A model known only by its id.
    pub fn new(id: impl Into<String>) -> Model {
        Model {
            id: id.into(),
            name: String::new(),
            alias: false,
        }
    }

    /// The id with its name, for a log line or a test.
    pub fn named(id: impl Into<String>, name: impl Into<String>) -> Model {
        Model {
            id: id.into(),
            name: name.into(),
            alias: false,
        }
    }
}

/// What a provider offers, for the settings picker.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelList {
    pub models: Vec<Model>,
    /// One plain sentence about what the list is missing, when it misses
    /// something the user should know about.
    pub note: Option<String>,
}

impl ModelList {
    pub fn new(models: Vec<Model>) -> ModelList {
        ModelList { models, note: None }
    }

    pub fn with_note(models: Vec<Model>, note: impl Into<String>) -> ModelList {
        ModelList {
            models,
            note: Some(note.into()),
        }
    }

    /// The ids alone, in order.
    pub fn ids(&self) -> Vec<String> {
        self.models.iter().map(|m| m.id.clone()).collect()
    }
}

/// Models the provider offers, for the settings picker.
pub async fn list_models(config: &ProviderConfig) -> Result<ModelList, AiError> {
    providers::list_models(config).await
}

/// A one-line check that the provider answers.
pub async fn test(config: &ProviderConfig) -> Result<String, AiError> {
    providers::test(config).await
}
