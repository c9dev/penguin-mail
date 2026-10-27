//! Sync for one account: window loading, history replay, thread and body
//! fetches, and triage. Each file adds methods to `AccountSync`.

mod fetch;
mod held;
mod history;
mod labels;
mod listed;
mod muted;
mod outbox;
mod prefetch;
mod receipts;
mod snapshot;
mod space;
mod store_mail;
mod threads;
mod trash;
mod unread_inbox;
mod window;
mod writes;

pub(crate) use outbox::message_id_header;
pub(crate) use snapshot::Snapshot;
pub(crate) use space::quiet;

pub use listed::Searched;
pub(crate) use store_mail::StoreMail;
pub use trash::Emptied;

use std::collections::{BTreeSet, HashSet};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use mailrs_domain::{AccountId, AccountState, ChangeEvent, EpochMillis, MessageMeta};
use mailrs_store::{Db, accounts, messages};

use crate::raw_cache::{RAW_CACHE_BYTES, RawCache};
use crate::{AccountServices, BackendError, RAW_LIMIT, SyncError};

/// Metadata requests in flight per account.
pub const FETCH_CONCURRENCY: usize = 10;
pub const DEFAULT_WINDOW_DAYS: i64 = 30;
pub const DEFAULT_BODY_CACHE_BYTES: i64 = 1 << 30;

/// The window that keeps all mail, whatever its age. Any count of days of
/// 0 or less reads the same way.
pub const ALL_MAIL: i64 = 0;

/// How many days back a window of `days` reaches: `None` for all mail.
pub fn window_reach(days: i64) -> Option<i64> {
    (days > 0).then_some(days)
}

/// Whether a window of `days` reaches further back than one of `than`.
pub fn window_longer(days: i64, than: i64) -> bool {
    match (window_reach(days), window_reach(than)) {
        (None, Some(_)) => true,
        (Some(days), Some(than)) => days > than,
        (_, None) => false,
    }
}

pub struct AccountSync {
    account_id: AccountId,
    services: AccountServices,
    /// The mail service of `services`, spoken to in store ids. Mail calls
    /// go through it, never through `services.mail`.
    mail: StoreMail,
    db: Db,
    events: crate::events::Outgoing,
    window_days: i64,
    body_cache_bytes: i64,
    /// The engine's network monitor, shared with foreground folder loads.
    network: Arc<std::sync::atomic::AtomicBool>,
    retry_max: Duration,
    wait_ceiling: Duration,
    /// Cached bodies read since the last write of their access times.
    touched: Arc<Mutex<Vec<(String, EpochMillis)>>>,
    /// Body bytes stored since the last eviction pass, `None` before the
    /// first one.
    unswept: Mutex<Option<i64>>,
    /// Attempts within the current newest-Inbox selection, including bodies
    /// evicted before opening, so a small cache cannot cause a download loop.
    prefetch_tried: Mutex<HashSet<String>>,
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
    /// The last small messages fetched whole, so a file opened right
    /// after its message costs no second fetch. One cache serves every
    /// account of the engine.
    raw: Arc<Mutex<RawCache>>,
    /// Counts the changes to this account's mail, so a search kept to
    /// list a folder, or a server's unread count, can tell it no longer
    /// says what the folder holds.
    mail_changes: tokio::sync::watch::Sender<u64>,
    /// Set once this run has compared its window with the one the stored
    /// mail was loaded for; see `follow_window`.
    window_followed: std::sync::atomic::AtomicBool,
}

/// How long a finished history replay speaks for the whole mailbox. The
/// engine replays every 30 seconds, so this still covers one missed tick.
pub const FRESH_FOR: Duration = Duration::from_secs(75);

impl AccountSync {
    pub fn new(
        account_id: AccountId,
        services: AccountServices,
        db: Db,
        events: async_channel::Sender<ChangeEvent>,
    ) -> Self {
        AccountSync {
            account_id,
            mail: StoreMail::new(services.mail.clone(), db.clone(), account_id),
            services,
            db,
            events: crate::events::Outgoing::new(events),
            window_days: DEFAULT_WINDOW_DAYS,
            body_cache_bytes: DEFAULT_BODY_CACHE_BYTES,
            network: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            retry_max: Duration::from_secs(8),
            wait_ceiling: crate::WAIT_CEILING,
            touched: Arc::default(),
            unswept: Mutex::default(),
            prefetch_tried: Mutex::default(),
            caught_up: Arc::default(),
            listed: Mutex::default(),
            hits: Mutex::default(),
            raw: Arc::new(Mutex::new(RawCache::new(RAW_CACHE_BYTES))),
            mail_changes: tokio::sync::watch::Sender::new(0),
            window_followed: std::sync::atomic::AtomicBool::new(false),
        }
    }

    pub fn with_limits(mut self, window_days: i64, body_cache_bytes: i64) -> Self {
        self.window_days = window_days;
        self.body_cache_bytes = body_cache_bytes;
        self
    }

    /// Shares the engine's network monitor with foreground folder loads.
    pub(crate) fn with_network(mut self, network: Arc<std::sync::atomic::AtomicBool>) -> Self {
        self.network = network;
        self
    }

