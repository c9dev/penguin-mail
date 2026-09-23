//! Runs one sync loop per account and reports changes on a channel.

use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use futures::FutureExt;
use mailrs_domain::{AccountId, AccountState, ChangeEvent};
use mailrs_store::Db;
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::account::{DEFAULT_BODY_CACHE_BYTES, DEFAULT_WINDOW_DAYS};
use crate::{
    AccountServices, AccountSync, BackendError, SyncError, backoff_delay, background, now_millis,
    poll_offset, with_jitter,
};

/// How often an account prunes, checks its inbox against Gmail's, and lists
/// its labels again. History says nothing about a label made, renamed or
/// recoloured elsewhere, so this is how the sidebar hears of one; a label
/// history does name is picked up by the replay at once.
const PRUNE_INTERVAL: Duration = Duration::from_secs(60 * 60);

#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub poll_interval: Duration,
    pub max_backoff: Duration,
    pub window_days: i64,
    pub body_cache_bytes: i64,
    /// Gap between backfill pages. A page of 100 messages costs 505 quota
    /// units, about two and a half seconds of an account's budget, so
    /// pages back to back leave the user nothing. The gap lets the bucket
    /// refill before anybody presses Delete.
    pub backfill_pause: Duration,
    /// The gap while the user is already waiting on Gmail.
    pub backfill_busy_pause: Duration,
    /// How long a crashed account loop waits before its one restart.
    pub restart_after: Duration,
}

impl Default for EngineConfig {
    fn default() -> Self {
        EngineConfig {
            poll_interval: Duration::from_secs(30),
            max_backoff: Duration::from_secs(300),
            window_days: DEFAULT_WINDOW_DAYS,
            body_cache_bytes: DEFAULT_BODY_CACHE_BYTES,
            backfill_pause: Duration::from_millis(500),
            backfill_busy_pause: Duration::from_secs(5),
            restart_after: Duration::from_secs(30),
        }
    }
}

pub struct SyncEngine {
    db: Db,
    config: EngineConfig,
    events: async_channel::Sender<ChangeEvent>,
    running: Mutex<HashMap<AccountId, Running>>,
}

struct Running {
    sync: Arc<AccountSync>,
    poke: Arc<Notify>,
    task: JoinHandle<()>,
}

impl SyncEngine {
    pub fn new(db: Db, config: EngineConfig) -> (Self, async_channel::Receiver<ChangeEvent>) {
        let (events, receiver) = async_channel::unbounded();
        (
            SyncEngine {
                db,
                config,
                events,
                running: Mutex::new(HashMap::new()),
            },
            receiver,
        )
    }

    /// Starts the account's loop, replacing one that is already running.
    /// Call it from inside a tokio runtime.
    pub fn start_account(&self, account_id: AccountId, services: AccountServices) {
        let sync = Arc::new(
            AccountSync::new(account_id, services, self.db.clone(), self.events.clone())
                .with_limits(self.config.window_days, self.config.body_cache_bytes),
        );
        let poke = Arc::new(Notify::new());
        let task = tokio::spawn(supervise(
            Arc::clone(&sync),
            Arc::clone(&poke),
            self.config.clone(),
        ));
        if let Some(previous) = self.lock().insert(account_id, Running { sync, poke, task }) {
            previous.task.abort();
        }
    }

    pub fn stop_account(&self, account_id: AccountId) {
        if let Some(running) = self.lock().remove(&account_id) {
            running.task.abort();
        }
    }

    /// Polls the account now instead of at the next interval.
    pub fn poke(&self, account_id: AccountId) {
        if let Some(running) = self.lock().get(&account_id) {
            running.poke.notify_one();
        }
    }

    pub fn poke_all(&self) {
        for running in self.lock().values() {
            running.poke.notify_one();
        }
    }

    pub fn is_running(&self, account_id: AccountId) -> bool {
        self.lock()
            .get(&account_id)
            .is_some_and(|r| !r.task.is_finished())
    }

    /// The account's sync handle, for opening threads, loading bodies, and triage.
    pub fn account(&self, account_id: AccountId) -> Result<Arc<AccountSync>, SyncError> {
        self.lock()
            .get(&account_id)
            .map(|r| Arc::clone(&r.sync))
            .ok_or(SyncError::UnknownAccount(account_id))
    }

