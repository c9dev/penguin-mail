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
mod caldav;
mod carddav;
mod dav_read;
pub mod finding;
mod google;

pub use google::withheld as withheld_by_grant;
pub mod imap;
pub mod microsoft;
pub mod pop3;
pub(crate) mod local;
mod pacing;
mod sieve;

pub use caldav::CalDav;
pub use any::{AnyAutoReply, AnyCalendar, AnyContacts, AnyIdentities, AnyMail, AnyRules};
pub use carddav::CardDav;
pub use local::LocalRules;
pub use google::{Google, ID_PAGE_SIZE, LIST_PAGE_SIZE};
pub use imap::{Imap, ImapApi, ImapSettings, Submit};
pub use microsoft::{GraphApi, Microsoft, MicrosoftSettings};
pub use pop3::{Pop3, Pop3Settings};
pub use pacing::{Priority, background, priority};
pub use sieve::SieveRules;

/// Where an account's rules run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RulesPlace {
    /// On the mail server, whether this computer is on or not.
    Server,
    /// On this computer, while Penguin Mail is open.
    ThisComputer,
}

use std::collections::HashMap;
use std::ops::RangeInclusive;
use std::sync::Arc;
use std::time::Duration;

use mailrs_domain::calendar as model;
use mailrs_domain::invitation::Answer;
use mailrs_domain::query::Query;
use mailrs_domain::{
    AccountId, EpochMillis, Filter, Location, MailSet, Membership, MessageMeta, RemoteMailbox, Role, Vacation,
};
use mailrs_gmail::{
    Answered, Busy, ConnectionsPage, ContactFields, Event, EventFields, LabelColor, Person, Series,
};
use mailrs_imap::{ImapClient, SmtpClient, UidSet};
use mailrs_mime::Parts;
use mailrs_pop3::Pop3Client;
use mailrs_store::Db;
use mailrs_store::threading::Links;

use crate::api::{AccountClient, DraftRef, SavedDraft};
#[cfg(any(test, feature = "fake"))]
use crate::fake::{FakeGmail, FakeGraph, FakeImap, FakePop3, FakeSmtp};
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
    /// The keywords the server stores. Any other a message carries stays
    /// on this computer, marked local, and never syncs.
    pub keywords: &'static [&'static str],
    /// The server reads a search in its own syntax, as the person typed
    /// it, so `SearchQuery::Native` reaches it untouched.
    pub native_search: bool,
    /// A message takes a new name on the server when it moves, as an IMAP
    /// message's mailbox and UID do. The engine then keeps remote refs.
    pub renames: bool,
    /// The change feed hands over each changed message whole, as it stands
    /// now, and names new mail no differently from a change: Graph's delta
    /// queries. A message it names that the store lacks is new to the
    /// store, and the engine fetches it.
    pub restates: bool,
    /// The account keeps tags: server mailboxes that are marks beside the
    /// one folder a message sits in.
    pub tags: bool,
    /// The server sorts the inbox into Focused and Other.
    pub focus: bool,
    /// The account's mailboxes live only in the store, as a POP3 account's
    /// do. A move, a flag or a mailbox change is complete once the store
    /// has it, so the engine sends nothing, and nothing prunes or relists
    /// the account's mail.
    pub local_mailboxes: bool,
}

/// How far a write got before the server refused the rest: the first
/// `taken` messages went through. The caller waits, if the error is
/// worth waiting out, and sends the rest.
#[derive(Debug, Clone)]
pub struct Unapplied {
    pub taken: usize,
    pub error: BackendError,
    /// Where the server moved the messages it took before the refusal.
    pub relocated: Vec<Relocated>,
}

/// Where a write moved a message on a server that renames what it moves:
/// `from` is the name the caller handed in, `to` the message's place now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relocated {
    pub from: String,
    pub to: Location,
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
    /// The messages `mailbox` expunged, by UID under `uidvalidity`. The
    /// set is the server's and one range can name four billion UIDs, so
    /// the engine tests each stored message's UID with
    /// [`UidSet::contains`] and never walks the set.
    Vanished {
        mailbox: String,
        uidvalidity: u32,
        uids: UidSet,
    },
    /// Every message `mailbox` holds now, by UID under `uidvalidity`, from
    /// a server that names no message it expunged. A stored message
    /// located in the mailbox that the set lacks has gone, as has one
    /// located under another UIDVALIDITY.
    Holds {
        mailbox: String,
        uidvalidity: u32,
        uids: UidSet,
    },
    /// The server renumbered `mailbox`, as a new UIDVALIDITY says, and
    /// every name the store holds for its messages is void. The engine
    /// lists that mailbox again, whose UIDs now belong to `uidvalidity`,
    /// before the other changes apply; the rest of the account is as it
    /// was.
    StateLost {
        mailbox: String,
        uidvalidity: u32,
    },
    /// The messages of the window in `mailbox` with a UID in `uids`, under
    /// `uidvalidity`, whose flag changes the server cannot name, as one
    /// without CONDSTORE cannot. The engine reads their keywords from the
    /// server a window at a time through [`MailBackend::keywords_in`] and
    /// compares them with the store's, so a look holds one window's flags
    /// and hands on only what differs.
    CompareKeywords {
        mailbox: String,
        uidvalidity: u32,
        uids: RangeInclusive<u32>,
    },
}

