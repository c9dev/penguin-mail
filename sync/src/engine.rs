//! Runs one sync loop per account and reports changes on a channel.

use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use futures::FutureExt;
use mailrs_domain::{AccountId, AccountState, ChangeEvent};
use mailrs_store::Db;
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::account::{DEFAULT_BODY_CACHE_BYTES, DEFAULT_WINDOW_DAYS};
use crate::raw_cache::{RAW_CACHE_BYTES, RawCache};
use crate::{
    AccountServices, AccountSync, AnyMail, BackendError, MailBackend, SyncError, background, backoff_delay,
    now_millis, poll_offset, with_jitter,
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
    /// Whether the computer has a network, as the app last heard it. Every
    /// account's loop reads the same flag.
    network: Arc<AtomicBool>,
    /// Whether the main window is open, which sets how often an account
    /// looks at mail nobody is watching.
    window_open: AtomicBool,
    /// The raw messages of every account, under one byte budget.
    raw: Arc<Mutex<RawCache>>,
}

struct Running {
    sync: Arc<AccountSync>,
    poke: Arc<Notify>,
    task: JoinHandle<()>,
}

impl SyncEngine {
    pub fn new(db: Db, config: EngineConfig) -> (Self, async_channel::Receiver<ChangeEvent>) {
        let (events, receiver) = async_channel::bounded(crate::events::CHANNEL_EVENTS);
        (
            SyncEngine {
                db,
                config,
                events,
                running: Mutex::new(HashMap::new()),
                network: Arc::new(AtomicBool::new(true)),
                window_open: AtomicBool::new(true),
                raw: Arc::new(Mutex::new(RawCache::new(RAW_CACHE_BYTES))),
            },
            receiver,
        )
    }

    /// Starts the account's loop, replacing one that is already running.
    /// Call it from inside a tokio runtime.
    ///
    /// An account with no mail gets no loop and is kept all the same, so
    /// its services answer for it.
    pub fn start_account(&self, account_id: AccountId, services: AccountServices) {
        services
            .mail
            .set_window_open(self.window_open.load(Ordering::SeqCst));
        let sync = Arc::new(
            AccountSync::new(account_id, services, self.db.clone(), self.events.clone())
                .with_limits(self.config.window_days, self.config.body_cache_bytes)
                .with_raw_cache(Arc::clone(&self.raw))
                .with_network(Arc::clone(&self.network)),
        );
        let poke = Arc::new(Notify::new());
        let mut running = self.lock();
        // The old loop may be in the middle of a POP3 check, and a server
        // that locks the maildrop refuses a second session until the first
        // one's connection closes. The new loop starts once the old task
        // has stopped and dropped its connection.
        let previous = running.remove(&account_id).map(|previous| {
            previous.task.abort();
            previous.task
        });
        let loop_ = supervise(
            Arc::clone(&sync),
            Arc::clone(&poke),
            Arc::clone(&self.network),
            self.config.clone(),
        );
        let task = tokio::spawn(async move {
            if let Some(previous) = previous {
                // Cancelled is the expected end; anything else ended it first.
                let _ = previous.await;
            }
            loop_.await;
        });
        running.insert(account_id, Running { sync, poke, task });
    }

    pub fn stop_account(&self, account_id: AccountId) {
        if let Some(running) = self.lock().remove(&account_id) {
            running.task.abort();
        }
        self.raw
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .forget(account_id);
    }

    /// Polls the account now instead of at the next interval.
    pub fn poke(&self, account_id: AccountId) {
        if let Some(running) = self.lock().get(&account_id) {
            running.sync.services().mail.check_now();
            running.poke.notify_one();
        }
    }

    pub fn poke_all(&self) {
        for running in self.lock().values() {
            running.sync.services().mail.check_now();
            running.poke.notify_one();
        }
    }

    /// Tells the loops whether the computer has a network. Without one a
    /// loop waits instead of asking Gmail and backing off again and again;
    /// a poke still sends it to Gmail once, since the app's view of the
    /// network can be wrong. Each change wakes the loops, so one that goes
    /// offline says so at once and one that comes back asks Gmail at once.
    pub fn set_network(&self, available: bool) {
        if self.network.swap(available, Ordering::SeqCst) != available {
            self.poke_all();
        }
    }

    /// Tells every account whether the main window is open. With only the
    /// tray running, an account looks at mailboxes other than its Inbox
    /// less often.
    pub fn set_window_open(&self, open: bool) {
        let was_open = self.window_open.swap(open, Ordering::SeqCst);
        for running in self.lock().values() {
            running.sync.services().mail.set_window_open(open);
            // The tray looks every few minutes; a window that opens now
            // should not show mail from before the last look.
            if open && !was_open {
                running.poke.notify_one();
            }
        }
    }

