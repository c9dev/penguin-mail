use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use mailrs_domain::{FlagColor, MailSet, Role, Target};
use mailrs_gmail::GmailError;
use mailrs_store::{accounts, flags, follow_ups, labels, reminders};

use super::{Connected, Harness, harness};
use crate::actions::DEPTH;
use crate::fake::{FakeGmail, meta};
use crate::{
    AccountServices, AccountSync, History, MailAction, MailActions, Outcome, Permitted,
    TriageAction, now_millis,
};

fn actions(h: &Harness) -> MailActions<Connected> {
    actions_over(h, [])
}

fn actions_over(
    h: &Harness,
    more: impl IntoIterator<Item = Arc<AccountSync>>,
) -> MailActions<Connected> {
    let mut connected = HashMap::from([(h.account_id, Arc::clone(&h.sync))]);
    for sync in more {
        connected.insert(sync.account_id(), sync);
    }
    MailActions::new(
        Arc::new(Connected(connected)),
        h.db.clone(),
        crate::OneClick::Fake(Arc::default()),
    )
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
    assert!(h.threads(MailSet::Role(Role::Inbox)).await.is_empty());
    assert_eq!(actions.newest(), Some(ARCHIVE), "looking leaves it there");
    assert_eq!(actions.newest(), Some(ARCHIVE));

    let undone = actions.undo().await.expect("an undo");
    assert_eq!(undone.outcome.done, [target]);
    assert_eq!(h.threads(MailSet::Role(Role::Inbox)).await, ["t1"]);
    assert_eq!(actions.newest(), None);
    assert!(actions.undo().await.is_none(), "undo works once");
}

/// Archiving three conversations and changing your mind three times gives
/// all three back, the last one first.
#[tokio::test]
async fn three_actions_are_undone_newest_first() {
    let h = harness().await;
    let now = now_millis();
    for (index, thread) in ["t1", "t2", "t3"].into_iter().enumerate() {
        let id = format!("m{index}");
        h.fake
            .seed(meta(&id, thread, now + index as i64, &["INBOX"]));
    }
    h.bootstrap_all().await;
    let actions = actions(&h);

    for thread in ["t1", "t2", "t3"] {
        let target = Target::thread(h.account_id, thread);
        actions.run(&[target], ARCHIVE, History::Record).await;
    }
    assert!(h.threads(MailSet::Role(Role::Inbox)).await.is_empty());

    for thread in ["t3", "t2", "t1"] {
        let undone = actions.undo().await.expect("an undo");
        assert_eq!(
            undone.outcome.done,
            [Target::thread(h.account_id, thread)],
            "the newest archive left goes back first"
        );
    }
    assert_eq!(h.threads(MailSet::Role(Role::Inbox)).await, ["t3", "t2", "t1"]);
    assert!(actions.undo().await.is_none(), "and the stack is empty");
}

/// The stack is bounded, so a long triage session cannot grow it without
/// end. Past the depth the oldest action drops out and stays done.
#[tokio::test]
async fn the_oldest_action_falls_off_a_full_stack() {
    let h = harness().await;
    let now = now_millis();
    let threads: Vec<String> = (0..DEPTH + 1).map(|i| format!("t{i}")).collect();
    for (index, thread) in threads.iter().enumerate() {
        let id = format!("m{index}");
        h.fake
            .seed(meta(&id, thread, now + index as i64, &["INBOX"]));
    }
    h.bootstrap_all().await;
    let actions = actions(&h);

    for thread in &threads {
        let target = Target::thread(h.account_id, thread);
        actions.run(&[target], ARCHIVE, History::Record).await;
    }
    for _ in 0..DEPTH {
        actions.undo().await.expect("an undo");
    }

    assert!(actions.undo().await.is_none(), "the stack holds no more");
    assert_eq!(
        h.threads(MailSet::Role(Role::Inbox)).await.len(),
        DEPTH,
        "the oldest archive stands"
    );
    assert!(h.labels_of("m0").await.is_empty());
}

