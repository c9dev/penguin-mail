//! What an account can be asked to do, split by kind of work, since
//! providers split it the same way: Fastmail reads mail over IMAP and its
//! calendar over CalDAV. Each kind is a trait here. A provider's adapter
//! implements the kinds it offers; [`Google`] is the only adapter so far.
//!
//! The mail trait still speaks a few of Gmail's shapes: drafts go by
//! Gmail's draft id, a search reaches the server in Gmail's syntax, and
//! mailbox colours come from Gmail's palette. Its feed of changes, its
//! listings and fetches, its server mailboxes, its operations, its errors
//! and its pacing are neutral.

mod any;
mod google;
mod pacing;

pub use any::{AnyAutoReply, AnyCalendar, AnyContacts, AnyIdentities, AnyMail, AnyRules};
pub use google::{Google, ID_PAGE_SIZE, LIST_PAGE_SIZE};
pub use pacing::{Priority, background, priority};

use std::sync::Arc;
use std::time::Duration;

use mailrs_domain::invitation::Answer;
use mailrs_domain::{
    EpochMillis, Filter, Membership, MessageBody, MessageMeta, RemoteMailbox, Role, Vacation,
};
use mailrs_gmail::{
    Answered, Busy, ConnectionsPage, ContactFields, Event, EventFields, LabelColor, Person, Series,
};

use crate::api::{AccountClient, DraftRef, SavedDraft};
#[cfg(any(test, feature = "fake"))]
use crate::fake::FakeGmail;
use crate::{BackendError, MailOp};

/// One address an account may send mail as: its own, or an alias whose
/// owner has confirmed it. The server keeps a display name and a signature
/// per address, so all three travel together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendAsAddress {
    pub email: String,
    pub name: Option<String>,
    /// The signature the server holds for this address, as plain text.
    pub signature: String,
    /// The address the server sends from when the writer picks none.
    pub default: bool,
}

/// What an account's mail service can do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MailCapabilities {
    /// A message can sit in several mailboxes at once, as with labels.
    pub labels: bool,
    /// The server groups messages into threads itself.
    pub server_threads: bool,
    /// The server files a copy of what the account sends.
    pub files_sent_mail: bool,
    /// The server sorts the inbox into categories.
    pub categories: bool,
    /// Mail can be erased for good, not only moved to the Trash.
    pub delete_forever: bool,
    /// The most messages one write may name.
    pub batch_limit: usize,
}

/// How far a write got before the server refused the rest: the first
/// `taken` messages went through. The caller waits, if the error is
/// worth waiting out, and sends the rest.
#[derive(Debug, Clone)]
pub struct Unapplied {
    pub taken: usize,
    pub error: BackendError,
}

/// Where a mail backend's feed of changes stands, in that backend's own
/// words: a Gmail history id, later an IMAP mailbox state or a Graph delta
/// link. The store keeps it as text in `accounts.sync_state`; only the
/// adapter that wrote it reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncState(String);

impl SyncState {
    pub fn new(text: impl Into<String>) -> SyncState {
        SyncState(text.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// What changed on the server since a sync state, and the state after.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Changes {
    pub changes: Vec<RemoteChange>,
    pub state: SyncState,
}

/// One change the server reports. A new message comes without its
/// metadata: which messages need fetching depends on what the store
/// already holds, which the adapter does not read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteChange {
    Added {
        id: String,
        thread_id: String,
    },
    Deleted {
        id: String,
    },
    Gained {
        id: String,
        thread_id: String,
        memberships: Vec<Membership>,
    },
    Lost {
        id: String,
        memberships: Vec<Membership>,
    },
}

/// What the server calls one message, with its thread, as a listing or a
/// search hands it over. For Gmail the id is the store's own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteRef {
    pub id: String,
    pub thread_id: String,
}

/// One message whose metadata a caller wants, with its thread when the
/// caller knows it. Only a known thread lets a fetch share one call
/// between messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Want {
    pub id: String,
    pub thread_id: Option<String>,
}

impl Want {
    /// A message whose thread the caller does not know.
    pub fn message(id: impl Into<String>) -> Want {
        Want {
            id: id.into(),
            thread_id: None,
        }
    }
}

impl From<RemoteRef> for Want {
    fn from(listed: RemoteRef) -> Want {
        Want {
            id: listed.id,
            thread_id: Some(listed.thread_id),
        }
    }
}

/// What a metadata fetch brought back.
#[derive(Debug, Default)]
pub struct Found {
    /// The wanted messages the server still has, in no particular order.
    pub metas: Vec<MessageMeta>,
    /// The wanted messages the server no longer has.
    pub gone: Vec<String>,
    /// Every message of each thread fetched along the way, including ones
    /// nobody wanted, for a caller that keeps whole threads.
    pub whole: Vec<Vec<MessageMeta>>,
    /// Threads asked for whole that the server no longer has.
    pub gone_threads: Vec<String>,
}

