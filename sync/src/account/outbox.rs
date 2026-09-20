//! Sending, drafts, search, attachments, and identity: calls the UI makes on
//! demand rather than as part of the sync loop.

use mailrs_domain::{Filter, MessageMeta, Vacation};
use mailrs_gmail::{GmailError, SendAs, html_to_text};

use super::AccountSync;
use crate::{GmailApi, SavedDraft, SyncError};

/// One address an account may send mail as: its own, or an alias whose owner
/// has confirmed it. Gmail keeps a display name and a signature per address,
/// so all three travel together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendAsAddress {
    pub email: String,
    pub name: Option<String>,
    /// The signature Gmail holds for this address, as plain text.
    pub signature: String,
    /// The address Gmail sends from when the writer picks none.
    pub default: bool,
}

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
    /// draft was deleted elsewhere, creates a new one.
    pub async fn save_draft(
        &self,
        raw: Vec<u8>,
        thread_id: Option<String>,
        draft_id: Option<String>,
    ) -> Result<SavedDraft, SyncError> {
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

    /// Sends a draft as Gmail holds it, as a scheduled send does. Returns
    /// `None` when the draft is gone, sent or deleted elsewhere.
    pub async fn send_draft(&self, draft_id: &str) -> Result<Option<String>, SyncError> {
        match self.api.send_draft(draft_id).await {
            Ok(id) => Ok(Some(id)),
            Err(GmailError::NotFound) => Ok(None),
            Err(err) => Err(err.into()),
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

    /// Every address this account may send mail from, its own included,
    /// with the display name and signature Gmail keeps for each. Aliases
    /// still waiting on their owner to confirm them are left out, because
    /// Gmail would refuse to send from one.
    pub async fn send_as(&self) -> Result<Vec<SendAsAddress>, SyncError> {
        Ok(self
            .api
            .send_as()
            .await?
            .into_iter()
            .filter(SendAs::is_verified)
            .map(|identity| SendAsAddress {
                name: Some(identity.display_name).filter(|n| !n.trim().is_empty()),
                email: identity.send_as_email,
                signature: html_to_text(&identity.signature),
                default: identity.is_default,
            })
            .collect())
    }

    /// The signature set in Gmail for the default identity, as plain text.
    pub async fn gmail_signature(&self) -> Result<Option<String>, SyncError> {
        Ok(self.api.signature().await?)
    }

    /// The message as it arrived, for View Source.
    pub async fn raw_message(&self, id: &str) -> Result<Vec<u8>, SyncError> {
        Ok(self.api.raw_message(id).await?)
    }

    pub async fn filters(&self) -> Result<Vec<Filter>, SyncError> {
        Ok(self.api.filters().await?)
    }

    pub async fn create_filter(&self, filter: Filter) -> Result<Filter, SyncError> {
        Ok(self.api.create_filter(&filter).await?)
    }

    pub async fn delete_filter(&self, id: &str) -> Result<(), SyncError> {
        match self.api.delete_filter(id).await {
            Ok(()) | Err(GmailError::NotFound) => Ok(()),
            Err(err) => Err(err.into()),
        }
    }

    pub async fn vacation(&self) -> Result<Vacation, SyncError> {
        Ok(self.api.vacation().await?)
    }

    pub async fn set_vacation(&self, vacation: Vacation) -> Result<(), SyncError> {
        Ok(self.api.set_vacation(&vacation).await?)
    }
}