    /// Whether the engine currently thinks the main window is open.
    pub fn window_open(&self) -> bool {
        self.window_open.load(Ordering::SeqCst)
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

/// Whether the account signs in with a saved password rather than a token.
fn signs_in_with_password(services: &AccountServices) -> bool {
    matches!(services.mail, AnyMail::Imap(_) | AnyMail::Pop3(_))
}

/// `password` says the account signs in with a saved password. A mail
/// server refuses every login while it restarts or updates, and the saved
/// password is still right afterwards, so a refusal there waits and tries
/// again instead of asking the person to sign in. A refused Google or
/// Microsoft token stays refused, so those accounts still ask.
fn classify(err: &SyncError, password: bool) -> Failure {
    match err {
        SyncError::Backend(BackendError::NeedsReauth) if password => Failure::Retry {
            state: AccountState::BackingOff,
            retry_after: None,
        },
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
async fn supervise(
    sync: Arc<AccountSync>,
    poke: Arc<Notify>,
    network: Arc<AtomicBool>,
    config: EngineConfig,
) {
    // An account with no mail has no change feed to follow and nothing to
    // backfill. Its calendar is read on the app's own timer, and the
    // calendar copy, which alone talks to its server, says how it is
    // doing. Here it says once that it started, then waits to be stopped.
    if !sync.services().serves_mail() {
        if let Err(err) = sync.restate().await {
            tracing::warn!(account = sync.account_id(), error = %err, "could not read the account state");
        }
        return std::future::pending().await;
    }
    let mut crashed = false;
    loop {
        let run = run_account(
            Arc::clone(&sync),
            Arc::clone(&poke),
            Arc::clone(&network),
            config.clone(),
        );
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
    network: Arc<AtomicBool>,
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
    // One watch runs across the loop's turns and starts again only when it
    // ends. A watch made anew at each turn would drop a waiting IDLE at
    // every poll and poke, and with it the connection. Gmail's never ends.
    let mail = &sync.services().mail;
    let mut watch = Box::pin(mail.watch());
    loop {
        if !network.load(Ordering::SeqCst) {
            if reported != Some(AccountState::Offline) {
                reported = Some(AccountState::Offline);
                report(&sync, AccountState::Offline).await;
            }
            // The network coming back pokes every loop, and so does Check
            // for Mail.
            poke.notified().await;
            next_poll = Instant::now();
        }
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
                    // A server that says when the Inbox changes wakes the
                    // loop at once. Gmail never does.
                    () = &mut watch => {
                        next_poll = Instant::now();
                        watch.set(mail.watch());
                    }
                }
            }
            Err(err) => match classify(&err, signs_in_with_password(sync.services())) {
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
/// Gmail's and list the labels when due, then load one backfill page. The
/// account's mail service may set its own gap between polls, as an IMAP
/// server with IDLE does. Returns true when more backfill pages remain.
/// An account whose mailboxes are local lists them once an hour and has
/// no window to prune or page.
pub(crate) async fn tick(
    sync: &AccountSync,
    next_poll: &mut Instant,
    next_prune: &mut Instant,
    stagger: &mut Duration,
    config: &EngineConfig,
) -> Result<bool, SyncError> {
    // An account whose mailboxes live only in the store is never pruned,
    // relisted or paged: the store is its only copy. It lists its
    // mailboxes once an hour, the first time before anything else, so the
    // Inbox exists before mail arrives.
    let local = sync.services().mail.capabilities().local_mailboxes;
    if local && Instant::now() >= *next_prune {
        sync.refresh_labels().await?;
        *next_prune = Instant::now() + PRUNE_INTERVAL;
        sync.set_checked_at(now_millis()).await?;
    }
    if Instant::now() >= *next_poll {
        sync.look_for_mail().await?;
        let every = sync
            .services()
            .mail
            .poll_interval()
            .unwrap_or(config.poll_interval);
        *next_poll = Instant::now() + every + std::mem::take(stagger);
    }
    // After the look at new mail, so a longer window never holds it up.
    sync.follow_window().await?;
    if local {
        return Ok(false);
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

    #[test]
    fn a_refused_password_waits_and_a_refused_token_asks() {
        let refused = SyncError::Backend(BackendError::NeedsReauth);
        assert!(matches!(
            classify(&refused, true),
            Failure::Retry {
                state: AccountState::BackingOff,
                retry_after: None
            }
        ));
        assert!(matches!(classify(&refused, false), Failure::Reauth));
    }
}
