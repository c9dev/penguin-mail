//! Provider state and the agent loop.

use std::sync::Arc;

use crate::{AgentEvent, AiError, ProviderConfig, ToolHost};

pub(crate) struct State {
    #[allow(dead_code)]
    config: ProviderConfig,
    #[allow(dead_code)]
    system_prompt: String,
}

impl State {
    pub(crate) fn new(config: ProviderConfig, system_prompt: String) -> State {
        State {
            config,
            system_prompt,
        }
    }

    pub(crate) async fn send(
        &mut self,
        _text: String,
        _host: Arc<dyn ToolHost>,
        _events: async_channel::Sender<AgentEvent>,
    ) -> Result<String, AiError> {
        todo!("implemented by the ai crate work")
    }
}

pub(crate) async fn list_models(_config: &ProviderConfig) -> Result<Vec<String>, AiError> {
    todo!("implemented by the ai crate work")
}

pub(crate) async fn test(_config: &ProviderConfig) -> Result<String, AiError> {
    todo!("implemented by the ai crate work")
}
