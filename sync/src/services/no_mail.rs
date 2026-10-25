//! The mail of an account that has none: a calendar account, which holds
//! a CalDAV server's calendars and nothing else. It stands where a mail
//! adapter stands, so `AccountServices` keeps one shape and a module that
//! asks an account for its calendar never learns the difference.
//!
//! The engine starts no mail loop for such an account, and the window
//! lists it nowhere mail is listed. What asks every account all the same,
//! as the storage reading does, hears that there is nothing to say: the
//! trait's own answers, no storage and no counts. Any other call is
//! answered `Unsupported`, as a service the account lacks is everywhere
//! else.

use std::ops::RangeInclusive;
use std::time::Duration;

use mailrs_domain::{MailSet, RemoteMailbox, Role};
use mailrs_gmail::LabelColor;
use mailrs_mime::Parts;

use super::{
    Backfill, Changes, Found, IdentityService, KeywordsPage, MailBackend, MailCapabilities,
    RawMessage, Relocated, RemoteRef, SearchQuery, SendAsAddress, SyncState, Unapplied, Want,
};
use crate::api::{DraftRef, SavedDraft};
use crate::{BackendError, MailOp};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NoMail;

impl MailBackend for NoMail {
    fn capabilities(&self) -> MailCapabilities {
        MailCapabilities {
            labels: false,
            server_threads: false,
            files_sent_mail: false,
            categories: false,
            delete_forever: false,
            // One, not none: a write is split into batches of this size,
            // and a batch of nothing never ends.
            batch_limit: 1,
            keywords: &[],
            native_search: false,
            paged_mailboxes: false,
            renames: false,
            restates: false,
            tags: false,
            focus: false,
            local_mailboxes: false,
            files_muted_replies: false,
        }
    }

    fn provider_name(&self) -> &str {
        mailrs_domain::Provider::CalDav.name()
    }

    fn person_waiting(&self) -> bool {
        false
    }

    async fn stand_by(&self, wait: Duration) {
        tokio::time::sleep(wait).await;
    }

    fn made_by_person(&self, _id: &str) -> bool {
        false
    }

    async fn mailboxes(&self) -> Result<Vec<RemoteMailbox>, BackendError> {
        Err(BackendError::Unsupported)
    }

    async fn changes(&self, _since: Option<&SyncState>) -> Result<Changes, BackendError> {
        Err(BackendError::Unsupported)
    }

    async fn create_mailbox(&self, _name: &str) -> Result<RemoteMailbox, BackendError> {
        Err(BackendError::Unsupported)
    }

    async fn rename_mailbox(&self, _id: &str, _name: &str) -> Result<RemoteMailbox, BackendError> {
        Err(BackendError::Unsupported)
    }

    async fn delete_mailbox(&self, _id: &str) -> Result<(), BackendError> {
        Err(BackendError::Unsupported)
    }

    async fn set_mailbox_color(
        &self,
        _id: &str,
        _color: &LabelColor,
    ) -> Result<RemoteMailbox, BackendError> {
        Err(BackendError::Unsupported)
    }

    async fn mailbox_threads(&self, _id: &str) -> Result<u64, BackendError> {
        Err(BackendError::Unsupported)
    }

    fn follow(&self, _mailbox: &str) -> bool {
        false
    }

    fn set_window_open(&self, _open: bool) {}

    fn poll_interval(&self) -> Option<Duration> {
        None
    }

    /// No server will ever say the Inbox changed.
    async fn watch(&self) {
        std::future::pending().await
    }

    async fn keywords_in(
        &self,
        _mailbox: &str,
        _uidvalidity: u32,
        _uids: RangeInclusive<u32>,
    ) -> Result<KeywordsPage, BackendError> {
        Err(BackendError::Unsupported)
    }

    async fn uidvalidity(&self, _mailbox: &str) -> Result<Option<u32>, BackendError> {
        Err(BackendError::Unsupported)
    }

    async fn keywords_stored(
        &self,
        _mailbox: &str,
    ) -> Result<&'static [&'static str], BackendError> {
        Err(BackendError::Unsupported)
    }

    async fn backfill(&self, _days: i64, _cursor: Option<&str>) -> Result<Backfill, BackendError> {
        Err(BackendError::Unsupported)
    }

    async fn window_ids(
        &self,
        _days: i64,
        _mailbox: Option<&str>,
    ) -> Result<Vec<RemoteRef>, BackendError> {
        Err(BackendError::Unsupported)
    }

    async fn inbox_ids(&self) -> Result<Vec<RemoteRef>, BackendError> {
        Err(BackendError::Unsupported)
    }

    async fn search(
        &self,
        _query: &SearchQuery,
        _limit: usize,
    ) -> Result<Vec<RemoteRef>, BackendError> {
        Err(BackendError::Unsupported)
    }

    async fn find_sent(&self, _message_id: &str) -> Result<Option<String>, BackendError> {
        Err(BackendError::Unsupported)
    }

    async fn fetch(&self, _wants: Vec<Want>) -> Result<Found, BackendError> {
        Err(BackendError::Unsupported)
    }

    async fn fetch_whole(&self, _threads: Vec<String>) -> Result<Found, BackendError> {
        Err(BackendError::Unsupported)
    }

    async fn fetch_raw(&self, _ids: &[String]) -> Result<Vec<RawMessage>, BackendError> {
        Err(BackendError::Unsupported)
    }

    async fn append(&self, _raw: &[u8], _mailbox: &str) -> Result<String, BackendError> {
        Err(BackendError::Unsupported)
    }

    async fn fetch_structure(&self, _id: &str) -> Result<Parts, BackendError> {
        Err(BackendError::Unsupported)
    }

    async fn fetch_part(&self, _id: &str, _path: &str) -> Result<Vec<u8>, BackendError> {
        Err(BackendError::Unsupported)
    }

    fn mailbox_for(&self, _role: Role) -> Option<String> {
        None
    }

    fn set_of(&self, id: &str) -> MailSet {
        MailSet::Mailbox(id.to_string())
    }

    async fn apply(
        &self,
        _messages: &[String],
        _ops: &[MailOp],
    ) -> Result<Vec<Relocated>, Unapplied> {
        Err(Unapplied {
            taken: 0,
            error: BackendError::Unsupported,
            relocated: Vec::new(),
        })
    }

    async fn send(&self, _raw: &[u8], _thread_id: Option<&str>) -> Result<String, BackendError> {
        Err(BackendError::Unsupported)
    }

    async fn save_draft(
        &self,
        _draft_id: Option<&str>,
        _raw: &[u8],
        _thread_id: Option<&str>,
    ) -> Result<SavedDraft, BackendError> {
        Err(BackendError::Unsupported)
    }

    async fn send_draft(&self, _draft_id: &str) -> Result<String, BackendError> {
        Err(BackendError::Unsupported)
    }

    async fn delete_draft(&self, _draft_id: &str) -> Result<(), BackendError> {
        Err(BackendError::Unsupported)
    }

    async fn list_drafts(&self) -> Result<Vec<DraftRef>, BackendError> {
        Err(BackendError::Unsupported)
    }
}

/// An account that sends nothing has no address to send from.
impl IdentityService for NoMail {
    async fn identities(&self) -> Result<Vec<SendAsAddress>, BackendError> {
        Ok(Vec::new())
    }
}
