//! Listing mail again: a mailbox the server renumbered, as a new
//! UIDVALIDITY says, and the whole account after the server lost its
//! place. Either way the engine keeps what it already held.

use mailrs_domain::{Location, MailSet};
use mailrs_imap::ImapError;
use mailrs_store::{accounts, bodies, mailboxes, remote_refs, threads};

use super::{days_ago, message};
use crate::TriageAction;
use crate::tests::{ImapHarness, imap_harness};

#[tokio::test]
async fn a_new_uidvalidity_relists_the_mailbox_and_keeps_ids_and_bodies() {
    let h = imap_harness().await;
    h.imap.deliver_flagged("INBOX", &message("a", "Kites", ""), &[], days_ago(3));
    h.imap.deliver_flagged("INBOX", &message("b", "Moss", ""), &[], days_ago(2));
    h.imap.deliver_flagged("INBOX", &message("c", "Ferns", ""), &[], days_ago(1));
    h.imap.deliver_flagged("Sent", &message("s", "Re: Kites", ""), &["\\Seen"], days_ago(1));
    h.bootstrap().await;
    h.sync.body("INBOX/1001/1").await.unwrap();
    h.imap.remote_expunge("INBOX", 2);
    h.imap.reset_uidvalidity("INBOX");

    h.sync.incremental().await.unwrap();

    assert_eq!(h.ids().await, ["INBOX/1001/1", "INBOX/1001/3", "Sent/1002/1"]);
    assert_eq!(h.location("INBOX/1001/1").await.as_deref(), Some("INBOX/1007/1"));
    assert_eq!(h.location("INBOX/1001/3").await.as_deref(), Some("INBOX/1007/2"));
    assert_eq!(
        h.location("Sent/1002/1").await.as_deref(),
        Some("Sent/1002/1"),
        "other mailboxes keep their names"
    );
    let account_id = h.account_id;
    let cached = h
        .db
        .read(move |c| bodies::peek_body(c, account_id, "INBOX/1001/1"))
        .await
        .unwrap();
    assert!(cached.is_some(), "the cached body survives");

    h.imap.set_flags("INBOX", 1, &["\\Flagged"]);
    h.sync.incremental().await.unwrap();
    assert!(h.stored("INBOX/1001/1").await.unwrap().is_flagged());
}

#[tokio::test]
async fn a_message_without_a_message_id_comes_back_as_new() {
    let h = imap_harness().await;
    let bare = b"From: Ann <ann@example.com>\r\nTo: me@example.com\r\nSubject: Bare\r\n\r\nHi.\r\n";
    h.imap.deliver_flagged("INBOX", bare, &[], days_ago(1));
    h.bootstrap().await;
    h.imap.reset_uidvalidity("INBOX");

    h.sync.incremental().await.unwrap();

    assert_eq!(h.ids().await, ["INBOX/1007/1"]);
}

/// A relisting that fails part-way leaves the sync state where it was, so
/// the next look lists the mailbox again and still keeps every id.
#[tokio::test]
async fn a_relisting_that_fails_runs_again_at_the_next_look() {
    let h = imap_harness().await;
    h.imap.deliver_flagged("INBOX", &message("a", "Kites", ""), &[], days_ago(2));
    h.imap.deliver_flagged("INBOX", &message("b", "Moss", ""), &[], days_ago(1));
    h.bootstrap().await;
    h.imap.reset_uidvalidity("INBOX");
    h.imap.fail_on("headers", ImapError::Network("reset".into()));

    assert!(h.sync.incremental().await.is_err());
    h.sync.incremental().await.unwrap();

    assert_eq!(h.ids().await, ["INBOX/1001/1", "INBOX/1001/2"]);
    assert_eq!(h.location("INBOX/1001/2").await.as_deref(), Some("INBOX/1007/2"));
}

/// Makes the stored sync state one the adapter cannot read, so the next
/// look lists the whole account again.
async fn lose_the_place(h: &ImapHarness) {
    let account_id = h.account_id;
    h.db.write(move |c| accounts::set_sync_state(c, account_id, "{\"history_id\":1}"))
        .await
        .unwrap();
}

