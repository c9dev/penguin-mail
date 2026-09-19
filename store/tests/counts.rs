mod common;

use common::mixed_mail;
use mailrs_domain::{Category, FlagColor};
use mailrs_store::flags;
use mailrs_store::threads::{self, Count, ThreadFilter};

#[test]
fn label_counts_match_the_query_per_mailbox() {
    let (conn, a, b) = mixed_mail();
    let counts = threads::label_counts(&conn).unwrap();
    for account in [a, b] {
        for label in ["INBOX", "SENT", "UNREAD", "STARRED", "CATEGORY_UPDATES"] {
            let filter = ThreadFilter::account(account, label);
            assert_eq!(
                counts.account(account, label),
                Count {
                    threads: threads::count_threads(&conn, &filter).unwrap(),
                    unread: threads::unread_threads(&conn, &filter).unwrap(),
                },
                "{account} {label}"
            );
        }
    }
    for label in ["INBOX", "SENT", "CATEGORY_SOCIAL"] {
        let filter = ThreadFilter::unified(label);
        assert_eq!(
            counts.unified(label),
            Count {
                threads: threads::count_threads(&conn, &filter).unwrap(),
                unread: threads::unread_threads(&conn, &filter).unwrap(),
            },
            "{label}"
        );
    }
    assert_eq!(
        counts.unified("INBOX"),
        Count {
            threads: 7,
            unread: 4
        }
    );
    assert_eq!(
        counts.account(a, "SENT"),
        Count {
            threads: 1,
            unread: 0
        }
    );
    assert_eq!(counts.account(a, "Label_nobody_has"), Count::default());
}

#[test]
fn flag_mailbox_counts_match_the_query_per_colour() {
    let (conn, _, b) = mixed_mail();
    flags::set_color(&conn, b, "tb2", None, Some(FlagColor::Blue)).unwrap();
    let counts = flags::mailbox_counts(&conn).unwrap();
    for color in FlagColor::ALL {
        let filter = ThreadFilter::unified("").with_flag(color);
        assert_eq!(
            counts.get(&color).copied().unwrap_or(0),
            threads::count_threads(&conn, &filter).unwrap(),
            "{color:?}"
        );
    }
    // tb3 is in Spam, so only tb2 counts, and it is blue now.
    assert_eq!(counts.get(&FlagColor::Blue), Some(&1));
    assert_eq!(counts.get(&FlagColor::Red), None);
}

#[test]
fn category_counts_match_the_query_per_category() {
    let (conn, a, _) = mixed_mail();
    for filter in [
        ThreadFilter::unified("INBOX"),
        ThreadFilter::account(a, "INBOX"),
    ] {
        let threaded = threads::category_unread_threads(&conn, &filter).unwrap();
        let single = threads::category_unread_messages(&conn, &filter).unwrap();
        for category in Category::ALL {
            let (any, none) = category.labels();
            let narrowed = filter.clone().with_labels(any, none);
            assert_eq!(
                threaded[&category],
                threads::unread_threads(&conn, &narrowed).unwrap(),
                "{category:?}"
            );
            assert_eq!(
                single[&category],
                threads::unread_messages(&conn, &narrowed).unwrap(),
                "{category:?}"
            );
        }
    }
    let unified = threads::category_unread_threads(&conn, &ThreadFilter::unified("INBOX")).unwrap();
    assert_eq!(unified[&Category::All], 4);
    assert_eq!(unified[&Category::Updates], 1);
    assert_eq!(unified[&Category::Social], 2);
    assert_eq!(unified[&Category::Primary], 1);
}
