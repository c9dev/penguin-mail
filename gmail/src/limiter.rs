//! How fast Penguin Mail may call Gmail, and whose call goes first.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::time::Instant;

/// Whose work a call belongs to. Anything the user asked for, and anything
/// the assistant does on their behalf, is foreground; the sync loops that
/// backfill the window and poll for history are background.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    Foreground,
    Background,
}

tokio::task_local! {
    static AMBIENT: Priority;
}

/// Runs `work` as background work, so the calls inside it wait behind the
/// user's. A task `work` spawns does not inherit this and counts as
/// foreground, so wrap the loop rather than the runtime.
pub async fn background<F: Future>(work: F) -> F::Output {
    AMBIENT.scope(Priority::Background, work).await
}

/// The priority of the call being made now: foreground, unless some caller
/// up the stack wrapped this work in [`background`].
pub fn priority() -> Priority {
    AMBIENT.try_with(|p| *p).unwrap_or(Priority::Foreground)
}

/// Units a background call leaves in the bucket for the user, as a share of
/// the burst. Gmail's burst is 250 units, so backfill stops at 100 left and
/// a `batchModify` the user asked for finds budget without waiting.
const BACKGROUND_RESERVE: f64 = 0.4;

/// How often a background call that stood aside looks again. Short enough
/// that backfill picks up where it left off once the user's call has gone.
const BACKGROUND_RECHECK: Duration = Duration::from_millis(100);

/// Token bucket over Gmail quota units. Gmail allows 250 units per user per second.
#[derive(Debug)]
pub struct QuotaLimiter {
    rate: f64,
    burst: f64,
    /// What a background call leaves behind for foreground work.
    reserve: f64,
    bucket: std::sync::Mutex<Bucket>,
    /// Foreground calls inside `acquire` right now.
    waiting: AtomicUsize,
}

#[derive(Debug)]
struct Bucket {
    tokens: f64,
    updated: Instant,
}

impl Bucket {
    fn refill(&mut self, rate: f64, burst: f64) {
        let now = Instant::now();
        let refill = now.duration_since(self.updated).as_secs_f64() * rate;
        self.tokens = (self.tokens + refill).min(burst);
        self.updated = now;
    }
}

impl QuotaLimiter {
    pub fn new(units_per_second: f64, burst: f64) -> Self {
        QuotaLimiter {
            rate: units_per_second,
            burst,
            reserve: burst * BACKGROUND_RESERVE,
            bucket: std::sync::Mutex::new(Bucket {
                tokens: burst,
                updated: Instant::now(),
            }),
            waiting: AtomicUsize::new(0),
        }
    }

    /// 200 units per second with a 250 unit burst, under Gmail's per-user limit.
    pub fn gmail() -> Self {
        Self::new(200.0, 250.0)
    }

    /// What Gmail itself allows one account: 250 units a second. The
    /// in-memory Gmail measures its callers against this and answers 429
    /// past it, as Gmail does.
    pub fn gmail_server() -> Self {
        Self::new(250.0, 250.0)
    }

    /// The whole project's budget. Google gives an OAuth client 1,200,000
    /// units a minute across every account it signs in, so six accounts
    /// spending their own 200 a second stay well inside it.
    pub fn project() -> Self {
        Self::new(20_000.0, 20_000.0)
    }

    /// Waits until `units` are available, then spends them. A foreground
    /// call takes what is there; a background call leaves the reserve
    /// behind and stands aside while a foreground call is waiting.
    pub async fn acquire(&self, units: u32, priority: Priority) {
        let units = f64::from(units);
        assert!(
            units <= self.burst,
            "request of {units} units exceeds burst of {}",
            self.burst
        );
        let _waiting = match priority {
            Priority::Foreground => Some(self.waiting()),
            Priority::Background => None,
        };
        // A background call may not dip into the reserve, unless the call
        // itself is larger than what the bucket holds beyond it.
        let floor = match priority {
            Priority::Foreground => 0.0,
            Priority::Background => self.reserve.min(self.burst - units),
        };
        loop {
            let wait = {
                let mut bucket = self.bucket.lock().expect("quota bucket poisoned");
                bucket.refill(self.rate, self.burst);
                let stand_aside = priority == Priority::Background && self.foreground_waiting();
                if !stand_aside && bucket.tokens >= units + floor {
                    bucket.tokens -= units;
                    return;
                }
                let short = ((units + floor) - bucket.tokens).max(0.0);
                match stand_aside {
                    true => BACKGROUND_RECHECK,
                    false => Duration::from_secs_f64(short / self.rate),
                }
            };
            tokio::time::sleep(wait).await;
        }
    }