/// A parent that holds no mail refuses SELECT; listing the account again
/// compares only the mailboxes the account keeps in step.
#[tokio::test]
async fn listing_the_account_again_passes_over_a_parent_that_holds_no_mail() {
    let h = imap_harness().await;
    h.imap.with(|s| s.mailbox_mut("Projects").no_select = true);
    h.imap.deliver_flagged("INBOX", &message("a", "Kites", ""), &[], days_ago(1));
    h.bootstrap().await;
    lose_the_place(&h).await;

    h.sync.incremental().await.unwrap();

    assert_eq!(h.ids().await, ["INBOX/1001/1"]);
}

/// A message that took a new name, here through a relisting, is the same
/// message when the account is listed again: nothing is fetched for it.
#[tokio::test]
async fn listing_the_account_again_reads_new_names_back_as_stored_ids() {
    let h = imap_harness().await;
    h.imap.deliver_flagged("INBOX", &message("a", "Kites", ""), &[], days_ago(1));
    h.bootstrap().await;
    h.imap.reset_uidvalidity("INBOX");
    h.sync.incremental().await.unwrap();
    lose_the_place(&h).await;
    let before = h.imap.calls_to("headers");

    h.sync.incremental().await.unwrap();

    assert_eq!(h.ids().await, ["INBOX/1001/1"]);
    assert_eq!(h.imap.calls_to("headers"), before, "{:?}", h.imap.calls());
}

/// The Inbox check compares the server's Inbox with the store's by store
/// id, so a message that took a new name is not fetched on every check.
#[tokio::test]
async fn the_inbox_check_reads_new_names_back_as_stored_ids() {
    let h = imap_harness().await;
    h.imap.deliver_flagged("INBOX", &message("a", "Kites", ""), &[], days_ago(1));
    h.bootstrap().await;
    h.imap.reset_uidvalidity("INBOX");
    h.sync.incremental().await.unwrap();
    let before = h.imap.calls_to("headers");

    h.sync.reconcile_inbox().await.unwrap();

    assert_eq!(h.imap.calls_to("headers"), before, "{:?}", h.imap.calls());
}

/// Puts the Inbox's message `a` in `Projects` as a move made by the app
/// would: the copy sits in Projects, the Inbox's is expunged, and the
/// stored message's ref names where it sits now. Returns the account.
async fn moved_to_projects() -> ImapHarness {
    let h = imap_harness().await;
    let raw = message("a", "Kites", "");
    let date = days_ago(1);
    h.imap.deliver_flagged("INBOX", &raw, &[], date);
    h.bootstrap().await;
    let uid = h.imap.deliver_flagged("Projects", &raw, &[], date);
    let uidvalidity = h.imap.with(|s| s.mailbox_mut("Projects").uidvalidity);
    h.imap.remote_expunge("INBOX", 1);
    let (account_id, at) = (
        h.account_id,
        Location {
            mailbox: "Projects".into(),
            uidvalidity,
            uid,
        },
    );
    h.db.write(move |c| remote_refs::locate(c, account_id, "INBOX/1001/1", &at))
        .await
        .unwrap();
    h.sync.incremental().await.unwrap();
    h.sync.refresh_labels().await.unwrap();
    h
}

/// A mailbox renamed keeps its messages: their refs take the new name,
/// so the next look at them finds them rather than reporting them gone.
#[tokio::test]
async fn a_renamed_mailbox_keeps_its_messages() {
    let h = moved_to_projects().await;

    h.sync.rename_label("Projects", "Work").await.unwrap();

    assert_eq!(h.location("INBOX/1001/1").await.as_deref(), Some("Work/1007/1"));
    let thread = h.thread_of("INBOX/1001/1").await.unwrap();
    h.sync.ensure_thread(&thread).await.unwrap();
    assert_eq!(h.ids().await, ["INBOX/1001/1"]);
}

