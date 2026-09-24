mod common;

use common::{meta, mixed_mail, store};
use mailrs_domain::gmail::set_of as set;
use mailrs_domain::{Category, FlagColor};
use mailrs_store::flags;
use mailrs_store::threads::{self, Count, ThreadFilter};

#[test]
fn mail_counts_match_the_query_per_mailbox() {
    let (conn, a, b) = mixed_mail();
    let counts = threads::mail_counts(&conn).unwrap();
    for account in [a, b] {
        for label in ["INBOX", "SENT", "UNREAD", "STARRED", "CATEGORY_UPDATES"] {
            let filter = ThreadFilter::account(account, set(label));
            assert_eq!(
                counts.account(account, &set(label)),
                Count {
                    threads: threads::count_threads(&conn, &filter).unwrap(),
                    unread: threads::unread_threads(&conn, &filter).unwrap(),
                },
                "{account} {label}"
            );
        }
    }
    for label in ["INBOX", "SENT", "CATEGORY_SOCIAL"] {
        let filter = ThreadFilter::unified(set(label));
        assert_eq!(
            counts.unified(&set(label)),
            Count {
                threads: threads::count_threads(&conn, &filter).unwrap(),
                unread: threads::unread_threads(&conn, &filter).unwrap(),
            },
            "{label}"
        );
    }
    // Seven inbox threads, less the trashed one and the one in Spam.
    assert_eq!(
        counts.unified(&set("INBOX")),
        Count {
            threads: 5,
            unread: 3
        }
    );
    assert_eq!(
        counts.account(a, &set("SENT")),
        Count {
            threads: 1,
            unread: 0
        }
    );
    assert_eq!(
        counts.account(a, &set("Label_nobody_has")),
        Count::default()
    );
}

/// A label keeps a thread while one of its messages carrying the label is
/// outside the Trash and Spam, whatever the rest of the thread carries.
#[test]
fn mail_counts_follow_the_messages_of_a_partly_trashed_thread() {
    let (conn, a, b) = mixed_mail();
    store(
        &conn,
        &[
            meta(a, "a6", "ta6", 610, &["INBOX", "UNREAD", "TRASH"]),
            meta(a, "a6r", "ta6", 620, &["INBOX", "UNREAD"]),
            meta(a, "a7", "ta7", 710, &["Label_x", "TRASH"]),
            meta(a, "a7r", "ta7", 720, &["TRASH", "UNREAD"]),
            meta(a, "a8", "ta8", 810, &["SENT", "TRASH"]),
            meta(a, "a8r", "ta8", 820, &["INBOX", "Label_x"]),
            meta(b, "b9", "tb9", 910, &["Label_y", "SPAM", "UNREAD"]),
            meta(b, "b9r", "tb9", 920, &["Label_y", "TRASH"]),
        ],
    );
    let counts = threads::mail_counts(&conn).unwrap();
    for account in [a, b] {
        for label in [
            "INBOX", "SENT", "UNREAD", "TRASH", "SPAM", "Label_x", "Label_y",
        ] {
            let filter = ThreadFilter::account(account, set(label));
            assert_eq!(
                counts.account(account, &set(label)),
                Count {
                    threads: threads::count_threads(&conn, &filter).unwrap(),
                    unread: threads::unread_threads(&conn, &filter).unwrap(),
                },
                "{account} {label}"
            );
        }
    }
    assert_eq!(
        counts.account(a, &set("INBOX")),
        Count {
            threads: 5,
            unread: 3
        }
    );
    assert_eq!(
        counts.account(a, &set("Label_x")),
        Count {
            threads: 1,
            unread: 0
        }
    );
    assert_eq!(
        counts.account(a, &set("SENT")),
        Count {
            threads: 1,
            unread: 0
        }
    );
    assert_eq!(counts.account(b, &set("Label_y")), Count::default());
    // Its reply in the Trash is not spam, so the thread shows there.
    assert_eq!(
        counts.account(b, &set("TRASH")),
        Count {
            threads: 1,
            unread: 1
        }
    );
}

#[test]
fn flag_mailbox_counts_match_the_query_per_colour() {
    let (conn, _, b) = mixed_mail();
    flags::set_color(&conn, b, "tb2", None, Some(FlagColor::Blue)).unwrap();
    let counts = flags::mailbox_counts(&conn).unwrap();
    for color in FlagColor::ALL {
        let filter = ThreadFilter::everything().with_flag(color);
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

/// Trashing one message of a starred thread leaves the thread in the Flag
/// mailbox while a message outside the Trash remains, and its count agrees.
#[test]
fn a_flag_count_keeps_a_thread_whose_other_message_is_trashed() {
    let (conn, a, _) = mixed_mail();
    store(
        &conn,
        &[
            meta(a, "s1", "ts", 910, &["INBOX", "STARRED"]),
            meta(a, "s2", "ts", 920, &["INBOX", "TRASH"]),
            meta(a, "g1", "tg", 930, &["STARRED", "TRASH"]),
        ],
    );
    let counts = flags::mailbox_counts(&conn).unwrap();
    let filter = ThreadFilter::everything().with_flag(FlagColor::Red);
    assert_eq!(threads::count_threads(&conn, &filter).unwrap(), 2);
    assert_eq!(counts.get(&FlagColor::Red), Some(&2));
}

#[test]
fn category_counts_match_the_query_per_category() {
    let (conn, a, _) = mixed_mail();
    for filter in [
        ThreadFilter::unified(set("INBOX")),
        ThreadFilter::account(a, set("INBOX")),
    ] {
        let threaded = threads::category_unread_threads(&conn, &filter).unwrap();
        let single = threads::category_unread_messages(&conn, &filter).unwrap();
        for category in Category::ALL {
            let (any, none) = category.categories();
            let narrowed = filter.clone().with_categories(any, none);
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
    let unified =
        threads::category_unread_threads(&conn, &ThreadFilter::unified(set("INBOX"))).unwrap();
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
        let filter = ThreadFilter::everything().from_senders(senders.clone());
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
