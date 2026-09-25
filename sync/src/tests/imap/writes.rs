use mailrs_domain::{ChangeEvent, MailSet, Role, Target};
use mailrs_imap::UidSet;
use mailrs_store::{bodies, mailboxes};

use super::{days_ago, message, offering};
use crate::fake::FakeImap;
use crate::services::ImapApi;
use crate::tests::{ImapHarness, fake_settings, imap_harness, imap_harness_on};
use crate::{MailBackend, TriageAction};

/// An account on `imap` holding one unread message from Ann in the Inbox,
/// loaded, with the thread it sits in.
async fn one_message_on(imap: FakeImap) -> (ImapHarness, String) {
    imap.deliver_flagged("INBOX", &message("a", "Kites", ""), &[], days_ago(1));
    let h = imap_harness_on(imap, fake_settings()).await;
    h.bootstrap().await;
    let thread = h.thread_of("INBOX/1001/1").await.expect("threaded");
    (h, thread)
}

/// The UIDs in `mailbox` on the server that are not marked deleted.
async fn undeleted(h: &ImapHarness, mailbox: &str) -> Vec<u32> {
    let mut uids = h.imap.search(mailbox, "UNDELETED").await.unwrap();
    uids.sort_unstable();
    uids
}

async fn archive(h: &ImapHarness, thread: &str) {
    h.sync
        .triage_thread(thread, &TriageAction::Archive)
        .await
        .unwrap();
}

#[tokio::test]
async fn archiving_moves_the_message_and_its_remote_ref_follows() {
    let (h, thread) = one_message_on(FakeImap::new()).await;

    archive(&h, &thread).await;

    assert!(undeleted(&h, "INBOX").await.is_empty());
    assert_eq!(undeleted(&h, "Archive").await, [1]);
    assert_eq!(h.ids().await, ["INBOX/1001/1"], "the store id stays");
    assert_eq!(
        h.location("INBOX/1001/1").await.as_deref(),
        Some("Archive/1006/1")
    );
    assert_eq!(
        h.stored("INBOX/1001/1").await.unwrap().held.mailboxes,
        ["Archive"]
    );
    assert!(
        !h.drain()
            .iter()
            .any(|e| matches!(e, ChangeEvent::ArchiveMade { .. })),
        "an Archive the server had is nothing new"
    );
}

#[tokio::test]
async fn with_move_but_no_uidplus_the_new_place_is_found_by_message_id() {
    let (h, thread) = one_message_on(offering(|c| c.uidplus = false)).await;

    archive(&h, &thread).await;

    assert_eq!(
        h.location("INBOX/1001/1").await.as_deref(),
        Some("Archive/1006/1")
    );
}

#[tokio::test]
async fn with_uidplus_but_no_move_only_the_copied_message_is_expunged() {
    let imap = offering(|c| c.moves = false);
    let (h, thread) = one_message_on(imap).await;
    // Another client marked this one deleted and left it for later.
    h.imap.deliver_flagged(
        "INBOX",
        &message("b", "Moss", ""),
        &["\\Deleted"],
        days_ago(1),
    );

    archive(&h, &thread).await;

    assert_eq!(h.imap.search("INBOX", "ALL").await.unwrap(), [2]);
    assert_eq!(
        h.location("INBOX/1001/1").await.as_deref(),
        Some("Archive/1006/1")
    );
}

#[tokio::test]
async fn with_neither_the_old_copy_stays_marked_and_the_message_shows_once() {
    let imap = offering(|c| {
        c.moves = false;
        c.uidplus = false;
    });
    let (h, thread) = one_message_on(imap).await;

    archive(&h, &thread).await;

    assert_eq!(h.imap.search("INBOX", "ALL").await.unwrap(), [1]);
    assert!(undeleted(&h, "INBOX").await.is_empty());
    assert_eq!(
        h.location("INBOX/1001/1").await.as_deref(),
        Some("Archive/1006/1")
    );
    h.sync.incremental().await.unwrap();
    assert_eq!(h.ids().await, ["INBOX/1001/1"]);
}

#[tokio::test]
async fn a_moved_message_keeps_its_id_body_and_news_after_the_feed_runs() {
    let (h, thread) = one_message_on(FakeImap::new()).await;
    h.sync.body("INBOX/1001/1").await.unwrap();

    archive(&h, &thread).await;
    h.sync.incremental().await.unwrap();

    assert_eq!(
        h.ids().await,
        ["INBOX/1001/1"],
        "the old copy's expunge deletes nothing"
    );
    let account_id = h.account_id;
    let cached =
        h.db.read(move |c| bodies::peek_body(c, account_id, "INBOX/1001/1"))
            .await
            .unwrap();
    assert!(cached.is_some());
    h.imap.set_flags("Archive", 1, &["\\Flagged"]);
    h.sync.incremental().await.unwrap();
    assert!(h.stored("INBOX/1001/1").await.unwrap().is_flagged());
}

