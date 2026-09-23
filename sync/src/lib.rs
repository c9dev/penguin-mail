//! Keeps the local store in step with Gmail, one loop per account.

mod account;
mod actions;
mod api;
mod backoff;
pub mod calendar;
pub mod config;
mod connect;
pub mod contacts;
mod engine;
mod error;
pub mod export;
pub mod hidden;
pub mod invitations;
pub mod lock;
pub mod mailbox;
pub mod newsletters;
pub mod outbox;
pub mod sign_in;
mod settings;
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

pub use account::{
    AccountSync, DEFAULT_BODY_CACHE_BYTES, DEFAULT_WINDOW_DAYS, FETCH_CONCURRENCY, Relabelled,
    SendAsAddress,
};
pub use actions::{
    Accounts, Categorized, Failure, History, MailAction, MailActions, NewLabels, Outcome, Returned,
    Undone,
};
#[cfg(any(test, feature = "fake"))]
pub use api::AnyGmail;
pub use api::{AccountClient, DraftRef, GmailApi, ID_PAGE_SIZE, LIST_PAGE_SIZE, SavedDraft};
pub use backoff::{MOST_TRIES, backoff_delay, poll_offset, retry_delay, with_jitter};
pub use calendar::Calendar;
pub use connect::connect_account;
pub use contacts::{Card, ContactBook, Refreshed};
pub use engine::{EngineConfig, SyncEngine};
pub use error::SyncError;
pub use hidden::HiddenAddress;
pub use invitations::{Change, Invitations, Opened, Sent, Told};
pub use mailbox::{
    Changed, Counts, Empty, Listing, Loaded, Mailbox, Mailboxes, PAGE, Scope, View, outbox_id,
    outbox_row, summarize_search, waiting_line,
};
pub use newsletters::Newsletters;
pub use outbox::{Cancelled, Drained, Outbox, Posted};
pub use settings::{AccountSettings, AutomaticReply, HIDE_MY_EMAIL_LABEL, Permitted};
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
