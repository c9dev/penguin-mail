//! Sending, drafts, search, attachments and exports: mail calls the UI makes
//! on demand rather than as part of the sync loop.

use std::collections::BTreeSet;
use std::sync::Arc;

use tokio::io::{AsyncWrite, AsyncWriteExt};

use mailrs_domain::translate::gettext;
use mailrs_domain::{ChangeEvent, Role};
use mailrs_store::messages::Change;
use mailrs_store::{drafts, messages, outbox};

use super::AccountSync;
use crate::{BackendError, SavedDraft, SyncError};

impl AccountSync {
    /// Sends raw RFC 822 bytes, then deletes `draft_id` if the message came
    /// from a draft. A draft that is already gone does not fail the send.
    /// A server that does not file what it sends gets a copy in Sent.
    /// Returns the sent message's id.
    pub async fn send(
        &self,
        raw: Vec<u8>,
        thread_id: Option<String>,
        draft_id: Option<String>,
    ) -> Result<String, SyncError> {
        let sent = self.mail.send(&raw, thread_id.as_deref()).await?;
        let message_id = self.file_sent(&raw).await.unwrap_or_else(|| sent.clone());
        if let Some(draft_id) = draft_id {
            // The message has gone out, so nothing after this may fail the
            // send: the outbox would send it again. A server that keeps one
            // copy of the same bytes sent the draft itself and filed it in
            // Sent: deleting it would delete the sent message.
            if draft_id != sent
                && let Err(err) = self.mail.delete_draft(&draft_id).await
                && !matches!(err, SyncError::Backend(BackendError::NotFound))
            {
                tracing::warn!(account = self.account_id, error = %err, "sent, but could not delete the draft");
            }
            self.forget_draft(&draft_id).await;
        }
        self.tell_local(&message_id).await;
        Ok(message_id)
    }

    /// On an account whose mailboxes are local the adapter wrote the
    /// change to the store itself, and no sync follows to say so, so the
    /// window hears of the thread here.
    async fn tell_local(&self, message_id: &str) {
        if !self.mail.capabilities().local_mailboxes {
            return;
        }
        let (account_id, id) = (self.account_id, message_id.to_string());
        if let Ok(Some(thread)) = self
            .db
            .read(move |c| messages::thread_id_of(c, account_id, &id))
            .await
        {
            self.emit_threads(BTreeSet::from([thread]));
        }
    }

    /// Files a copy of `raw` in Sent for a server that does not file what
    /// it sends, and answers the copy's name. The message has gone out
    /// whatever happens here, so a failure never reaches the caller, which
    /// would send the message again. A failed append gets one more try,
    /// unless a look finds the first one landed after all; when that fails
    /// too, the person hears that Sent holds no copy.
    async fn file_sent(&self, raw: &[u8]) -> Option<String> {
        let mail = &self.mail;
        if mail.capabilities().files_sent_mail {
            return None;
        }
        let Some(sent) = mail.mailbox_for(Role::Sent) else {
            tracing::warn!(
                account = self.account_id,
                "sent, but the server has no Sent mailbox for a copy"
            );
            return None;
        };
        let first = match mail.append(raw, &sent).await {
            Ok(name) => return Some(name),
            Err(err) => err,
        };
        tracing::warn!(account = self.account_id, error = %first, "sent, but could not file a copy in Sent; trying again");
        if let Ok(Some(found)) = self.sent_copy(raw).await {
            return Some(found);
        }
        match mail.append(raw, &sent).await {
            Ok(name) => Some(name),
            Err(err) => {
                tracing::warn!(account = self.account_id, error = %err, "sent, but could not file a copy in Sent");
                self.emit(ChangeEvent::WriteFailed {
                    account_id: self.account_id,
                    message: gettext(
                        "The message was sent, but Penguin Mail could not keep a copy in Sent.",
                    ),
                });
                None
            }
        }
    }

    /// The id of the sent message carrying the same `Message-ID` header as
    /// `raw`, when the server holds one. A send whose answer never came
    /// back may still have gone out, and this is how a retry finds out
    /// before sending the message a second time. One search: 5 quota units
    /// on Gmail, a SEARCH of the Sent mailbox on IMAP. A draft carries the
    /// same header as the message it becomes, so the search asks for sent
    /// mail alone. Bytes without the header answer `None`.
    pub async fn sent_copy(&self, raw: &[u8]) -> Result<Option<String>, SyncError> {
        let Some(id) = message_id_header(raw) else {
            return Ok(None);
        };
        self.mail.find_sent(&id).await
    }

    /// Saves a draft on the server, replacing `draft_id` when given. If
    /// that draft was deleted elsewhere, creates a new one. On IMAP the new
    /// copy goes into Drafts before the old one goes, so a failure leaves
    /// two drafts rather than none.
    pub async fn save_draft(
        &self,
        raw: Vec<u8>,
        thread_id: Option<String>,
        draft_id: Option<String>,
    ) -> Result<SavedDraft, SyncError> {
        if let Some(id) = &draft_id {
            self.not_held(id).await?;
        }
        self.put_back_draft(raw, thread_id, draft_id).await
    }

