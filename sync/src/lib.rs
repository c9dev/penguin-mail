//! Keeps the local store in step with Gmail, one loop per account.

mod account;
mod api;
mod backoff;
mod error;
mod triage;

#[cfg(test)]
mod fake;
#[cfg(test)]
mod tests;

pub use account::{AccountSync, DEFAULT_BODY_CACHE_BYTES, DEFAULT_WINDOW_DAYS, FETCH_CONCURRENCY};
pub use api::{AccountClient, GmailApi, LIST_PAGE_SIZE};
pub use backoff::backoff_delay;
pub use error::SyncError;
pub use triage::TriageAction;

use mailrs_domain::EpochMillis;

/// Wall-clock time in Gmail's unit.
pub fn now_millis() -> EpochMillis {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as EpochMillis)
        .unwrap_or(0)
}
