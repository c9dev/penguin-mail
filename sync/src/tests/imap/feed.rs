use mailrs_domain::ChangeEvent;
use mailrs_imap::{ImapError, UidSet};

use super::{adapter, days_ago, fill, message, offering};
use crate::fake::FakeImap;
use crate::tests::heap::HeapMark;
use crate::tests::{ImapHarness, fake_settings, imap_harness_on};
use crate::{MailBackend, RemoteChange};

/// An account on `imap` holding one unread message, with its window
/// loaded.
async fn synced_on(imap: FakeImap) -> ImapHarness {
    imap.deliver_flagged("INBOX", &message("a", "Kites", ""), &[], days_ago(1));
    let h = imap_harness_on(imap, fake_settings()).await;
    h.bootstrap().await;
    h
}

/// New mail, flags set and cleared, and an expunge, all made by another
/// client, reach the store.
async fn changes_elsewhere_reach_the_store(h: ImapHarness) {
    h.imap.deliver_flagged("INBOX", &message("b", "Moss", ""), &[], days_ago(0));
    h.sync.incremental().await.unwrap();
    assert_eq!(h.ids().await, ["INBOX/1001/1", "INBOX/1001/2"]);
    assert!(h.drain().iter().any(|e| matches!(
        e,
        ChangeEvent::NewMail { message_ids, .. } if message_ids == &["INBOX/1001/2".to_string()]
    )));

    h.imap.set_flags("INBOX", 1, &["\\Seen", "\\Flagged"]);
    h.sync.incremental().await.unwrap();
    let a = h.stored("INBOX/1001/1").await.unwrap();
    assert!(!a.is_unread() && a.is_flagged());

    h.imap.set_flags("INBOX", 1, &[]);
    h.sync.incremental().await.unwrap();
    let a = h.stored("INBOX/1001/1").await.unwrap();
    assert!(a.is_unread() && !a.is_flagged());

    h.imap.remote_expunge("INBOX", 2);
    h.sync.incremental().await.unwrap();
    assert_eq!(h.ids().await, ["INBOX/1001/1"]);
}

#[tokio::test]
async fn under_qresync_changes_elsewhere_reach_the_store() {
    let h = synced_on(offering(|_| {})).await;
    changes_elsewhere_reach_the_store(h).await;
}

#[tokio::test]
async fn with_condstore_alone_changes_elsewhere_reach_the_store() {
    let h = synced_on(offering(|c| c.qresync = false)).await;
    changes_elsewhere_reach_the_store(h).await;
}

#[tokio::test]
async fn with_neither_changes_elsewhere_reach_the_store() {
    let h = synced_on(
        offering(|c| {
            c.qresync = false;
            c.condstore = false;
        }),
    )
    .await;
    changes_elsewhere_reach_the_store(h).await;
}

#[tokio::test]
async fn with_neither_a_quiet_look_announces_nothing() {
    let h = synced_on(
        offering(|c| {
            c.qresync = false;
            c.condstore = false;
        }),
    )
    .await;
    h.sync.incremental().await.unwrap();
    h.drain();

    h.sync.incremental().await.unwrap();

    assert!(
        !h.drain()
            .iter()
            .any(|e| matches!(e, ChangeEvent::ThreadsChanged { .. })),
        "a look that finds the flags as they were redraws no list"
    );
}

/// A QRESYNC SELECT that answers past its byte budget drops the
/// connection; the look falls back to a plain SELECT, once, and still
/// finds what changed, through CONDSTORE. Two accounts run the same
/// single-message arrival, one of them with the failure planted, so the
/// only difference between their SELECT counts is the one extra SELECT
/// the fallback costs.
#[tokio::test]
async fn a_qresync_select_past_its_budget_falls_back_to_a_plain_look() {
    let ordinary = synced_on(offering(|_| {})).await;
    ordinary
        .imap
        .deliver_flagged("INBOX", &message("b", "Moss", ""), &[], days_ago(0));
    let before = ordinary.imap.calls_to("select");
    ordinary.sync.incremental().await.unwrap();
    let ordinary_selects = ordinary.imap.calls_to("select") - before;

    let retried = synced_on(offering(|_| {})).await;
    retried
        .imap
        .deliver_flagged("INBOX", &message("b", "Moss", ""), &[], days_ago(0));
    retried
        .imap
        .fail_on("select", ImapError::Protocol("too many changes".into()));
    let before = retried.imap.calls_to("select");
    retried.sync.incremental().await.unwrap();
    let retried_selects = retried.imap.calls_to("select") - before;

    assert_eq!(retried.ids().await, ["INBOX/1001/1", "INBOX/1001/2"]);
    assert_eq!(
        retried_selects,
        ordinary_selects + 1,
        "the failed QRESYNC SELECT costs exactly one extra plain SELECT"
    );
}