    /// [`AccountSync::save_draft`] for the outbox, which saves over a held
    /// copy only once the server has let go of it.
    pub(crate) async fn put_back_draft(
        &self,
        raw: Vec<u8>,
        thread_id: Option<String>,
        draft_id: Option<String>,
    ) -> Result<SavedDraft, SyncError> {
        let thread_id = thread_id.as_deref();
        let saved = match self
            .mail
            .save_draft(draft_id.as_deref(), &raw, thread_id)
            .await
        {
            Err(SyncError::Backend(BackendError::NotFound)) if draft_id.is_some() => {
                self.mail.save_draft(None, &raw, thread_id).await?
            }
            other => other?,
        };
        // Gmail's answer names both the draft and the message it now
        // holds, which is the pair `drafts.list` would otherwise be asked
        // for. Keeping it here is what leaves every draft this app writes
        // out of that listing.
        self.remember_draft(&saved.draft_id, &saved.message_id)
            .await;
        self.tell_local(&saved.message_id).await;
        Ok(saved)
    }

    /// Refuses to save over the draft `draft_id` while the server holds it
    /// for Send Later: the server sends the copy it holds whatever happens
    /// to the draft, so an edit saved over it would go out beside it.
    async fn not_held(&self, draft_id: &str) -> Result<(), SyncError> {
        let (account_id, draft) = (self.account_id, draft_id.to_string());
        let row = self
            .db
            .read(move |c| outbox::find_draft(c, account_id, &draft))
            .await?;
        if row.is_some_and(|row| row.held.is_some()) {
            return Err(BackendError::Refused(gettext(
                "The server holds this message to send at its hour. Cancel Send before you change it.",
            ))
            .into());
        }
        Ok(())
    }

    /// Sends a draft as the server holds it, as a scheduled send does.
    /// Returns `None` when the draft is gone, sent or deleted elsewhere. A
    /// server that does not file what it sends gets a copy of the draft in
    /// Sent, read before the draft goes.
    pub async fn send_draft(&self, draft_id: &str) -> Result<Option<String>, SyncError> {
        let copy = match self.mail.capabilities().files_sent_mail {
            true => None,
            false => match self.raw(draft_id).await {
                Ok(raw) => Some(raw),
                Err(SyncError::Backend(BackendError::NotFound)) => None,
                Err(err) => return Err(err),
            },
        };
        let sent = match self.mail.send_draft(draft_id).await {
            Ok(id) => Some(id),
            Err(SyncError::Backend(BackendError::NotFound)) => None,
            Err(err) => return Err(err),
        };
        let sent = match (sent, copy) {
            (Some(id), Some(raw)) => Some(self.file_sent(&raw).await.unwrap_or(id)),
            (sent, _) => sent,
        };
        self.forget_draft(draft_id).await;
        if let Some(id) = &sent {
            self.tell_local(id).await;
        }
        Ok(sent)
    }

