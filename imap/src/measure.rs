//! What one connection holds and spends on each path, measured against a
//! scripted server at the limits the guard allows, beside what a real
//! server of 200,000 messages sends. They print what they measure instead
//! of asserting a bound, and run by hand in a release build:
//!
//! ```sh
//! cargo test -p mailrs-imap --release --lib -- --ignored --nocapture --test-threads=1 measure
//! ```
//!
//! The server task runs on a worker thread and the client on the test
//! thread, so the thread-local heap count in `testing` sees the client's
//! allocations alone: async-imap's buffer, the parsed responses, what the
//! reader keeps and what the command returns.

use std::time::{Duration, Instant};

use tokio::io::DuplexStream;

use crate::connection::Conn;
use crate::guard::{COMMAND_BYTES, IDLE_BYTES, MAX_LINE, MAX_LITERAL, SEARCH_BYTES, SELECT_BYTES};
use crate::testing::{HeapMark, pipe, selected, server};
use crate::{Login, Since, UidSet};

const GREETING: &str = "* OK [CAPABILITY IMAP4rev1 AUTH=PLAIN] ready";
const ALL: &str = "IDLE QRESYNC CONDSTORE MOVE UIDPLUS SPECIAL-USE ENABLE";
const MIB: f64 = (1 << 20) as f64;

/// A signed-in connection to a server that answers SELECT as `selected`
/// does and hands every other command to `rest`.
async fn connect(mut rest: impl FnMut(&str) -> Vec<String> + Send + 'static) -> Conn<DuplexStream> {
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let stream = pipe(
        GREETING,
        server(ALL, seen, move |command| match command {
            c if c.starts_with("SELECT") => selected(),
            other => rest(other),
        }),
    );
    Conn::login(stream, false, &Login::new("ann@example.com", "pw"))
        .await
        .unwrap()
}

/// Answers the first command starting with `verb` with `lines`, built
/// before the measurement starts, and anything else with OK.
fn once(
    verb: &'static str,
    lines: Vec<String>,
) -> impl FnMut(&str) -> Vec<String> + Send + 'static {
    let mut lines = Some(lines);
    move |command: &str| match command.starts_with(verb) {
        true => lines.take().unwrap_or_default(),
        false => vec!["{tag} OK".into()],
    }
}

fn report(what: &str, mark: &HeapMark, started: Instant) {
    eprintln!(
        "{what}: peak heap {:.2} MiB ({} bytes), {:.3} s",
        mark.peak() as f64 / MIB,
        mark.peak(),
        started.elapsed().as_secs_f64()
    );
}

fn fetch_line(uid: u32, attributes: &str) -> String {
    format!("* {uid} FETCH (UID {uid} {attributes})")
}

