//! What the store's public reads answer over a mailbox holding every kind
//! of mail Gmail hands over, recorded before the store moved from labels
//! to mailboxes and keywords. The move rewrites the SQL behind each of
//! these reads and none of their answers, so this compares every answer
//! with the one in `golden/reads.txt`, line by line.
//!
//! `PENGUIN_MAIL_BLESS=1` records the answers again. Only a tree whose
//! answers are known good may do that.

mod common;

use std::fmt::Write as _;
use std::path::Path;

use common::{meta, store};
use mailrs_domain::{AccountId, Category, FlagColor, MailboxKind, MessageMeta, RemoteMailbox};
use mailrs_store::threads::{self, ThreadFilter};
use mailrs_store::{
    accounts, contacts, drafts, flags, follow_ups, labels, mailboxes, messages, newsletters, window,
};
use rusqlite::Connection;

const HOUR: i64 = 60 * 60 * 1000;
const DAY: i64 = 24 * HOUR;
const NOW: i64 = 100 * DAY;

/// Every label id the reads are asked about: each system label, each
/// category, a person's labels listed and unlisted, a label Gmail uses
/// but never lists, and one nobody has.
const LABELS: [&str; 19] = [
    "INBOX",
    "SENT",
    "DRAFT",
    "TRASH",
    "SPAM",
    "IMPORTANT",
    "STARRED",
    "UNREAD",
    "MUTE",
    "CHAT",
    "Label_1",
    "Label_2",
    "Label_9",
    "CATEGORY_PERSONAL",
    "CATEGORY_SOCIAL",
    "CATEGORY_UPDATES",
    "CATEGORY_PROMOTIONS",
    "CATEGORY_FORUMS",
    "nobody",
];

fn listed(id: &str, name: &str, kind: MailboxKind, color: Option<&str>) -> RemoteMailbox {
    RemoteMailbox {
        id: id.into(),
        name: name.into(),
        kind,
        role: mailrs_domain::gmail::role_of(id),
        color: color.map(str::to_string),
        hidden: false,
    }
}

fn newsletter(mut m: MessageMeta) -> MessageMeta {
    m.list_unsubscribe = Some(format!("<https://lists.example.com/leave/{}>", m.id));
    m.one_click = true;
    m
}