/// The top of the vanished range the next test's server names: `1:*`.
const VANISHED_TOP: u32 = u32::MAX;

/// A QRESYNC server can name any range as vanished, `1:*` among them, in
/// a line of a few bytes. The look tests each stored message's UID against
/// the set rather than walking it, so a range of four billion UIDs costs
/// what a range of one does, and mail in other mailboxes stays. The
/// Inbox's UIDs start at two million, as on a server that has held mail
/// for years.
#[tokio::test]
async fn a_vanished_range_of_every_uid_deletes_what_the_mailbox_held_and_stays_small() {
    let imap = offering(|_| {});
    imap.with(|s| s.mailbox_mut("INBOX").uidnext = 2_000_000);
    imap.deliver_flagged("INBOX", &message("a", "Kites", ""), &[], days_ago(1));
    imap.deliver_flagged("INBOX", &message("b", "Moss", ""), &[], days_ago(1));
    imap.deliver_flagged("Archive", &message("c", "Fresh", ""), &["\\Seen"], days_ago(1));
    let h = imap_harness_on(imap, fake_settings()).await;
    h.bootstrap().await;
    h.sync.incremental().await.unwrap();
    assert_eq!(
        h.ids().await,
        ["Archive/1006/1", "INBOX/1001/2000000", "INBOX/1001/2000001"]
    );
    h.imap
        .with(|s| s.mailbox_mut("INBOX").vanish_next = Some(UidSet::range(1, VANISHED_TOP)));

    let started = std::time::Instant::now();
    let mark = HeapMark::start();
    h.sync.incremental().await.unwrap();
    let (peak, took) = (mark.peak(), started.elapsed());
    eprintln!("a vanished 1:{VANISHED_TOP} look held {peak} bytes and took {took:?}");

    assert_eq!(h.ids().await, ["Archive/1006/1"]);
    assert!(peak < 256 << 10, "the look held {peak} bytes");
    assert!(took.as_secs() < 2, "the look took {took:?}");
}

/// Without QRESYNC a look learns what went by listing every UID the
/// mailbox holds. It hands the engine that list as ranges, not a name per
/// message, so a mailbox of 50,000 messages costs the search's answer and
/// little more.
#[tokio::test]
async fn with_condstore_alone_a_look_at_a_large_mailbox_holds_its_uids_as_ranges() {
    let imap = offering(|c| c.qresync = false);
    fill(&imap, "INBOX", 50_000);
    let (_imap, adapter) = adapter(imap);
    let start = adapter.changes(None).await.unwrap().state;

    let mark = HeapMark::start();
    let look = adapter.changes(Some(&start)).await.unwrap();
    let peak = mark.peak();
    eprintln!("a CONDSTORE look at 50,000 messages held {peak} bytes");

    let holds = look.changes.iter().find_map(|c| match c {
        RemoteChange::Holds { mailbox, uids, .. } if mailbox == "INBOX" => Some(uids),
        _ => None,
    });
    assert_eq!(holds, Some(&UidSet::range(1, 50_000)));
    assert!(peak < 512 << 10, "the look held {peak} bytes");
}

/// A server with a mailbox of 120,000 messages. The client drops the
/// connection past 100,000 answers to one command, so every search and
/// flags fetch over a whole mailbox goes in UID ranges of 50,000.
fn large_inbox(change: impl FnOnce(&mut mailrs_imap::Capabilities)) -> FakeImap {
    let imap = offering(change);
    fill(&imap, "INBOX", 120_000);
    imap
}

/// Every search and flags fetch in the call log names a UID set that
/// spans at most 50,000 UIDs, and none runs to `*`.
fn windowed(imap: &FakeImap) -> bool {
    imap.calls().iter().all(|call| {
        let words: Vec<&str> = call.split(' ').collect();
        let set = match words.first() {
            Some(&"search") => words.iter().skip_while(|w| **w != "UID").nth(1),
            Some(&"flags") => words.get(2),
            _ => return true,
        };
        let bounds: Option<Vec<u32>> = set.map(|set| {
            set.split([',', ':'])
                .map(|n| n.parse::<u32>().unwrap_or(u32::MAX))
                .collect()
        });
        bounds.is_some_and(|b| {
            let (low, high) = (b.iter().min().copied(), b.iter().max().copied());
            low.zip(high)
                .is_some_and(|(low, high)| high != u32::MAX && high - low < 50_000)
        })
    })
}

#[tokio::test]
async fn with_neither_a_look_at_a_mailbox_of_120_000_goes_in_windows() {
    let (imap, adapter) = adapter(large_inbox(|c| {
        c.qresync = false;
        c.condstore = false;
    }));
    let start = adapter.changes(None).await.unwrap().state;
    imap.remote_expunge("INBOX", 7);

    let look = adapter.changes(Some(&start)).await.unwrap();

    let holds = look.changes.iter().find_map(|c| match c {
        RemoteChange::Holds { uids, .. } => Some(uids.clone()),
        _ => None,
    });
    assert_eq!(holds.map(|u| (u.len(), u.contains(7))), Some((119_999, false)));
    assert!(windowed(&imap), "{:?}", imap.calls());
}

