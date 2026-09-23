//! Sending, drafts, search, attachments, and identity: calls the UI makes on
//! demand rather than as part of the sync loop.

use std::collections::BTreeSet;

use mailrs_domain::{EpochMillis, Filter, Vacation};
use mailrs_store::{drafts, messages};

use super::AccountSync;
use crate::{
    AnyAutoReply, AnyCalendar, AnyContacts, AutoReplyService, BackendError, CalendarService,
    ContactsService, IdentityService, MailBackend, RulesService, SavedDraft, SendAsAddress,
    SyncError,
};

impl AccountSync {
    /// Sends raw RFC 822 bytes, then deletes `draft_id` if the message came
    /// from a draft. A draft that is already gone does not fail the send.
    /// Returns the sent message's id.
    pub async fn send(
        &self,
        raw: Vec<u8>,
        thread_id: Option<String>,
        draft_id: Option<String>,
    ) -> Result<String, SyncError> {
        let message_id = self.services.mail.send(&raw, thread_id.as_deref()).await?;
        if let Some(draft_id) = draft_id {
            if let Err(err) = self.services.mail.delete_draft(&draft_id).await
                && !matches!(err, BackendError::NotFound)
            {
                tracing::warn!(account = self.account_id, error = %err, "sent, but could not delete the draft");
            }
            self.forget_draft(&draft_id).await;
        }
        Ok(message_id)
    }

    /// The id of the sent message carrying the same `Message-ID` header as
    /// `raw`, when Gmail holds one. A send whose answer never came back
    /// may still have gone out, and this is how a retry finds out before
    /// sending the message a second time. One search, 5 quota units. A
    /// draft carries the same header as the message it becomes, so the
    /// search asks for sent mail alone. Bytes without the header answer
    /// `None`.
    pub async fn sent_copy(&self, raw: &[u8]) -> Result<Option<String>, SyncError> {
        let Some(id) = message_id_header(raw) else {
            return Ok(None);
        };
        let query = format!("in:sent rfc822msgid:{id}");
        let page = self.services.mail.list_messages(&query, None, 1).await?;
        Ok(page.messages.into_iter().next().map(|m| m.id))
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
            .services
            .mail
            .save_draft(draft_id.as_deref(), &raw, thread_id)
            .await
        {
            Err(BackendError::NotFound) if draft_id.is_some() => {
                self.services.mail.save_draft(None, &raw, thread_id).await?
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
        let sent = match self.services.mail.send_draft(draft_id).await {
            Ok(id) => Some(id),
            Err(BackendError::NotFound) => None,
            Err(err) => return Err(err.into()),
        };
        self.forget_draft(draft_id).await;
        Ok(sent)
    }

    pub async fn delete_draft(&self, draft_id: &str) -> Result<(), SyncError> {
        match self.services.mail.delete_draft(draft_id).await {
            Ok(()) | Err(BackendError::NotFound) => {}
            Err(err) => return Err(err.into()),
        }
        self.forget_draft(draft_id).await;
        Ok(())
    }

    /// Deletes the draft whose message is `message_id`, from Gmail and then
    /// from the store, so the Drafts mailbox drops it now rather than at
    /// the next history pass. Gmail deletes a draft for good, with no copy
    /// in the Trash. Returns false when Gmail holds no such draft.
    pub async fn discard_draft(&self, message_id: &str) -> Result<bool, SyncError> {
        let Some(draft_id) = self.draft_id_for(message_id).await? else {
            return Ok(false);
        };
        self.delete_draft(&draft_id).await?;
        let (account_id, id) = (self.account_id, message_id.to_string());
        let thread = self
            .db
            .write(move |c| {
                let thread = messages::delete_message(c, account_id, &id)?;
                if let Some(thread) = &thread {
                    messages::refresh_thread(c, account_id, thread)?;
                }
                Ok(thread)
            })
            .await?;
        self.emit_threads(thread.into_iter().collect::<BTreeSet<_>>());
        Ok(true)
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
        let listed = self.services.mail.list_drafts().await?;
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

    pub async fn attachment(
        &self,
        message_id: &str,
        attachment_id: &str,
    ) -> Result<Vec<u8>, SyncError> {
        Ok(self.services.mail.attachment(message_id, attachment_id).await?)
    }

    /// The name the account's server puts on its outgoing mail, if one
    /// is set.
    pub async fn display_name(&self) -> Result<Option<String>, SyncError> {
        let identities = self.services.identities.identities().await?;
        Ok(identities
            .into_iter()
            .find(|address| address.default)
            .and_then(|address| address.name))
    }

    /// Every address this account may send mail from, its own included,
    /// with the display name and signature the server keeps for each.
    pub async fn send_as(&self) -> Result<Vec<SendAsAddress>, SyncError> {
        Ok(self.services.identities.identities().await?)
    }

    /// The signature of the default address, as plain text.
    pub async fn gmail_signature(&self) -> Result<Option<String>, SyncError> {
        let identities = self.services.identities.identities().await?;
        Ok(identities
            .into_iter()
            .find(|address| address.default)
            .map(|address| address.signature)
            .filter(|text| !text.is_empty()))
    }

    /// The message as it arrived, for View Source and for saving one
    /// message as an `.eml` file.
    pub async fn raw_message(&self, id: &str) -> Result<Vec<u8>, SyncError> {
        Ok(self.services.mail.raw_message(id).await?)
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
                .services
                .mail
                .thread_metadata(thread_id)
                .await?
                .into_iter()
                .map(|meta| meta.id)
                .collect(),
        };
        let mut mbox = Vec::new();
        for id in ids {
            crate::export::append(&mut mbox, &self.services.mail.raw_message(&id).await?);
        }
        Ok(mbox)
    }

    pub async fn filters(&self) -> Result<Vec<Filter>, SyncError> {
        Ok(self.services.rules.filters().await?)
    }

    pub async fn create_filter(&self, filter: Filter) -> Result<Filter, SyncError> {
        Ok(self.services.rules.create_filter(&filter).await?)
    }

    pub async fn delete_filter(&self, id: &str) -> Result<(), SyncError> {
        match self.services.rules.delete_filter(id).await {
            Ok(()) | Err(BackendError::NotFound) => Ok(()),
            Err(err) => Err(err.into()),
        }
    }

    pub async fn vacation(&self) -> Result<Vacation, SyncError> {
        Ok(self.auto_reply()?.vacation().await?)
    }

    pub async fn set_vacation(&self, vacation: Vacation) -> Result<(), SyncError> {
        Ok(self.auto_reply()?.set_vacation(&vacation).await?)
    }

    /// One page of the account's contacts. See `ContactBook`.
    pub async fn connections(
        &self,
        page_token: Option<&str>,
        sync_token: Option<&str>,
    ) -> Result<mailrs_gmail::ConnectionsPage, SyncError> {
        Ok(self.contacts()?.connections(page_token, sync_token).await?)
    }

    /// The bytes of one contact photo.
    pub async fn contact_photo(&self, url: &str) -> Result<Vec<u8>, SyncError> {
        Ok(self.contacts()?.contact_photo(url).await?)
    }

    /// Adds a contact to the account's contacts.
    pub async fn create_contact(
        &self,
        fields: &mailrs_gmail::ContactFields,
    ) -> Result<mailrs_gmail::Person, SyncError> {
        Ok(self.contacts()?.create_contact(fields).await?)
    }

    /// Changes the fields `fields` names on the contact `resource`.
    pub async fn update_contact(
        &self,
        resource: &str,
        fields: &mailrs_gmail::ContactFields,
    ) -> Result<mailrs_gmail::Person, SyncError> {
        Ok(self.contacts()?.update_contact(resource, fields).await?)
    }

    /// Answers an invitation through the account's calendar as `me`.
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
            .calendar()?
            .answer_invitation(ical_uid, me, answer, occurrence)
            .await?)
    }