/// Two accounts. The first has every system label listed, two of its own
/// labels, a category on most inbox mail, a sent message nobody answered,
/// a draft, a thread whose first message is in the Trash while its reply
/// is in the inbox, starred mail in the Trash and in Spam, a muted thread
/// carrying a label nobody listed, two threads that share a date, and old
/// archived mail the window prunes. The second has fewer labels listed and
/// a thread split between Spam and the Trash.
fn mailbox() -> (Connection, AccountId, AccountId) {
    let conn = mailrs_store::open_in_memory().unwrap();
    let a = accounts::insert_account(&conn, "a@example.com", 0).unwrap();
    let b = accounts::insert_account(&conn, "b@example.com", 0).unwrap();
    let system = |id: &str| listed(id, id, MailboxKind::System, None);
    let mut a_labels: Vec<RemoteMailbox> = [
        "INBOX",
        "SENT",
        "DRAFT",
        "TRASH",
        "SPAM",
        "IMPORTANT",
        "UNREAD",
        "STARRED",
        "CATEGORY_PERSONAL",
        "CATEGORY_SOCIAL",
        "CATEGORY_UPDATES",
        "CATEGORY_PROMOTIONS",
        "CATEGORY_FORUMS",
    ]
    .into_iter()
    .map(system)
    .collect();
    a_labels.push(listed(
        "Label_1",
        "Work",
        MailboxKind::Label,
        Some("#16a766"),
    ));
    a_labels.push(listed("Label_2", "Work/Clients", MailboxKind::Label, None));
    mailboxes::replace_listed(&conn, a, &a_labels).unwrap();
    let b_labels = vec![
        system("INBOX"),
        system("UNREAD"),
        system("STARRED"),
        listed("Label_1", "Home", MailboxKind::Label, Some("#fb4c2f")),
    ];
    mailboxes::replace_listed(&conn, b, &b_labels).unwrap();
    store(
        &conn,
        &[
            meta(
                a,
                "a1",
                "t1",
                NOW - HOUR,
                &["INBOX", "UNREAD", "CATEGORY_PERSONAL", "IMPORTANT"],
            ),
            newsletter(meta(
                a,
                "a2",
                "t2",
                NOW - 2 * HOUR,
                &["INBOX", "CATEGORY_UPDATES"],
            )),
            meta(
                a,
                "a3",
                "t3",
                NOW - 3 * HOUR,
                &["INBOX", "UNREAD", "STARRED", "CATEGORY_SOCIAL"],
            ),
            meta(
                a,
                "a4",
                "t3",
                NOW - 2 * HOUR - HOUR / 2,
                &["INBOX", "CATEGORY_FORUMS"],
            ),
            meta(a, "a5", "t4", NOW - 5 * DAY, &["SENT"]),
            meta(a, "a6", "t5", NOW - 4 * HOUR, &["DRAFT"]),
            meta(a, "a7", "t6", NOW - 6 * HOUR, &["INBOX", "TRASH", "UNREAD"]),
            meta(a, "a8", "t6", NOW - 5 * HOUR, &["INBOX"]),
            meta(a, "a9", "t7", NOW - 7 * HOUR, &["TRASH", "STARRED"]),
            meta(
                a,
                "a10",
                "t8",
                NOW - 8 * HOUR,
                &["SPAM", "UNREAD", "STARRED"],
            ),
            meta(a, "a11", "t9", NOW - 40 * DAY, &["Label_1"]),
            meta(
                a,
                "a12",
                "t10",
                NOW - 9 * HOUR,
                &["INBOX", "MUTE", "Label_9", "CHAT"],
            ),
            newsletter(meta(
                a,
                "a13",
                "t11",
                NOW - 9 * HOUR,
                &["INBOX", "UNREAD", "CATEGORY_PROMOTIONS"],
            )),
            meta(a, "a14", "t12", NOW - 10 * HOUR, &["Label_2", "STARRED"]),
            meta(a, "a15", "t13", NOW - 11 * HOUR, &["SENT", "TRASH"]),
            meta(
                a,
                "a16",
                "t13",
                NOW - 10 * HOUR - HOUR / 2,
                &["INBOX", "Label_1"],
            ),
            meta(
                b,
                "b1",
                "u1",
                NOW - HOUR - HOUR / 2,
                &["INBOX", "UNREAD", "CATEGORY_SOCIAL"],
            ),
            meta(
                b,
                "b2",
                "u2",
                NOW - 3 * HOUR,
                &["INBOX", "STARRED", "Label_1"],
            ),
            meta(
                b,
                "b3",
                "u3",
                NOW - 12 * HOUR,
                &["SPAM", "Label_1", "UNREAD"],
            ),
            meta(b, "b4", "u3", NOW - 11 * HOUR, &["TRASH", "Label_1"]),
            meta(b, "b5", "u4", NOW - 2 * DAY, &["INBOX"]),
        ],
    );
    drafts::remember(&conn, a, "d1", "a6").unwrap();
    flags::set_color(&conn, a, "t3", Some("a3"), Some(FlagColor::Blue)).unwrap();
    flags::set_color(&conn, a, "t7", None, Some(FlagColor::Purple)).unwrap();
    flags::set_color(&conn, b, "u2", None, Some(FlagColor::Green)).unwrap();
    (conn, a, b)
}