/// The keywords one message carries on the server, by its UID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeywordsOf {
    pub uid: u32,
    pub keywords: Vec<String>,
}

/// One window of [`MailBackend::keywords_in`]'s answer: the messages it
/// covered, lowest UID first, and the UID the next window starts at.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeywordsPage {
    pub found: Vec<KeywordsOf>,
    /// The UIDs this page covered, which the caller compares; the next
    /// page starts above them.
    pub covered: Option<RangeInclusive<u32>>,
    /// The keywords this mailbox's own PERMANENTFLAGS store, read as it
    /// was selected. Unset (empty) when nothing was covered.
    pub storable: &'static [&'static str],
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
    /// The messages each message's `In-Reply-To` and `References` name,
    /// by message id, from a server that keeps no threads. The engine
    /// threads the message from them.
    pub links: HashMap<String, Links>,
    /// Where each message sits on the server, by message id, from a
    /// server whose name for a message changes when it moves.
    pub located: HashMap<String, Location>,
}

/// One page of the sync window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backfill {
    pub refs: Vec<RemoteRef>,
    /// The cursor for the next page; `None` after the last.
    pub next: Option<String>,
}

/// A message under this many bytes is fetched raw and read whole; one at
/// or over it, or of unknown size, is fetched as its structure and its
/// text parts, and its files come one at a time when opened. A raw
/// message carries every file inside it, so without the line a 20 MB
/// attachment would download before the text showed.
pub const RAW_LIMIT: i64 = 2 * 1024 * 1024;

/// A message as it arrived, in RFC 822 form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawMessage {
    pub id: String,
    pub bytes: Vec<u8>,
}

/// A search. A backend that speaks the syntax takes what the person typed
/// as they typed it, which is how every Gmail operator keeps working.
/// Folders and smart mailboxes search with a tree, which each backend
/// prints in its own syntax.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchQuery {
    Native(String),
    Tree(Query),
}

/// The services an organization refused a Microsoft account this run,
/// learned from Graph's answers. The account offers each one it refused
/// as missing, with a reason naming the organization.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Refused {
    pub calendar: bool,
    pub contacts: bool,
    pub rules: bool,
    pub auto_reply: bool,
}