/// Undoing an archive for a conversation that has been trashed since
/// would pull it back into the inbox, so Undo leaves it where it is and
/// says so.
#[tokio::test]
async fn undo_leaves_mail_that_moved_since_where_it_is() {
    let h = harness().await;
    h.fake.seed(meta("a", "t1", now_millis(), &["INBOX"]));
    h.bootstrap_all().await;
    let actions = actions(&h);
    let target = Target::thread(h.account_id, "t1");

    actions
        .run(std::slice::from_ref(&target), ARCHIVE, History::Record)
        .await;
    let trash = MailAction::Triage(TriageAction::Trash);
    actions
        .run(std::slice::from_ref(&target), trash, History::Skip)
        .await;

    let undone = actions.undo().await.expect("an undo");
    assert!(undone.outcome.done.is_empty(), "nothing went back");
    let failed: Vec<Target> = undone
        .outcome
        .failed
        .iter()
        .map(|f| f.target.clone())
        .collect();
    assert_eq!(failed, [target]);
    let said = undone.outcome.first_error().unwrap();
    assert!(said.contains("has moved since"), "{said}");
    assert_eq!(h.labels_of("a").await, ["TRASH"]);
    assert!(h.threads(MailSet::Role(Role::Inbox)).await.is_empty());
}

/// An action recorded with `History::Skip` never reaches the stack, so
/// Undo still reverses the one before it.
#[tokio::test]
async fn a_skipped_action_stays_off_the_stack() {
    let h = harness().await;
    let now = now_millis();
    h.fake.seed(meta("a", "t1", now, &["INBOX"]));
    h.fake.seed(meta("b", "t2", now + 1, &["INBOX"]));
    h.bootstrap_all().await;
    let actions = actions(&h);

    let recorded = Target::thread(h.account_id, "t1");
    actions.run(&[recorded], ARCHIVE, History::Record).await;
    let skipped = Target::thread(h.account_id, "t2");
    actions.run(&[skipped], ARCHIVE, History::Skip).await;

    let undone = actions.undo().await.expect("an undo");
    assert_eq!(undone.outcome.done, [Target::thread(h.account_id, "t1")]);
    assert_eq!(h.threads(MailSet::Role(Role::Inbox)).await, ["t1"], "t2 stays archived");
    assert!(actions.undo().await.is_none());
}

/// Undo with nothing recorded changes nothing and says nothing.
#[tokio::test]
async fn undo_with_an_empty_stack_does_nothing() {
    let h = harness().await;
    h.fake.seed(meta("a", "t1", now_millis(), &["INBOX"]));
    h.bootstrap_all().await;

    assert!(actions(&h).undo().await.is_none());
    assert_eq!(h.threads(MailSet::Role(Role::Inbox)).await, ["t1"]);
}

