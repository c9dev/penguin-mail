//! The inbox rules Outlook runs on the server.

use mailrs_domain::Filter;

use super::{GraphApi, Microsoft};
use crate::BackendError;
use crate::services::RulesService;

impl<G: GraphApi> RulesService for Microsoft<G> {
    async fn filters(&self) -> Result<Vec<Filter>, BackendError> {
        Err(BackendError::Unsupported)
    }

    async fn create_filter(&self, _filter: &Filter) -> Result<Filter, BackendError> {
        Err(BackendError::Unsupported)
    }

    async fn delete_filter(&self, _id: &str) -> Result<(), BackendError> {
        Err(BackendError::Unsupported)
    }
}