/// The services one account is served by. A provider that lacks one leaves
/// its field `None`, and the module that needs it answers
/// `BackendError::Unsupported`.
#[derive(Clone)]
pub struct AccountServices {
    pub mail: AnyMail,
    pub calendar: Option<AnyCalendar>,
    pub contacts: Option<AnyContacts>,
    pub rules: Option<AnyRules>,
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
            rules: Some(AnyRules::Google(google.clone())),
            auto_reply: Some(AnyAutoReply::Google(google.clone())),
            identities: AnyIdentities::Google(google),
        }
    }

    /// An account on an IMAP server: its mail and the address it sends
    /// from. `connect_imap` adds the calendar, contacts, rules and
    /// automatic reply the account's servers offer.
    pub fn imap(imap: ImapClient, smtp: SmtpClient, settings: ImapSettings) -> Self {
        let adapter = Imap::new(Arc::new(imap), Arc::new(smtp), settings);
        AccountServices {
            mail: AnyMail::Imap(adapter.clone()),
            calendar: None,
            contacts: None,
            rules: None,
            auto_reply: None,
            identities: AnyIdentities::Imap(adapter),
        }
    }

    pub fn with_calendar(mut self, calendar: AnyCalendar) -> Self {
        self.calendar = Some(calendar);
        self
    }

    pub fn with_contacts(mut self, contacts: AnyContacts) -> Self {
        self.contacts = Some(contacts);
        self
    }

    pub fn with_rules(mut self, rules: AnyRules) -> Self {
        self.rules = Some(rules);
        self
    }

    pub fn with_auto_reply(mut self, auto_reply: AnyAutoReply) -> Self {
        self.auto_reply = Some(auto_reply);
        self
    }

    /// The IMAP adapter an account's mail runs on, for a service that
    /// names its folders, such as Sieve rules.
    pub fn imap_adapter(&self) -> Option<Imap<ImapClient, SmtpClient>> {
        match &self.mail {
            AnyMail::Imap(adapter) => Some(adapter.clone()),
            _ => None,
        }
    }

    #[cfg(any(test, feature = "fake"))]
    pub fn fake_imap_adapter(&self) -> Option<Imap<FakeImap, FakeSmtp>> {
        match &self.mail {
            AnyMail::FakeImap(adapter) => Some(adapter.clone()),
            _ => None,
        }
    }

    /// A Microsoft account: every service over the one Graph client.
    pub fn microsoft(graph: mailrs_graph::Graph, settings: MicrosoftSettings) -> Self {
        let adapter = Microsoft::new(Arc::new(graph), settings);
        AccountServices {
            mail: AnyMail::Microsoft(adapter.clone()),
            calendar: Some(AnyCalendar::Microsoft(adapter.clone())),
            contacts: Some(AnyContacts::Microsoft(adapter.clone())),
            rules: Some(AnyRules::Microsoft(adapter.clone())),
            auto_reply: Some(AnyAutoReply::Microsoft(adapter.clone())),
            identities: AnyIdentities::Microsoft(adapter),
        }
    }

    /// The in-memory Graph, for tests and the demo, as an Outlook account.
    #[cfg(any(test, feature = "fake"))]
    pub fn fake_microsoft(fake: Arc<FakeGraph>) -> Self {
        let settings = MicrosoftSettings {
            address: "me@outlook.com".into(),
            provider_name: "Outlook".into(),
            window_days: crate::DEFAULT_WINDOW_DAYS,
        };
        AccountServices::fake_microsoft_with(fake, settings)
    }

    /// The in-memory Graph with `settings`.
    #[cfg(any(test, feature = "fake"))]
    pub fn fake_microsoft_with(fake: Arc<FakeGraph>, settings: MicrosoftSettings) -> Self {
        let adapter = Microsoft::new(fake, settings);
        AccountServices {
            mail: AnyMail::FakeMicrosoft(adapter.clone()),
            calendar: Some(AnyCalendar::FakeMicrosoft(adapter.clone())),
            contacts: Some(AnyContacts::FakeMicrosoft(adapter.clone())),
            rules: Some(AnyRules::FakeMicrosoft(adapter.clone())),
            auto_reply: Some(AnyAutoReply::FakeMicrosoft(adapter.clone())),
            identities: AnyIdentities::FakeMicrosoft(adapter),
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
            rules: Some(AnyRules::Fake(google.clone())),
            auto_reply: Some(AnyAutoReply::Fake(google.clone())),
            identities: AnyIdentities::Fake(google),
        }
    }

    /// The in-memory Gmail with other capabilities, for tests of an
    /// account whose server does less than Gmail.
    #[cfg(any(test, feature = "fake"))]
    pub fn fake_with_capabilities(gmail: Arc<FakeGmail>, caps: MailCapabilities) -> Self {
        let mut services = AccountServices::fake(Arc::clone(&gmail));
        services.mail = AnyMail::Fake(Google::new(gmail).with_capabilities(caps));
        services
    }

    /// The in-memory IMAP server, as a Fastmail account that files no copy
    /// of what it sends, for tests and the demo.
    #[cfg(any(test, feature = "fake"))]
    pub fn fake_imap(imap: Arc<FakeImap>, smtp: Arc<FakeSmtp>) -> Self {
        let settings = ImapSettings {
            address: "me@example.com".into(),
            provider_name: "Fastmail".into(),
            files_sent_mail: false,
            window_days: crate::DEFAULT_WINDOW_DAYS,
        };
        AccountServices::fake_imap_with(imap, smtp, settings)
    }

    /// The in-memory IMAP server with `settings`.
    #[cfg(any(test, feature = "fake"))]
    pub fn fake_imap_with(imap: Arc<FakeImap>, smtp: Arc<FakeSmtp>, settings: ImapSettings) -> Self {
        let adapter = Imap::new(imap, smtp, settings);
        AccountServices {
            mail: AnyMail::FakeImap(adapter.clone()),
            calendar: None,
            contacts: None,
            rules: None,
            auto_reply: None,
            identities: AnyIdentities::FakeImap(adapter),
        }
    }

    /// A POP3 account: its mail and address, over the store, with rules
    /// kept on this computer. `connect_pop3` adds the calendar and
    /// contacts found for it.
    pub fn pop3(
        db: Db,
        account_id: AccountId,
        client: Pop3Client,
        smtp: SmtpClient,
        settings: Pop3Settings,
    ) -> Self {
        let adapter = Pop3::new(db.clone(), account_id, Arc::new(smtp), Arc::new(client), settings);
        AccountServices {
            mail: AnyMail::Pop3(adapter.clone()),
            calendar: None,
            contacts: None,
            rules: Some(AnyRules::Local(LocalRules::new(db, account_id))),
            auto_reply: None,
            identities: AnyIdentities::Pop3(adapter),
        }
    }

    /// The POP3 adapter an account's mail runs on, for its CalDAV replies
    /// and its downloader.
    pub fn pop3_adapter(&self) -> Option<Pop3<SmtpClient, Pop3Client>> {
        match &self.mail {
            AnyMail::Pop3(adapter) => Some(adapter.clone()),
            _ => None,
        }
    }

    /// A POP3 account over the in-memory POP3 server and SMTP sink.
    #[cfg(any(test, feature = "fake"))]
    pub fn fake_pop3(
        db: Db,
        account_id: AccountId,
        client: Arc<FakePop3>,
        smtp: Arc<FakeSmtp>,
        settings: Pop3Settings,
    ) -> Self {
        let adapter = Pop3::new(db.clone(), account_id, smtp, client, settings);
        AccountServices {
            mail: AnyMail::FakePop3(adapter.clone()),
            calendar: None,
            contacts: None,
            rules: Some(AnyRules::Local(LocalRules::new(db, account_id))),
            auto_reply: None,
            identities: AnyIdentities::FakePop3(adapter),
        }
    }

    #[cfg(any(test, feature = "fake"))]
    pub fn fake_pop3_adapter(&self) -> Option<Pop3<FakeSmtp, FakePop3>> {
        match &self.mail {
            AnyMail::FakePop3(adapter) => Some(adapter.clone()),
            _ => None,
        }
    }

    pub fn capabilities(&self) -> MailCapabilities {
        self.mail.capabilities()
    }

    /// What the account's own consent leaves off, read from its mail
    /// adapter: an IMAP account withholds nothing, since it grants
    /// nothing to Google to begin with.
    pub fn withheld(&self) -> Withheld {
        self.mail.withheld()
    }

    pub fn offers(&self) -> Offers {
        let caps = self.capabilities();
        let features = CalendarFeatures::of(self.calendar.as_ref());
        let refused = self.mail.refused();
        Offers {
            labels: caps.labels,
            categories: caps.categories,
            delete_forever: caps.delete_forever,
            calendar: self.calendar.is_some() && !refused.calendar,
            contacts: self.contacts.is_some() && !refused.contacts,
            rules: self.rules.is_some() && !refused.rules,
            auto_reply: self.auto_reply.is_some() && !refused.auto_reply,
            // Gmail, IMAP and Graph search on the server. A POP3 account's
            // server keeps nothing to search; its mail is all in the store.
            search: !caps.local_mailboxes,
            tags: caps.tags,
            focused: caps.focus,
            event_files: features.event_files,
            moves_events: features.moves_events,
            calendar_list: features.calendar_list,
            quiet_changes: features.quiet_changes,
        }
    }
}

