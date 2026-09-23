//! Times the reads the sidebar and the thread list make, on a copy of a
//! real store, and prints the counts they answer so a run before a change
//! and a run after it can be compared line by line. Ignored by default.
//! Point `PENGUIN_MAIL_MEASURE_DB` at a copy, never at the live file:
//! opening it runs any migration still pending.

use std::path::Path;
use std::time::{Duration, Instant};

use mailrs_domain::Category;
use mailrs_store::threads::{self, ThreadFilter};
use mailrs_store::{accounts, flags, labels};

/// Runs `read` a few times to warm the page cache, then 21 more, and
/// prints the median and the best.
fn time<T>(name: &str, mut read: impl FnMut() -> T) {
    for _ in 0..3 {
        std::hint::black_box(read());
    }
    let mut times: Vec<Duration> = (0..21)
        .map(|_| {
            let started = Instant::now();
            std::hint::black_box(read());
            started.elapsed()
        })
        .collect();
    times.sort();
    println!("time {name}: median {:?}, best {:?}", times[10], times[0]);
}

#[test]
#[ignore = "needs a copy of a real store in PENGUIN_MAIL_MEASURE_DB"]
fn the_sidebar_and_the_lists_on_a_real_store() {
    let path = std::env::var("PENGUIN_MAIL_MEASURE_DB")
        .expect("PENGUIN_MAIL_MEASURE_DB names a copy of a store");
    let started = Instant::now();
    let conn = mailrs_store::open_connection(Path::new(&path)).unwrap();
    println!(
        "time open, with any pending migration: {:?}",
        started.elapsed()
    );
    let inbox = ThreadFilter::unified("INBOX");
    let (social, _) = Category::Social.labels();
    let social_inbox = ThreadFilter::unified("INBOX").with_labels(social, &[]);
    time("sidebar counts", || {
        (
            threads::label_counts(&conn).unwrap(),
            flags::mailbox_counts(&conn).unwrap(),
        )
    });
    time("inbox unread", || {
        threads::unread_threads(&conn, &inbox).unwrap()
    });
    time("inbox first page", || {
        threads::list_threads_after(&conn, &inbox, None, 101).unwrap()
    });
    time("social first page", || {
        threads::list_threads_after(&conn, &social_inbox, None, 101).unwrap()
    });
    time("category unread", || {
        threads::category_unread_threads(&conn, &inbox).unwrap()
    });

    let counts = threads::label_counts(&conn).unwrap();
    for account in accounts::list_accounts(&conn).unwrap() {
        for label in labels::list_labels(&conn, account.id).unwrap() {
            println!(
                "answer count {} {}: {:?}",
                account.id,
                label.id,
                counts.account(account.id, &label.id)
            );
        }
    }
    for label in ["INBOX", "STARRED", "SENT", "DRAFT", "MUTE", "UNREAD"] {
        println!("answer unified {label}: {:?}", counts.unified(label));
    }
    println!(
        "answer inbox unread: {}",
        threads::unread_threads(&conn, &inbox).unwrap()
    );
    let first: Vec<String> = threads::list_threads_after(&conn, &social_inbox, None, 101)
        .unwrap()
        .into_iter()
        .map(|t| t.id)
        .collect();
    println!("answer social first page: {first:?}");
    let mut by_category: Vec<(String, i64)> = threads::category_unread_threads(&conn, &inbox)
        .unwrap()
        .into_iter()
        .map(|(c, n)| (c.key().to_string(), n))
        .collect();
    by_category.sort();
    println!("answer category unread: {by_category:?}");
}
