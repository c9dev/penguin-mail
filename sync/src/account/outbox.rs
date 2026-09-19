//! Sending, drafts, search, attachments, and identity: calls the UI makes on
//! demand rather than as part of the sync loop.

use mailrs_domain::MessageMeta;
use mailrs_gmail::GmailError;

use super::AccountSync;
use crate::{GmailApi, SyncError};

impl<G: GmailApi> AccountSync<G> {
    /// Sends raw RFC 822 bytes, then deletes `draft_id` if the message came
    /// from a draft. A draft that is already gone does not fail the send.
    /// Returns the sent message's id.
    pub async fn send(
        &self,
        raw: Vec<u8>,
        thread_id: Option<String>,
        draft_id: Option<String>,
    ) -> Result<String, SyncError> {
        let message_id = self.api.send(&raw, thread_id.as_deref()).await?;
        if let Some(draft_id) = draft_id
            && let Err(err) = self.api.delete_draft(&draft_id).await
            && !matches!(err, GmailError::NotFound)
        {
            tracing::warn!(account = self.account_id, error = %err, "sent, but could not delete the draft");
        }
        Ok(message_id)
    }

    /// Saves a draft in Gmail, replacing `draft_id` when given. If that
    /// draft was deleted elsewhere, creates a new one. Returns the draft id.
    pub async fn save_draft(
        &self,
        raw: Vec<u8>,
        thread_id: Option<String>,
        draft_id: Option<String>,
    ) -> Result<String, SyncError> {
        let thread_id = thread_id.as_deref();
        match self
            .api
            .save_draft(draft_id.as_deref(), &raw, thread_id)
            .await
        {
            Err(GmailError::NotFound) if draft_id.is_some() => {
                Ok(self.api.save_draft(None, &raw, thread_id).await?)
            }
            other => Ok(other?),
        }
    }

    pub async fn delete_draft(&self, draft_id: &str) -> Result<(), SyncError> {
        match self.api.delete_draft(draft_id).await {
            Ok(()) | Err(GmailError::NotFound) => Ok(()),
            Err(err) => Err(err.into()),
        }
    }

    /// The draft backed by `message_id`, for reopening a draft in the composer.
    pub async fn draft_id_for(&self, message_id: &str) -> Result<Option<String>, SyncError> {
        Ok(self.api.draft_for_message(message_id).await?)
    }

    /// Runs a Gmail search and returns up to `limit` messages, newest first.
    /// Results are not stored; opening one stores its thread.
    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<MessageMeta>, SyncError> {
        let page = self.api.list_messages(query, None).await?;
        let ids: Vec<String> = page
            .messages
            .into_iter()
            .take(limit)
            .map(|m| m.id)
            .collect();
        let mut metas = self.fetch_metadata(&ids).await?;
        metas.sort_by(|a, b| b.date.cmp(&a.date).then_with(|| a.id.cmp(&b.id)));
        Ok(metas)
    }

    pub async fn attachment(
        &self,
        message_id: &str,
        attachment_id: &str,
    ) -> Result<Vec<u8>, SyncError> {
        Ok(self.api.attachment(message_id, attachment_id).await?)
    }

    /// The name Gmail puts on this account's outgoing mail, if one is set.
    pub async fn display_name(&self) -> Result<Option<String>, SyncError> {
        Ok(self.api.display_name().await?)
    }
}