/// What a calendar adapter can do beyond reading and writing events.
/// Gated here, once, so no adapter is asked for a write it can never do:
/// an `Unsupported` answer would keep its change in the queue and every
/// later calendar change of the account would wait behind it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CalendarFeatures {
    event_files: bool,
    moves_events: bool,
    calendar_list: bool,
    /// A change can go without mailing the guests. Graph mails them on
    /// every change an organizer makes and has no switch against it.
    quiet_changes: bool,
}

impl CalendarFeatures {
    const NONE: CalendarFeatures =
        CalendarFeatures { event_files: false, moves_events: false, calendar_list: false, quiet_changes: false };
    const MICROSOFT: CalendarFeatures =
        CalendarFeatures { event_files: false, moves_events: false, calendar_list: true, quiet_changes: false };
    const GOOGLE: CalendarFeatures =
        CalendarFeatures { event_files: true, moves_events: true, calendar_list: true, quiet_changes: true };
    /// CalDAV keeps no files, moves an event by copying its resource, which
    /// the queue must not rely on, and lists the calendars its server
    /// holds without letting the app edit the list.
    const CALDAV: CalendarFeatures =
        CalendarFeatures { event_files: false, moves_events: false, calendar_list: false, quiet_changes: true };

    /// The match has no wildcard, so an adapter added to `AnyCalendar`
    /// has to say what it can do.
    fn of(calendar: Option<&AnyCalendar>) -> CalendarFeatures {
        match calendar {
            None => CalendarFeatures::NONE,
            Some(AnyCalendar::Google(_)) => CalendarFeatures::GOOGLE,
            #[cfg(any(test, feature = "fake"))]
            Some(AnyCalendar::Fake(_)) => CalendarFeatures::GOOGLE,
            // Graph holds no files for an event and keeps each event in
            // its calendar, but it lists, makes and changes calendars.
            Some(AnyCalendar::Microsoft(_)) => CalendarFeatures::MICROSOFT,
            #[cfg(any(test, feature = "fake"))]
            Some(AnyCalendar::FakeMicrosoft(_)) => CalendarFeatures::MICROSOFT,
            Some(AnyCalendar::Dav(_)) => CalendarFeatures::CALDAV,
            #[cfg(any(test, feature = "fake"))]
            Some(AnyCalendar::FakeDav(_)) => CalendarFeatures::CALDAV,
            Some(AnyCalendar::Pop3Dav(_)) => CalendarFeatures::CALDAV,
            #[cfg(any(test, feature = "fake"))]
            Some(AnyCalendar::FakePop3Dav(_)) => CalendarFeatures::CALDAV,
        }
    }
}

