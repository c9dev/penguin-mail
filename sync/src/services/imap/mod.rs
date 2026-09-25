//! The IMAP adapter: mail over IMAP and sending over SMTP, for any
//! provider that serves them. `I` is the IMAP client and `S` the SMTP
//! client, the real ones or the fakes for tests and the demo. The server
//! keeps no threads and names a message by mailbox, UIDVALIDITY and UID,
//! so the engine threads the mail itself and follows each message's
//! remote ref. This adapter never opens the store.

mod api;
mod bodies;
mod cadence;
mod feed;
mod keywords;
mod mailboxes;
mod outgoing;
mod search;
mod state;
mod syntax;
mod window;
mod writes;

pub use api::{ImapApi, Submit};

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::ops::RangeInclusive;
use std::time::Duration;

use mailrs_domain::mailbox::keyword;
use mailrs_domain::{MailSet, RemoteMailbox, Role};
use mailrs_gmail::LabelColor;
use mailrs_imap::{Capabilities, ImapError, Selected, Since};
use mailrs_mime::Parts;
use tokio::time::Instant;

use super::{
    Backfill, Changes, Found, IdentityService, KeywordsPage, MailBackend, MailCapabilities,
    RawMessage, Relocated, RemoteRef, SearchQuery, SendAsAddress, SyncState, Unapplied, Want,
};
use crate::api::{DraftRef, SavedDraft};
use crate::{BackendError, MailOp};
use mailboxes::Folder;

/// What an IMAP account's settings say about its server, from the
/// provider table or the person's own entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImapSettings {
    /// The address the account signs in with and sends as.
    pub address: String,
    /// The provider's name as people know it, such as `Fastmail`.
    pub provider_name: String,
    /// The server files a copy of what the account sends.
    pub files_sent_mail: bool,
    /// Days of mail the account keeps besides the whole Inbox.
    pub window_days: i64,
}

/// The keywords every server stores: its system flags.
const SYSTEM_KEYWORDS: &[&str] = &[
    keyword::SEEN,
    keyword::FLAGGED,
    keyword::ANSWERED,
    keyword::DRAFT,
];

/// The most messages one IMAP command names.
const BATCH_LIMIT: usize = 500;

/// An IMAP account's mail and identity, over the IMAP client `I` and the
/// SMTP client `S`.
pub struct Imap<I, S> {
    api: Arc<I>,
    smtp: Arc<S>,
    settings: Arc<ImapSettings>,
    known: Arc<Mutex<Known>>,
}

/// What the adapter has learned from the server since it started. None of
/// it is kept: the next start asks again. Nothing here grows with a
/// mailbox's message count; a look that needs to compare against what a
/// message carried before reads the store instead of keeping a copy.
#[derive(Default)]
struct Known {
    /// The server's capabilities, asked once.
    capabilities: Option<Capabilities>,
    /// The last listing, which roles and names are read from.
    folders: Vec<Folder>,
    /// The server's hierarchy delimiter.
    delimiter: Option<char>,
    /// The keywords each mailbox's own PERMANENTFLAGS take, recorded as it
    /// is selected. Bounded by how many mailboxes the account has, not by
    /// their size.
    keywords: HashMap<String, &'static [&'static str]>,
    /// Messages whose text the client's guard refused this session, the
    /// latest [`UNREADABLE_KEPT`], so opening one again does not ask for
    /// the part and lose the connection each time.
    unreadable: VecDeque<String>,
    /// Mailboxes a person opened, synced besides the four the account
    /// always keeps in step. Bounded by how many mailboxes a person opens,
    /// not by their size; a restart rebuilds it from the sync state.
    followed: BTreeSet<String>,
    /// Where a window listing last left a mailbox, for a mailbox the feed
    /// has not kept state for yet: a mailbox just followed starts from
    /// here rather than from whatever the server holds at the next look,
    /// which would miss mail that arrived in between. Bounded by how many
    /// mailboxes the account has, not their size.
    recent: HashMap<String, state::Kept>,
    /// Only the tray runs; the window is closed.
    tray_only: bool,
    /// When the slow poll last looked at every synced mailbox.
    last_slow: Option<Instant>,
    /// Every look covers every synced mailbox, for tests that should not
    /// wait for the slow poll.
    every_look: bool,
    /// The last run a move took whose new places the Message-ID search
    /// failed to find. The retry of that run searches again rather than
    /// moving messages that have left. One run at most.
    unfound: Option<writes::Unfound>,
    /// IDLE attempts on the Inbox that failed in a row, so a server that
    /// keeps refusing or dropping it is watched less and less often. A
    /// success, including one the guard ends early, resets it.
    idle_failures: u32,
}