#[tokio::test]
async fn undo_brings_the_message_back_to_the_inbox_under_the_same_id() {
    let (h, thread) = one_message_on(FakeImap::new()).await;
    let target = Target::thread(h.account_id, &thread);
    let applied = h
        .sync
        .triage_all(std::slice::from_ref(&target), &TriageAction::Archive)
        .await
        .unwrap();

    h.sync
        .change_all(&[target], &crate::ops::undo_ops(&applied[0]), "Undo")
        .await
        .unwrap();

    assert_eq!(undeleted(&h, "INBOX").await, [2]);
    assert_eq!(
        h.location("INBOX/1001/1").await.as_deref(),
        Some("INBOX/1001/2")
    );
    assert_eq!(
        h.stored("INBOX/1001/1").await.unwrap().held.mailboxes,
        ["INBOX"]
    );
}

#[tokio::test]
async fn keywords_the_server_refuses_stay_on_this_computer() {
    let imap = FakeImap::new();
    imap.with(|s| {
        if let Some(inbox) = s.mailboxes.get_mut("INBOX") {
            inbox.permanent_flags = ["\\Seen", "\\Flagged", "\\Answered", "\\Draft", "\\Deleted"]
                .map(String::from)
                .to_vec();
        }
    });
    let (h, thread) = one_message_on(imap).await;
    assert!(
        !h.sync
            .services()
            .capabilities()
            .keywords
            .contains(&"$muted")
    );

    h.sync
        .triage_thread(&thread, &TriageAction::Mute)
        .await
        .unwrap();

    assert!(h.stored("INBOX/1001/1").await.unwrap().is_muted());
    let account_id = h.account_id;
    let local: bool =
        h.db.read(move |c| {
            Ok(c.query_row(
                "SELECT local FROM message_keywords \
                 WHERE account_id = ?1 AND message_id = 'INBOX/1001/1' AND keyword = '$muted'",
                [account_id],
                |row| row.get(0),
            )?)
        })
        .await
        .unwrap();
    assert!(local);
    let on_server = h
        .imap
        .flags("Archive", &UidSet::from_uids([1]), None)
        .await
        .unwrap();
    assert!(
        on_server[0]
            .flags
            .iter()
            .all(|f| !f.eq_ignore_ascii_case("$muted")),
        "the server never heard of it"
    );
    h.imap.set_flags("Archive", 1, &["\\Seen"]);
    h.sync.incremental().await.unwrap();
    assert!(
        h.stored("INBOX/1001/1").await.unwrap().is_muted(),
        "a sync leaves it alone"
    );
}

#[tokio::test]
async fn delete_forever_expunges_the_message() {
    let (h, thread) = one_message_on(FakeImap::new()).await;

    let erased = h
        .sync
        .erase_all(&[Target::thread(h.account_id, &thread)])
        .await
        .unwrap();

    assert!(erased[0].is_ok());
    assert!(h.imap.search("INBOX", "ALL").await.unwrap().is_empty());
    assert!(h.ids().await.is_empty());
}

#[tokio::test]
async fn archiving_on_a_server_without_an_archive_makes_one_once() {
    let h = imap_harness().await;
    h.imap.delete("Archive").await.unwrap();
    h.imap
        .deliver_flagged("INBOX", &message("a", "Kites", ""), &[], days_ago(2));
    h.imap
        .deliver_flagged("INBOX", &message("b", "Moss", ""), &[], days_ago(1));
    h.bootstrap().await;

    for id in ["INBOX/1001/1", "INBOX/1001/2"] {
        let thread = h.thread_of(id).await.unwrap();
        archive(&h, &thread).await;
    }

    let listed = h.imap.list().await.unwrap();
    assert_eq!(listed.iter().filter(|l| l.name == "Archive").count(), 1);
    assert_eq!(undeleted(&h, "Archive").await, [1, 2]);
    let account_id = h.account_id;
    let stored =
        h.db.read(move |c| mailboxes::listed(c, account_id))
            .await
            .unwrap();
    assert!(
        stored
            .iter()
            .any(|m| m.id == "Archive" && m.role == Some(Role::Archive))
    );
    assert_eq!(
        h.sync.services().mail.set_of("Archive"),
        MailSet::Role(Role::Archive)
    );
    let made: Vec<String> = h
        .drain()
        .into_iter()
        .filter_map(|event| match event {
            ChangeEvent::ArchiveMade { name, .. } => Some(name),
            _ => None,
        })
        .collect();
    assert_eq!(made, ["Archive"], "said once, for the first archive only");
}