/// What the person left unticked on Google's consent screen, once per
/// scope Penguin Mail asks for. `true` means the account never granted
/// it, so the feature it serves turns off with a reason rather than
/// failing as an error. `NONE` fits an account whose grants are not
/// known yet, so nothing turns off before the first read says otherwise.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Withheld {
    pub settings: bool,
    pub delete: bool,
    pub contacts: bool,
    pub change_contacts: bool,
    pub calendar: bool,
    pub calendar_list: bool,
    /// Making, renaming and deleting the account's own calendars.
    pub calendars: bool,
    /// Changing the account's calendar list: a calendar's colour and
    /// whether it is hidden on every device, and subscribing to one.
    pub change_calendar_list: bool,
    /// Putting files in the account's Drive, which attaching a file from
    /// this computer to an event needs. Reading attachments does not.
    pub drive: bool,
}

impl Withheld {
    pub const NONE: Withheld = Withheld {
        settings: false,
        delete: false,
        contacts: false,
        change_contacts: false,
        calendar: false,
        calendar_list: false,
        calendars: false,
        change_calendar_list: false,
        drive: false,
    };

    pub fn is_empty(self) -> bool {
        self == Withheld::NONE
    }
}

/// What one account can do, read once from its services: the mail
/// capabilities the window shows or hides something for, and which of the
/// optional services it has. The window and the assistant read this, not
/// the services themselves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Offers {
    /// Mail can sit in several mailboxes; otherwise it moves between
    /// folders.
    pub labels: bool,
    pub categories: bool,
    pub delete_forever: bool,
    pub calendar: bool,
    pub contacts: bool,
    pub rules: bool,
    pub auto_reply: bool,
    /// The server searches past the mail kept on this computer. False for
    /// an account whose mailboxes are local, whose whole mail the store
    /// holds.
    pub search: bool,
    /// The account keeps tags beside its folders, so the window offers
    /// Tags… beside Move to Folder.
    pub tags: bool,
    /// The inbox splits into Focused and Other.
    pub focused: bool,
    /// An event can carry files the person attaches from this computer.
    pub event_files: bool,
    /// An event can move to another calendar of the account.
    pub moves_events: bool,
    /// The calendar list can change: a new calendar, a rename, a colour,
    /// a subscription, removing one.
    pub calendar_list: bool,
    /// A change to an event can go without mailing its guests, so the
    /// window may offer to send none. Outlook mails them on every change.
    pub quiet_changes: bool,
}

impl Offers {
    /// Everything Gmail offers (it has neither tags nor Focused), and as the window assumes of an account
    /// that has not started yet, so nothing disappears for a moment.
    pub const EVERYTHING: Offers = Offers {
        labels: true,
        categories: true,
        delete_forever: true,
        calendar: true,
        contacts: true,
        rules: true,
        auto_reply: true,
        search: true,
        tags: false,
        focused: false,
        event_files: true,
        moves_events: true,
        calendar_list: true,
        quiet_changes: true,
    };

