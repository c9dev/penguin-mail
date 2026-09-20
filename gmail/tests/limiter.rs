use std::sync::Arc;
use std::time::Duration;

use mailrs_gmail::{OAuthClient, QuotaLimiter, QuotaPool};
use tokio::time::Instant;

#[tokio::test(start_paused = true)]
async fn the_burst_is_free_and_then_the_rate_applies() {
    let limiter = QuotaLimiter::new(100.0, 50.0);
    let start = Instant::now();
    limiter.acquire(50).await;
    assert_eq!(start.elapsed(), Duration::ZERO);
    limiter.acquire(25).await;
    let waited = start.elapsed();
    assert!(
        waited >= Duration::from_millis(250) && waited < Duration::from_millis(260),
        "{waited:?}"
    );
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

#[tokio::test]
async fn one_account_gets_one_bucket_however_many_clients_it_has() {
    let pool = QuotaPool::new();
    assert!(Arc::ptr_eq(
        &pool.account("ann@x.com"),
        &pool.account("ann@x.com")
    ));
    assert!(!Arc::ptr_eq(
        &pool.account("ann@x.com"),
        &pool.account("bo@x.com")
    ));
}

#[tokio::test]
async fn every_account_of_one_oauth_client_draws_on_the_same_pool() {
    let oauth = OAuthClient::new("cid", "secret");
    let clone = oauth.clone();
    assert!(
        Arc::ptr_eq(
            &oauth.account_quota("ann@x.com"),
            &clone.account_quota("ann@x.com")
        ),
        "a cloned OAuth client hands out the same buckets"
    );
}

#[tokio::test(start_paused = true)]
async fn two_clients_for_one_account_spend_one_budget() {
    let pool = QuotaPool::new();
    let (first, second) = (pool.account("ann@x.com"), pool.account("ann@x.com"));
    let start = Instant::now();

    // Gmail's burst is 250 units. Spending it twice takes a second bite
    // out of the same bucket, so the second client waits.
    first.acquire(250).await;
    assert_eq!(start.elapsed(), Duration::ZERO);
    second.acquire(200).await;

    let waited = start.elapsed();
    assert!(waited >= Duration::from_secs(1), "waited {waited:?}");
}
