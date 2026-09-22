mod common;

use common::{meta, mixed_mail, store};
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
    // Seven inbox threads, less the trashed one and the one in Spam.
    assert_eq!(
        counts.unified("INBOX"),
        Count {
            threads: 5,
            unread: 3
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
    assert_eq!(unified[&Category::All], 3);
    assert_eq!(unified[&Category::Updates], 1);
    assert_eq!(unified[&Category::Social], 2);
    // The one unread thread without a category label is in the Trash.
    assert_eq!(unified[&Category::Primary], 0);
}

#[test]
fn sender_counts_match_the_query_per_vip_row() {
    let (conn, a, b) = mixed_mail();
    // b1@example.com also writes in ta3, so ta3 has mail from two senders,
    // and a2@example.com writes in the read thread tb2.
    store(
        &conn,
        &[
            meta(a, "b1", "ta3", 350, &["INBOX"]),
            meta(b, "a2", "tb2", 750, &["INBOX"]),
        ],
    );
    let everyone: Vec<String> = ["a2", "A3", "a5", "b1", "b2", "nobody"]
        .iter()
        .map(|id| format!("{id}@Example.com"))
        .collect();
    let counts = threads::sender_counts(&conn, &everyone).unwrap();
    let mut rows: Vec<Vec<String>> = everyone.iter().map(|e| vec![e.clone()]).collect();
    rows.push(everyone.clone());
    rows.push(vec![everyone[1].clone(), everyone[3].clone()]);
    for senders in rows {
        let filter = ThreadFilter::unified("").from_senders(senders.clone());
        assert_eq!(
            counts.unread(&senders),
            threads::unread_threads(&conn, &filter).unwrap(),
            "{senders:?}"
        );
    }
    // ta2, ta3 and tb1 are unread; ta5 is in the Trash and tb2 is read.
    assert_eq!(counts.unread(&everyone), 3);
    // a3 and b1 share ta3, which counts once.
    assert_eq!(counts.unread(&everyone[1..4]), 2);
    assert_eq!(counts.unread(&everyone[5..]), 0);
    assert_eq!(
        threads::sender_counts(&conn, &[])
            .unwrap()
            .unread(&everyone),
        0
    );
}