    /// What the account lacks, in the order Preferences lists it.
    pub fn missing(&self) -> Vec<Missing> {
        [
            (self.calendar, Missing::Calendar),
            (self.contacts, Missing::Contacts),
            (self.rules, Missing::Rules),
            (self.auto_reply, Missing::AutoReply),
            (self.delete_forever, Missing::DeleteForever),
            (self.categories, Missing::Categories),
        ]
        .into_iter()
        .filter(|(has, _)| !has)
        .map(|(_, missing)| missing)
        .collect()
    }
}

/// One thing an account cannot do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Missing {
    Calendar,
    Contacts,
    Rules,
    AutoReply,
    DeleteForever,
    Categories,
}

/// Listing, reading, changing and sending mail.
pub trait MailBackend: Send + Sync + 'static {
    fn capabilities(&self) -> MailCapabilities;

    /// Who runs the account's server, in the words a person reads: a
    /// brand such as "Gmail", or an IMAP account's own provider name.
    fn provider_name(&self) -> &str;

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

    /// Starts keeping `mailbox` in step, for a mailbox the account does
    /// not sync on its own. A server that keeps every mailbox in step
    /// ignores it and answers `false`. Answers whether this is the first
    /// time anyone asked to follow `mailbox`, so a caller that already
    /// asked once does no work a second time.
    fn follow(&self, mailbox: &str) -> bool;

    /// Tells the backend whether the main window is open, which sets how
    /// often it looks at mail nobody is watching.
    fn set_window_open(&self, open: bool);

    /// How long the engine waits between looks at the change feed. `None`
    /// keeps the engine's own interval.
    fn poll_interval(&self) -> Option<Duration>;

    /// Resolves when the account should look at its Inbox: the server said
    /// something changed there, or its watch failed and a minute passed.
    /// A server that never says so never resolves this.
    fn watch(&self) -> impl Future<Output = ()> + Send;

    /// The keywords of the window's messages in `mailbox` with a UID in
    /// `uids`, as they stand on the server now, for a
    /// [`RemoteChange::CompareKeywords`]. It answers the lowest part of
    /// `uids` the server takes in one go; the caller asks again above
    /// [`KeywordsPage::covered`]. An empty page with nothing covered means
    /// the mailbox's UIDs no longer belong to `uidvalidity`, or `uids`
    /// was empty. A server that names its flag changes never asks for it.
    fn keywords_in(
        &self,
        mailbox: &str,
        uidvalidity: u32,
        uids: RangeInclusive<u32>,
    ) -> impl Future<Output = Result<KeywordsPage, BackendError>> + Send;

    /// The UIDVALIDITY the UIDs of `mailbox` belong to now, on a server
    /// that names a message by mailbox and UID; `None` on one that names
    /// it by an id of its own, as Gmail does.
    fn uidvalidity(
        &self,
        mailbox: &str,
    ) -> impl Future<Output = Result<Option<u32>, BackendError>> + Send;

    /// The keywords the server stores on messages in `mailbox`. On IMAP
    /// each mailbox's PERMANENTFLAGS decide, read with a SELECT the first
    /// time this session asks; a server that stores the same everywhere
    /// answers [`MailCapabilities::keywords`].
    fn keywords_stored(
        &self,
        mailbox: &str,
    ) -> impl Future<Output = Result<&'static [&'static str], BackendError>> + Send;

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

    /// The message's parts with the bytes of the text parts its body
    /// needs, and without its files: for a message too large to fetch
    /// raw.
    fn fetch_structure(&self, id: &str)
    -> impl Future<Output = Result<Parts, BackendError>> + Send;

    /// One part of a message, by part path, without the rest of it.
    fn fetch_part(
        &self,
        id: &str,
        path: &str,
    ) -> impl Future<Output = Result<Vec<u8>, BackendError>> + Send;

    /// The server's id for its mailbox with `role`, where it has one.
    fn mailbox_for(&self, role: Role) -> Option<String>;

    /// The mail set the mailbox `id` stands for. A server that lists a
    /// flag, unread mail or a category among its mailboxes, as Gmail lists
    /// `STARRED`, `UNREAD` and `CATEGORY_SOCIAL`, says so here; any other
    /// mailbox is itself, or the mailbox with its role.
    fn set_of(&self, id: &str) -> MailSet;

    /// Applies `ops` to `messages`, in order. On a refusal it says how
    /// many messages from the front went through. `MailOp::Destroy`
    /// comes alone and answers `BackendError::NeedsPermission` until the
    /// account grants the delete permission. Answers where the server put
    /// each message it moved, on a server that renames what it moves.
    fn apply(
        &self,
        messages: &[String],
        ops: &[MailOp],
    ) -> impl Future<Output = Result<Vec<Relocated>, Unapplied>> + Send;

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
}