/// Signing an account out leaves nothing to reverse its actions through,
/// so its entries go and the other accounts' stay.
#[tokio::test]
async fn a_signed_out_account_takes_its_undos_with_it() {
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
        AccountSync::new(
            other_id,
            AccountServices::fake(Arc::clone(&other_fake)),
            h.db.clone(),
            sender,
        )
        .with_retry_max(Duration::from_millis(10)),
    );
    other.bootstrap().await.unwrap();
    let actions = actions_over(&h, [other]);

    let mine = Target::thread(h.account_id, "t1");
    actions
        .run(std::slice::from_ref(&mine), ARCHIVE, History::Record)
        .await;
    let theirs = Target::thread(other_id, "t2");
    actions.run(&[theirs], ARCHIVE, History::Record).await;
    actions.forget_account(other_id);

    let undone = actions.undo().await.expect("an undo");
    assert_eq!(
        undone.outcome.done,
        [mine],
        "my own archive still goes back"
    );
    assert!(
        actions.undo().await.is_none(),
        "the other account's is gone"
    );
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
async fn archiving_one_message_leaves_the_rest_of_its_thread_in_the_inbox() {
    let h = harness().await;
    let now = now_millis();
    for (index, id) in ["a", "b", "c"].into_iter().enumerate() {
        h.fake
            .seed(meta(id, "t1", now - 1000 + index as i64, &["INBOX"]));
    }
    h.bootstrap_all().await;
    let actions = actions(&h);
    let target = Target {
        message_id: Some("b".into()),
        ..Target::thread(h.account_id, "t1")
    };

    let outcome = actions
        .run(std::slice::from_ref(&target), ARCHIVE, History::Record)
        .await;
    assert_eq!(outcome.done, std::slice::from_ref(&target));
    assert!(h.labels_of("b").await.is_empty());
    assert_eq!(h.labels_of("a").await, ["INBOX"]);
    assert_eq!(h.labels_of("c").await, ["INBOX"]);
    assert_eq!(h.threads(MailSet::Role(Role::Inbox)).await, ["t1"], "the thread stays");

    let undone = actions.undo().await.expect("an undo");
    assert_eq!(undone.outcome.done, [target]);
    assert_eq!(h.labels_of("b").await, ["INBOX"]);
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
        AccountSync::new(
            other_id,
            AccountServices::fake(Arc::clone(&other_fake)),
            h.db.clone(),
            sender,
        )
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
    assert!(h.threads(MailSet::Role(Role::Inbox)).await.is_empty());
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
        AccountSync::new(
            busy_id,
            AccountServices::fake(Arc::clone(&busy_fake)),
            h.db.clone(),
            sender,
        )
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
    assert!(h.threads(MailSet::Role(Role::Inbox)).await.is_empty(), "t1 went to the trash");
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
    assert!(h.threads(MailSet::Role(Role::Inbox)).await.is_empty());

    actions.undo().await.unwrap();
    assert_eq!(reminder().await, None);
    assert_eq!(h.threads(MailSet::Role(Role::Inbox)).await, ["t1"]);
}

/// Dismissing a follow-up goes on the same stack as archiving, so Ctrl+Z
/// after a dismissal brings the follow-up back rather than reaching past
/// it to the archive before.
#[tokio::test]
async fn a_dismissed_follow_up_comes_back_on_undo_in_turn() {
    let h = harness().await;
    let sent = now_millis() - 5 * 24 * 60 * 60 * 1000;
    h.fake.seed(meta("s", "t1", sent, &["SENT"]));
    h.fake.seed(meta("a", "t2", sent, &["INBOX"]));
    h.bootstrap_all().await;
    let actions = actions(&h);
    let waiting = || async {
        let now = now_millis();
        h.db.read(move |c| follow_ups::waiting(c, now))
            .await
            .unwrap()
            .into_iter()
            .map(|f| f.thread_id)
            .collect::<Vec<_>>()
    };
    assert_eq!(waiting().await, ["t1"]);

    let archived = Target::thread(h.account_id, "t2");
    actions.run(&[archived], ARCHIVE, History::Record).await;
    h.fake.reset_usage();
    let dismissed = Target::thread(h.account_id, "t1");
    let outcome = actions
        .run(
            std::slice::from_ref(&dismissed),
            MailAction::DismissFollowUp,
            History::Record,
        )
        .await;
    assert_eq!(outcome.done, std::slice::from_ref(&dismissed));
    assert!(waiting().await.is_empty());
    assert_eq!(h.fake.usage().calls, 0, "Gmail knows nothing of Follow Up");

    let undone = actions.undo().await.expect("an undo");
    assert_eq!(undone.action, MailAction::DismissFollowUp);
    assert_eq!(undone.outcome.done, [dismissed]);
    assert_eq!(waiting().await, ["t1"]);
    assert!(h.threads(MailSet::Role(Role::Inbox)).await.is_empty(), "the archive stands");

    actions.undo().await.expect("the archive before it");
    assert_eq!(h.threads(MailSet::Role(Role::Inbox)).await, ["t2"]);
}

