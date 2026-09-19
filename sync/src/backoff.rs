use std::time::Duration;

/// Delay before retry number `attempt` (0-based): 2^attempt seconds, capped
/// at `max`, scaled by up to ±20% using `jitter` in [-1, 1].
pub fn backoff_delay(attempt: u32, max: Duration, jitter: f64) -> Duration {
    let base = Duration::from_secs(1u64 << attempt.min(20)).min(max);
    base.mul_f64(1.0 + 0.2 * jitter.clamp(-1.0, 1.0))
}