/// The account's calendar. Every call answers
/// `BackendError::NeedsPermission` until the account grants the calendar
/// permission.
pub trait CalendarService: Send + Sync + 'static {
    /// Answers the event `ical_uid` names as `me`, and lets the server
    /// tell the organizer. `occurrence` is the start of the one occurrence
    /// to answer; `None` answers the series. `note` goes to the organizer
    /// with the answer.
    fn answer_invitation(
        &self,
        ical_uid: &str,
        me: &str,
        answer: Answer,
        occurrence: Option<EpochMillis>,
        note: Option<&str>,
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

    /// Every calendar on the account, for the local copy.
    fn calendars(&self) -> impl Future<Output = Result<Vec<model::Calendar>, BackendError>> + Send;

    /// One page of changes to `calendar` since `token`, or of the whole
    /// calendar from `from` without one.
    fn event_changes(
        &self,
        calendar: &str,
        token: Option<&str>,
        page: Option<&str>,
        from: EpochMillis,
    ) -> impl Future<Output = Result<model::EventPage, BackendError>> + Send;

    /// One page of the events of `calendar` that overlap `from` to `to`,
    /// for a range older than the copy reaches. It is a read apart from
    /// the sync token and leaves the token alone.
    fn event_range(
        &self,
        calendar: &str,
        from: EpochMillis,
        to: EpochMillis,
        page: Option<&str>,
    ) -> impl Future<Output = Result<model::EventPage, BackendError>> + Send;

    /// Creates `event` under its own id when `create`, or changes it to
    /// match, and mails its guests when `notify` says so. `etag` refuses
    /// the write with `BackendError::Changed` when the event moved on
    /// since.
    fn put_event(
        &self,
        event: &model::Event,
        etag: Option<&str>,
        create: bool,
        notify: model::Notify,
    ) -> impl Future<Output = Result<model::Event, BackendError>> + Send;

    /// Deletes an event on `calendar`, and mails its guests when `notify`
    /// says so.
    fn remove_event(
        &self,
        calendar: &str,
        id: &str,
        etag: Option<&str>,
        notify: model::Notify,
    ) -> impl Future<Output = Result<(), BackendError>> + Send;

    /// Imports `event` into its calendar under its iCalendar UID, for a
    /// file the person chose to keep, and invites nobody. Importing a UID
    /// the calendar already holds updates that event rather than making a
    /// second one. Answers the event as the provider now holds it.
    fn import_event(
        &self,
        event: &model::Event,
    ) -> impl Future<Output = Result<model::Event, BackendError>> + Send;

    /// Uploads a file waiting on this computer (`file.waiting` names its
    /// path) to the provider's file store, and answers it as an
    /// attachment an event can link to. `sent` counts the bytes as they go
    /// out. A file no longer at its path answers
    /// `BackendError::FileMissing`.
    fn upload_attachment(
        &self,
        file: &model::Attachment,
        sent: std::sync::Arc<std::sync::atomic::AtomicU64>,
    ) -> impl Future<Output = Result<model::Attachment, BackendError>> + Send;

    /// Lets `email` open the file `file_id` the app uploaded, without the
    /// provider mailing them.
    fn share_file(&self, file_id: &str, email: &str) -> impl Future<Output = Result<(), BackendError>> + Send;

    /// Moves `event` from its calendar to `destination` on the same
    /// account, a series with its changed occurrences, and mails its
    /// guests when `notify` says so. Answers the event on `destination`.
    fn move_event(
        &self,
        event: &model::Event,
        destination: &str,
        notify: model::Notify,
    ) -> impl Future<Output = Result<model::Event, BackendError>> + Send;

    /// Answers event `id` on `calendar` as the guest `me`: a series'
    /// own id answers every occurrence, an occurrence's id that one
    /// alone. `note` goes with the answer for the organizer to read;
    /// `None` leaves an earlier one as it was. Answers the event as the
    /// server now holds it.
    fn answer_event(
        &self,
        calendar: &str,
        id: &str,
        me: &str,
        answer: Answer,
        note: Option<&str>,
    ) -> impl Future<Output = Result<model::Event, BackendError>> + Send;

    /// Makes `edit` true of the calendar `calendar` on the account's
    /// list. An edit that puts a calendar on the list answers it under
    /// the server's own id; the others answer `None`, or the calendar as
    /// the server now lists it.
    fn edit_list(
        &self,
        calendar: &str,
        edit: &model::list::ListEdit,
    ) -> impl Future<Output = Result<Option<model::Calendar>, BackendError>> + Send;
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

    /// Replaces rule `old_id` with `new`, in place so it keeps its turn.
    /// An adapter that cannot answers `Unsupported`, and the caller
    /// creates the new rule and deletes the old one instead.
    fn replace_filter(
        &self,
        old_id: &str,
        new: &Filter,
    ) -> impl Future<Output = Result<Filter, BackendError>> + Send {
        let _ = (old_id, new);
        async { Err(BackendError::Unsupported) }
    }

    /// Lets the next write replace rules the person wrote elsewhere. Only
    /// a ManageSieve account holds such rules; the default does nothing.
    fn take_over(&self) -> impl Future<Output = ()> + Send {
        async {}
    }

    /// Whether a change made while the server is down waits in the rule
    /// queue and goes out when it answers. Only ManageSieve does this.
    fn queues_offline(&self) -> bool {
        false
    }
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
                keywords: &["$seen", "$flagged", "$muted"],
                native_search: true,
                batch_limit: 1000,
                renames: false,
                restates: false,
                tags: false,
                focus: false,
                local_mailboxes: false,
            }
        );
    }

    /// A rules adapter that says nothing about replacing or taking over
    /// answers `Unsupported` to the first and does nothing for the second.
    #[tokio::test]
    async fn rules_replace_and_take_over_have_defaults() {
        struct Bare;
        impl RulesService for Bare {
            async fn filters(&self) -> Result<Vec<Filter>, BackendError> {
                Ok(Vec::new())
            }
            async fn create_filter(&self, filter: &Filter) -> Result<Filter, BackendError> {
                Ok(filter.clone())
            }
            async fn delete_filter(&self, _id: &str) -> Result<(), BackendError> {
                Ok(())
            }
        }
        let replaced = Bare.replace_filter("a", &Filter::default()).await;
        assert!(matches!(replaced, Err(BackendError::Unsupported)));
        Bare.take_over().await;
    }

    #[test]
    fn gmail_offers_everything() {
        let services = AccountServices::fake(Arc::new(FakeGmail::new()));
        assert_eq!(services.offers(), Offers::EVERYTHING);
        assert!(services.offers().missing().is_empty());
    }

    #[test]
    fn offers_read_tags_and_focus_from_the_capabilities() {
        let gmail = Arc::new(FakeGmail::new());
        let caps = MailCapabilities {
            labels: false,
            tags: true,
            focus: true,
            local_mailboxes: false,
            ..AccountServices::fake(Arc::clone(&gmail)).capabilities()
        };
        let offers = AccountServices::fake_with_capabilities(gmail, caps).offers();
        assert!(offers.tags && offers.focused && !offers.labels);
        assert_eq!((Offers::EVERYTHING.tags, Offers::EVERYTHING.focused), (false, false));
    }

    #[test]
    fn gmail_offers_all_three_calendar_gates_and_imap_none() {
        let gmail = AccountServices::fake(Arc::new(FakeGmail::new())).offers();
        assert!(gmail.event_files && gmail.moves_events && gmail.calendar_list && gmail.quiet_changes);
        let imap = AccountServices::fake_imap(Arc::new(FakeImap::new()), Arc::new(FakeSmtp::default())).offers();
        assert!(!imap.event_files && !imap.moves_events && !imap.calendar_list && !imap.quiet_changes);
    }

    #[test]
    fn an_account_says_what_it_lacks() {
        let mut services = AccountServices::fake(Arc::new(FakeGmail::new()));
        services.calendar = None;
        services.rules = None;
        assert_eq!(services.offers().missing(), [Missing::Calendar, Missing::Rules]);
    }

    #[test]
    fn an_account_searches_on_its_server_unless_its_mailboxes_are_local() {
        let gmail = Arc::new(FakeGmail::new());
        assert!(AccountServices::fake(Arc::clone(&gmail)).offers().search);
        let imap = AccountServices::fake_imap(
            Arc::new(crate::fake::FakeImap::new()),
            Arc::new(crate::fake::FakeSmtp::default()),
        );
        assert!(imap.offers().search, "IMAP answers native_search false and still searches on the server");
        let caps = MailCapabilities {
            local_mailboxes: true,
            ..AccountServices::fake(Arc::clone(&gmail)).capabilities()
        };
        assert!(!AccountServices::fake_with_capabilities(gmail, caps).offers().search);
    }

    #[tokio::test]
    async fn every_service_reaches_the_one_mailbox() {
        let gmail = Arc::new(FakeGmail::new());
        let services = AccountServices::fake(Arc::clone(&gmail));
        let rules = services.rules.as_ref().expect("Gmail has rules");
        rules
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