/// The most messages [`Known::unreadable`] remembers.
const UNREADABLE_KEPT: usize = 256;

// By hand, since a derive would ask for `I: Clone` and `S: Clone` and the
// clients are shared rather than copied.
impl<I, S> Clone for Imap<I, S> {
    fn clone(&self) -> Self {
        Imap {
            api: Arc::clone(&self.api),
            smtp: Arc::clone(&self.smtp),
            settings: Arc::clone(&self.settings),
            known: Arc::clone(&self.known),
        }
    }
}

impl<I, S> Imap<I, S> {
    pub fn new(api: Arc<I>, smtp: Arc<S>, settings: ImapSettings) -> Self {
        Imap {
            api,
            smtp,
            settings: Arc::new(settings),
            known: Arc::new(Mutex::new(Known::default())),
        }
    }

    fn known(&self) -> MutexGuard<'_, Known> {
        self.known.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether the client's guard refused the text of message `name` this
    /// session.
    fn is_unreadable(&self, name: &str) -> bool {
        self.known().unreadable.iter().any(|n| n == name)
    }

    /// Remembers that the client's guard refused the text of message
    /// `name`, forgetting the oldest past [`UNREADABLE_KEPT`].
    fn mark_unreadable(&self, name: &str) {
        let mut known = self.known();
        if known.unreadable.iter().any(|n| n == name) {
            return;
        }
        if known.unreadable.len() == UNREADABLE_KEPT {
            known.unreadable.pop_front();
        }
        known.unreadable.push_back(name.to_string());
    }

    /// Makes every look at the feed cover every synced mailbox, for tests
    /// that check a mailbox other than the Inbox right after a change.
    #[cfg(any(test, feature = "fake"))]
    pub fn look_at_every_mailbox(&self) {
        self.known().every_look = true;
    }

    /// Whether `mailbox` is followed as a person opened it, for a test
    /// that checks a mailbox deleted elsewhere leaves the followed set.
    #[cfg(any(test, feature = "fake"))]
    pub fn is_followed(&self, mailbox: &str) -> bool {
        self.known().followed.contains(mailbox)
    }
}

impl<I: ImapApi, S: Submit> Imap<I, S> {
    /// Selects `mailbox`, with QRESYNC's parameters when `since` gives
    /// them, and notes which keywords this mailbox's own PERMANENTFLAGS
    /// let the server store.
    async fn select(&self, mailbox: &str, since: Option<Since>) -> Result<Selected, BackendError> {
        let selected = self.api.select(mailbox, since).await?;
        self.note_keywords(mailbox, &selected);
        Ok(selected)
    }

    /// Remembers which keywords `mailbox`'s own PERMANENTFLAGS let the
    /// server store there. Every mailbox can differ, so a look or a write
    /// in one never decides for another.
    fn note_keywords(&self, mailbox: &str, selected: &Selected) {
        self.known()
            .keywords
            .insert(mailbox.to_string(), keywords::stored_keywords(&selected.permanent_flags));
    }

    /// The keywords `mailbox`'s own PERMANENTFLAGS store, or the system
    /// flags alone when the mailbox has not been selected yet.
    fn stored_keywords(&self, mailbox: &str) -> &'static [&'static str] {
        self.known()
            .keywords
            .get(mailbox)
            .copied()
            .unwrap_or(SYSTEM_KEYWORDS)
    }

    /// The server's capabilities, asked once. The client asks after the
    /// login, when CONDSTORE and QRESYNC often first show.
    async fn capabilities_now(&self) -> Result<Capabilities, BackendError> {
        let known = self.known().capabilities;
        if let Some(capabilities) = known {
            return Ok(capabilities);
        }
        let capabilities = self.api.capabilities().await?;
        self.known().capabilities = Some(capabilities);
        Ok(capabilities)
    }
}