/// One page of the sync window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backfill {
    pub refs: Vec<RemoteRef>,
    /// The cursor for the next page; `None` after the last.
    pub next: Option<String>,
}

/// A message as it arrived, in RFC 822 form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawMessage {
    pub id: String,
    pub bytes: Vec<u8>,
}

/// A search, as the person wrote it. A backend that speaks the syntax
/// takes the text as typed, which is how every Gmail operator keeps
/// working. A neutral query tree arrives with the first provider that
/// lacks Gmail's syntax.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchQuery {
    Native(String),
}

/// The services one account is served by. A provider that lacks one leaves
/// its field `None`, and the module that needs it answers
/// `BackendError::Unsupported`.
#[derive(Clone)]
pub struct AccountServices {
    pub mail: AnyMail,
    pub calendar: Option<AnyCalendar>,
    pub contacts: Option<AnyContacts>,
    pub rules: AnyRules,
    pub auto_reply: Option<AnyAutoReply>,
    pub identities: AnyIdentities,
}

impl AccountServices {
    /// A Google account: every service, all over the one client, which
    /// spends one quota bucket for all of them.
    pub fn google(client: AccountClient) -> Self {
        let google = Google::new(Arc::new(client));
        AccountServices {
            mail: AnyMail::Google(google.clone()),
            calendar: Some(AnyCalendar::Google(google.clone())),
            contacts: Some(AnyContacts::Google(google.clone())),
            rules: AnyRules::Google(google.clone()),
            auto_reply: Some(AnyAutoReply::Google(google.clone())),
            identities: AnyIdentities::Google(google),
        }
    }

    /// The in-memory Gmail, for tests and the demo, through the same
    /// adapter a real account uses.
    #[cfg(any(test, feature = "fake"))]
    pub fn fake(gmail: Arc<FakeGmail>) -> Self {
        let google = Google::new(gmail);
        AccountServices {
            mail: AnyMail::Fake(google.clone()),
            calendar: Some(AnyCalendar::Fake(google.clone())),
            contacts: Some(AnyContacts::Fake(google.clone())),
            rules: AnyRules::Fake(google.clone()),
            auto_reply: Some(AnyAutoReply::Fake(google.clone())),
            identities: AnyIdentities::Fake(google),
        }
    }

    pub fn capabilities(&self) -> MailCapabilities {
        self.mail.capabilities()
    }
}