#[tokio::test]
async fn with_condstore_alone_a_look_at_a_mailbox_of_120_000_goes_in_windows() {
    let (imap, adapter) = adapter(large_inbox(|c| c.qresync = false));
    let start = adapter.changes(None).await.unwrap().state;
    imap.set_flags("INBOX", 110_000, &["\\Seen"]);

    let look = adapter.changes(Some(&start)).await.unwrap();

    assert!(look.changes.iter().any(|c| matches!(
        c,
        RemoteChange::Gained { id, .. } if id == "INBOX/1001/110000"
    )));
    assert!(windowed(&imap), "{:?}", imap.calls());
}

#[tokio::test]
async fn the_window_of_an_inbox_of_120_000_is_listed_in_windows() {
    let (imap, adapter) = adapter(large_inbox(|_| {}));

    let refs = adapter.window_ids(30, Some("INBOX")).await.unwrap();

    assert_eq!(refs.len(), 120_000);
    assert_eq!(refs[0].id, "INBOX/1001/120000", "newest first");
    assert!(windowed(&imap), "{:?}", imap.calls());
}

/// A CHANGEDSINCE fetch the client drops past its budget costs the
/// connection; the look compares the window's flags with the store's as
/// for a server without CONDSTORE, once, and still finds the change.
#[tokio::test]
async fn a_changedsince_fetch_past_its_budget_falls_back_to_the_windows_flags() {
    let h = synced_on(offering(|c| c.qresync = false)).await;
    h.sync.incremental().await.unwrap();
    h.imap.set_flags("INBOX", 1, &["\\Seen"]);
    h.imap
        .fail_on("flags", ImapError::Protocol("too many answers".into()));

    h.sync.incremental().await.unwrap();

    assert!(!h.stored("INBOX/1001/1").await.unwrap().is_unread());
}

/// When a later window's CHANGEDSINCE fetch fails, the look falls back
/// only for the windows not yet read, so a change in an earlier one is
/// reported once.
#[tokio::test]
async fn a_changedsince_fetch_that_fails_in_a_later_window_reads_only_what_is_left() {
    let (imap, adapter) = adapter(large_inbox(|c| c.qresync = false));
    let start = adapter.changes(None).await.unwrap().state;
    imap.set_flags("INBOX", 10, &["\\Seen"]);
    imap.fail_on(
        "flags INBOX 50001:100000",
        ImapError::Protocol("too many answers".into()),
    );

    let look = adapter.changes(Some(&start)).await.unwrap();

    let gained = look
        .changes
        .iter()
        .filter(|c| matches!(c, RemoteChange::Gained { id, .. } if id == "INBOX/1001/10"))
        .count();
    assert_eq!(gained, 1);
    assert!(look.changes.iter().any(|c| matches!(
        c,
        RemoteChange::CompareKeywords { uids, .. } if *uids == (50_001..=120_000)
    )));
    assert!(
        !imap
            .calls()
            .iter()
            .any(|c| c.starts_with("flags INBOX 1:50000") && !c.contains("changedsince")),
        "{:?}",
        imap.calls()
    );
}

/// A server with neither QRESYNC nor CONDSTORE names no flag change, so
/// its flags are compared with the store's. The adapter hands the engine
/// no message's flags in the look itself, so a look at a large mailbox
/// that changed nothing holds a window's search answer and little more.
#[tokio::test]
async fn with_neither_a_look_at_a_large_mailbox_hands_over_no_flags() {
    let (_imap, adapter) = adapter(large_inbox(|c| {
        c.qresync = false;
        c.condstore = false;
    }));
    let start = adapter.changes(None).await.unwrap().state;

    let mark = HeapMark::start();
    let look = adapter.changes(Some(&start)).await.unwrap();
    let peak = mark.peak();
    eprintln!("a look with neither at 120,000 messages held {peak} bytes");

    assert!(look.changes.len() < 10, "{} changes", look.changes.len());
    assert!(look.changes.iter().any(|c| matches!(
        c,
        RemoteChange::CompareKeywords { mailbox, uids, .. }
            if mailbox == "INBOX" && *uids == (1..=120_000)
    )));
    assert!(peak < 512 << 10, "the look held {peak} bytes");
}

