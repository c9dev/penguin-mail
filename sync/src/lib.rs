//! Keeps the local store in step with Gmail, one loop per account.

mod account;
mod actions;
mod api;
mod backoff;
pub mod calendar;
pub mod calendar_copy;
pub mod calendar_reach;
pub mod config;
mod connect;
pub mod contacts;
mod engine;
mod error;
pub mod events;
pub mod export;
pub mod hidden;
pub mod invitations;
pub mod lock;
pub mod mailbox;
pub mod masked;
pub mod newsletters;
mod one_click;
mod ops;
pub mod outbox;
pub mod passwords;
pub mod raw_cache;
pub mod read_inbox;
pub mod rules;
pub mod services;
mod settings;
pub mod sign_in;
pub mod switch;
mod triage;
pub mod unsubscribe;

/// An in-memory Gmail. Sync's own tests always have it; anyone else asks
/// for the `fake` feature, as `penguin-mail` does for `--demo`.
#[cfg(any(test, feature = "fake"))]
pub mod fake;
#[cfg(test)]
mod tests;

/// How long one mail action waits on a Gmail that keeps saying it is busy
/// before it stops and reports what did not go through. Gmail's own
/// `Retry-After` runs to a second or two, so a minute covers a long run of
/// them; past that the user deserves to hear rather than keep waiting.
pub const WAIT_CEILING: std::time::Duration = std::time::Duration::from_secs(60);

/// The stack each thread of a runtime that runs `SyncEngine` gets. An
/// account's sync nests dozens of async functions without spawning, and a
/// debug build polling it against a real IMAP server overflowed Tokio's
/// 2 MiB default but ran in 4 MiB. The stack is reserved address space;
/// only the pages a thread touches take memory.
pub const WORKER_STACK: usize = 8 << 20;

pub use account::{
    ALL_MAIL, AccountSync, DEFAULT_BODY_CACHE_BYTES, DEFAULT_WINDOW_DAYS, Emptied,
    FETCH_CONCURRENCY, Searched, window_longer, window_reach,
};
pub use actions::{
    Accounts, Categorized, Failure, History, MailAction, MailActions, NewLabels, Outcome, Recorded,
    Returned, Undone,
};
pub use api::{AccountClient, DraftRef, GmailApi, SavedDraft};
pub use backoff::{MOST_TRIES, backoff_delay, poll_offset, retry_delay, with_jitter};
pub use calendar::{Added, Calendar};
pub use connect::{
    Clients, Connected, Connector, JmapGrant, JmapGrantRow, JmapSessions, KEYRING, Lacks, auth_of,
    connect_account, connect_caldav, connect_imap, connect_jmap, connect_microsoft,
    connect_microsoft_at, connect_pop3, jmap_grant, pop3_servers_for, server_of, servers_for,
    smtp_login,
};
pub use contacts::{Card, ContactBook, Refreshed};
pub use engine::{EngineConfig, SyncEngine};
pub use error::{BackendError, SyncError};
pub use hidden::HiddenAddress;
pub use invitations::{Change, Invitations, Opened, Sent, Spot, Told, Waiting};
pub use mailbox::{
    Changed, Counts, Empty, Listing, Loaded, Mailbox, Mailboxes, PAGE, Scope, Stop, View,
    outbox_id, outbox_row, summarize_search, waiting_line,
};
pub use newsletters::Newsletters;
pub use one_click::OneClick;
pub use ops::{MailOp, MovedFrom};
pub use outbox::{Cancelled, Drained, Outbox, OutboxHold, Posted, Reheld, Reopened, server_holds};
pub use services::{
    AccountServices, AnyAutoReply, AnyCalendar, AnyContacts, AnyDav, AnyGmail, AnyGraph,
    AnyIdentities, AnyImap, AnyJmap, AnyMail, AnyPop3, AnyRules, AnySieve, AnySmtp,
    AutoReplyService, Backfill, CalDav, CalendarService, CardDav, Changes, Claimed,
    ContactsService, Found, Google, GraphApi, Held, ID_PAGE_SIZE, IdentityService, Imap, ImapApi,
    ImapSettings, Jmap, JmapApi, JmapSettings, JmapSieve, KeywordsOf, KeywordsPage, LIST_PAGE_SIZE,
    LocalRules, Look, Looked, MailBackend, MailCapabilities, Microsoft, MicrosoftSettings, Missing,
    NoMail, Offers, Pop3, Pop3Settings, Priority, RAW_LIMIT, RawMessage, ReceiptOut, Refused,
    Relocated, RemoteChange, RemoteRef, RulesBeside, RulesPlace, RulesService, SERVER_REPLY_SCRIPT,
    SearchQuery, SendAsAddress, Settled, SieveRules, Storage, Submit, SyncState, Unapplied, Unheld,
    Want, Withheld, background, withheld_by_grant,
};
pub use settings::{
    AccountSettings, AutomaticReply, HIDE_MY_EMAIL_LABEL, Permitted, Replaced, RuleList, SentRules,
};
pub use triage::TriageAction;
pub use unsubscribe::{Leave, Unsubscribe};

use mailrs_domain::EpochMillis;

/// Wall-clock time in Gmail's unit.
pub fn now_millis() -> EpochMillis {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as EpochMillis)
        .unwrap_or(0)
}
