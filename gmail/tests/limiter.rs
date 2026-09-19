use std::time::Duration;

use mailrs_gmail::QuotaLimiter;
use tokio::time::Instant;

#[tokio::test(start_paused = true)]
async fn the_burst_is_free_and_then_the_rate_applies() {
    let limiter = QuotaLimiter::new(100.0, 50.0);
    let start = Instant::now();
    limiter.acquire(50).await;
    assert_eq!(start.elapsed(), Duration::ZERO);
    limiter.acquire(25).await;
    let waited = start.elapsed();
    assert!(waited >= Duration::from_millis(250) && waited < Duration::from_millis(260), "{waited:?}");
}

#[tokio::test(start_paused = true)]
async fn tokens_refill_while_idle() {
    let limiter = QuotaLimiter::new(100.0, 50.0);
    limiter.acquire(50).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let before = Instant::now();
    limiter.acquire(50).await;
    assert_eq!(before.elapsed(), Duration::ZERO);
}

#[tokio::test]
#[should_panic(expected = "exceeds burst")]
async fn a_request_larger_than_the_burst_panics() {
    QuotaLimiter::new(10.0, 5.0).acquire(6).await;
}