#[tokio::test]
async fn a_reminder_that_comes_due_brings_its_conversation_back_unread() {
    let h = harness().await;
    h.fake.seed(meta("a", "t1", now_millis(), &["INBOX"]));
    h.fake.seed(meta("b", "t2", now_millis(), &["INBOX"]));
    h.bootstrap_all().await;
    let actions = actions(&h);
    let now = now_millis();
    let remind = |thread: &'static str, at| {
        let actions = &actions;
        let target = Target::thread(h.account_id, thread);
        async move {
            actions
                .run(&[target], MailAction::Remind { at }, History::Record)
                .await
        }
    };
    remind("t1", now - 1000).await;
    remind("t2", now + 86_400_000).await;
    assert!(h.threads(MailSet::Role(Role::Inbox)).await.is_empty());

    let returned = actions.return_due(now).await.unwrap();
    assert_eq!(returned.len(), 1, "only the reminder that is due");
    assert_eq!(returned[0].target, Target::thread(h.account_id, "t1"));
    let newest = returned[0].newest.as_ref().expect("its newest message");
    assert_eq!(newest.id, "a");
    assert_eq!(h.threads(MailSet::Role(Role::Inbox)).await, ["t1"]);
    assert!(h.labels_of("a").await.contains(&"UNREAD".to_string()));
    let account_id = h.account_id;
    let left = h.db.read(reminders::list).await.unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(
        (left[0].account_id, left[0].thread_id.as_str()),
        (account_id, "t2")
    );

    assert!(
        actions.return_due(now).await.unwrap().is_empty(),
        "a returned reminder is gone"
    );
}

