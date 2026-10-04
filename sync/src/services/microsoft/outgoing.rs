//! Sending mail, and saving, sending and listing drafts.

use super::{GraphApi, Microsoft};
use crate::BackendError;
use crate::api::{DraftRef, SavedDraft};

impl<G: GraphApi> Microsoft<G> {
    pub(super) async fn send_raw(&self, _raw: &[u8]) -> Result<String, BackendError> {
        Err(BackendError::Unsupported)
    }

    pub(super) async fn save(&self, _old: Option<&str>, _raw: &[u8]) -> Result<SavedDraft, BackendError> {
        Err(BackendError::Unsupported)
    }

    pub(super) async fn send_saved(&self, _draft: &str) -> Result<String, BackendError> {
        Err(BackendError::Unsupported)
    }

    pub(super) async fn drop_draft(&self, _draft: &str) -> Result<(), BackendError> {
        Err(BackendError::Unsupported)
    }

    pub(super) async fn drafts(&self) -> Result<Vec<DraftRef>, BackendError> {
        Err(BackendError::Unsupported)
    }
}
