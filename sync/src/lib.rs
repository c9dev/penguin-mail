//! Keeps the local store in step with Gmail, one loop per account.

mod account;
mod actions;
mod api;
mod backoff;
pub mod config;
mod connect;
mod engine;
mod error;
pub mod mailbox;
mod settings;
mod triage;

/// An in-memory Gmail. Sync's own tests always have it; anyone else asks
/// for the `fake` feature, as `penguin-mail` does for `--demo`.
#[cfg(any(test, feature = "fake"))]
pub mod fake;
#[cfg(test)]
mod tests;

pub use account::{AccountSync, DEFAULT_BODY_CACHE_BYTES, DEFAULT_WINDOW_DAYS, FETCH_CONCURRENCY};
pub use actions::{Accounts, Failure, History, MailAction, MailActions, Outcome};
#[cfg(any(test, feature = "fake"))]
pub use api::AnyGmail;
pub use api::{AccountClient, GmailApi, LIST_PAGE_SIZE, SavedDraft};
pub use backoff::backoff_delay;
pub use connect::connect_account;
pub use engine::{EngineConfig, SyncEngine};
pub use error::SyncError;
pub use mailbox::{
    Changed, Counts, Empty, Listing, Mailbox, Mailboxes, PAGE, Scope, View, summarize_search,
};
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
