//! Searching the account's mail on the server.

use super::{GraphApi, Microsoft};
use crate::BackendError;
use crate::services::{RemoteRef, SearchQuery};

impl<G: GraphApi> Microsoft<G> {
    pub(super) async fn find(&self, _query: &SearchQuery, _limit: usize) -> Result<Vec<RemoteRef>, BackendError> {
        Err(BackendError::Unsupported)
    }
}