/// Listing, reading, changing and sending mail.
pub trait MailBackend: Send + Sync + 'static {
    fn capabilities(&self) -> MailCapabilities;

    /// Whether the person is waiting on this account's server now.
    /// Backfill reads it between pages and gives way.
    fn person_waiting(&self) -> bool;

    /// Sits out `wait` on the person's behalf, as a mail action waiting
    /// out a rate limit does. The account's background work stands aside
    /// for the whole wait.
    fn stand_by(&self, wait: Duration) -> impl Future<Output = ()> + Send;

    /// Whether a person made the mailbox `id` rather than the server. A
    /// change naming such a mailbox the store has not listed means the
    /// mailbox list changed.
    fn made_by_person(&self, id: &str) -> bool;

    /// Every mailbox the server lists.
    fn mailboxes(&self) -> impl Future<Output = Result<Vec<RemoteMailbox>, BackendError>> + Send;

    /// With no state, where the feed stands now and no changes. With one,
    /// every change since it, and where the feed stands after. Answers
    /// `BackendError::StateLost` when the server no longer keeps changes
    /// that old or cannot read the state, and the caller lists the mail
    /// again.
    fn changes(
        &self,
        since: Option<&SyncState>,
    ) -> impl Future<Output = Result<Changes, BackendError>> + Send;

    /// Makes a mailbox a person names. Slashes nest it under another.
    fn create_mailbox(
        &self,
        name: &str,
    ) -> impl Future<Output = Result<RemoteMailbox, BackendError>> + Send;

    fn rename_mailbox(
        &self,
        id: &str,
        name: &str,
    ) -> impl Future<Output = Result<RemoteMailbox, BackendError>> + Send;

    fn delete_mailbox(&self, id: &str) -> impl Future<Output = Result<(), BackendError>> + Send;

    fn set_mailbox_color(
        &self,
        id: &str,
        color: &LabelColor,
    ) -> impl Future<Output = Result<RemoteMailbox, BackendError>> + Send;

    /// How many conversations the mailbox holds on the server, not only in
    /// the part this computer keeps.
    fn mailbox_threads(&self, id: &str) -> impl Future<Output = Result<u64, BackendError>> + Send;

    /// One page of the sync window, newest first: the last `days` days of
    /// mail and everything in the inbox. `cursor` is the page before's
    /// `next`. Answers `BackendError::StateLost` when the server no longer
    /// takes that cursor, and the caller lists the window from the top.
    fn backfill(
        &self,
        days: i64,
        cursor: Option<&str>,
    ) -> impl Future<Output = Result<Backfill, BackendError>> + Send;

    /// Every message in the sync window, or the ones of it in `mailbox`,
    /// ids only.
    fn window_ids(
        &self,
        days: i64,
        mailbox: Option<&str>,
    ) -> impl Future<Output = Result<Vec<RemoteRef>, BackendError>> + Send;

    /// Every message in the inbox, whatever its age, ids only.
    fn inbox_ids(&self) -> impl Future<Output = Result<Vec<RemoteRef>, BackendError>> + Send;

    /// At most `limit` messages `query` matches, newest first.
    fn search(
        &self,
        query: &SearchQuery,
        limit: usize,
    ) -> impl Future<Output = Result<Vec<RemoteRef>, BackendError>> + Send;

    /// The sent message carrying the `Message-ID` header `message_id`, when
    /// the server holds one.
    fn find_sent(
        &self,
        message_id: &str,
    ) -> impl Future<Output = Result<Option<String>, BackendError>> + Send;

    /// Metadata for `wants` at the least cost the server allows. A message
    /// the server no longer has comes back among the gone, not as an error.
    fn fetch(&self, wants: Vec<Want>) -> impl Future<Output = Result<Found, BackendError>> + Send;

    /// Every message of each thread.
    fn fetch_whole(
        &self,
        threads: Vec<String>,
    ) -> impl Future<Output = Result<Found, BackendError>> + Send;

    /// The messages as they arrived, by the server's ids, in order.
    fn fetch_raw(
        &self,
        ids: &[String],
    ) -> impl Future<Output = Result<Vec<RawMessage>, BackendError>> + Send;

    /// Files a copy of `raw` in `mailbox`, for a server that does not file
    /// what it sends. Returns the copy's id.
    fn append(
        &self,
        raw: &[u8],
        mailbox: &str,
    ) -> impl Future<Output = Result<String, BackendError>> + Send;

    fn message_body(
        &self,
        id: &str,
    ) -> impl Future<Output = Result<MessageBody, BackendError>> + Send;

    /// The server's id for its mailbox with `role`, where it has one.
    fn mailbox_for(&self, role: Role) -> Option<String>;

    /// Applies `ops` to `messages`, in order. On a refusal it says how
    /// many messages from the front went through. `MailOp::Destroy`
    /// comes alone and answers `BackendError::NeedsPermission` until the
    /// account grants the delete permission.
    fn apply(
        &self,
        messages: &[String],
        ops: &[MailOp],
    ) -> impl Future<Output = Result<(), Unapplied>> + Send;

    /// Sends raw RFC 822 bytes. Returns the new message id.
    fn send(
        &self,
        raw: &[u8],
        thread_id: Option<&str>,
    ) -> impl Future<Output = Result<String, BackendError>> + Send;

    /// Creates a draft, or replaces draft `draft_id`.
    fn save_draft(
        &self,
        draft_id: Option<&str>,
        raw: &[u8],
        thread_id: Option<&str>,
    ) -> impl Future<Output = Result<SavedDraft, BackendError>> + Send;

    /// Sends a draft as the server holds it. Returns the sent message's id.
    fn send_draft(
        &self,
        draft_id: &str,
    ) -> impl Future<Output = Result<String, BackendError>> + Send;

    fn delete_draft(&self, draft_id: &str)
    -> impl Future<Output = Result<(), BackendError>> + Send;

    /// Every draft in the account, each with the message inside it.
    fn list_drafts(&self) -> impl Future<Output = Result<Vec<DraftRef>, BackendError>> + Send;

    fn attachment(
        &self,
        message_id: &str,
        attachment_id: &str,
    ) -> impl Future<Output = Result<Vec<u8>, BackendError>> + Send;
}

