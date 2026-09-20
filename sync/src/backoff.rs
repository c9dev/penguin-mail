use std::time::Duration;

use mailrs_domain::AccountId;

/// Delay before retry number `attempt` (0-based): 2^attempt seconds, capped
/// at `max`, scaled by up to ±20% using `jitter` in [-1, 1].
pub fn backoff_delay(attempt: u32, max: Duration, jitter: f64) -> Duration {
    with_jitter(
        Duration::from_secs(1u64 << attempt.min(20)).min(max),
        jitter,
    )
}

/// `delay` moved by up to a fifth either way, using `jitter` in [-1, 1].
/// Gmail hands every account the same `Retry-After`, so without this six
/// accounts come back on the same tick and are limited all over again.
pub fn with_jitter(delay: Duration, jitter: f64) -> Duration {
    delay.mul_f64(1.0 + 0.2 * jitter.clamp(-1.0, 1.0))
}

/// Where in the poll cycle an account's tick falls. Six accounts started
/// together would otherwise ask Gmail for their history on the same second,
/// every 30 seconds; this spreads them over the cycle and keeps each one on
/// its own offset for as long as it runs.
pub fn poll_offset(account_id: AccountId, interval: Duration) -> Duration {
    // A cheap mix, so neighbouring account ids land far apart.
    let mixed = (account_id as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    let share = (mixed >> 40) as f64 / f64::from(1u32 << 24);
    interval.mul_f64(share)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn six_accounts_poll_at_six_different_points_in_the_cycle() {
        let interval = Duration::from_secs(30);
        let offsets: Vec<Duration> = (1..=6).map(|id| poll_offset(id, interval)).collect();
        for offset in &offsets {
            assert!(*offset < interval, "{offset:?} falls inside the cycle");
        }
        let mut spread = offsets.clone();
        spread.sort();
        spread.dedup();
        assert_eq!(spread.len(), 6, "no two accounts share a tick: {offsets:?}");
        let closest = spread.windows(2).map(|w| w[1] - w[0]).min().unwrap();
        assert!(
            closest > Duration::from_secs(1),
            "the nearest pair is {closest:?} apart"
        );
    }

    #[test]
    fn an_account_keeps_its_offset() {
        let interval = Duration::from_secs(30);
        assert_eq!(poll_offset(4, interval), poll_offset(4, interval));
    }

    #[test]
    fn jitter_stays_within_a_fifth() {
        let delay = Duration::from_secs(10);
        assert_eq!(with_jitter(delay, 0.0), delay);
        assert_eq!(with_jitter(delay, 1.0), Duration::from_secs(12));
        assert_eq!(with_jitter(delay, -1.0), Duration::from_secs(8));
        assert_eq!(with_jitter(delay, 9.0), Duration::from_secs(12));
    }
}