#[tokio::test]
async fn a_reminder_gmail_refuses_waits_for_the_next_pass() {
    let h = harness().await;
    h.fake.seed(meta("a", "t1", now_millis(), &["INBOX"]));
    h.bootstrap_all().await;
    let actions = actions(&h);
    let now = now_millis();
    let target = Target::thread(h.account_id, "t1");
    actions
        .run(
            std::slice::from_ref(&target),
            MailAction::Remind { at: now - 1000 },
            History::Record,
        )
        .await;

    h.fake.fail_next(GmailError::Http {
        status: 400,
        body: "no".into(),
    });
    assert!(actions.return_due(now).await.unwrap().is_empty());
    assert!(h.threads(MailSet::Role(Role::Inbox)).await.is_empty(), "still archived");

    let returned = actions.return_due(now).await.unwrap();
    assert_eq!(returned.len(), 1, "the next pass brings it back");
    assert_eq!(h.threads(MailSet::Role(Role::Inbox)).await, ["t1"]);
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
    assert!(h.threads(MailSet::Role(Role::Inbox)).await.is_empty());
    assert_eq!(h.threads(MailSet::muted()).await, ["t1"]);

    let undone = actions.undo().await.expect("an undo");
    assert_eq!(undone.outcome.done, [target]);
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
    assert_eq!(h.threads(MailSet::Role(Role::Inbox)).await, ["t1"]);
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
    assert!(h.threads(MailSet::Role(Role::Inbox)).await.is_empty());
    assert_eq!(h.threads(MailSet::muted()).await, ["t1"]);
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
        create: true,
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

/// Declining new labels labels the mail in accounts that have the name and
/// leaves an account without it untouched, with no label made there.
#[tokio::test]
async fn labelling_by_name_without_creating_skips_accounts_that_lack_the_label() {
    let h = harness().await;
    h.fake.with(|s| {
        s.labels.push(mailrs_gmail::RemoteLabel {
            id: "Label_receipts".into(),
            name: "Receipts".into(),
            kind: Some("user".into()),
            color: None,
        })
    });
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
    let (sender, _events) = async_channel::unbounded();
    let other = Arc::new(AccountSync::new(
        other_id,
        AccountServices::fake(Arc::clone(&other_fake)),
        h.db.clone(),
        sender,
    ));
    other.bootstrap().await.unwrap();
    let (mine, theirs) = (
        Target::thread(h.account_id, "t1"),
        Target::thread(other_id, "t2"),
    );

    let outcome = actions_over(&h, [other])
        .run(
            &[mine.clone(), theirs.clone()],
            MailAction::Label {
                add: vec!["receipts".into()],
                remove: vec![],
                create: false,
            },
            History::Record,
        )
        .await;
    assert_eq!(outcome.done, [mine]);
    assert_eq!(outcome.failed.len(), 1);
    assert_eq!(outcome.failed[0].target, theirs);
    assert!(
        h.labels_of("a")
            .await
            .contains(&"Label_receipts".to_string())
    );
    assert!(
        other_fake.with(|s| s.labels.iter().all(|l| l.name != "receipts")),
        "no label is made in the other account"
    );
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
    assert_eq!(h.threads(MailSet::Role(Role::Inbox)).await, ["t2"], "the rest is untouched");
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
    assert_eq!(h.threads(MailSet::Role(Role::Inbox)).await, ["t1"]);
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
    assert_eq!(h.threads(MailSet::Role(Role::Inbox)).await, ["t1"]);
    assert!(
        h.threads(MailSet::Role(Role::Sent)).await.is_empty(),
        "the thread lost the sent label with its only sent message"
    );
}

/// Transactions committed to the harness's database so far, counted from
/// the commit frames in its write-ahead log. The log only grows in a test
/// this small, so the count only goes up.
fn commits(h: &Harness) -> usize {
    let wal = std::fs::read(h._dir.path().join("mail.db-wal")).unwrap_or_default();
    if wal.len() < 32 {
        return 0;
    }
    let word = |at: usize| u32::from_be_bytes([wal[at], wal[at + 1], wal[at + 2], wal[at + 3]]);
    let page = word(8) as usize;
    let salt = (word(16), word(20));
    let mut count = 0;
    let mut at = 32;
    while at + 24 + page <= wal.len() {
        if (word(at + 8), word(at + 12)) == salt && word(at + 4) != 0 {
            count += 1;
        }
        at += 24 + page;
    }
    count
}

/// Flag, Remind and Dismiss Follow-Up write the store once for the whole
/// selection, not once per conversation.
#[tokio::test]
async fn a_flag_or_reminder_on_many_conversations_writes_the_store_once() {
    let h = harness().await;
    let now = now_millis();
    let targets: Vec<Target> = (0..10)
        .map(|i| {
            h.fake.seed(meta(
                &format!("m{i}"),
                &format!("t{i}"),
                now + i,
                &["INBOX"],
            ));
            Target::thread(h.account_id, format!("t{i}"))
        })
        .collect();
    h.bootstrap_all().await;
    let actions = actions(&h);

    for action in [
        MailAction::Flag(Some(FlagColor::Red)),
        MailAction::Remind {
            at: now + 86_400_000,
        },
        MailAction::DismissFollowUp,
    ] {
        let before = commits(&h);
        let outcome = actions.run(&targets, action.clone(), History::Record).await;
        assert_eq!(outcome.done.len(), 10, "{action:?}");
        let label_change = usize::from(!matches!(action, MailAction::DismissFollowUp));
        assert_eq!(
            commits(&h) - before,
            label_change + 1,
            "{action:?}: the label change, then one write for the rest"
        );
    }
    assert_eq!(
        colors(&h, "t3").await,
        [("m3".into(), Some(FlagColor::Red))]
    );

    actions.undo().await.unwrap();
    actions.undo().await.unwrap();
    actions.undo().await.unwrap();
    assert_eq!(colors(&h, "t3").await, [("m3".into(), None)]);
    assert_eq!(h.threads(MailSet::Role(Role::Inbox)).await.len(), 10);
}
