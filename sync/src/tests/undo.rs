//! Undo puts back what an action changed and nothing more. An action lands
//! on messages that already differ from each other: part of a thread read,
//! one reply archived, a label on some messages only. Reversing the action
//! by name would treat them all alike; Undo reverses each message's own
//! change instead.

use std::collections::HashMap;
use std::sync::Arc;

use mailrs_domain::{FlagColor, MailSet, Role, Target};

use super::{Connected, Harness, harness};
use crate::fake::meta;
use crate::{History, MailAction, MailActions, TriageAction, now_millis};

fn actions(h: &Harness) -> MailActions<Connected> {
    let connected = HashMap::from([(h.account_id, Arc::clone(&h.sync))]);
    MailActions::new(
        Arc::new(Connected(connected)),
        h.db.clone(),
        crate::OneClick::Fake(Arc::default()),
    )
}

/// What the fake Gmail holds on a message, sorted.
fn in_gmail(h: &Harness, id: &str) -> Vec<String> {
    let mut labels = h.fake.with(|s| mailrs_gmail::labels::label_ids(&s.messages[id]));
    labels.sort();
    labels
}

async fn run_and_undo(h: &Harness, targets: &[Target], action: MailAction) {
    let actions = actions(h);
    let outcome = actions.run(targets, action, History::Record).await;
    assert!(outcome.failed.is_empty(), "{:?}", outcome.failed);
    let undone = actions.undo().await.expect("an undo");
    assert!(undone.outcome.failed.is_empty(), "{:?}", undone.outcome);
    assert_eq!(undone.outcome.done, targets);
}

#[tokio::test]
async fn undoing_trash_on_an_archived_thread_leaves_it_archived() {
    let h = harness().await;
    h.fake.seed(meta("a", "t1", now_millis(), &["INBOX"]));
    h.bootstrap_all().await;
    // Archived on another device: the store and Gmail both lack INBOX.
    h.fake.remote_relabel("a", &[], &["INBOX"]);
    h.sync.incremental().await.unwrap();
    let target = Target::thread(h.account_id, "t1");

    run_and_undo(
        &h,
        std::slice::from_ref(&target),
        MailAction::Triage(TriageAction::Trash),
    )
    .await;

    assert!(h.labels_of("a").await.is_empty(), "archived, as it was");
    assert!(in_gmail(&h, "a").is_empty(), "Gmail agrees");
    assert!(h.threads(MailSet::Role(Role::Inbox)).await.is_empty());
}

#[tokio::test]
async fn undoing_mark_read_leaves_the_messages_that_were_read_alone() {
    let h = harness().await;
    let now = now_millis();
    h.fake.seed(meta("a", "t1", now - 2000, &["INBOX"]));
    h.fake.seed(meta("b", "t1", now - 1000, &["INBOX"]));
    h.fake.seed(meta("c", "t1", now, &["INBOX", "UNREAD"]));
    h.bootstrap_all().await;
    let target = Target::thread(h.account_id, "t1");

    run_and_undo(&h, &[target], MailAction::Triage(TriageAction::MarkRead)).await;

    assert_eq!(h.labels_of("a").await, ["INBOX"]);
    assert_eq!(h.labels_of("b").await, ["INBOX"]);
    assert_eq!(h.labels_of("c").await, ["INBOX", "UNREAD"]);
    assert_eq!(in_gmail(&h, "a"), ["INBOX"]);
    assert_eq!(in_gmail(&h, "c"), ["INBOX", "UNREAD"]);
    assert_eq!(
        h.fake.with(|s| s.remote_writes.last().cloned()),
        Some("modify c +UNREAD -".into()),
        "only the message that changed goes back"
    );
}

#[tokio::test]
async fn undoing_a_label_takes_it_off_only_where_it_went_on() {
    let h = harness().await;
    let now = now_millis();
    h.fake
        .seed(meta("a", "t1", now - 1000, &["INBOX", "Label_1"]));
    h.fake.seed(meta("b", "t1", now, &["INBOX"]));
    h.bootstrap_all().await;
    let target = Target::thread(h.account_id, "t1");

    run_and_undo(
        &h,
        &[target],
        MailAction::Triage(TriageAction::AddLabel("Label_1".into())),
    )
    .await;

    assert_eq!(h.labels_of("a").await, ["INBOX", "Label_1"]);
    assert_eq!(h.labels_of("b").await, ["INBOX"]);
    assert_eq!(in_gmail(&h, "a"), ["INBOX", "Label_1"]);
}