/// The store files a renamed mailbox's mail under its new name at once,
/// so the next listing of mailboxes takes nothing away and the sidebar
/// counts it without fetching anything again.
#[tokio::test]
async fn a_renamed_mailbox_keeps_its_memberships_and_counts() {
    let h = imap_harness().await;
    h.imap
        .deliver_flagged("INBOX", &message("a", "Kites", ""), &[], days_ago(1));
    h.bootstrap().await;
    h.imap.add_mailbox("Projects", None);
    h.sync.refresh_labels().await.unwrap();
    let thread = h.thread_of("INBOX/1001/1").await.unwrap();
    h.sync
        .triage_thread(&thread, &TriageAction::MoveTo("Projects".into()))
        .await
        .unwrap();

    h.sync.rename_label("Projects", "Work").await.unwrap();
    h.sync.refresh_labels().await.unwrap();

    assert_eq!(
        h.stored("INBOX/1001/1").await.unwrap().held.mailboxes,
        ["Work"]
    );
    let account_id = h.account_id;
    let (counts, listed) =
        h.db.read(move |c| Ok((threads::mail_counts(c)?, mailboxes::listed(c, account_id)?)))
            .await
            .unwrap();
    assert_eq!(
        counts
            .account(account_id, &MailSet::Mailbox("Work".into()))
            .threads,
        1
    );
    assert!(listed.iter().all(|m| m.id != "Projects"));
    let fetched = h.imap.calls_to("headers");
    h.sync.incremental().await.unwrap();
    assert_eq!(
        h.imap.calls_to("headers"),
        fetched,
        "nothing is fetched again"
    );
    assert_eq!(h.ids().await, ["INBOX/1001/1"]);
}

/// A server may give a renamed mailbox a new UIDVALIDITY; the rename then
/// lists it again, and its messages keep their ids.
#[tokio::test]
async fn a_mailbox_renamed_under_a_new_uidvalidity_is_listed_again() {
    let h = moved_to_projects().await;
    h.imap.reset_uidvalidity("Projects");

    h.sync.rename_label("Projects", "Work").await.unwrap();

    assert_eq!(h.location("INBOX/1001/1").await.as_deref(), Some("Work/1008/1"));
    assert_eq!(h.ids().await, ["INBOX/1001/1"]);
}

/// An IMAP RENAME moves the mailboxes nested under the one renamed, so
/// the rename asks for no second RENAME and the refs of mail in a nested
/// mailbox follow it too.
#[tokio::test]
async fn renaming_a_mailbox_takes_the_ones_nested_under_it_along() {
    let h = imap_harness().await;
    let raw = message("a", "Kites", "");
    let date = days_ago(1);
    h.imap.deliver_flagged("INBOX", &raw, &[], date);
    h.bootstrap().await;
    h.imap.add_mailbox("Projects", None);
    let uid = h.imap.deliver_flagged("Projects/2026", &raw, &[], date);
    let uidvalidity = h.imap.with(|s| s.mailbox_mut("Projects/2026").uidvalidity);
    h.imap.remote_expunge("INBOX", 1);
    let (account_id, at) = (
        h.account_id,
        Location {
            mailbox: "Projects/2026".into(),
            uidvalidity,
            uid,
        },
    );
    h.db.write(move |c| remote_refs::locate(c, account_id, "INBOX/1001/1", &at))
        .await
        .unwrap();
    h.sync.refresh_labels().await.unwrap();

    h.sync.rename_label("Projects", "Work").await.unwrap();

    assert_eq!(
        h.location("INBOX/1001/1").await,
        Some(format!("Work/2026/{uidvalidity}/{uid}"))
    );
}

/// A followed mailbox stays followed under its new name, from where the
/// feed left it, so mail that arrives after the rename reaches the store.
#[tokio::test]
async fn a_renamed_followed_mailbox_keeps_its_place_in_the_feed() {
    let h = imap_harness().await;
    h.bootstrap().await;
    h.imap.add_mailbox("Projects", None);
    h.sync.refresh_labels().await.unwrap();
    h.sync.follow_mailbox("Projects").await.unwrap();
    h.sync.incremental().await.unwrap();

    h.sync.rename_label("Projects", "Work").await.unwrap();
    h.imap
        .deliver_flagged("Work", &message("a", "Kites", ""), &[], days_ago(1));
    h.sync.incremental().await.unwrap();

    assert!(h.is_followed("Work"));
    let uidvalidity = h.imap.with(|s| s.mailbox_mut("Work").uidvalidity);
    assert_eq!(h.ids().await, [format!("Work/{uidvalidity}/1")]);
}
