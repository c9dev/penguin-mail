use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use mailrs_domain::{FlagColor, Target};
use mailrs_gmail::GmailError;
use mailrs_store::{accounts, flags, labels, reminders};

use super::{Connected, Harness, harness};
use crate::fake::{FakeGmail, meta};
use crate::{
    AccountSync, History, MailAction, MailActions, Outcome, Permitted, TriageAction, now_millis,
};

fn actions(h: &Harness) -> MailActions<Connected> {
    actions_over(h, [])
}

fn actions_over(
    h: &Harness,
    more: impl IntoIterator<Item = Arc<AccountSync<FakeGmail>>>,
) -> MailActions<Connected> {
    let mut connected = HashMap::from([(h.account_id, Arc::clone(&h.sync))]);
    for sync in more {
        connected.insert(sync.account_id(), sync);
    }
    MailActions::new(Arc::new(Connected(connected)), h.db.clone())
}

async fn colors(h: &Harness, thread_id: &str) -> Vec<(String, Option<FlagColor>)> {
    let (account_id, thread_id) = (h.account_id, thread_id.to_string());
    h.db.read(move |c| flags::colors(c, account_id, &thread_id, None))
        .await
        .unwrap()
}

const ARCHIVE: MailAction = MailAction::Triage(TriageAction::Archive);

#[tokio::test]
async fn archive_then_undo_puts_the_thread_back() {
    let h = harness().await;
    h.fake.seed(meta("a", "t1", now_millis(), &["INBOX"]));
    h.bootstrap_all().await;
    let actions = actions(&h);
    let target = Target::thread(h.account_id, "t1");

    let outcome = actions
        .run(std::slice::from_ref(&target), ARCHIVE, History::Record)
        .await;
    assert_eq!(outcome.done, std::slice::from_ref(&target));
    assert!(h.threads("INBOX").await.is_empty());

    let undone = actions.undo().await.expect("an undo");
    assert_eq!(undone.done, [target]);
    assert_eq!(h.threads("INBOX").await, ["t1"]);
    assert!(actions.undo().await.is_none(), "undo works once");
}

#[tokio::test]
async fn a_message_target_changes_only_that_message() {
    let h = harness().await;
    let now = now_millis();
    h.fake.seed(meta("a", "t1", now - 1000, &["INBOX"]));
    h.fake.seed(meta("b", "t1", now, &["INBOX"]));
    h.bootstrap_all().await;
    let target = Target {
        message_id: Some("b".into()),
        ..Target::thread(h.account_id, "t1")
    };

    actions(&h).run(&[target], ARCHIVE, History::Record).await;
    assert_eq!(h.labels_of("a").await, ["INBOX"]);
    assert!(h.labels_of("b").await.is_empty());
    assert_eq!(
        h.fake.with(|s| s.remote_writes.clone()),
        ["modify b + -INBOX"]
    );
}

#[tokio::test]
async fn a_failing_account_does_not_stop_the_others() {
    let h = harness().await;
    h.fake.seed(meta("a", "t1", now_millis(), &["INBOX"]));
    h.bootstrap_all().await;
    let other_id =
        h.db.write(|c| accounts::insert_account(c, "you@example.com", 0))
            .await
            .unwrap();
    let other_fake = Arc::new(FakeGmail::new());
    other_fake.seed(mailrs_domain::MessageMeta {
        account_id: other_id,
        ..meta("x", "t2", now_millis(), &["INBOX"])
    });
    other_fake.with(|s| s.page_size = 1000);
    let (sender, _events) = async_channel::unbounded();
    let other = Arc::new(
        AccountSync::new(other_id, Arc::clone(&other_fake), h.db.clone(), sender)
            .with_retry_max(Duration::from_millis(10)),
    );
    other.bootstrap().await.unwrap();
    other_fake.fail_next(GmailError::NeedsReauth);
    let refused = Target::thread(other_id, "t2");
    let unknown = Target::thread(99, "t3");
    let fine = Target::thread(h.account_id, "t1");

    let outcome = actions_over(&h, [other])
        .run(
            &[refused.clone(), unknown.clone(), fine.clone()],
            ARCHIVE,
            History::Record,
        )
        .await;
    assert_eq!(outcome.done, [fine]);
    let failed: Vec<Target> = outcome.failed.iter().map(|f| f.target.clone()).collect();
    assert_eq!(failed, [refused, unknown]);
    assert!(outcome.first_error().unwrap().starts_with("Archive failed"));
    assert!(h.threads("INBOX").await.is_empty());
}

