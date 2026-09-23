//! Sync for one account: window loading, history replay, thread and body
//! fetches, and triage. Each file adds methods to `AccountSync`.

mod fetch;
mod history;
mod labels;
mod listed;
mod outbox;
mod threads;
mod window;
mod writes;
pub use writes::Relabelled;

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mailrs_domain::{AccountId, AccountState, ChangeEvent, EpochMillis, MessageMeta};
use mailrs_store::{Db, accounts};

use crate::{GmailApi, SyncError};

/// Metadata requests in flight per account.
pub const FETCH_CONCURRENCY: usize = 10;
pub const DEFAULT_WINDOW_DAYS: i64 = 30;
pub const DEFAULT_BODY_CACHE_BYTES: i64 = 1 << 30;

pub struct AccountSync<G> {
    account_id: AccountId,
    api: Arc<G>,
    db: Db,
    events: async_channel::Sender<ChangeEvent>,
    window_days: i64,
    body_cache_bytes: i64,
    retry_max: Duration,
    wait_ceiling: Duration,
    /// Cached bodies read since the last write of their access times.
    touched: Arc<Mutex<Vec<(String, EpochMillis)>>>,
    /// Body bytes stored since the last eviction pass, `None` before the
    /// first one.
    unswept: Mutex<Option<i64>>,
    /// When the last history replay left the store up to date. Opening a
    /// thread within [`FRESH_FOR`] of it trusts the store and asks Gmail
    /// nothing.
    caught_up: Arc<Mutex<Option<Instant>>>,
    /// Whole threads a Gmail search fetched, by thread id, kept so opening
    /// one stores it without asking Gmail again.
    listed: Mutex<std::collections::HashMap<String, listed::Listed>>,
    /// The messages each thread had among a search's hits, kept so Delete
    /// Forever on a row the store lacks knows what to erase.
    hits: Mutex<std::collections::HashMap<String, listed::Hits>>,
}

/// How long a finished history replay speaks for the whole mailbox. The
/// engine replays every 30 seconds, so this still covers one missed tick.
pub const FRESH_FOR: Duration = Duration::from_secs(75);

impl<G: GmailApi> AccountSync<G> {
    pub fn new(
        account_id: AccountId,
        api: Arc<G>,
        db: Db,
        events: async_channel::Sender<ChangeEvent>,
    ) -> Self {
        AccountSync {
            account_id,
            api,
            db,
            events,
            window_days: DEFAULT_WINDOW_DAYS,
            body_cache_bytes: DEFAULT_BODY_CACHE_BYTES,
            retry_max: Duration::from_secs(8),
            wait_ceiling: crate::WAIT_CEILING,
            touched: Arc::default(),
            unswept: Mutex::default(),
            caught_up: Arc::default(),
            listed: Mutex::default(),
            hits: Mutex::default(),
        }
    }

    pub fn with_limits(mut self, window_days: i64, body_cache_bytes: i64) -> Self {
        self.window_days = window_days;
        self.body_cache_bytes = body_cache_bytes;
        self
    }

    /// Caps the wait between retries of a triage write.
    pub fn with_retry_max(mut self, retry_max: Duration) -> Self {
        self.retry_max = retry_max;
        self
    }

    /// Caps how long one mail action waits on a busy Gmail in total before
    /// it stops and reports what did not go through.
    pub fn with_wait_ceiling(mut self, ceiling: Duration) -> Self {
        self.wait_ceiling = ceiling;
        self
    }

    /// Records that history replay left the store up to date.
    pub(crate) fn mark_caught_up(&self) {
        *self.caught_up.lock().expect("caught up") = Some(Instant::now());
    }

    pub fn account_id(&self) -> AccountId {
        self.account_id
    }

    /// Whether a user action is waiting on this account's Gmail budget.
    /// The engine reads it between backfill pages and gives way.
    pub(crate) fn foreground_waiting(&self) -> bool {
        self.api
            .quota()
            .is_some_and(|quota| quota.foreground_waiting())
    }

    /// Counts the caller as a user action waiting on Gmail until the guard
    /// drops, so backfill stands aside for the whole wait.
    pub(crate) fn waiting(&self) -> Option<mailrs_gmail::Waiting<'_>> {
        self.api.quota().map(|quota| quota.waiting())
    }

    pub async fn set_state(&self, state: AccountState) -> Result<(), SyncError> {
        let account_id = self.account_id;
        self.db
            .write(move |c| accounts::set_state(c, account_id, state))
            .await?;
        self.emit(ChangeEvent::AccountStateChanged { account_id, state });
        Ok(())
    }

    /// When the engine last pruned this account and checked its inbox.
    pub async fn checked_at(&self) -> Option<EpochMillis> {
        let account_id = self.account_id;
        self.db
            .read(move |c| accounts::checked_at(c, account_id))
            .await
            .unwrap_or_else(|err| {
                tracing::warn!(account = account_id, error = %err, "could not read the last inbox check");
                None
            })
    }

    pub async fn set_checked_at(&self, at: EpochMillis) -> Result<(), SyncError> {
        let account_id = self.account_id;
        self.db
            .write(move |c| accounts::set_checked_at(c, account_id, at))
            .await?;
        Ok(())
    }

    fn emit(&self, event: ChangeEvent) {
        // The channel is unbounded, so this only fails when nobody listens.
        let _ = self.events.try_send(event);
    }

    fn emit_threads(&self, thread_ids: BTreeSet<String>) {
        if !thread_ids.is_empty() {
            self.emit(ChangeEvent::ThreadsChanged {
                account_id: self.account_id,
                thread_ids: thread_ids.into_iter().collect(),
            });
        }
    }

    /// Metadata for `ids`, whose threads the caller does not know, a
    /// `messages.get` each. Messages deleted since they were listed are
    /// skipped.
    pub async fn fetch_metadata(&self, ids: &[String]) -> Result<Vec<MessageMeta>, SyncError> {
        let wants = ids.iter().map(fetch::Want::message).collect();
        Ok(self.fetch(wants).await?.metas)
    }
}
