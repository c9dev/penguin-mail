use std::sync::Arc;
use std::time::Duration;

use mailrs_gmail::{OAuthClient, Priority, QuotaLimiter, QuotaPool};
use tokio::time::Instant;

#[tokio::test(start_paused = true)]
async fn the_burst_is_free_and_then_the_rate_applies() {
    let limiter = QuotaLimiter::new(100.0, 50.0);
    let start = Instant::now();
    limiter.acquire(50, Priority::Foreground).await;
    assert_eq!(start.elapsed(), Duration::ZERO);
    limiter.acquire(25, Priority::Foreground).await;
    let waited = start.elapsed();
    assert!(
        waited >= Duration::from_millis(250) && waited < Duration::from_millis(260),
        "{waited:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn tokens_refill_while_idle() {
    let limiter = QuotaLimiter::new(100.0, 50.0);
    limiter.acquire(50, Priority::Foreground).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let before = Instant::now();
    limiter.acquire(50, Priority::Foreground).await;
    assert_eq!(before.elapsed(), Duration::ZERO);
}

#[tokio::test]
#[should_panic(expected = "exceeds burst")]
async fn a_request_larger_than_the_burst_panics() {
    QuotaLimiter::new(10.0, 5.0)
        .acquire(6, Priority::Foreground)
        .await;
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
    first.acquire(250, Priority::Foreground).await;
    assert_eq!(start.elapsed(), Duration::ZERO);
    second.acquire(200, Priority::Foreground).await;

    let waited = start.elapsed();
    assert!(waited >= Duration::from_secs(1), "waited {waited:?}");
}

#[tokio::test(start_paused = true)]
async fn a_user_action_gets_the_budget_before_a_queued_backfill() {
    let limiter = Arc::new(QuotaLimiter::gmail());
    // Backfill has just spent the burst, as it does page after page.
    limiter.acquire(250, Priority::Background).await;
    let start = Instant::now();

    // A page of metadata queues up behind an empty bucket, and the user
    // presses Delete a moment later.
    let backfill = {
        let limiter = Arc::clone(&limiter);
        tokio::spawn(async move {
            for _ in 0..20 {
                limiter.acquire(5, Priority::Background).await;
            }
            Instant::now()
        })
    };
    tokio::time::sleep(Duration::from_millis(10)).await;
    let action = {
        let limiter = Arc::clone(&limiter);
        tokio::spawn(async move {
            limiter.acquire(50, Priority::Foreground).await;
            Instant::now()
        })
    };

    let (deleted, backfilled) = (action.await.unwrap(), backfill.await.unwrap());

    // 50 units refill in a quarter of a second. The backfill wants 100
    // units of its own plus the 100 it leaves the user, so it waits a
    // second even though it asked first.
    let waited = deleted - start;
    assert!(
        waited < Duration::from_millis(400),
        "the user waited {waited:?}"
    );
    assert!(
        deleted < backfilled,
        "the user's call came after the backfill's"
    );
}

#[tokio::test(start_paused = true)]
async fn backfill_leaves_the_user_a_batch_worth_of_budget() {
    let limiter = Arc::new(QuotaLimiter::gmail());
    let backfill = {
        let limiter = Arc::clone(&limiter);
        tokio::spawn(async move {
            loop {
                limiter.acquire(5, Priority::Background).await;
            }
        })
    };

    // Two seconds of metadata fetches, back to back.
    tokio::time::sleep(Duration::from_secs(2)).await;

    assert!(
        limiter.try_acquire(50),
        "backfill drained the bucket and a batchModify found nothing"
    );
    backfill.abort();
}

#[tokio::test(start_paused = true)]
async fn a_refusal_halves_the_pace_and_a_quiet_minute_rebuilds_it() {
    let limiter = QuotaLimiter::gmail();
    assert_eq!(limiter.rate(), 200.0);

    limiter.slow_down();
    assert_eq!(limiter.rate(), 100.0, "one refusal should halve the pace");

    limiter.slow_down();
    assert_eq!(limiter.rate(), 50.0);

    // Two minutes of calls Gmail accepts bring the pace back.
    tokio::time::sleep(Duration::from_secs(120)).await;
    assert_eq!(limiter.rate(), 200.0, "the pace should climb back");
}

#[tokio::test(start_paused = true)]
async fn refusals_stop_at_a_floor_that_still_makes_progress() {
    let limiter = QuotaLimiter::gmail();
    for _ in 0..20 {
        limiter.slow_down();
    }
    assert_eq!(
        limiter.rate(),
        20.0,
        "the pace should not fall past the floor"
    );
}

#[tokio::test(start_paused = true)]
async fn a_slowed_account_paces_its_calls_to_the_lower_rate() {
    let limiter = QuotaLimiter::gmail();
    limiter.slow_down();
    limiter.slow_down();
    limiter.slow_down();
    // 25 units a second, so ten metadata fetches take about two seconds.
    let start = tokio::time::Instant::now();
    for _ in 0..10 {
        limiter.acquire(5, Priority::Foreground).await;
    }
    let spent = start.elapsed();
    assert!(
        spent >= Duration::from_millis(1500),
        "ten calls at 25 units a second should take about two seconds, took {spent:?}"
    );
}