/// One account holding out does not swallow the rest of the selection,
/// and the account that gave up says how much of it did not land.
#[tokio::test]
async fn an_account_that_waits_out_its_ceiling_reports_what_it_left() {
    let h = harness().await;
    h.fake.seed(meta("a", "t1", now_millis(), &["INBOX"]));
    h.bootstrap_all().await;
    let busy_id =
        h.db.write(|c| accounts::insert_account(c, "you@example.com", 0))
            .await
            .unwrap();
    let busy_fake = Arc::new(FakeGmail::new());
    busy_fake.seed(mailrs_domain::MessageMeta {
        account_id: busy_id,
        ..meta("x", "t2", now_millis(), &["INBOX"])
    });
    busy_fake.with(|s| s.page_size = 1000);
    let (sender, events) = async_channel::unbounded();
    let busy = Arc::new(
        AccountSync::new(busy_id, Arc::clone(&busy_fake), h.db.clone(), sender)
            .with_retry_max(Duration::from_millis(10))
            .with_wait_ceiling(Duration::from_millis(60)),
    );
    busy.bootstrap().await.unwrap();
    // Gmail says it is busy for longer than the action may wait.
    for _ in 0..10 {
        busy_fake.fail_next(GmailError::RateLimited {
            retry_after: Some(Duration::from_millis(50)),
        });
    }
    let (held_up, fine) = (
        Target::thread(busy_id, "t2"),
        Target::thread(h.account_id, "t1"),
    );

    let outcome = actions_over(&h, [busy])
        .run(
            &[held_up.clone(), fine.clone()],
            MailAction::Triage(TriageAction::Trash),
            History::Record,
        )
        .await;

    assert_eq!(outcome.done, [fine], "the other account still went through");
    let failed: Vec<Target> = outcome.failed.iter().map(|f| f.target.clone()).collect();
    assert_eq!(failed, [held_up]);
    let told: Vec<String> = std::iter::from_fn(|| events.try_recv().ok())
        .filter_map(|e| match e {
            mailrs_domain::ChangeEvent::WriteFailed { message, .. } => Some(message),
            _ => None,
        })
        .collect();
    assert_eq!(
        told,
        ["Gmail stayed busy for a moment, so move to trash did not go through for 1 conversation."]
    );
    assert!(h.threads("INBOX").await.is_empty(), "t1 went to the trash");
}

#[tokio::test]
async fn undoing_a_new_colour_restores_the_earlier_one() {
    let h = harness().await;
    h.fake
        .seed(meta("a", "t1", now_millis(), &["INBOX", "STARRED"]));
    h.bootstrap_all().await;
    let account_id = h.account_id;
    h.db.write(move |c| flags::set_color(c, account_id, "t1", None, Some(FlagColor::Blue)))
        .await
        .unwrap();
    let actions = actions(&h);
    let target = Target::thread(h.account_id, "t1");

    actions
        .run(
            &[target],
            MailAction::Flag(Some(FlagColor::Green)),
            History::Record,
        )
        .await;
    assert_eq!(
        colors(&h, "t1").await,
        [("a".into(), Some(FlagColor::Green))]
    );

    actions.undo().await.unwrap();
    assert_eq!(
        colors(&h, "t1").await,
        [("a".into(), Some(FlagColor::Blue))]
    );
    assert_eq!(
        h.labels_of("a").await,
        ["INBOX", "STARRED"],
        "the thread was flagged before, so it keeps its star"
    );
}

