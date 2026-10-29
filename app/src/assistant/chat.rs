//! A chat owns its model, source lifetimes, and each question's setup.
//! The pane supplies choices and draws events; tool calls still reach the
//! window through the mail host and the approval channel.

use std::path::PathBuf;
use std::sync::Arc;

use mailrs_ai::{AgentEvent, BoxFuture, Conversation};

use super::sources::{self, ApprovalRequest, Source, Toolbox};
use super::{Host, ToolRequest};
use crate::settings::{Feature, Settings, WebSearch};

#[derive(Clone, Copy)]
pub struct Tools {
    pub mcp: bool,
    pub skills: bool,
}

/// One question's events, cancellation handle, and work for the runtime.
/// Choices belong to this question even if the pane changes them while
/// the model answers.
pub struct Question {
    pub events: async_channel::Receiver<AgentEvent>,
    pub stop: async_channel::Sender<()>,
    pub answer: BoxFuture<anyhow::Result<String>>,
}

pub struct Chat {
    conversation: Option<Arc<tokio::sync::Mutex<Conversation>>>,
    sources: Vec<Arc<dyn Source>>,
    requests: async_channel::Sender<ToolRequest>,
    approvals: async_channel::Sender<ApprovalRequest>,
}

impl Chat {
    pub fn new(
        requests: async_channel::Sender<ToolRequest>,
        approvals: async_channel::Sender<ApprovalRequest>,
    ) -> Self {
        Self {
            conversation: None,
            sources: Vec::new(),
            requests,
            approvals,
        }
    }

    pub fn clear(&mut self) {
        self.conversation = None;
        self.sources.clear();
    }

    pub fn ask(
        &mut self,
        settings: Settings,
        tools: Tools,
        text: String,
        picked: Arc<[PathBuf]>,
    ) -> Result<Question, String> {
        let config = super::model_for(&settings.ai, Feature::Assistant)?;
        let conversation = match &self.conversation {
            Some(conversation) => {
                sources::refresh_web(&settings, &mut self.sources);
                Arc::clone(conversation)
            }
            None => {
                self.sources = sources::for_settings(&settings);
                let conversation = Arc::new(tokio::sync::Mutex::new(Conversation::new(
                    config.clone(),
                    super::SYSTEM_PROMPT.to_string(),
                )));
                self.conversation = Some(Arc::clone(&conversation));
                conversation
            }
        };
        let language = super::language::selected(&settings.ai.language);
        let base = format!(
            "{}\n\n{}",
            super::SYSTEM_PROMPT,
            super::language::instruction(&language)
        );
        let (prompt, sources) = sources::for_turn(&base, &self.sources, tools.mcp, tools.skills);
        let web = settings.ai.web_search != WebSearch::Off;
        let reasoning = settings.ai.assistant_reasoning;
        let rounds = settings.ai.max_rounds;
        let host = Arc::new(Toolbox::new(
            Host::new(super::run::specs(), self.requests.clone()).with_picked_files(picked),
            sources,
            self.approvals.clone(),
            settings.assistant_allowed_tools,
        ));
        let (events, received) = async_channel::unbounded();
        let (stop, stopped) = async_channel::bounded::<()>(1);
        let answer = Box::pin(async move {
            let mut conversation = conversation.lock().await;
            conversation.set_provider(config);
            conversation.set_thinking(reasoning);
            conversation.set_rounds(rounds);
            conversation.set_system_prompt(prompt);
            conversation.set_web(web);
            tokio::select! {
                reply = conversation.send(text, host, events) => reply.map_err(anyhow::Error::from),
                _ = stopped.recv() => Err(anyhow::anyhow!("Stopped.")),
            }
        });
        Ok(Question {
            events: received,
            stop,
            answer,
        })
    }
}

#[cfg(test)]
mod tests;
