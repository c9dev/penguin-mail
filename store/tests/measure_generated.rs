//! Times the calendar and contact reads on a generated store: 5,000 events
//! with three guests each, and 50,000 messages, a tenth of them sent.
//! Ignored by default; run it in release with `--ignored --nocapture` to
//! compare a change with what came before.

use std::time::{Duration, Instant};

use mailrs_domain::calendar::{Access, Calendar, Event, Guest};
use mailrs_domain::{Address, MessageMeta};
use mailrs_store::calendar::{self, CalendarScope};
use mailrs_store::messages::{self, Change};
use mailrs_store::{accounts, contacts};

const HOUR: i64 = 60 * 60 * 1000;
const START: i64 = 1_792_368_000_000;

/// Runs `read` a few times to warm the page cache, then 11 more, and
/// prints the median and the best.
fn time<T>(name: &str, mut read: impl FnMut() -> T) {
    for _ in 0..2 {
        std::hint::black_box(read());
    }
    let mut times: Vec<Duration> = (0..11)
        .map(|_| {
            let started = Instant::now();
            std::hint::black_box(read());
            started.elapsed()
        })
        .collect();
    times.sort();
    println!("time {name}: median {:?}, best {:?}", times[5], times[0]);
}

fn calendar_named(id: &str) -> Calendar {
    Calendar {
        id: id.into(),
        name: id.into(),
        color: "#3584e4".into(),
        access: Access::Owner,
        zone: "UTC".into(),
        primary: id == "primary",
        shown: true,
        hidden: false,
        reminders: Vec::new(),
        all_day_reminders: None,
        supports_teams: false,
    }
}

fn event(n: i64) -> Event {
    Event {
        calendar: "primary".into(),
        id: format!("e{n}"),
        uid: format!("E{n}@example.com"),
        etag: "\"1\"".into(),
        start: START + n * HOUR,
        end: START + n * HOUR + HOUR,
        zone: "UTC".into(),
        title: format!("Event {n}"),
        busy: true,
        guests: (0..3)
            .map(|g| Guest {
                email: format!("guest{}@example.com", (n + g) % 400),
                name: Some(format!("Guest {g}")),
                answer: None,
                organizer: g == 0,
                me: g == 2,
            })
            .collect(),
        ..Event::default()
    }
}

fn message(account_id: i64, n: i64) -> MessageMeta {
    let sent = n % 10 == 0;
    let person = |k: i64| Address {
        name: Some(format!("Person {k}")),
        email: format!("person{k}@example.org"),
    };
    let mut meta = MessageMeta {
        account_id,
        id: format!("m{n}"),
        thread_id: format!("t{}", n / 3),
        rfc822_msgid: Some(format!("<m{n}@example.org>")),
        from: Some(if sent {
            Address {
                name: None,
                email: "me@example.com".into(),
            }
        } else {
            person(n % 3_000)
        }),
        to: vec![if sent {
            person(n % 700)
        } else {
            Address {
                name: None,
                email: "me@example.com".into(),
            }
        }],
        cc: vec![person(n % 50)],
        subject: format!("Subject {n}"),
        date: START - n * 60_000,
        snippet: String::new(),
        size: 2_000,
        has_attachments: false,
        held: Default::default(),
        roles: vec![],
        list_unsubscribe: None,
        one_click: false,
    };
    let labels: Vec<String> = vec![if sent { "SENT".into() } else { "INBOX".into() }];
    mailrs_gmail::labels::set_label_ids(&mut meta, &labels);
    meta
}

#[test]
#[ignore = "a measurement; run with --ignored --nocapture in release"]
fn calendar_and_contact_reads_on_a_generated_store() {
    let dir = tempfile::tempdir().unwrap();
    let mut conn = mailrs_store::open_connection(&dir.path().join("mail.db")).unwrap();
    let id = accounts::insert_account(&conn, "me@example.com", 0).unwrap();
    for (key, role) in mailrs_gmail::labels::ROLES {
        mailrs_store::mailboxes::upsert(
            &conn,
            id,
            &mailrs_domain::RemoteMailbox {
                id: key.into(),
                name: key.into(),
                kind: mailrs_domain::MailboxKind::System,
                role: Some(role),
                color: None,
                hidden: false,
            },
        )
        .unwrap();
    }
    calendar::save_calendars(&conn, id, &[calendar_named("primary")]).unwrap();
    let events: Vec<Event> = (0..5_000).map(event).collect();
    let tx = conn.transaction().unwrap();
    time("save 500 events", || {
        calendar::save_events(&tx, id, &events[..500], 1).unwrap()
    });
    calendar::save_events(&tx, id, &events, 1).unwrap();
    for chunk in (0..50_000).collect::<Vec<i64>>().chunks(1_000) {
        let changes: Vec<Change> = chunk
            .iter()
            .map(|&n| Change::Upsert {
                meta: Box::new(message(id, n)),
                generation: 1,
            })
            .collect();
        messages::apply(&tx, id, &changes).unwrap();
    }
    tx.commit().unwrap();

    time("a week of occurrences", || {
        calendar::occurrences(
            &conn,
            &[id],
            START,
            START + 7 * 24 * HOUR,
            CalendarScope::Shown,
        )
        .unwrap()
    });
    time("a month of occurrences", || {
        calendar::occurrences(
            &conn,
            &[id],
            START,
            START + 30 * 24 * HOUR,
            CalendarScope::Shown,
        )
        .unwrap()
    });
    time("invitations waiting", || {
        calendar::waiting(&conn, &[id], START, 50).unwrap()
    });
    time("search events", || {
        calendar::search(&conn, &[id], "event 1", START, CalendarScope::Shown, 50).unwrap()
    });
    time("events by uid", || {
        calendar::with_uid(
            &conn,
            id,
            "e4000@EXAMPLE.com",
            START,
            START + 400 * 24 * HOUR,
        )
        .unwrap()
    });
    time("series by uid", || {
        calendar::series_with_uid(&conn, id, "e4000@example.com").unwrap()
    });
    // The store's writer counts new mail into the kept tally when idle;
    // each step below does what it would, then reads.
    time("contacts, every message counted again", || {
        conn.execute("UPDATE contact_marks SET stale = 1", [])
            .unwrap();
        contacts::catch_up(&conn).unwrap();
    });
    time("contacts, nothing new", || {
        contacts::suggestions(&conn).unwrap()
    });
    let put = |meta: MessageMeta| {
        messages::apply(
            &conn,
            id,
            &[Change::Upsert {
                meta: Box::new(meta),
                generation: 1,
            }],
        )
        .unwrap();
        contacts::catch_up(&conn).unwrap();
        contacts::suggestions(&conn).unwrap()
    };
    let mut n = 50_000;
    time("contacts after a new Inbox message", || {
        n += if n % 10 == 9 { 2 } else { 1 };
        put(message(id, n))
    });
    let mut sent = 50_000;
    time("contacts after a new Sent message", || {
        sent += 10;
        put(message(id, sent))
    });
    time("contacts after a Sent message read again", || {
        put(message(id, 0))
    });
    time("contacts while the kept tally is stale", || {
        conn.execute("UPDATE contact_marks SET stale = 1", [])
            .unwrap();
        contacts::suggestions(&conn).unwrap()
    });
}
