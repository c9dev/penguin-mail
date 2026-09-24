//! The IMAP adapter: mail over IMAP and sending over SMTP, for any
//! provider that serves them. `I` is the IMAP client and `S` the SMTP
//! client, the real ones or the fakes for tests and the demo. The server
//! keeps no threads and names a message by mailbox, UIDVALIDITY and UID,
//! so the engine threads the mail itself and follows each message's
//! remote ref. This adapter never opens the store.

mod api;
mod mailboxes;

pub use api::{ImapApi, Submit};

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use mailrs_domain::mailbox::keyword;
use mailrs_domain::{MailSet, RemoteMailbox, Role};
use mailrs_gmail::LabelColor;
use mailrs_imap::ImapError;
use mailrs_mime::Parts;

use super::{
    Backfill, Changes, Found, IdentityService, MailBackend, MailCapabilities, RawMessage,
    RemoteRef, SearchQuery, SendAsAddress, SyncState, Unapplied, Want,
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
/// it is kept: the next start asks again.
#[derive(Default)]
struct Known {
    /// The last listing, which roles and names are read from.
    folders: Vec<Folder>,
    /// The server's hierarchy delimiter.
    delimiter: Option<char>,
    /// The keywords the Inbox's PERMANENTFLAGS take, once it was selected.
    keywords: Option<&'static [&'static str]>,
}

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
            keywords: self.known().keywords.unwrap_or(SYSTEM_KEYWORDS),
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

    async fn changes(&self, _since: Option<&SyncState>) -> Result<Changes, BackendError> {
        Err(BackendError::Unsupported)
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
        Ok(u64::from(self.api.select(id, None).await?.exists))
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

    async fn apply(&self, _messages: &[String], _ops: &[MailOp]) -> Result<(), Unapplied> {
        Err(Unapplied {
            taken: 0,
            error: BackendError::Unsupported,
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