/// IMAP's errors as kinds, so the retry rules and the account states read
/// them as they read Gmail's. A refused sign-in and IMAP switched off both
/// need the person; a full connection limit is waited out like a rate
/// limit; a mailbox the server does not have is `NotFound`, which the feed
/// reads as a mailbox deleted elsewhere. A value the client refused before
/// sending (`Invalid`) never reached the server; it is a bug in the
/// caller, reported the same as a plain refusal. A TLS failure keeps its
/// host in the refusal's words, for the dialog.
impl From<ImapError> for BackendError {
    fn from(err: ImapError) -> Self {
        match err {
            ImapError::Network(detail) => BackendError::Offline(detail),
            ImapError::Auth { .. } | ImapError::ImapDisabled { .. } => BackendError::NeedsReauth,
            ImapError::TooManyConnections { .. } => BackendError::RateLimited(None),
            ImapError::NoMailbox(_) => BackendError::NotFound,
            ImapError::Unsupported(_) => BackendError::Unsupported,
            other @ (ImapError::Tls { .. }
            | ImapError::Protocol(_)
            | ImapError::Refused(_)
            | ImapError::Invalid(_)) => BackendError::Refused(other.to_string()),
        }
    }
}

impl<I: ImapApi, S: Submit> MailBackend for Imap<I, S> {
    fn capabilities(&self) -> MailCapabilities {
        MailCapabilities {
            labels: false,
            server_threads: false,
            files_sent_mail: self.settings.files_sent_mail,
            categories: false,
            delete_forever: true,
            batch_limit: BATCH_LIMIT,
            keywords: self.stored_keywords("INBOX"),
            native_search: false,
        }
    }

    /// Nothing paces an IMAP server the way Gmail's quota does, so nobody
    /// waits on it.
    fn person_waiting(&self) -> bool {
        false
    }

    async fn stand_by(&self, wait: Duration) {
        tokio::time::sleep(wait).await;
    }

    /// Every mailbox but the Inbox, the role mailboxes and a flagged view
    /// is one a person made.
    fn made_by_person(&self, id: &str) -> bool {
        if id.eq_ignore_ascii_case("INBOX") {
            return false;
        }
        self.known()
            .folders
            .iter()
            .find(|f| f.id == id)
            .is_none_or(|f| f.role.is_none() && !f.flagged)
    }

    async fn mailboxes(&self) -> Result<Vec<RemoteMailbox>, BackendError> {
        self.list_mailboxes().await
    }

    async fn changes(&self, since: Option<&SyncState>) -> Result<Changes, BackendError> {
        self.feed(since).await
    }

    async fn create_mailbox(&self, name: &str) -> Result<RemoteMailbox, BackendError> {
        self.make_mailbox(name).await
    }

    async fn rename_mailbox(&self, id: &str, name: &str) -> Result<RemoteMailbox, BackendError> {
        self.rename_mailbox_to(id, name).await
    }

    async fn delete_mailbox(&self, id: &str) -> Result<(), BackendError> {
        self.remove_mailbox(id).await
    }

    /// IMAP keeps no colour for a mailbox.
    async fn set_mailbox_color(
        &self,
        _id: &str,
        _color: &LabelColor,
    ) -> Result<RemoteMailbox, BackendError> {
        Err(BackendError::Unsupported)
    }

    /// IMAP counts messages, not conversations, so this answers the
    /// messages the mailbox holds.
    async fn mailbox_threads(&self, id: &str) -> Result<u64, BackendError> {
        let parent_only = self
            .known()
            .folders
            .iter()
            .any(|f| f.id == id && f.parent_only);
        if parent_only {
            return Ok(0);
        }
        Ok(u64::from(self.select(id, None).await?.exists))
    }

    fn follow(&self, mailbox: &str) {
        self.known().followed.insert(mailbox.to_string());
    }

    fn set_window_open(&self, open: bool) {
        self.known().tray_only = !open;
    }

    fn poll_interval(&self) -> Option<Duration> {
        let known = self.known();
        let idle = known.capabilities.as_ref().is_some_and(|c| c.idle);
        Some(cadence::poll_every(idle, known.tray_only))
    }

    async fn watch(&self) {
        self.watch_inbox().await
    }

    async fn keywords_in(
        &self,
        mailbox: &str,
        uidvalidity: u32,
        uids: RangeInclusive<u32>,
    ) -> Result<KeywordsPage, BackendError> {
        self.window_keywords(mailbox, uidvalidity, uids).await
    }

