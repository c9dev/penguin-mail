use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use mailrs_domain::{FlagColor, Target};
use mailrs_gmail::GmailError;
use mailrs_store::{accounts, flags, labels, reminders};

use super::{Connected, Harness, harness};
use crate::fake::{FakeGmail, meta};
use crate::{AccountSync, History, MailAction, MailActions, TriageAction, now_millis};

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
