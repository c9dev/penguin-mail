//! Sending and drafts. A message goes to Graph as the MIME the composer
//! wrote, base64 in `sendMail`, so OpenPGP and S/MIME reach the recipient
//! untouched, and Graph files the copy in Sent Items. A draft is a message
//! in Drafts; saving it again makes the new one before deleting the old,
//! so a failure never leaves the person without their draft.

use mailrs_domain::Role;
use mailrs_graph::{Fields, GraphError, Listing};

use super::{GraphApi, Microsoft, backend};
use crate::BackendError;
use crate::api::{DraftRef, SavedDraft};

/// The Message-ID of `raw` without its angle brackets, the only name the
/// sent message has until Graph lists it in Sent Items.
fn message_id_of(raw: &[u8]) -> Option<String> {
    mailrs_mime::parts(raw).and_then(|parts| parts.header("Message-ID").map(|id| id.trim().trim_matches(['<', '>']).to_string()))
}

impl<G: GraphApi> Microsoft<G> {
    pub(super) async fn send_raw(&self, raw: &[u8]) -> Result<String, BackendError> {
        let id = message_id_of(raw).ok_or_else(|| BackendError::Refused("the message has no Message-ID".into()))?;
        self.graph().send_mime(raw).await.map_err(backend)?;
        Ok(id)
    }

    pub(super) async fn save(&self, old: Option<&str>, raw: &[u8]) -> Result<SavedDraft, BackendError> {
        let made = self.graph().create_draft_mime(raw).await.map_err(backend)?;
        if let Some(old) = old {
            match self.graph().delete_message(old).await {
                Ok(()) | Err(GraphError::NotFound) => {}
                Err(err) => tracing::warn!(%err, "the draft before this one stays in Drafts"),
            }
        }
        Ok(SavedDraft {
            draft_id: made.id.clone(),
            message_id: made.id.clone(),
            thread_id: made.conversation_id.unwrap_or(made.id),
        })
    }

    pub(super) async fn send_saved(&self, draft: &str) -> Result<String, BackendError> {
        let held = self.graph().messages(&[draft.to_string()]).await.map_err(backend)?;
        let message_id = held
            .into_iter()
            .next()
            .ok_or(BackendError::NotFound)?
            .map_err(backend)?
            .internet_message_id
            .map(|m| m.trim_matches(['<', '>']).to_string())
            .unwrap_or_else(|| draft.to_string());
        self.graph().send_draft(draft).await.map_err(backend)?;
        Ok(message_id)
    }

    pub(super) async fn drop_draft(&self, draft: &str) -> Result<(), BackendError> {
        match self.graph().delete_message(draft).await {
            Ok(()) | Err(GraphError::NotFound) => Ok(()),
            Err(err) => Err(backend(err)),
        }
    }

    /// The newest hundred drafts; nobody keeps more open, and the page
    /// bound holds memory.
    pub(super) async fn drafts(&self) -> Result<Vec<DraftRef>, BackendError> {
        self.synced().await?;
        let Some(drafts) = self.known().roles.get(&Role::Drafts).cloned() else {
            return Ok(Vec::new());
        };
        let listing = Listing { folder: Some(drafts), top: 100, fields: Fields::Ids, ..Listing::default() };
        let page = self.graph().list_messages(&listing, None).await.map_err(backend)?;
        Ok(page.value.into_iter().map(|m| DraftRef { draft_id: m.id.clone(), message_id: m.id }).collect())
    }
}