/// The account's calendar. Every call answers
/// `BackendError::NeedsPermission` until the account grants the calendar
/// permission.
pub trait CalendarService: Send + Sync + 'static {
    /// Answers the event `ical_uid` names as `me`, and lets the server
    /// tell the organizer. `occurrence` is the start of the one occurrence
    /// to answer; `None` answers the series.
    fn answer_invitation(
        &self,
        ical_uid: &str,
        me: &str,
        answer: Answer,
        occurrence: Option<EpochMillis>,
    ) -> impl Future<Output = Result<Answered, BackendError>> + Send;

    /// What the calendar already holds between `from` and `to`.
    fn busy_between(
        &self,
        from: EpochMillis,
        to: EpochMillis,
    ) -> impl Future<Output = Result<Vec<Busy>, BackendError>> + Send;

    /// How the repeating event `ical_uid` names repeats, with what is left
    /// of it from `from`. `None` when there is no such event or it does
    /// not repeat.
    fn series(
        &self,
        ical_uid: &str,
        from: EpochMillis,
    ) -> impl Future<Output = Result<Option<Series>, BackendError>> + Send;

    /// Every event on the primary calendar that overlaps `from` to `to`,
    /// in the order they start.
    fn events_between(
        &self,
        from: EpochMillis,
        to: EpochMillis,
    ) -> impl Future<Output = Result<Vec<Event>, BackendError>> + Send;

    /// Puts a new event on the primary calendar and invites its guests.
    fn create_event(
        &self,
        fields: &EventFields,
    ) -> impl Future<Output = Result<Event, BackendError>> + Send;

    /// Changes what `fields` sets on event `id` and tells its guests.
    fn update_event(
        &self,
        id: &str,
        fields: &EventFields,
    ) -> impl Future<Output = Result<Event, BackendError>> + Send;

    /// Takes event `id` off the primary calendar and tells its guests.
    fn delete_event(&self, id: &str) -> impl Future<Output = Result<(), BackendError>> + Send;
}

/// The account's address book.
pub trait ContactsService: Send + Sync + 'static {
    /// One page of the contacts. `sync_token` from the last read asks for
    /// changes alone. Answers `BackendError::NeedsPermission` until the
    /// account grants the contacts permission.
    fn connections(
        &self,
        page_token: Option<&str>,
        sync_token: Option<&str>,
    ) -> impl Future<Output = Result<ConnectionsPage, BackendError>> + Send;

    /// The bytes of one contact photo.
    fn contact_photo(
        &self,
        url: &str,
    ) -> impl Future<Output = Result<Vec<u8>, BackendError>> + Send;

    /// Adds a contact. Answers `BackendError::NeedsPermission` until the
    /// account grants the permission to change contacts.
    fn create_contact(
        &self,
        fields: &ContactFields,
    ) -> impl Future<Output = Result<Person, BackendError>> + Send;

    /// Changes the fields `fields` names on the contact `resource`.
    fn update_contact(
        &self,
        resource: &str,
        fields: &ContactFields,
    ) -> impl Future<Output = Result<Person, BackendError>> + Send;
}

/// The rules the server runs on arriving mail.
pub trait RulesService: Send + Sync + 'static {
    fn filters(&self) -> impl Future<Output = Result<Vec<Filter>, BackendError>> + Send;

    fn create_filter(
        &self,
        filter: &Filter,
    ) -> impl Future<Output = Result<Filter, BackendError>> + Send;

    fn delete_filter(&self, id: &str) -> impl Future<Output = Result<(), BackendError>> + Send;
}

/// The automatic reply the server sends while the person is away.
pub trait AutoReplyService: Send + Sync + 'static {
    fn vacation(&self) -> impl Future<Output = Result<Vacation, BackendError>> + Send;

    fn set_vacation(
        &self,
        vacation: &Vacation,
    ) -> impl Future<Output = Result<(), BackendError>> + Send;
}

/// The addresses the account sends as.
pub trait IdentityService: Send + Sync + 'static {
    /// Every address the account may send from, its own included, each
    /// with its display name and signature. An address the server would
    /// refuse to send from is left out.
    fn identities(&self) -> impl Future<Output = Result<Vec<SendAsAddress>, BackendError>> + Send;
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use mailrs_domain::Filter;

    use super::*;
    use crate::fake::FakeGmail;

    #[test]
    fn a_google_account_has_every_service() {
        let services = AccountServices::fake(Arc::new(FakeGmail::new()));
        assert!(services.calendar.is_some());
        assert!(services.contacts.is_some());
        assert!(services.auto_reply.is_some());
        assert_eq!(
            services.capabilities(),
            MailCapabilities {
                labels: true,
                server_threads: true,
                files_sent_mail: true,
                categories: true,
                delete_forever: true,
                batch_limit: 1000,
            }
        );
    }

    #[tokio::test]
    async fn every_service_reaches_the_one_mailbox() {
        let gmail = Arc::new(FakeGmail::new());
        let services = AccountServices::fake(Arc::clone(&gmail));
        services
            .rules
            .create_filter(&Filter::block("pest@example.com"))
            .await
            .unwrap();
        assert_eq!(gmail.with(|s| s.filters.len()), 1);
        let mailboxes = services.mail.mailboxes().await.unwrap();
        assert!(
            mailboxes
                .iter()
                .any(|m| m.id == "INBOX" && m.role == Some(mailrs_domain::Role::Inbox))
        );
        let identities = services.identities.identities().await.unwrap();
        assert_eq!(identities[0].email, "me@example.com");
        let calendar = services.calendar.as_ref().expect("Gmail has a calendar");
        assert!(calendar.events_between(0, 1).await.unwrap().is_empty());
    }
}