#[tokio::test]
async fn remind_stores_the_subject_archives_and_undo_cancels_it() {
    let h = harness().await;
    h.fake.seed(meta("a", "t1", now_millis(), &["INBOX"]));
    h.bootstrap_all().await;
    let actions = actions(&h);
    let at = now_millis() + 86_400_000;
    let account_id = h.account_id;
    let reminder = || async {
        h.db.read(move |c| reminders::get(c, account_id, "t1"))
            .await
            .unwrap()
    };

    let outcome = actions
        .run(
            &[Target::thread(account_id, "t1")],
            MailAction::Remind { at },
            History::Record,
        )
        .await;
    assert_eq!(outcome.done.len(), 1);
    let stored = reminder().await.expect("a reminder");
    assert_eq!(
        (stored.subject.as_str(), stored.remind_at),
        ("Subject a", at)
    );
    assert!(h.threads("INBOX").await.is_empty());

    actions.undo().await.unwrap();
    assert_eq!(reminder().await, None);
    assert_eq!(h.threads("INBOX").await, ["t1"]);
}

#[tokio::test]
async fn muting_labels_the_thread_and_takes_it_out_of_the_inbox() {
    let h = harness().await;
    h.fake.seed(meta("a", "t1", now_millis(), &["INBOX"]));
    h.bootstrap_all().await;
    let actions = actions(&h);
    let target = Target::thread(h.account_id, "t1");

    let outcome = actions
        .run(
            std::slice::from_ref(&target),
            MailAction::Mute { muted: true },
            History::Record,
        )
        .await;
    assert_eq!(outcome.done, std::slice::from_ref(&target));
    assert_eq!(h.labels_of("a").await, ["MUTE"]);
    assert!(h.threads("INBOX").await.is_empty());
    assert_eq!(h.threads("MUTE").await, ["t1"]);

    let undone = actions.undo().await.expect("an undo");
    assert_eq!(undone.done, [target]);
    assert_eq!(h.labels_of("a").await, ["INBOX"]);
}

#[tokio::test]
async fn unmuting_drops_the_label_and_brings_the_thread_back() {
    let h = harness().await;
    h.fake.seed(meta("a", "t1", now_millis(), &["MUTE"]));
    h.bootstrap_all().await;
    let target = Target::thread(h.account_id, "t1");

    let outcome = actions(&h)
        .run(
            &[target],
            MailAction::Mute { muted: false },
            History::Record,
        )
        .await;
    assert!(outcome.failed.is_empty(), "{:?}", outcome.failed);
    assert_eq!(h.labels_of("a").await, ["INBOX"]);
    assert_eq!(h.threads("INBOX").await, ["t1"]);
}

/// Gmail's own filters archive a reply to a muted thread, so the app never
/// sees it in the inbox. The fake does the same.
#[tokio::test]
async fn a_reply_to_a_muted_thread_arrives_archived() {
    let h = harness().await;
    h.fake.seed(meta("a", "t1", now_millis(), &["INBOX"]));
    h.bootstrap_all().await;
    let target = Target::thread(h.account_id, "t1");
    actions(&h)
        .run(&[target], MailAction::Mute { muted: true }, History::Record)
        .await;

    h.fake
        .deliver(meta("b", "t1", now_millis() + 1, &["INBOX", "UNREAD"]));
    h.sync.incremental().await.unwrap();

    assert_eq!(h.labels_of("b").await, ["MUTE", "UNREAD"]);
    assert!(h.threads("INBOX").await.is_empty());
    assert_eq!(h.threads("MUTE").await, ["t1"]);
}

#[tokio::test]
async fn labelling_by_name_creates_a_missing_label() {
    let h = harness().await;
    h.fake.seed(meta("a", "t1", now_millis(), &["INBOX"]));
    h.bootstrap_all().await;
    let target = Target::thread(h.account_id, "t1");
    let receipts = MailAction::Label {
        add: vec!["Receipts".into()],
        remove: vec![],
    };
    let actions = actions(&h);

    let outcome = actions
        .run(
            std::slice::from_ref(&target),
            receipts.clone(),
            History::Record,
        )
        .await;
    assert!(outcome.failed.is_empty(), "{:?}", outcome.failed);
    let account_id = h.account_id;
    let stored =
        h.db.read(move |c| labels::list_labels(c, account_id))
            .await
            .unwrap();
    let created = stored
        .iter()
        .find(|l| l.name == "Receipts")
        .expect("the label exists");
    assert!(h.labels_of("a").await.contains(&created.id));

    actions.run(&[target], receipts, History::Record).await;
    let named = h
        .fake
        .with(|s| s.labels.iter().filter(|l| l.name == "Receipts").count());
    assert_eq!(named, 1, "the second run finds the label by name");
}