    /// What the account's calendar already holds between `from` and `to`.
    pub async fn busy_between(
        &self,
        from: EpochMillis,
        to: EpochMillis,
    ) -> Result<Vec<mailrs_gmail::Busy>, SyncError> {
        Ok(self.calendar()?.busy_between(from, to).await?)
    }

    /// How the repeating event `ical_uid` names repeats, as the calendar
    /// holds it, with what is left of it from `from`.
    pub async fn series(
        &self,
        ical_uid: &str,
        from: EpochMillis,
    ) -> Result<Option<mailrs_gmail::Series>, SyncError> {
        Ok(self.calendar()?.series(ical_uid, from).await?)
    }

    /// Every event on the account's primary calendar between `from` and
    /// `to`.
    pub async fn events_between(
        &self,
        from: EpochMillis,
        to: EpochMillis,
    ) -> Result<Vec<mailrs_gmail::Event>, SyncError> {
        Ok(self.calendar()?.events_between(from, to).await?)
    }

    pub async fn create_event(
        &self,
        fields: &mailrs_gmail::EventFields,
    ) -> Result<mailrs_gmail::Event, SyncError> {
        Ok(self.calendar()?.create_event(fields).await?)
    }

    pub async fn update_event(
        &self,
        id: &str,
        fields: &mailrs_gmail::EventFields,
    ) -> Result<mailrs_gmail::Event, SyncError> {
        Ok(self.calendar()?.update_event(id, fields).await?)
    }

    pub async fn delete_event(&self, id: &str) -> Result<(), SyncError> {
        Ok(self.calendar()?.delete_event(id).await?)
    }

    fn calendar(&self) -> Result<&AnyCalendar, BackendError> {
        self.services
            .calendar
            .as_ref()
            .ok_or(BackendError::Unsupported)
    }

    fn contacts(&self) -> Result<&AnyContacts, BackendError> {
        self.services
            .contacts
            .as_ref()
            .ok_or(BackendError::Unsupported)
    }

    fn auto_reply(&self) -> Result<&AnyAutoReply, BackendError> {
        self.services
            .auto_reply
            .as_ref()
            .ok_or(BackendError::Unsupported)
    }
}

/// The `Message-ID` header of RFC 822 bytes, without its angle brackets.
/// Only the header block is read, and a header folded onto the next line
/// is unfolded first.
fn message_id_header(raw: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(raw);
    let head = text
        .split("\r\n\r\n")
        .next()
        .and_then(|h| h.split("\n\n").next())
        .unwrap_or_default();
    let mut unfolded: Vec<String> = Vec::new();
    for line in head.lines() {
        match (line.starts_with([' ', '\t']), unfolded.last_mut()) {
            (true, Some(previous)) => previous.push_str(line),
            _ => unfolded.push(line.to_string()),
        }
    }
    unfolded.iter().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        let id = value.trim().trim_matches(['<', '>']).trim();
        (name.trim().eq_ignore_ascii_case("message-id") && !id.is_empty()).then(|| id.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::message_id_header;

    #[test]
    fn the_message_id_comes_from_the_header_block_alone() {
        let raw = b"Subject: Hi\r\nMessage-Id:\r\n <a1@example.com>\r\n\r\nMessage-ID: <body@x>";
        assert_eq!(message_id_header(raw).as_deref(), Some("a1@example.com"));
        assert_eq!(
            message_id_header(b"Subject: Hi\r\n\r\nMessage-ID: <x@y>"),
            None
        );
    }
}