fn filters(a: AccountId, b: AccountId) -> Vec<(String, ThreadFilter)> {
    let mut filters: Vec<(String, ThreadFilter)> = Vec::new();
    for label in LABELS.iter().copied().chain([""]) {
        filters.push((format!("unified {label:?}"), ThreadFilter::unified(label)));
    }
    for label in ["INBOX", "Label_1", "Label_2", "TRASH", "SPAM", ""] {
        filters.push((
            format!("account a {label:?}"),
            ThreadFilter::account(a, label),
        ));
        filters.push((
            format!("account b {label:?}"),
            ThreadFilter::account(b, label),
        ));
    }
    for category in Category::ALL {
        let (any, none) = category.categories();
        filters.push((
            format!("inbox {}", category.key()),
            ThreadFilter::unified("INBOX").with_labels(any, none),
        ));
        filters.push((
            format!("account a inbox {}", category.key()),
            ThreadFilter::account(a, "INBOX").with_labels(any, none),
        ));
    }
    for color in [
        FlagColor::Red,
        FlagColor::Blue,
        FlagColor::Green,
        FlagColor::Purple,
    ] {
        filters.push((
            format!("flag {color:?}"),
            ThreadFilter::unified("").with_flag(color),
        ));
    }
    filters.push((
        "inbox flag Red".into(),
        ThreadFilter::unified("INBOX").with_flag(FlagColor::Red),
    ));
    let senders = || vec!["A3@example.com".to_string(), "b2@example.com".to_string()];
    filters.push((
        "senders".into(),
        ThreadFilter::unified("").from_senders(senders()),
    ));
    filters.push((
        "inbox senders".into(),
        ThreadFilter::unified("INBOX").from_senders(senders()),
    ));
    filters.push((
        "named threads".into(),
        ThreadFilter::unified("INBOX").with_threads(vec!["t3".into(), "t6".into(), "u2".into()]),
    ));
    filters.push((
        "account a named threads".into(),
        ThreadFilter::account(a, "").with_threads(vec!["t6".into(), "t13".into()]),
    ));
    filters
}