/// Whether the fake still holds a message.
fn in_gmail(h: &Harness, id: &str) -> bool {
    h.fake.with(|s| s.messages.contains_key(id))
}

#[tokio::test]
async fn deleting_forever_takes_the_mail_out_of_gmail_and_the_store() {
    let h = harness().await;
    let now = now_millis();
    h.fake.seed(meta("a", "t1", now, &["INBOX"]));
    h.fake.seed(meta("b", "t2", now + 1, &["INBOX"]));
    h.bootstrap_all().await;
    let actions = actions(&h);
    let target = Target::thread(h.account_id, "t1");

    // The Delete key in the inbox moves it to the Trash first.
    let trashed = MailAction::Triage(TriageAction::Trash);
    actions
        .run(std::slice::from_ref(&target), trashed, History::Record)
        .await;
    assert!(h.labels_of("a").await.contains(&"TRASH".to_string()));

    let erased = actions.erase(std::slice::from_ref(&target)).await.unwrap();
    assert_eq!(
        erased,
        Permitted::Done(Outcome {
            done: vec![target],
            failed: vec![],
        })
    );
    assert!(!in_gmail(&h, "a"), "Gmail no longer holds the message");
    assert!(h.thread("t1").await.is_none(), "the store keeps no row");
    assert!(h.labels_of("a").await.is_empty(), "and no labels");
    assert_eq!(h.threads("INBOX").await, ["t2"], "the rest is untouched");
    assert!(actions.undo().await.is_none(), "erasing records no undo");
}

#[tokio::test]
async fn deleting_forever_without_the_permission_changes_nothing() {
    let h = harness().await;
    h.fake.seed(meta("a", "t1", now_millis(), &["INBOX"]));
    h.bootstrap_all().await;
    let actions = actions(&h);

    h.fake.fail_next(GmailError::MissingScope);
    let erased = actions
        .erase(&[Target::thread(h.account_id, "t1")])
        .await
        .unwrap();

    assert_eq!(erased, Permitted::NeedsPermission);
    assert!(in_gmail(&h, "a"), "Gmail still holds the message");
    assert_eq!(h.threads("INBOX").await, ["t1"]);
    assert_eq!(h.labels_of("a").await, ["INBOX"]);
}

#[tokio::test]
async fn erasing_one_message_leaves_the_rest_of_its_thread_consistent() {
    let h = harness().await;
    let now = now_millis();
    h.fake.seed(meta("a", "t1", now, &["INBOX", "UNREAD"]));
    h.fake.seed(meta("b", "t1", now + 1, &["SENT"]));
    h.bootstrap_all().await;
    let actions = actions(&h);
    let target = Target {
        account_id: h.account_id,
        thread_id: "t1".into(),
        message_id: Some("b".into()),
    };

    let erased = actions.erase(std::slice::from_ref(&target)).await.unwrap();
    assert_eq!(erased.done().map(|o| o.done), Some(vec![target]));

    assert!(!in_gmail(&h, "b") && in_gmail(&h, "a"));
    assert!(h.labels_of("b").await.is_empty());
    assert_eq!(h.labels_of("a").await, ["INBOX", "UNREAD"]);
    let thread = h.thread("t1").await.expect("the thread survives");
    assert_eq!(thread.message_count, 1);
    assert!(thread.unread);
    assert_eq!(h.threads("INBOX").await, ["t1"]);
    assert!(
        h.threads("SENT").await.is_empty(),
        "the thread lost the sent label with its only sent message"
    );
}