    pub async fn delete_draft(&self, draft_id: &str) -> Result<(), SyncError> {
        match self.mail.delete_draft(draft_id).await {
            Ok(()) | Err(SyncError::Backend(BackendError::NotFound)) => {}
            Err(err) => return Err(err),
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
                let delete = Change::Erase { message_id: id };
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
        let listed = self.mail.list_drafts().await?;
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
    pub(super) async fn remember_draft(&self, draft_id: &str, message_id: &str) {
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
    /// alone. A cached raw message without that path, which a path read
    /// from the server's structure can name, falls back to the part alone
    /// too.
    pub async fn attachment(
        &self,
        message_id: &str,
        part_path: &str,
    ) -> Result<Vec<u8>, SyncError> {
        let raw = match self.cached_raw(message_id) {
            Some(raw) => Some(raw),
            None if self.small(message_id).await? => Some(self.raw(message_id).await?),
            None => None,
        };
        match raw.and_then(|raw| mailrs_mime::part(&raw, part_path)) {
            Some(bytes) => Ok(bytes),
            None => self.mail.fetch_part(message_id, part_path).await,
        }
    }

    /// The message as it arrived, for View Source, a signature check and
    /// saving one message as an `.eml` file. A large message comes whole
    /// here too, since each of these needs every byte.
    pub async fn raw_message(&self, id: &str) -> Result<Arc<Vec<u8>>, SyncError> {
        self.raw(id).await
    }

    /// A conversation as an mbox file, oldest message first, or the one
    /// message `message_id` names when the list shows messages rather than
    /// conversations. Each message costs a fetch from the server, so a
    /// long conversation is a handful of calls; the caller runs this off
    /// the user's thread.
    pub async fn export_mbox(
        &self,
        thread_id: &str,
        message_id: Option<&str>,
    ) -> Result<Vec<u8>, SyncError> {
        let mut mbox = Vec::new();
        self.export_mbox_to(thread_id, message_id, &mut mbox)
            .await?;
        Ok(mbox)
    }

    /// `export_mbox`, written to `out` one message at a time. The messages
    /// are fetched in small batches ([`export_batches`]), which a server
    /// that reads many at once answers in one round trip, and each one is
    /// written and dropped before the next, so a long conversation costs a
    /// batch's bytes at most and not the whole thread's.
    pub async fn export_mbox_to<W: AsyncWrite + Unpin>(
        &self,
        thread_id: &str,
        message_id: Option<&str>,
        out: &mut W,
    ) -> Result<(), SyncError> {
        let ids: Vec<String> = match message_id {
            Some(id) => vec![id.to_string()],
            None => self.thread_ids(thread_id).await?,
        };
        let account_id = self.account_id;
        let sizes = {
            let ids = ids.clone();
            self.db
                .read(move |c| {
                    ids.iter()
                        .map(|id| messages::size_of(c, account_id, id))
                        .collect::<mailrs_store::Result<Vec<_>>>()
                })
                .await?
        };
        let mut entry = Vec::new();
        for batch in export_batches(&sizes) {
            let raws = self.mail.fetch_raw(&ids[batch]).await?;
            for raw in raws {
                entry.clear();
                crate::export::append(&mut entry, &raw.bytes);
                drop(raw);
                out.write_all(&entry).await.map_err(SyncError::io)?;
            }
        }
        out.flush().await.map_err(SyncError::io)?;
        Ok(())
    }

    /// The messages of a conversation, oldest first. A server that keeps
    /// no threads cannot name what local threading put together, so the
    /// store says; a thread the store lacks is one a search listed by its
    /// one message's location, which the server can name.
    async fn thread_ids(&self, thread_id: &str) -> Result<Vec<String>, SyncError> {
        if self.local_threads() {
            let (account_id, thread) = (self.account_id, thread_id.to_string());
            let stored: Vec<String> = self
                .db
                .read(move |c| {
                    Ok(messages::thread_messages(c, account_id, &thread)?
                        .into_iter()
                        .map(|m| m.id)
                        .collect())
                })
                .await?;
            if !stored.is_empty() {
                return Ok(stored);
            }
        }
        let found = self.mail.fetch_whole(vec![thread_id.to_string()]).await?;
        if !found.gone_threads.is_empty() {
            return Err(BackendError::NotFound.into());
        }
        Ok(found
            .whole
            .into_iter()
            .flatten()
            .map(|meta| meta.id)
            .collect())
    }
}

/// The most messages an export asks the server for at once.
const EXPORT_BATCH: usize = 20;

/// The most stored bytes one export batch may add up to, so a batch never
/// holds much more in memory than one message the raw path would.
const EXPORT_BATCH_BYTES: i64 = 2 * crate::RAW_LIMIT;

/// The messages of an export, by their stored `sizes`, cut into runs the
/// server is asked for together. A message of unknown size, or one too
/// large to share the budget, goes alone.
fn export_batches(sizes: &[Option<i64>]) -> Vec<std::ops::Range<usize>> {
    let mut batches = Vec::new();
    let (mut start, mut bytes) = (0, 0);
    for (at, size) in sizes.iter().enumerate() {
        let size = size
            .filter(|s| *s > 0 && *s <= EXPORT_BATCH_BYTES)
            .unwrap_or(EXPORT_BATCH_BYTES);
        if at > start && (at - start == EXPORT_BATCH || bytes + size > EXPORT_BATCH_BYTES) {
            batches.push(start..at);
            (start, bytes) = (at, 0);
        }
        bytes += size;
    }
    if start < sizes.len() {
        batches.push(start..sizes.len());
    }
    batches
}

/// The `Message-ID` header of RFC 822 bytes, without its angle brackets.
/// Only the header block is read, and a header folded onto the next line
/// is unfolded first.
pub(crate) fn message_id_header(raw: &[u8]) -> Option<String> {
    mailrs_mime::head::message_id(raw)
}

#[cfg(test)]
mod tests {
    use super::{EXPORT_BATCH_BYTES, export_batches, message_id_header};

    #[test]
    fn an_export_asks_for_small_messages_together_and_large_ones_alone() {
        let small = Some(1_000);
        assert_eq!(export_batches(&[small; 45]), [0..20, 20..40, 40..45]);
        let big = Some(EXPORT_BATCH_BYTES + 1);
        assert_eq!(
            export_batches(&[small, big, small, None, small]),
            [0..1, 1..2, 2..3, 3..4, 4..5]
        );
        let half = Some(EXPORT_BATCH_BYTES / 2);
        assert_eq!(export_batches(&[half, half, half]), [0..2, 2..3]);
        assert!(export_batches(&[]).is_empty());
    }

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