    pub(crate) fn network_available(&self) -> bool {
        self.network.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Shares `cache` with the other accounts, so all of them keep their
    /// raw messages under one budget.
    pub fn with_raw_cache(mut self, cache: Arc<Mutex<RawCache>>) -> Self {
        self.raw = cache;
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

    /// The message as the server holds it, from the raw cache or from one
    /// fetch by the server's current name for it, which fills the cache
    /// when the message is under the limit by its bytes or by the size the
    /// store holds for it.
    pub(crate) async fn raw(&self, message_id: &str) -> Result<Arc<Vec<u8>>, SyncError> {
        if let Some(bytes) = self.cached_raw(message_id) {
            return Ok(bytes);
        }
        let fetched = self
            .mail
            .fetch_raw(&[message_id.to_string()])
            .await?
            .into_iter()
            .next()
            .ok_or(BackendError::NotFound)?;
        let bytes = Arc::new(fetched.bytes);
        // View Source and a signature check fetch a large message raw too;
        // keeping it would push out the small ones the cache is for. A
        // message `small` sends down the raw path is kept whatever its
        // real length: Gmail's size estimate can put it under the limit
        // while its bytes come to more, and its files are read from these
        // bytes next.
        // A POP3 account's bytes are in the store already, and keeping a
        // second copy here would double them.
        let local = self.mail.capabilities().local_mailboxes;
        if !local && ((bytes.len() as i64) < RAW_LIMIT || self.small(message_id).await?) {
            self.raw.lock().unwrap_or_else(PoisonError::into_inner).put(
                self.account_id,
                message_id.to_string(),
                Arc::clone(&bytes),
            );
        }
        Ok(bytes)
    }

    /// The message's raw bytes when the raw cache holds them.
    pub(crate) fn cached_raw(&self, message_id: &str) -> Option<Arc<Vec<u8>>> {
        self.raw
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(self.account_id, message_id)
    }

    /// Whether `message_id` goes by the raw path: a stored message whose
    /// reported size is known and under the limit. Anything else goes by
    /// its structure, so an unknown size never downloads a large file.
    pub(crate) async fn small(&self, message_id: &str) -> Result<bool, SyncError> {
        let (account_id, key) = (self.account_id, message_id.to_string());
        let size = self
            .db
            .read(move |c| messages::size_of(c, account_id, &key))
            .await?;
        Ok(size.is_some_and(|s| s > 0 && s < RAW_LIMIT))
    }

    /// Records that history replay left the store up to date.
    pub(crate) fn mark_caught_up(&self) {
        *self.caught_up.lock().expect("caught up") = Some(Instant::now());
    }

    pub fn account_id(&self) -> AccountId {
        self.account_id
    }

    /// The services this account is served by. Mail goes through
    /// [`AccountSync::mail`], which speaks in store ids.
    pub fn services(&self) -> &AccountServices {
        &self.services
    }

    /// The account's mail service, spoken to in store ids.
    pub(crate) fn mail(&self) -> &StoreMail {
        &self.mail
    }

    /// Whether a message on this account's server takes a new name when
    /// it moves.
    pub(super) fn renames(&self) -> bool {
        self.mail.renames()
    }

    /// Whether a user action is waiting on this account's server. The
    /// engine reads it between backfill pages and gives way.
    pub(crate) fn foreground_waiting(&self) -> bool {
        self.mail.person_waiting()
    }

    pub async fn set_state(&self, state: AccountState) -> Result<(), SyncError> {
        let account_id = self.account_id;
        self.db
            .write(move |c| accounts::set_state(c, account_id, state))
            .await?;
        self.emit(ChangeEvent::AccountStateChanged { account_id, state });
        Ok(())
    }

    /// Says the account's state as the store keeps it, and changes
    /// nothing. An account with no mail runs no loop to say how it is
    /// doing, and the window reads what a started account offers when it
    /// hears of it.
    pub(crate) async fn restate(&self) -> Result<(), SyncError> {
        let account_id = self.account_id;
        let kept = self
            .db
            .read(move |c| accounts::account(c, account_id))
            .await?;
        if let Some(account) = kept {
            self.emit(ChangeEvent::AccountStateChanged {
                account_id,
                state: account.state,
            });
        }
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

    /// How many times this account's mail has changed. Only the count
    /// moving matters: a listing taken at one count is out of date at the
    /// next.
    pub fn mail_changes(&self) -> u64 {
        *self.mail_changes.borrow()
    }

    /// Wakes at each change to this account's mail, including a write the
    /// server takes after the store already shows it. It does not keep
    /// the account alive.
    pub fn watch_mail_changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.mail_changes.subscribe()
    }

    /// Marks the account's mail changed. Every event that names changed or
    /// new mail does this, and so does a write once the server has it,
    /// since a search the server answered between the two missed it.
    fn mail_changed(&self) {
        self.mail_changes.send_modify(|changes| *changes += 1);
    }

    fn emit(&self, event: ChangeEvent) {
        if matches!(
            event,
            ChangeEvent::ThreadsChanged { .. } | ChangeEvent::NewMail { .. }
        ) {
            self.mail_changed();
        }
        self.events.send(event);
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
        let wants = ids.iter().map(crate::Want::message).collect();
        Ok(self.fetch(wants).await?.metas)
    }
}