#[tokio::test]
async fn undoing_a_label_removal_puts_it_back_only_where_it_was() {
    let h = harness().await;
    let now = now_millis();
    h.fake
        .seed(meta("a", "t1", now - 1000, &["INBOX", "Label_1"]));
    h.fake.seed(meta("b", "t1", now, &["INBOX"]));
    h.bootstrap_all().await;
    let target = Target::thread(h.account_id, "t1");

    run_and_undo(
        &h,
        &[target],
        MailAction::Triage(TriageAction::RemoveLabel("Label_1".into())),
    )
    .await;

    assert_eq!(h.labels_of("a").await, ["INBOX", "Label_1"]);
    assert_eq!(h.labels_of("b").await, ["INBOX"], "b never had it");
    assert_eq!(in_gmail(&h, "b"), ["INBOX"]);
}

#[tokio::test]
async fn undoing_mute_puts_back_only_the_messages_that_were_in_the_inbox() {
    let h = harness().await;
    let now = now_millis();
    h.fake.seed(meta("a", "t1", now - 1000, &["SENT"]));
    h.fake.seed(meta("b", "t1", now, &["INBOX"]));
    h.bootstrap_all().await;
    let target = Target::thread(h.account_id, "t1");

    run_and_undo(&h, &[target], MailAction::Mute { muted: true }).await;

    assert_eq!(h.labels_of("a").await, ["SENT"], "a sent reply stays out");
    assert_eq!(h.labels_of("b").await, ["INBOX"]);
    assert_eq!(in_gmail(&h, "a"), ["SENT"]);
}

#[tokio::test]
async fn undoing_a_flag_unstars_only_the_messages_it_starred() {
    let h = harness().await;
    let now = now_millis();
    h.fake
        .seed(meta("a", "t1", now - 1000, &["INBOX", "STARRED"]));
    h.fake.seed(meta("b", "t1", now, &["INBOX"]));
    h.bootstrap_all().await;
    let target = Target::thread(h.account_id, "t1");

    run_and_undo(&h, &[target], MailAction::Flag(Some(FlagColor::Red))).await;

    assert_eq!(h.labels_of("a").await, ["INBOX", "STARRED"]);
    assert_eq!(h.labels_of("b").await, ["INBOX"]);
    assert_eq!(in_gmail(&h, "a"), ["INBOX", "STARRED"]);
}

/// A bulk undo goes back in as few calls as the changes allow: every
/// message that needs the same change shares one `batchModify`.
#[tokio::test]
async fn a_bulk_undo_groups_the_same_reversal_into_one_batch() {
    let h = harness().await;
    let now = now_millis();
    let mut targets = Vec::new();
    for index in 0..24 {
        // Half the threads sit in the inbox and half are archived.
        let labels: &[&str] = if index % 2 == 0 { &["INBOX"] } else { &[] };
        let (id, thread) = (format!("m{index}"), format!("t{index}"));
        h.fake.seed(meta(&id, &thread, now - index, labels));
        targets.push(Target::thread(h.account_id, thread));
    }
    h.bootstrap_all().await;
    // The window lists inbox mail and recent mail, so both halves are in.
    assert_eq!(h.all_threads().await.len(), 24);

    let actions = actions(&h);
    actions
        .run(
            &targets,
            MailAction::Triage(TriageAction::Trash),
            History::Record,
        )
        .await;
    h.fake.reset_usage();
    let undone = actions.undo().await.expect("an undo");

    assert!(undone.outcome.failed.is_empty(), "{:?}", undone.outcome);
    assert_eq!(undone.outcome.done.len(), 24);
    let usage = h.fake.usage();
    assert_eq!(usage.calls_to("users.messages.batchModify"), 2);
    assert_eq!(usage.calls_to("users.messages.modify"), 0);
    assert_eq!(h.threads(MailSet::Role(Role::Inbox)).await.len(), 12);
    assert_eq!(in_gmail(&h, "m1"), Vec::<String>::new());
    assert_eq!(in_gmail(&h, "m0"), ["INBOX"]);
}

/// Unread is the absence of a mark rather than a mark, so undoing Mark
/// Unread must read again only what the action made unread.
#[tokio::test]
async fn undoing_mark_unread_leaves_the_unread_messages_unread() {
    let h = harness().await;
    let now = now_millis();
    h.fake
        .seed(meta("a", "t1", now - 1000, &["INBOX", "UNREAD"]));
    h.fake.seed(meta("b", "t1", now, &["INBOX"]));
    h.bootstrap_all().await;

    run_and_undo(
        &h,
        &[Target::thread(h.account_id, "t1")],
        MailAction::Triage(TriageAction::MarkUnread),
    )
    .await;

    assert_eq!(h.labels_of("a").await, ["INBOX", "UNREAD"]);
    assert_eq!(h.labels_of("b").await, ["INBOX"]);
    assert_eq!(in_gmail(&h, "a"), ["INBOX", "UNREAD"]);
    assert_eq!(in_gmail(&h, "b"), ["INBOX"]);
}