fn answers() -> String {
    let (conn, a, b) = mailbox();
    let mut out = String::new();
    let mut say = |name: String, value: String| {
        writeln!(out, "{name}: {value}").unwrap();
    };
    for account in [a, b] {
        say(
            format!("labels of {account}"),
            format!("{:?}", labels::list_labels(&conn, account).unwrap()),
        );
    }
    for (name, filter) in filters(a, b) {
        say(
            format!("{name} threads"),
            format!(
                "{:?}",
                threads::list_threads(&conn, &filter, 0, 100).unwrap()
            ),
        );
        say(
            format!("{name} messages"),
            format!(
                "{:?}",
                threads::list_messages(&conn, &filter, 0, 100).unwrap()
            ),
        );
        let first = threads::list_threads_after(&conn, &filter, None, 2).unwrap();
        let next = threads::list_threads_after(&conn, &filter, first.last(), 2).unwrap();
        say(format!("{name} second page"), format!("{next:?}"));
        say(
            format!("{name} count"),
            format!("{}", threads::count_threads(&conn, &filter).unwrap()),
        );
        say(
            format!("{name} unread threads"),
            format!("{}", threads::unread_threads(&conn, &filter).unwrap()),
        );
        say(
            format!("{name} unread messages"),
            format!("{}", threads::unread_messages(&conn, &filter).unwrap()),
        );
    }
    for (name, filter) in [
        ("unified inbox", ThreadFilter::unified("INBOX")),
        ("account a inbox", ThreadFilter::account(a, "INBOX")),
    ] {
        let by_thread = threads::category_unread_threads(&conn, &filter).unwrap();
        let by_message = threads::category_unread_messages(&conn, &filter).unwrap();
        for category in Category::ALL {
            say(
                format!("{name} category {} unread", category.key()),
                format!(
                    "{:?} {:?}",
                    by_thread.get(&category),
                    by_message.get(&category)
                ),
            );
        }
    }
    let counts = threads::label_counts(&conn).unwrap();
    for label in LABELS {
        say(
            format!("count unified {label}"),
            format!("{:?}", counts.unified(label)),
        );
        for account in [a, b] {
            say(
                format!("count {account} {label}"),
                format!("{:?}", counts.account(account, label)),
            );
            let mut held: Vec<String> = messages::labelled(&conn, account, label)
                .unwrap()
                .into_iter()
                .collect();
            held.sort();
            say(format!("labelled {account} {label}"), format!("{held:?}"));
        }
    }
    let every_thread = [
        "t1", "t2", "t3", "t4", "t5", "t6", "t7", "t8", "t9", "t10", "t11", "t12", "t13",
    ];
    for thread in every_thread {
        say(
            format!("thread {thread}"),
            format!("{:?}", threads::get_thread(&conn, a, thread).unwrap()),
        );
        say(
            format!("messages of {thread}"),
            format!("{:?}", messages::thread_messages(&conn, a, thread).unwrap()),
        );
    }
    for thread in ["u1", "u2", "u3", "u4"] {
        say(
            format!("thread {thread}"),
            format!("{:?}", threads::get_thread(&conn, b, thread).unwrap()),
        );
        say(
            format!("messages of {thread}"),
            format!("{:?}", messages::thread_messages(&conn, b, thread).unwrap()),
        );
    }
    for n in 1..=16 {
        let id = format!("a{n}");
        say(
            format!("labels of message {id}"),
            format!("{:?}", messages::labels_of(&conn, a, &id).unwrap()),
        );
    }
    say(
        "by ids".into(),
        format!(
            "{:?}",
            messages::by_ids(&conn, a, &["a13".into(), "a3".into(), "zz".into()]).unwrap()
        ),
    );
    let mut existing: Vec<String> =
        messages::existing_ids(&conn, a, &["a1".into(), "b1".into(), "zz".into()])
            .unwrap()
            .into_iter()
            .collect();
    existing.sort();
    say("existing ids".into(), format!("{existing:?}"));
    for thread in ["t1", "u1", "zz"] {
        say(
            format!("account of {thread}"),
            format!("{:?}", threads::account_of(&conn, thread).unwrap()),
        );
    }
    let colours = flags::counts(&conn).unwrap();
    let mailbox_colours = flags::mailbox_counts(&conn).unwrap();
    for color in FlagColor::ALL {
        say(
            format!("flag {color:?} counts"),
            format!(
                "{:?} {:?}",
                colours.get(&color),
                mailbox_colours.get(&color)
            ),
        );
    }
    say(
        "colours of t3".into(),
        format!("{:?}", flags::colors(&conn, a, "t3", None).unwrap()),
    );
    let senders = threads::sender_counts(
        &conn,
        &[
            "a3@example.com".into(),
            "B1@example.com".into(),
            "nobody@example.com".into(),
        ],
    )
    .unwrap();
    let groups: [&[&str]; 3] = [
        &["a3@example.com"],
        &["b1@example.com"],
        &["a3@example.com", "b1@example.com"],
    ];
    for asked in groups {
        let asked: Vec<String> = asked.iter().map(|s| s.to_string()).collect();
        say(
            format!("unread from {asked:?}"),
            format!("{}", senders.unread(&asked)),
        );
    }
    say(
        "draft of a6".into(),
        format!("{:?}", drafts::draft_of(&conn, a, "a6").unwrap()),
    );
    say(
        "draft of a1".into(),
        format!("{:?}", drafts::draft_of(&conn, a, "a1").unwrap()),
    );
    say(
        "waiting".into(),
        format!("{:?}", follow_ups::waiting(&conn, NOW).unwrap()),
    );
    say(
        "waiting count".into(),
        format!("{}", follow_ups::waiting_count(&conn, NOW).unwrap()),
    );
    say(
        "newsletters".into(),
        format!("{:?}", newsletters::list(&conn, a, NOW - 30 * DAY).unwrap()),
    );
    say(
        "correspondents".into(),
        format!("{:?}", contacts::list_correspondents(&conn).unwrap()),
    );
    say(
        "pruned".into(),
        format!(
            "{:?}",
            window::prune_window(&conn, a, NOW - 30 * DAY).unwrap()
        ),
    );
    say(
        "after pruning".into(),
        format!(
            "{:?}",
            threads::list_threads(&conn, &ThreadFilter::account(a, ""), 0, 100).unwrap()
        ),
    );
    out
}

#[test]
fn every_read_answers_as_it_did_before_the_move() {
    let answers = answers();
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/reads.txt");
    if std::env::var_os("PENGUIN_MAIL_BLESS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &answers).unwrap();
        return;
    }
    let recorded = std::fs::read_to_string(&path)
        .expect("tests/golden/reads.txt, recorded with PENGUIN_MAIL_BLESS=1");
    for (line, (now, then)) in answers.lines().zip(recorded.lines()).enumerate() {
        assert_eq!(now, then, "line {} of golden/reads.txt", line + 1);
    }
    assert_eq!(answers.lines().count(), recorded.lines().count());
}
