//! Sending, drafts, search, attachments, and identity: calls the UI makes on
//! demand rather than as part of the sync loop.

use mailrs_domain::{EpochMillis, Filter, MessageMeta, Vacation};
use mailrs_gmail::{GmailError, MessageRef, SendAs, html_to_text};
use mailrs_store::drafts;

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
        if let Some(draft_id) = draft_id {
            if let Err(err) = self.api.delete_draft(&draft_id).await
                && !matches!(err, GmailError::NotFound)
            {
                tracing::warn!(account = self.account_id, error = %err, "sent, but could not delete the draft");
            }
            self.forget_draft(&draft_id).await;
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
        let saved = match self
            .api
            .save_draft(draft_id.as_deref(), &raw, thread_id)
            .await
        {
            Err(GmailError::NotFound) if draft_id.is_some() => {
                self.api.save_draft(None, &raw, thread_id).await?
            }
            other => other?,
        };
        // Gmail's answer names both the draft and the message it now
        // holds, which is the pair `drafts.list` would otherwise be asked
        // for. Keeping it here is what leaves every draft this app writes
        // out of that listing.
        self.remember_draft(&saved.draft_id, &saved.message_id)
            .await;
        Ok(saved)
    }

    /// Sends a draft as Gmail holds it, as a scheduled send does. Returns
    /// `None` when the draft is gone, sent or deleted elsewhere.
    pub async fn send_draft(&self, draft_id: &str) -> Result<Option<String>, SyncError> {
        let sent = match self.api.send_draft(draft_id).await {
            Ok(id) => Some(id),
            Err(GmailError::NotFound) => None,
            Err(err) => return Err(err.into()),
        };
        self.forget_draft(draft_id).await;
        Ok(sent)
    }

    pub async fn delete_draft(&self, draft_id: &str) -> Result<(), SyncError> {
        match self.api.delete_draft(draft_id).await {
            Ok(()) | Err(GmailError::NotFound) => {}
            Err(err) => return Err(err.into()),
        }
        self.forget_draft(draft_id).await;
        Ok(())
    }

    /// The draft backed by `message_id`, for reopening a draft in the
    /// composer. The store answers for a draft it already knows, which
    /// costs nothing. Otherwise `drafts.list` walks the whole account, and
    /// every pair it hands back is stored, so the account pays that once
    /// rather than once per draft opened.
    pub async fn draft_id_for(&self, message_id: &str) -> Result<Option<String>, SyncError> {
        let account_id = self.account_id;
        let wanted = message_id.to_string();
        if let Some(draft_id) = self
            .db
            .read(move |c| drafts::draft_of(c, account_id, &wanted))
            .await?
        {
            return Ok(Some(draft_id));
        }
        let listed = self.api.list_drafts().await?;
        let found = listed
            .iter()
            .find(|d| d.message_id == message_id)
            .map(|d| d.draft_id.clone());
        let pairs: Vec<(String, String)> = listed
            .into_iter()
            .map(|d| (d.draft_id, d.message_id))
            .collect();
        self.pair_written(
            self.db
                .write(move |c| drafts::replace_all(c, account_id, &pairs))
                .await,
        );
        Ok(found)
    }

    /// Records which message a draft holds now.
    async fn remember_draft(&self, draft_id: &str, message_id: &str) {
        let account_id = self.account_id;
        let (draft_id, message_id) = (draft_id.to_string(), message_id.to_string());
        self.pair_written(
            self.db
                .write(move |c| drafts::remember(c, account_id, &draft_id, &message_id))
                .await,
        );
    }

    /// Drops the stored pair for a draft that has left Gmail.
    async fn forget_draft(&self, draft_id: &str) {
        let account_id = self.account_id;
        let draft_id = draft_id.to_string();
        self.pair_written(
            self.db
                .write(move |c| drafts::forget(c, account_id, &draft_id))
                .await,
        );
    }

    /// A stored pair saves a lookup and does nothing else, and the Gmail
    /// call that produced it has already gone through, so a store that
    /// refuses the write gets a line in the log. Failing the call over it
    /// would have the caller save or send the same message a second time.
    fn pair_written(&self, outcome: mailrs_store::Result<()>) {
        if let Err(err) = outcome {
            tracing::warn!(account = self.account_id, error = %err, "could not store which draft holds which message");
        }
    }

    /// Runs a Gmail search and returns up to `limit` messages, newest first.
    /// Results are not stored; opening one stores its thread.
    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<MessageMeta>, SyncError> {
        let found = self.search_ids(query, limit).await?;
        self.metadata_of(&found).await
    }

    /// The messages a Gmail search returns, by id and thread, newest
    /// first, at most `limit` of them. One call of 5 quota units, whatever the count, so a caller
    /// takes the ids first and pays for metadata only as it shows rows.
    pub async fn search_ids(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<MessageRef>, SyncError> {
        let page = self.api.list_messages(query, None).await?;
        Ok(page.messages.into_iter().take(limit).collect())
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

    /// The message as it arrived, for View Source and for saving one
    /// message as an `.eml` file.
    pub async fn raw_message(&self, id: &str) -> Result<Vec<u8>, SyncError> {
        Ok(self.api.raw_message(id).await?)
    }

    /// A conversation as an mbox file, oldest message first, or the one
    /// message `message_id` names when the list shows messages rather than
    /// conversations. Each message costs a `messages.get`, so a long
    /// conversation is a handful of calls; the caller runs this off the
    /// user's thread.
    pub async fn export_mbox(
        &self,
        thread_id: &str,
        message_id: Option<&str>,
    ) -> Result<Vec<u8>, SyncError> {
        let ids: Vec<String> = match message_id {
            Some(id) => vec![id.to_string()],
            None => self
                .api
                .thread_metadata(thread_id)
                .await?
                .into_iter()
                .map(|meta| meta.id)
                .collect(),
        };
        let mut mbox = Vec::new();
        for id in ids {
            crate::export::append(&mut mbox, &self.api.raw_message(&id).await?);
        }
        Ok(mbox)
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

    /// Posts RFC 8058's one-click request to a mailing list's `url`.
    pub async fn one_click_unsubscribe(&self, url: &str) -> Result<(), SyncError> {
        Ok(self.api.one_click_unsubscribe(url).await?)
    }

    pub async fn vacation(&self) -> Result<Vacation, SyncError> {
        Ok(self.api.vacation().await?)
    }

    pub async fn set_vacation(&self, vacation: Vacation) -> Result<(), SyncError> {
        Ok(self.api.set_vacation(&vacation).await?)
    }

    /// One page of the account's Google contacts. See `ContactBook`.
    pub async fn connections(
        &self,
        page_token: Option<&str>,
        sync_token: Option<&str>,
    ) -> Result<mailrs_gmail::ConnectionsPage, SyncError> {
        Ok(self.api.connections(page_token, sync_token).await?)
    }

    /// The bytes of one contact photo.
    pub async fn contact_photo(&self, url: &str) -> Result<Vec<u8>, SyncError> {
        Ok(self.api.contact_photo(url).await?)
    }

    /// Adds a contact to the account's Google contacts.
    pub async fn create_contact(
        &self,
        fields: &mailrs_gmail::ContactFields,
    ) -> Result<mailrs_gmail::Person, SyncError> {
        Ok(self.api.create_contact(fields).await?)
    }

    /// Changes the fields `fields` names on the contact `resource`.
    pub async fn update_contact(
        &self,
        resource: &str,
        fields: &mailrs_gmail::ContactFields,
    ) -> Result<mailrs_gmail::Person, SyncError> {
        Ok(self.api.update_contact(resource, fields).await?)
    }

    /// Answers an invitation through Google Calendar as `me`.
    /// `occurrence` names one occurrence of a repeating event; `None`
    /// answers the series.
    pub async fn answer_invitation(
        &self,
        ical_uid: &str,
        me: &str,
        answer: mailrs_domain::invitation::Answer,
        occurrence: Option<EpochMillis>,
    ) -> Result<mailrs_gmail::Answered, SyncError> {
        Ok(self
            .api
            .answer_invitation(ical_uid, me, answer, occurrence)
            .await?)
    }

    /// What the account's calendar already holds between `from` and `to`.
    pub async fn busy_between(
        &self,
        from: EpochMillis,
        to: EpochMillis,
    ) -> Result<Vec<mailrs_gmail::Busy>, SyncError> {
        Ok(self.api.busy_between(from, to).await?)
    }

    /// How the repeating event `ical_uid` names repeats, as the calendar
    /// holds it, with what is left of it from `from`.
    pub async fn series(
        &self,
        ical_uid: &str,
        from: EpochMillis,
    ) -> Result<Option<mailrs_gmail::Series>, SyncError> {
        Ok(self.api.series(ical_uid, from).await?)
    }

    /// Every event on the account's primary calendar between `from` and
    /// `to`.
    pub async fn events_between(
        &self,
        from: EpochMillis,
        to: EpochMillis,
    ) -> Result<Vec<mailrs_gmail::Event>, SyncError> {
        Ok(self.api.events_between(from, to).await?)
    }

    pub async fn create_event(
        &self,
        fields: &mailrs_gmail::EventFields,
    ) -> Result<mailrs_gmail::Event, SyncError> {
        Ok(self.api.create_event(fields).await?)
    }

    pub async fn update_event(
        &self,
        id: &str,
        fields: &mailrs_gmail::EventFields,
    ) -> Result<mailrs_gmail::Event, SyncError> {
        Ok(self.api.update_event(id, fields).await?)
    }

    pub async fn delete_event(&self, id: &str) -> Result<(), SyncError> {
        Ok(self.api.delete_event(id).await?)
    }
}
