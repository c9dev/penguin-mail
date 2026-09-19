use std::time::Duration;

use tokio::sync::Mutex;
use tokio::time::Instant;

/// Token bucket over Gmail quota units. Gmail allows 250 units per user per second.
pub struct QuotaLimiter {
    rate: f64,
    burst: f64,
    bucket: Mutex<Bucket>,
}

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