    pub fn shutdown(&self) {
        for (_, running) in self.lock().drain() {
            running.task.abort();
        }
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<AccountId, Running>> {
        self.running.lock().expect("engine lock poisoned")
    }
}

impl Drop for SyncEngine {
    fn drop(&mut self) {
        self.shutdown();
    }
}

enum Failure {
    Reauth,
    Retry {
        state: AccountState,
        retry_after: Option<Duration>,
    },
}

fn classify(err: &SyncError) -> Failure {
    match err {
        SyncError::Backend(BackendError::NeedsReauth) => Failure::Reauth,
        SyncError::Backend(BackendError::Offline(_)) => Failure::Retry {
            state: AccountState::Offline,
            retry_after: None,
        },
        SyncError::Backend(BackendError::RateLimited(retry_after)) => Failure::Retry {
            state: AccountState::BackingOff,
            retry_after: *retry_after,
        },
        _ => Failure::Retry {
            state: AccountState::BackingOff,
            retry_after: None,
        },
    }
}

/// Runs the account's loop, and runs it again once, after a pause, if it
/// panics. A second panic stops the account and records it as stopped, so
/// the sidebar says so instead of the account going quiet.
async fn supervise(sync: Arc<AccountSync>, poke: Arc<Notify>, config: EngineConfig) {
    let mut crashed = false;
    loop {
        let run = run_account(Arc::clone(&sync), Arc::clone(&poke), config.clone());
        if AssertUnwindSafe(run).catch_unwind().await.is_ok() {
            // The loop ends by itself only when Google rejects the refresh
            // token, and it has recorded that already.
            return;
        }
        if crashed {
            tracing::error!(
                account = sync.account_id(),
                "the sync loop crashed again; stopping the account"
            );
            report(&sync, AccountState::Stopped).await;
            return;
        }
        crashed = true;
        tracing::error!(
            account = sync.account_id(),
            "the sync loop crashed; starting it again"
        );
        tokio::time::sleep(config.restart_after).await;
    }
}

async fn run_account(
    sync: Arc<AccountSync>,
    poke: Arc<Notify>,
    config: EngineConfig,
) {
    let mut reported: Option<AccountState> = None;
    let mut failures: u32 = 0;
    let mut next_poll = Instant::now();
    // A restart within the hour picks up where the last run's hour left
    // off, rather than listing Gmail's inbox again straight away.
    let mut next_prune = Instant::now() + until_check(sync.checked_at().await, now_millis());
    // Six accounts start together but should not ask for their history on
    // the same second afterwards, so each one takes its own place in the
    // cycle from its second poll on.
    let mut stagger = poll_offset(sync.account_id(), config.poll_interval);
    loop {
        // Everything this loop asks Gmail for is background work, so it
        // waits behind whatever the user is doing and leaves the account
        // budget the user's next action needs.
        match background(tick(
            &sync,
            &mut next_poll,
            &mut next_prune,
            &mut stagger,
            &config,
        ))
        .await
        {
            Ok(more_backfill) => {
                failures = 0;
                if reported != Some(AccountState::Ok) {
                    reported = Some(AccountState::Ok);
                    report(&sync, AccountState::Ok).await;
                }
                if more_backfill {
                    // Backfill has the whole mailbox to load and no hurry.
                    // A gap between pages keeps the account's budget from
                    // running at nothing, and a longer one gets out of the
                    // way of a user action that is already waiting.
                    let pause = match sync.foreground_waiting() {
                        true => config.backfill_busy_pause,
                        false => config.backfill_pause,
                    };
                    tokio::select! {
                        _ = tokio::time::sleep(pause) => {}
                        // Somebody asked for a refresh, which still wants
                        // history before the next page.
                        _ = poke.notified() => next_poll = Instant::now(),
                    }
                    continue;
                }
                tokio::select! {
                    _ = tokio::time::sleep_until(next_poll) => {}
                    _ = poke.notified() => next_poll = Instant::now(),
                }
            }
            Err(err) => match classify(&err) {
                Failure::Reauth => {
                    tracing::warn!(
                        account = sync.account_id(),
                        "Google rejected the refresh token; add the account again"
                    );
                    report(&sync, AccountState::NeedsReauth).await;
                    return;
                }
                Failure::Retry { state, retry_after } => {
                    tracing::warn!(account = sync.account_id(), error = %err, "sync failed; backing off");
                    if reported != Some(state) {
                        reported = Some(state);
                        report(&sync, state).await;
                    }
                    // Gmail hands every account the same Retry-After, so
                    // wait a jittered version of it rather than the number
                    // itself, or all six come back on the same tick.
                    let jitter = rand::random_range(-1.0..=1.0);
                    let delay = match retry_after {
                        Some(after) => with_jitter(after, jitter),
                        None => backoff_delay(failures, config.max_backoff, jitter),
                    };
                    failures = failures.saturating_add(1);
                    tokio::select! {
                        _ = tokio::time::sleep(delay) => {}
                        _ = poke.notified() => {}
                    }
                }
            },
        }
    }
}

/// One pass: poll history when due, prune, check the inbox against
/// Gmail's and list the labels when due, then load one backfill page. Returns true when more
/// backfill pages remain.
async fn tick(
    sync: &AccountSync,
    next_poll: &mut Instant,
    next_prune: &mut Instant,
    stagger: &mut Duration,
    config: &EngineConfig,
) -> Result<bool, SyncError> {
    if Instant::now() >= *next_poll {
        sync.incremental().await?;
        *next_poll = Instant::now() + config.poll_interval + std::mem::take(stagger);
    }
    if Instant::now() >= *next_prune {
        sync.prune(now_millis()).await?;
        sync.reconcile_inbox().await?;
        sync.refresh_labels().await?;
        *next_prune = Instant::now() + PRUNE_INTERVAL;
        sync.set_checked_at(now_millis()).await?;
    }
    sync.backfill_step().await
}

/// How long to wait before the first prune and inbox check, given when
/// the last one ran: nothing when it never ran or ran an hour ago, else
/// the rest of that hour.
fn until_check(
    last: Option<mailrs_domain::EpochMillis>,
    now: mailrs_domain::EpochMillis,
) -> Duration {
    let Some(last) = last else {
        return Duration::ZERO;
    };
    let since = now.saturating_sub(last);
    if since < 0 {
        return Duration::ZERO;
    }
    PRUNE_INTERVAL.saturating_sub(Duration::from_millis(since as u64))
}

async fn report(sync: &AccountSync, state: AccountState) {
    if let Err(err) = sync.set_state(state).await {
        tracing::error!(account = sync.account_id(), error = %err, "could not record the account state");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: i64 = 60 * 60 * 1000;

    #[test]
    fn a_recent_check_waits_out_the_rest_of_its_hour() {
        let now = 10 * HOUR;
        assert_eq!(until_check(None, now), Duration::ZERO);
        assert_eq!(until_check(Some(now - 2 * HOUR), now), Duration::ZERO);
        assert_eq!(
            until_check(Some(now - HOUR / 4), now),
            Duration::from_secs(45 * 60)
        );
        // A clock that went backwards checks now rather than waiting.
        assert_eq!(until_check(Some(now + HOUR), now), Duration::ZERO);
    }
}