    async fn uidvalidity(&self, mailbox: &str) -> Result<Option<u32>, BackendError> {
        Ok(Some(self.select(mailbox, None).await?.uidvalidity))
    }

    async fn backfill(&self, days: i64, cursor: Option<&str>) -> Result<Backfill, BackendError> {
        self.backfill_page(days, cursor).await
    }

    async fn window_ids(
        &self,
        days: i64,
        mailbox: Option<&str>,
    ) -> Result<Vec<RemoteRef>, BackendError> {
        self.window_refs(days, mailbox).await
    }

    async fn inbox_ids(&self) -> Result<Vec<RemoteRef>, BackendError> {
        self.inbox_refs().await
    }

    async fn search(
        &self,
        query: &SearchQuery,
        limit: usize,
    ) -> Result<Vec<RemoteRef>, BackendError> {
        self.search_server(query, limit).await
    }

    async fn find_sent(&self, message_id: &str) -> Result<Option<String>, BackendError> {
        self.sent_with(message_id).await
    }

    async fn fetch(&self, wants: Vec<Want>) -> Result<Found, BackendError> {
        self.fetch_metas(wants).await
    }

    async fn fetch_whole(&self, threads: Vec<String>) -> Result<Found, BackendError> {
        self.fetch_lone(threads).await
    }

    async fn fetch_raw(&self, ids: &[String]) -> Result<Vec<RawMessage>, BackendError> {
        self.raw_messages(ids).await
    }

    /// Files a copy of what the account sent, read, as a mail program
    /// would have filed it.
    async fn append(&self, raw: &[u8], mailbox: &str) -> Result<String, BackendError> {
        self.file(raw, mailbox, &["\\Seen".to_string()]).await
    }

    async fn fetch_structure(&self, id: &str) -> Result<Parts, BackendError> {
        self.structure_of(id).await
    }

    async fn fetch_part(&self, id: &str, path: &str) -> Result<Vec<u8>, BackendError> {
        self.part_of(id, path).await
    }

    fn mailbox_for(&self, role: Role) -> Option<String> {
        let listed = self
            .known()
            .folders
            .iter()
            .find(|f| f.role == Some(role))
            .map(|f| f.id.clone());
        // Every IMAP server has an Inbox by that name, listed or not.
        listed.or_else(|| (role == Role::Inbox).then(|| "INBOX".to_string()))
    }

    fn set_of(&self, id: &str) -> MailSet {
        let known = self.known();
        match known.folders.iter().find(|f| f.id == id) {
            Some(folder) if folder.flagged => MailSet::flagged(),
            Some(Folder {
                role: Some(role), ..
            }) => MailSet::Role(*role),
            _ if id.eq_ignore_ascii_case("INBOX") => MailSet::Role(Role::Inbox),
            _ => MailSet::Mailbox(id.to_string()),
        }
    }

    async fn apply(
        &self,
        messages: &[String],
        ops: &[MailOp],
    ) -> Result<Vec<Relocated>, Unapplied> {
        self.write(messages, ops).await
    }

    /// Sends over SMTP. The server keeps no threads, so `thread_id` has
    /// nothing to join, and the answer is the message's Message-ID.
    async fn send(&self, raw: &[u8], _thread_id: Option<&str>) -> Result<String, BackendError> {
        self.submit(raw).await
    }

    async fn save_draft(
        &self,
        draft_id: Option<&str>,
        raw: &[u8],
        thread_id: Option<&str>,
    ) -> Result<SavedDraft, BackendError> {
        self.save(draft_id, raw, thread_id).await
    }

    async fn send_draft(&self, draft_id: &str) -> Result<String, BackendError> {
        self.send_saved(draft_id).await
    }

    async fn delete_draft(&self, draft_id: &str) -> Result<(), BackendError> {
        self.erase(draft_id).await
    }

    async fn list_drafts(&self) -> Result<Vec<DraftRef>, BackendError> {
        self.drafts().await
    }
}

impl<I: ImapApi, S: Submit> IdentityService for Imap<I, S> {
    /// The account's own address. An IMAP server keeps no list of the
    /// addresses a person may send as; custom identities live in
    /// Preferences.
    async fn identities(&self) -> Result<Vec<SendAsAddress>, BackendError> {
        Ok(vec![SendAsAddress {
            email: self.settings.address.clone(),
            name: None,
            signature: String::new(),
            default: true,
        }])
    }
}
