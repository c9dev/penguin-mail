use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;
use tokio::time::Instant;

/// Token bucket over Gmail quota units. Gmail allows 250 units per user per second.
#[derive(Debug)]
pub struct QuotaLimiter {
    rate: f64,
    burst: f64,
    bucket: Mutex<Bucket>,
}

#[derive(Debug)]
struct Bucket {
    tokens: f64,
    updated: Instant,
}

impl QuotaLimiter {
    pub fn new(units_per_second: f64, burst: f64) -> Self {
        QuotaLimiter {
            rate: units_per_second,
            burst,
            bucket: Mutex::new(Bucket {
                tokens: burst,
                updated: Instant::now(),
            }),
        }
    }

    /// 200 units per second with a 250 unit burst, under Gmail's per-user limit.
    pub fn gmail() -> Self {
        Self::new(200.0, 250.0)
    }

    /// The whole project's budget. Google gives an OAuth client 1,200,000
    /// units a minute across every account it signs in, so six accounts
    /// spending their own 200 a second stay well inside it.
    pub fn project() -> Self {
        Self::new(20_000.0, 20_000.0)
    }

    /// Waits until `units` are available, then spends them.
    pub async fn acquire(&self, units: u32) {
        let units = f64::from(units);
        assert!(
            units <= self.burst,
            "request of {units} units exceeds burst of {}",
            self.burst
        );
        loop {
            let wait = {
                let mut bucket = self.bucket.lock().await;
                let now = Instant::now();
                let refill = now.duration_since(bucket.updated).as_secs_f64() * self.rate;
                bucket.tokens = (bucket.tokens + refill).min(self.burst);
                bucket.updated = now;
                if bucket.tokens >= units {
                    bucket.tokens -= units;
                    return;
                }
                Duration::from_secs_f64((units - bucket.tokens) / self.rate)
            };
            tokio::time::sleep(wait).await;
        }
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
    pub async fn acquire(&self, units: u32) {
        self.project.acquire(units).await;
        self.account.acquire(units).await;
    }
}