/// The engine reads the keywords to compare a window of 50,000 UIDs at a
/// time, and each answer says which UIDs it covered.
#[tokio::test]
async fn the_keywords_to_compare_come_a_window_at_a_time() {
    let (imap, adapter) = adapter(large_inbox(|c| {
        c.qresync = false;
        c.condstore = false;
    }));
    imap.set_flags("INBOX", 7, &["\\Seen", "$Work"]);

    let page = adapter.keywords_in("INBOX", 1001, 1..=120_000).await.unwrap();

    assert_eq!(page.covered, Some(1..=50_000));
    assert_eq!(page.found.len(), 50_000);
    assert_eq!(page.found[6].uid, 7);
    let mut keywords = page.found[6].keywords.clone();
    keywords.sort();
    assert_eq!(keywords, ["$seen", "$work"]);
    assert!(windowed(&imap), "{:?}", imap.calls());
    let renumbered = adapter.keywords_in("INBOX", 999, 1..=120_000).await.unwrap();
    assert_eq!(renumbered.covered, None, "a mailbox renumbered since answers nothing");
}

/// A change in a later window reaches the store: the engine asks for
/// every window of the comparison, here an empty one below the Inbox's
/// UIDs and the one that holds them.
#[tokio::test]
async fn with_neither_a_change_past_the_first_window_reaches_the_store() {
    let imap = offering(|c| {
        c.qresync = false;
        c.condstore = false;
    });
    imap.with(|s| s.mailbox_mut("INBOX").uidnext = 60_000);
    imap.deliver_flagged("INBOX", &message("a", "Kites", ""), &[], days_ago(1));
    imap.deliver_flagged("INBOX", &message("b", "Moss", ""), &["\\Seen"], days_ago(1));
    let h = imap_harness_on(imap, fake_settings()).await;
    h.bootstrap().await;
    h.sync.incremental().await.unwrap();
    h.imap.set_flags("INBOX", 60_000, &["\\Seen", "\\Flagged"]);
    h.imap.set_flags("INBOX", 60_001, &[]);

    h.sync.incremental().await.unwrap();

    let a = h.stored("INBOX/1001/60000").await.unwrap();
    assert!(!a.is_unread() && a.is_flagged());
    assert!(h.stored("INBOX/1001/60001").await.unwrap().is_unread());
}

/// RFC 3501 lets a server leave UIDNEXT out of SELECT. The look then asks
/// the server for its highest UID and searches only above what it held
/// before, rather than every UID from 1 on each poll.
#[tokio::test]
async fn a_server_that_leaves_uidnext_out_is_asked_only_for_mail_above_what_it_held() {
    let imap = offering(|_| {});
    imap.with(|s| s.omit_uidnext = true);
    imap.deliver_flagged("INBOX", &message("a", "Kites", ""), &[], days_ago(1));
    imap.deliver_flagged("INBOX", &message("b", "Moss", ""), &[], days_ago(1));
    let h = imap_harness_on(imap, fake_settings()).await;
    h.bootstrap().await;
    h.sync.incremental().await.unwrap();
    h.imap.deliver_flagged("INBOX", &message("c", "Rain", ""), &[], days_ago(0));

    h.sync.incremental().await.unwrap();

    assert_eq!(h.ids().await, ["INBOX/1001/1", "INBOX/1001/2", "INBOX/1001/3"]);
    let calls = h.imap.calls();
    assert!(!calls.iter().any(|c| c.contains(":*")), "{calls:?}");
    assert!(calls.contains(&"search INBOX UID 3:3 UNDELETED".to_string()), "{calls:?}");
}

/// What one poll holds, end to end, for an account whose Inbox of 120,000
/// messages from yesterday is loaded and has changed nothing, on a server
/// with neither QRESYNC nor CONDSTORE: the test thread's peak (the
/// adapter), the whole process's peak (the store's threads too), and the
/// time. Run alone, in release:
/// `cargo test -p mailrs-sync --release --lib -- --ignored --nocapture measure_an_engine_poll`
#[tokio::test]
#[ignore = "a measurement, run by hand in release"]
async fn measure_an_engine_poll_of_a_large_inbox_without_condstore() {
    use crate::tests::heap::ProcessMark;

    let imap = offering(|c| {
        c.qresync = false;
        c.condstore = false;
    });
    fill(&imap, "INBOX", 120_000);
    let h = imap_harness_on(imap, fake_settings()).await;
    let started = std::time::Instant::now();
    h.bootstrap().await;
    eprintln!("loading 120,000 messages took {:?}", started.elapsed());
    h.sync.incremental().await.unwrap();
    h.drain();

    let (thread, process) = (HeapMark::start(), ProcessMark::start());
    let started = std::time::Instant::now();
    h.sync.incremental().await.unwrap();
    let (thread, process, took) = (thread.peak(), process.peak(), started.elapsed());
    eprintln!(
        "neither: an engine poll held {thread} bytes on the test thread and {process} bytes \
         in the process at its peak, took {took:?}, and sent {} events",
        h.drain().len()
    );
}