/// A body literal of `len` bytes as the server sends it, with the FETCH
/// line before it and the tagged answer after.
fn body_answer(len: usize) -> Vec<String> {
    let mut body = String::with_capacity(len + 1);
    body.extend(std::iter::repeat_n('a', len));
    body.push(')');
    vec![
        format!("* 1 FETCH (UID 5 BODY[] {{{len}}}"),
        body,
        "{tag} OK".into(),
    ]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[ignore = "a measurement, run by hand in a release build"]
async fn measure_body_fetch() {
    let largest = usize::try_from(MAX_LITERAL).unwrap();
    let mut conn = connect(once("UID FETCH", body_answer(largest))).await;
    conn.select("INBOX", None).await.unwrap();
    let mark = HeapMark::start();
    let started = Instant::now();
    let body = conn.body("INBOX", 5, "").await.unwrap().unwrap();
    report(
        "body, hostile: the largest literal, 128 MiB",
        &mark,
        started,
    );
    assert_eq!(body.len(), largest);
    drop(body);

    let past = format!("* 1 FETCH (UID 5 BODY[] {{{}}}", largest + 1);
    let mut conn = connect(once("UID FETCH", vec![past, "abc".into()])).await;
    conn.select("INBOX", None).await.unwrap();
    let mark = HeapMark::start();
    let started = Instant::now();
    let answer = tokio::time::timeout(Duration::from_secs(5), conn.body("INBOX", 5, "")).await;
    report(
        "body, hostile: one byte past the largest literal",
        &mark,
        started,
    );
    assert!(matches!(answer, Ok(Err(crate::ImapError::Protocol(_)))));

    let real = 25 << 20;
    let mut conn = connect(once("UID FETCH", body_answer(real))).await;
    conn.select("INBOX", None).await.unwrap();
    let mark = HeapMark::start();
    let started = Instant::now();
    let body = conn.body("INBOX", 5, "").await.unwrap().unwrap();
    report("body, real: a 25 MiB message", &mark, started);
    assert_eq!(body.len(), real);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[ignore = "a measurement, run by hand in a release build"]
async fn measure_other_commands() {
    let ten: Vec<String> = (0..10).map(|i| format!("k{i}")).collect();
    let hostile: Vec<String> = (1..=100_000u32)
        .map(|uid| fetch_line(uid, &format!("FLAGS ({}) MODSEQ (9)", ten.join(" "))))
        .chain(["{tag} OK".to_string()])
        .collect();
    let mut conn = connect(once("UID FETCH", hostile)).await;
    conn.select("INBOX", None).await.unwrap();
    let mark = HeapMark::start();
    let started = Instant::now();
    let flags = conn
        .flags("INBOX", &UidSet::from_uid(1), None)
        .await
        .unwrap();
    report(
        "flags, hostile: 100,000 answers of 10 flags, both caps met",
        &mark,
        started,
    );
    assert_eq!(flags.len(), 100_000);
    drop(flags);

    // The costliest lines a server can send: status lines just under the
    // line cap, which async-imap parses again on each 4 KiB read until
    // the line ends, as many as the command's budget holds.
    let junk = format!("* OK {}", "x".repeat(MAX_LINE - 8));
    let count = usize::try_from(COMMAND_BYTES).unwrap() / (junk.len() + 2) - 1;
    let lines: Vec<String> = std::iter::repeat_n(junk, count)
        .chain(["{tag} OK".to_string()])
        .collect();
    let mut conn = connect(once("UID FETCH", lines)).await;
    conn.select("INBOX", None).await.unwrap();
    let mark = HeapMark::start();
    let started = Instant::now();
    let flags = conn
        .flags("INBOX", &UidSet::from_uid(1), None)
        .await
        .unwrap();
    report(
        &format!(
            "flags, hostile: {count} status lines of {} bytes, just under the budget",
            MAX_LINE - 2
        ),
        &mark,
        started,
    );
    assert!(flags.is_empty());

    let real: Vec<String> = (1..=100_000u32)
        .map(|uid| fetch_line(uid, "FLAGS (\\Seen \\Answered) MODSEQ (9)"))
        .chain(["{tag} OK".to_string()])
        .collect();
    let mut conn = connect(once("UID FETCH", real)).await;
    conn.select("INBOX", None).await.unwrap();
    let mark = HeapMark::start();
    let started = Instant::now();
    let flags = conn
        .flags("INBOX", &UidSet::from_uid(1), None)
        .await
        .unwrap();
    report(
        "flags, real: a window of 100,000 messages with two flags each",
        &mark,
        started,
    );
    assert_eq!(flags.len(), 100_000);
    drop(flags);

    let header = format!("From: a@b.pt\r\nSubject: {}\r\n\r\n", "x".repeat(700));
    let headers: Vec<String> = (1..=1_000u32)
        .map(|uid| {
            fetch_line(
                uid,
                &format!(
                    "FLAGS (\\Seen) INTERNALDATE \"17-Feb-2026 10:00:00 +0100\" RFC822.SIZE 4201 MODSEQ (9) BODY[HEADER.FIELDS (FROM SUBJECT)] {{{}}}\r\n{header}",
                    header.len()
                ),
            )
        })
        .chain(["{tag} OK".to_string()])
        .collect();
    let mut conn = connect(once("UID FETCH", headers)).await;
    conn.select("INBOX", None).await.unwrap();
    let mark = HeapMark::start();
    let started = Instant::now();
    let fetched = conn
        .headers("INBOX", &UidSet::range(1, 1_000))
        .await
        .unwrap();
    report("headers, real: a window of 1,000 messages", &mark, started);
    assert_eq!(fetched.len(), 1_000);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[ignore = "a measurement, run by hand in a release build"]
async fn measure_idle() {
    let line = "* 1 EXISTS";
    let count = usize::try_from(IDLE_BYTES).unwrap() / (line.len() + 2) - 100;
    let hostile: Vec<String> = std::iter::repeat_n(line.to_string(), count).collect();
    let conn = connect(once("IDLE", hostile)).await;
    let mark = HeapMark::start();
    let started = Instant::now();
    let (conn, woke) = conn.idle("INBOX", Duration::from_secs(60)).await.unwrap();
    report(
        &format!("idle, hostile: {count} news lines, just under the budget"),
        &mark,
        started,
    );
    assert_eq!(woke, crate::Woke::Changed);
    drop(conn);

    let real = vec!["* 4 EXISTS".to_string(), "* 1 RECENT".into()];
    let conn = connect(once("IDLE", real)).await;
    let mark = HeapMark::start();
    let started = Instant::now();
    let (_, woke) = conn.idle("INBOX", Duration::from_secs(60)).await.unwrap();
    report("idle, real: one new message", &mark, started);
    assert_eq!(woke, crate::Woke::Changed);
}

/// A VANISHED line of `bytes` bytes, its UIDs from `uids`.
fn vanished_line(bytes: usize, mut uids: impl Iterator<Item = u32>) -> String {
    let mut line = String::from("* VANISHED (EARLIER) ");
    let mut first = true;
    while line.len() < bytes {
        if !first {
            line.push(',');
        }
        first = false;
        line.push_str(&uids.next().unwrap().to_string());
    }
    line
}

/// Selects with QRESYNC against a server whose SELECT answer is `answer`.
async fn select_since(answer: Vec<String>, known: Option<UidSet>) -> (HeapMark, Instant, usize) {
    let since = Since {
        uidvalidity: 7,
        modseq: 80,
        known,
    };
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let stream = pipe(GREETING, server(ALL, seen, once("SELECT", answer)));
    let mut conn = Conn::login(stream, false, &Login::new("ann@example.com", "pw"))
        .await
        .unwrap();
    let mark = HeapMark::start();
    let started = Instant::now();
    let state = conn.select("INBOX", Some(&since)).await.unwrap();
    (
        mark,
        started,
        state.vanished.ranges().len() + state.changed.len(),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[ignore = "a measurement, run by hand in a release build"]
async fn measure_select_with_vanished() {
    // Lines just under the line cap, as many as fit the 4 MiB budget
    // beside SELECT's own answer.
    let budget = usize::try_from(SELECT_BYTES).unwrap();
    let line_bytes = MAX_LINE - 16;
    let lines = (budget - 4_096) / (line_bytes + 2);
    let repeated: Vec<String> = (0..lines)
        .map(|_| vanished_line(line_bytes, std::iter::repeat(1)))
        .chain(selected())
        .collect();
    let (mark, started, kept) = select_since(repeated, None).await;
    report(
        &format!("select, hostile: {lines} VANISHED lines of {line_bytes} bytes repeating one UID"),
        &mark,
        started,
    );
    assert_eq!(kept, 1);

    let mut odd = (0u32..).map(|i| i * 2 + 1);
    let distinct: Vec<String> = (0..lines)
        .map(|_| vanished_line(line_bytes, &mut odd))
        .chain(selected())
        .collect();
    let (mark, started, kept) = select_since(distinct, None).await;
    report(
        &format!(
            "select, hostile: {lines} VANISHED lines of {line_bytes} bytes of distinct UIDs ({kept} ranges kept)"
        ),
        &mark,
        started,
    );

    // FETCH lines of 64 flags each, the most a message may carry, as
    // many as the budget holds: what a QRESYNC SELECT keeps at most.
    let flags: Vec<String> = (0..crate::parse::MAX_FLAGS)
        .map(|i| format!("k{i}"))
        .collect();
    let line = fetch_line(1, &format!("FLAGS ({}) MODSEQ (91)", flags.join(" ")));
    // Later lines carry longer UIDs, so leave them room.
    let count = (budget - 4_096) / (line.len() + 12);
    let flagged: Vec<String> = (1..=count as u32)
        .map(|uid| fetch_line(uid, &format!("FLAGS ({}) MODSEQ (91)", flags.join(" "))))
        .chain(selected())
        .collect();
    let (mark, started, kept) = select_since(flagged, None).await;
    report(
        &format!(
            "select, hostile: {count} changed messages of 64 flags each, just under the budget ({kept} kept)"
        ),
        &mark,
        started,
    );

    // A mailbox of 200,000 messages after a week away: a tenth expunged
    // one by one, and as many flag changes as the budget holds.
    let changed = 75_000u32;
    let mut real = vec![format!(
        "* VANISHED (EARLIER) {}",
        (1..=20_000u32)
            .map(|i| (i * 10).to_string())
            .collect::<Vec<_>>()
            .join(",")
    )];
    real.extend((1..=changed).map(|uid| fetch_line(uid, "FLAGS (\\Seen) MODSEQ (91)")));
    real.extend(selected());
    let bytes: usize = real.iter().map(|l| l.len() + 2).sum();
    let (mark, started, kept) = select_since(real, Some(UidSet::range(1, 200_000))).await;
    report(
        &format!(
            "select, real: 20,000 vanished and {changed} changed of 200,000 ({bytes} bytes, {kept} kept)"
        ),
        &mark,
        started,
    );
}

fn search_line(uids: impl Iterator<Item = u32>, bytes: usize) -> String {
    let mut line = String::from("* SEARCH");
    for uid in uids {
        let next = uid.to_string();
        if line.len() + 1 + next.len() > bytes {
            break;
        }
        line.push(' ');
        line.push_str(&next);
    }
    line
}

async fn search(line: String) -> (HeapMark, Instant, usize) {
    let mut conn = connect(once("UID SEARCH", vec![line, "{tag} OK".into()])).await;
    conn.select("INBOX", None).await.unwrap();
    let mark = HeapMark::start();
    let started = Instant::now();
    let uids = conn.search("INBOX", "ALL").await.unwrap();
    (mark, started, uids.len())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[ignore = "a measurement, run by hand in a release build"]
async fn measure_search() {
    // The tagged answer and the CRLF share the budget with the line.
    let bytes = usize::try_from(SEARCH_BYTES).unwrap() - 100;
    let line = search_line(std::iter::repeat(1), bytes);
    let uids = (line.len() - 8) / 2;
    let (mark, started, kept) = search(line).await;
    report(
        &format!("search, hostile: one line of {uids} repeats of UID 1, just under the budget"),
        &mark,
        started,
    );
    assert_eq!(kept, 1);

    let line = search_line(1_000_000.., bytes);
    let (mark, started, kept) = search(line).await;
    report(
        &format!("search, hostile: one line of {kept} distinct UIDs, just under the budget"),
        &mark,
        started,
    );

    let line = search_line(1..=200_000, bytes);
    let (mark, started, kept) = search(line).await;
    report(
        "search, real: every UID of a 200,000-message mailbox",
        &mark,
        started,
    );
    assert_eq!(kept, 200_000);

    let line = search_line(150_001..=200_000, bytes);
    let (mark, started, kept) = search(line).await;
    report("search, real: a window of 50,000 UIDs", &mark, started);
    assert_eq!(kept, 50_000);
}
