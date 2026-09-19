//! Sync for one account: window loading, history replay, thread and body
//! fetches, and triage. Each file adds methods to `AccountSync`.

mod history;
mod labels;
mod outbox;
mod threads;
mod window;
mod writes;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use mailrs_domain::{AccountId, AccountState, ChangeEvent, MessageMeta};
use mailrs_gmail::GmailError;
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
}

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

    pub fn account_id(&self) -> AccountId {
        self.account_id
    }

    pub async fn set_state(&self, state: AccountState) -> Result<(), SyncError> {
        let account_id = self.account_id;
        self.db
            .write(move |c| accounts::set_state(c, account_id, state))
            .await?;
        self.emit(ChangeEvent::AccountStateChanged { account_id, state });
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

    /// Metadata for `ids`, with bounded concurrency. Messages deleted since
    /// they were listed are skipped.
    async fn fetch_metadata(&self, ids: &[String]) -> Result<Vec<MessageMeta>, SyncError> {
        let results: Vec<Result<MessageMeta, GmailError>> = futures::stream::iter(ids.to_vec())
            .map(|id| {
                let api = Arc::clone(&self.api);
                async move { api.message_metadata(&id).await }
            })
            .buffer_unordered(FETCH_CONCURRENCY)
            .collect()
            .await;
        let mut metas = Vec::with_capacity(results.len());
        for result in results {
            match result {
                Ok(meta) => metas.push(meta),
                Err(GmailError::NotFound) => {}
                Err(err) => return Err(err.into()),
            }
        }
        Ok(metas)
    }
}
