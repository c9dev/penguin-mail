//! Keeps the local store in step with Gmail, one loop per account.

mod account;
mod actions;
mod api;
mod backoff;
pub mod config;
mod connect;
pub mod contacts;
mod engine;
mod error;
pub mod export;
pub mod invitations;
pub mod mailbox;
pub mod outbox;
mod settings;
mod triage;

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
    AccountSync, DEFAULT_BODY_CACHE_BYTES, DEFAULT_WINDOW_DAYS, FETCH_CONCURRENCY, SendAsAddress,
};
pub use actions::{Accounts, Failure, History, MailAction, MailActions, Outcome};
#[cfg(any(test, feature = "fake"))]
pub use api::AnyGmail;
pub use api::{AccountClient, DraftRef, GmailApi, LIST_PAGE_SIZE, SavedDraft};
pub use backoff::{MOST_TRIES, backoff_delay, poll_offset, retry_delay, with_jitter};
pub use connect::connect_account;
pub use contacts::{Card, ContactBook, Refreshed};
pub use engine::{EngineConfig, SyncEngine};
pub use error::SyncError;
pub use invitations::{Change, Invitations, Opened, Sent, Told};
pub use mailbox::{
    Changed, Counts, Empty, Listing, Mailbox, Mailboxes, PAGE, Scope, View, outbox_id, outbox_row,
    summarize_search,
};
pub use outbox::{Drained, Outbox, Posted};
pub use settings::{
    AccountSettings, AutomaticReply, HIDE_MY_EMAIL_LABEL, HiddenFilters, Permitted,
};
pub use triage::TriageAction;

use mailrs_domain::EpochMillis;

/// Wall-clock time in Gmail's unit.
pub fn now_millis() -> EpochMillis {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as EpochMillis)
        .unwrap_or(0)
}
