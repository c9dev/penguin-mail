//! Sending, drafts, search, attachments and exports: mail calls the UI makes
//! on demand rather than as part of the sync loop.

use mailrs_store::messages::Change;
use mailrs_store::{drafts, messages};

use super::AccountSync;
use crate::{BackendError, MailBackend, SavedDraft, SyncError};

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
        Ok(self.services.mail.find_sent(&id).await?)
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
        let threads = self
            .db
            .write(move |c| {
                let delete = Change::Delete { message_id: id };
                Ok(messages::apply(c, account_id, &[delete])?.threads)
            })
            .await?;
        self.emit_threads(threads);
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

    /// One file of a message, by its part path: from the raw message when
    /// the cache holds it or the message is small, and otherwise that part
    /// alone.
    pub async fn attachment(&self, message_id: &str, part_path: &str) -> Result<Vec<u8>, SyncError> {
        let raw = match self.cached_raw(message_id) {
            Some(raw) => Some(raw),
            None if self.small(message_id).await? => Some(self.raw(message_id).await?),
            None => None,
        };
        match raw {
            Some(raw) => Ok(mailrs_mime::part(&raw, part_path).ok_or(BackendError::NotFound)?),
            None => Ok(self.services.mail.fetch_part(message_id, part_path).await?),
        }
    }

    /// The message as it arrived, for View Source, a signature check and
    /// saving one message as an `.eml` file. A large message comes whole
    /// here too, since each of these needs every byte.
    pub async fn raw_message(&self, id: &str) -> Result<Vec<u8>, SyncError> {
        Ok(self.raw(id).await?.as_ref().clone())
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
            None => {
                let found = self
                    .services
                    .mail
                    .fetch_whole(vec![thread_id.to_string()])
                    .await?;
                if !found.gone_threads.is_empty() {
                    return Err(BackendError::NotFound.into());
                }
                found
                    .whole
                    .into_iter()
                    .flatten()
                    .map(|meta| meta.id)
                    .collect()
            }
        };
        let mut mbox = Vec::new();
        for raw in self.services.mail.fetch_raw(&ids).await? {
            crate::export::append(&mut mbox, &raw.bytes);
        }
        Ok(mbox)
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