    /// Spends `units` when the bucket holds them, and says whether it did.
    /// Gmail itself answers 429 rather than waiting, so the in-memory Gmail
    /// prices its refusals through this.
    pub fn try_acquire(&self, units: u32) -> bool {
        let units = f64::from(units);
        let mut bucket = self.bucket.lock().expect("quota bucket poisoned");
        bucket.refill(self.rate, self.burst);
        if bucket.tokens >= units {
            bucket.tokens -= units;
            return true;
        }
        false
    }

    /// Whether a foreground call is waiting on this bucket.
    pub fn foreground_waiting(&self) -> bool {
        self.waiting.load(Ordering::Relaxed) > 0
    }

    /// Counts one foreground call as waiting until the guard drops.
    pub fn waiting(&self) -> Waiting<'_> {
        self.waiting.fetch_add(1, Ordering::Relaxed);
        Waiting(&self.waiting)
    }
}

/// One foreground call waiting on Gmail. Background work stands aside while
/// a guard is alive, so keep it only as long as the wait.
#[derive(Debug)]
pub struct Waiting<'a>(&'a AtomicUsize);

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// The quota one OAuth client may spend: a bucket for each account it has
/// signed in, and one for the project that every account waits on too.
/// Ask it for an account's bucket rather than building a limiter beside
/// each client, or two clients for one address each spend that address's
/// full budget and Gmail answers 429.
#[derive(Debug)]
pub struct QuotaPool {
    project: Arc<QuotaLimiter>,
    accounts: std::sync::Mutex<HashMap<String, Arc<AccountQuota>>>,
}

impl Default for QuotaPool {
    fn default() -> Self {
        QuotaPool::new()
    }
}

impl QuotaPool {
    pub fn new() -> Self {
        QuotaPool {
            project: Arc::new(QuotaLimiter::project()),
            accounts: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// The bucket for `email`, the same one every time.
    pub fn account(&self, email: &str) -> Arc<AccountQuota> {
        let mut accounts = self.accounts.lock().expect("quota pool poisoned");
        Arc::clone(accounts.entry(email.to_string()).or_insert_with(|| {
            Arc::new(AccountQuota {
                account: QuotaLimiter::gmail(),
                project: Arc::clone(&self.project),
            })
        }))
    }
}

/// One account's share of the project's quota.
#[derive(Debug)]
pub struct AccountQuota {
    account: QuotaLimiter,
    project: Arc<QuotaLimiter>,
}

impl AccountQuota {
    /// A budget shared with nobody, for a client outside a pool such as the
    /// one the consent flow builds.
    pub fn standalone() -> Self {
        AccountQuota {
            account: QuotaLimiter::gmail(),
            project: Arc::new(QuotaLimiter::project()),
        }
    }

    /// Waits until both buckets hold `units`, then spends them.
    pub async fn acquire(&self, units: u32, priority: Priority) {
        self.project.acquire(units, priority).await;
        self.account.acquire(units, priority).await;
    }

    /// Whether the user is waiting on this account's budget. Backfill and
    /// history polling read it and slow down while it is true.
    pub fn foreground_waiting(&self) -> bool {
        self.account.foreground_waiting()
    }

    /// Counts the caller as foreground work waiting on this account until
    /// the guard drops. A mail action waiting out Gmail's `Retry-After`
    /// holds one, so backfill stands aside for the whole wait and not only
    /// while the retry asks the bucket.
    pub fn waiting(&self) -> Waiting<'_> {
        self.account.waiting()
    }
}
