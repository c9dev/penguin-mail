//! The tools for what waits and what can be taken back: Send Later, the
//! Outbox, reminders, unmuting and undo.

use chrono::{Duration, Local};
use mailrs_domain::{Address, EpochMillis, system_label};
use mailrs_gmail::GmailError;
use mailrs_store::outbox::{self, Queued};
use mailrs_store::reminders;
use mailrs_sync::{Posted, outbox_row};
use serde_json::{Value, json};

use super::super::fake::{Harness, ME, YOU};
use super::{harness, target, thread_ids};
use crate::compose::Draft;

/// A local time `days` from now, in the shape the tools take.
fn in_days(days: i64) -> (String, EpochMillis) {
    let at = Local::now() + Duration::days(days);
    let text = at.format("%Y-%m-%dT%H:%M").to_string();
    let minute = at.timestamp_millis() / 60_000 * 60_000;
    (text, minute)
}

/// A message to queue, with a composer the app can reopen.
fn message(h: &Harness, subject: &str) -> Queued {
    let mut draft = Draft::new(
        h.account_id,
        Address {
            name: Some("Dana".into()),
            email: ME.into(),
        },
    );
    draft.subject = subject.into();
    Queued {
        account_id: h.account_id,
        subject: subject.into(),
        recipients: "ann@example.com".into(),
        send_at: in_days(1).1,
        raw: Some(format!("Subject: {subject}\r\n\r\nhello").into_bytes()),
        composer: serde_json::to_string(&draft).unwrap(),
        ..Queued::default()
    }
}

/// Puts a message in Send Later, as Gmail's draft, and gives back its row.
async fn scheduled(h: &Harness, subject: &str) -> Value {
    let outbox = h.tools.outbox();
    let Posted::Waiting(_) = outbox.schedule(message(h, subject)).await.unwrap() else {
        panic!("Send Later keeps the message");
    };
    row(h, "send_later", subject).await
}

/// Puts a message in the Outbox, stuck on a problem, and gives back its row.
async fn stuck(h: &Harness, subject: &str) -> Value {
    let mut stuck = message(h, subject);
    stuck.problem = Some("Gmail said no".into());
    h.db.write(move |c| outbox::put(c, &stuck)).await.unwrap();
    row(h, "outbox", subject).await
}

/// The row `list_mail` gives for `subject` in `mailbox`.
async fn row(h: &Harness, mailbox: &str, subject: &str) -> Value {
    let listed = h.ok("list_mail", json!({"mailbox": mailbox})).await;
    listed["conversations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["subject"] == subject)
        .unwrap_or_else(|| panic!("{subject} is in {mailbox}: {listed}"))
        .clone()
}

/// The target a waiting row stands for.
fn named(row: &Value) -> Value {
    json!({
        "account": row["account"],
        "thread_id": row["thread_id"],
        "message_id": row["message_id"],
    })
}

async fn waiting(h: &Harness) -> Vec<Queued> {
    h.db.read(outbox::list).await.unwrap()
}

#[tokio::test]
async fn list_mail_says_why_each_queued_message_waits_and_when_it_goes() {
    let h = harness().await;
    let monday = scheduled(&h, "Monday").await;
    assert_eq!(monday["account"], ME);
    assert_eq!(monday["to"], "To ann@example.com");
    assert!(
        monday["waiting"].as_str().unwrap().starts_with("Sends "),
        "{monday}"
    );
    assert_eq!(monday["sends_at"], in_days(1).0);

    let broken = stuck(&h, "Broken").await;
    assert!(broken["thread_id"].as_str().unwrap().starts_with("outbox:"));
    assert!(
        broken["waiting"]
            .as_str()
            .unwrap()
            .starts_with("Gmail said no. Trying again"),
        "{broken}"
    );
}

/// Send Later spans every account, so naming one narrows the rows.
#[tokio::test]
async fn list_mail_narrows_the_queue_to_the_account_named() {
    let h = Harness::with_second(super::mail(), vec![]).await;
    scheduled(&h, "Monday").await;
    let mine = h
        .ok("list_mail", json!({"mailbox": "send_later", "account": ME}))
        .await;
    assert_eq!(mine["count"], 1);
    let theirs = h
        .ok(
            "list_mail",
            json!({"mailbox": "send_later", "account": YOU}),
        )
        .await;
    assert_eq!(theirs["count"], 0);
}

#[tokio::test]
async fn send_now_asks_then_sends_a_scheduled_message() {
    let h = harness().await;
    let monday = scheduled(&h, "Monday").await;
    let done = h.ok("send_now", json!({"targets": [named(&monday)]})).await;
    assert_eq!(done["sent"], 1);
    assert_eq!(h.asked().questions, ["Send “Monday” now?"]);
    assert_eq!(h.gmail.with(|s| s.sent.len()), 1, "the draft went out");
    assert!(waiting(&h).await.is_empty());
    assert_eq!(h.asked().queue_changed, 1);
    // Gmail files a copy under Sent, which the model can then find.
    let filed = h.gmail.with(|s| {
        s.messages
            .values()
            .any(|m| m.subject == "Monday" && m.label_ids.iter().any(|l| l == "SENT"))
    });
    assert!(filed, "the sent message is in Sent");
}

#[tokio::test]
async fn send_now_on_a_stuck_message_that_still_fails_leaves_it_in_the_outbox() {
    let h = harness().await;
    let broken = stuck(&h, "Broken").await;
    h.gmail.fail_next(GmailError::Network("offline".into()));
    let done = h.ok("send_now", json!({"targets": [named(&broken)]})).await;
    assert_eq!(done["sent"], 0);
    assert_eq!(done["still_waiting"], json!(["Broken"]));
    assert_eq!(waiting(&h).await.len(), 1);
}

#[tokio::test]
async fn cancel_send_puts_a_scheduled_message_back_in_drafts() {
    let h = harness().await;
    let monday = scheduled(&h, "Monday").await;
    let done = h
        .ok("cancel_send", json!({"targets": [named(&monday)]}))
        .await;
    assert_eq!(done["in_drafts"], 1);
    assert_eq!(
        h.asked().questions,
        ["Cancel sending “Monday”? It goes back to Drafts."]
    );
    assert!(waiting(&h).await.is_empty());
    assert_eq!(h.gmail.with(|s| s.drafts.len()), 1, "the draft stays");
    assert!(h.gmail.with(|s| s.sent.is_empty()));
}

/// A message Gmail never had, cancelled while Gmail still refuses it, opens
/// in a composer before its only copy leaves the table.
#[tokio::test]
async fn cancel_send_reopens_a_message_gmail_never_had() {
    let h = harness().await;
    let outbox = h.tools.outbox();
    h.gmail.fail_next(GmailError::Network("offline".into()));
    let Posted::Waiting(id) = outbox.schedule(message(&h, "Offline")).await.unwrap() else {
        panic!("Send Later keeps a message it could not hand to Gmail");
    };
    let target = json!({"account": ME, "thread_id": outbox_row(id)});
    h.gmail.fail_next(GmailError::NeedsReauth);
    let done = h.ok("cancel_send", json!({"targets": [target]})).await;
    assert_eq!(done["in_drafts"], 0);
    assert_eq!(done["opened_in_composer"], 1);
    assert_eq!(h.asked().reopened[0].subject, "Offline");
    assert!(waiting(&h).await.is_empty(), "the writer has it open now");
}

#[tokio::test]
async fn delete_queued_drops_an_outbox_message_and_leaves_send_later_to_cancel_send() {
    let h = harness().await;
    let broken = stuck(&h, "Broken").await;
    let monday = scheduled(&h, "Monday").await;
    assert_eq!(
        h.run("delete_queued", json!({"targets": [named(&monday)]}))
            .await,
        Err(
            "Those messages are in Send Later, not the Outbox. Use cancel_send to stop them."
                .into()
        )
    );
    assert!(h.asked().questions.is_empty(), "nothing to ask about");

    let done = h
        .ok("delete_queued", json!({"targets": [named(&broken)]}))
        .await;
    assert_eq!(done["deleted"], 1);
    assert_eq!(
        h.asked().questions,
        ["Delete “Broken” from the Outbox? It will not be sent."]
    );
    let left = waiting(&h).await;
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].subject, "Monday");
    assert!(h.gmail.with(|s| s.sent.is_empty()));
}

#[tokio::test]
async fn reschedule_moves_send_later_and_refuses_the_outbox() {
    let h = harness().await;
    let monday = scheduled(&h, "Monday").await;
    let (text, at) = in_days(3);
    let done = h
        .ok(
            "reschedule",
            json!({"targets": [named(&monday)], "at": text}),
        )
        .await;
    assert_eq!(done["rescheduled"], 1);
    assert_eq!(done["sends_at"], text);
    assert!(
        h.asked().questions[0].starts_with("Send “Monday” "),
        "{}",
        h.asked().questions[0]
    );
    assert_eq!(waiting(&h).await[0].send_at, at);

    let broken = stuck(&h, "Broken").await;
    assert!(
        h.run(
            "reschedule",
            json!({"targets": [named(&broken)], "at": in_days(2).0})
        )
        .await
        .is_err_and(|e| e.contains("Outbox"))
    );
    assert!(
        h.run(
            "reschedule",
            json!({"targets": [named(&monday)], "at": "2020-01-01T09:00"})
        )
        .await
        .is_err_and(|e| e.contains("past"))
    );
}

#[tokio::test]
async fn a_row_nothing_waits_for_is_refused() {
    let h = harness().await;
    for tool in ["send_now", "cancel_send", "delete_queued"] {
        let answer = h.run(tool, json!({"targets": [target("t1")]})).await;
        assert!(
            answer
                .as_ref()
                .is_err_and(|e| e.starts_with("Nothing in Send Later")),
            "{tool}: {answer:?}"
        );
    }
    assert!(h.asked().questions.is_empty());
}

#[tokio::test]
async fn reminders_are_listed_moved_and_cancelled() {
    let h = harness().await;
    let (first, _) = in_days(1);
    h.ok("remind_me", json!({"targets": [target("t1")], "at": first}))
        .await;

    let listed = h.ok("list_reminders", json!({})).await;
    assert_eq!(listed["reminders"][0]["thread_id"], "t1");
    assert_eq!(listed["reminders"][0]["subject"], "Kite plans");
    assert_eq!(listed["reminders"][0]["returns_at"], first);
    let row = row(&h, "reminders", "Kite plans").await;
    assert_eq!(row["returns_at"], first);
    assert!(row["waiting"].as_str().unwrap().starts_with("Returns "));

    let (later, at) = in_days(4);
    let moved = h
        .ok(
            "change_reminder",
            json!({"targets": [target("t1")], "at": later}),
        )
        .await;
    assert_eq!(moved["done"], 1);
    assert!(h.asked().questions[0].starts_with("Bring “Kite plans” back "));
    let (account_id, stored) = (h.account_id, "t1");
    let reminder =
        h.db.read(move |c| reminders::get(c, account_id, stored))
            .await
            .unwrap()
            .expect("the reminder stays");
    assert_eq!(reminder.remind_at, at);

    let cancelled = h
        .ok("cancel_reminder", json!({"targets": [target("t1")]}))
        .await;
    assert_eq!(cancelled["done"], 1);
    assert_eq!(
        h.asked().questions[1],
        "Cancel the reminder on “Kite plans” and put it back in the Inbox?"
    );
    assert!(
        h.ok("list_reminders", json!({})).await["reminders"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        h.labels_of("m1")
            .await
            .contains(&system_label::INBOX.to_string())
    );

    assert!(
        h.run("cancel_reminder", json!({"targets": [target("t1")]}))
            .await
            .is_err_and(|e| e.contains("has no reminder"))
    );
}

#[tokio::test]
async fn unmute_brings_a_muted_thread_back_and_list_mail_finds_muted_mail() {
    let h = harness().await;
    h.ok("mute", json!({"targets": [target("t1")]})).await;
    let muted = h.ok("list_mail", json!({"mailbox": "muted"})).await;
    assert_eq!(thread_ids(&muted), ["t1"]);

    let done = h.ok("unmute", json!({"targets": [target("t1")]})).await;
    assert_eq!(done["done"], 1);
    let labels = h.labels_of("m1").await;
    assert!(labels.contains(&system_label::INBOX.to_string()));
    assert!(!labels.contains(&system_label::MUTE.to_string()));
    assert!(h.asked().questions.is_empty(), "unmuting is Ctrl+Z away");
}

#[tokio::test]
async fn organize_takes_mail_out_of_the_trash_with_move_to_inbox() {
    let h = harness().await;
    h.ok(
        "organize",
        json!({"targets": [target("t2")], "action": "trash"}),
    )
    .await;
    h.ok(
        "organize",
        json!({"targets": [target("t2")], "action": "move_to_inbox"}),
    )
    .await;
    assert!(h.gmail.with(|s| {
        s.remote_writes
            .iter()
            .any(|w| w == "modify m2 +INBOX -TRASH")
    }));
    assert!(
        h.gmail
            .with(|s| s.messages["m2"].has_label(system_label::INBOX))
    );
    let labels = h.labels_of("m2").await;
    assert!(labels.contains(&system_label::INBOX.to_string()));
    assert!(!labels.contains(&system_label::TRASH.to_string()));
}

#[tokio::test]
async fn undo_asks_then_takes_back_the_newest_change() {
    let h = harness().await;
    assert_eq!(
        h.run("undo", json!({})).await,
        Err("There is nothing to undo.".into())
    );
    assert!(h.asked().questions.is_empty());

    h.ok(
        "organize",
        json!({"targets": [target("t1")], "action": "archive"}),
    )
    .await;
    assert!(
        !h.labels_of("m1")
            .await
            .contains(&system_label::INBOX.to_string())
    );
    let done = h.ok("undo", json!({})).await;
    assert_eq!(done["undone"], "Archive");
    assert_eq!(done["done"], 1);
    assert_eq!(h.asked().questions, ["Undo “Archive”?"]);
    assert_eq!(
        h.asked().undone.len(),
        1,
        "the window redraws what came back"
    );
    assert!(
        h.labels_of("m1")
            .await
            .contains(&system_label::INBOX.to_string())
    );

    h.ok(
        "organize",
        json!({"targets": [target("t2")], "action": "archive"}),
    )
    .await;
    h.effects.asked.borrow_mut().approves = false;
    assert_eq!(
        h.run("undo", json!({})).await,
        Err("The user declined.".into())
    );
    assert!(
        !h.labels_of("m2")
            .await
            .contains(&system_label::INBOX.to_string()),
        "a declined undo leaves the archive done"
    );
}

#[tokio::test]
async fn list_mail_reads_a_smart_mailbox_by_name() {
    let h = harness().await;
    h.ok(
        "create_smart_mailbox",
        json!({"name": "From Ann", "conditions": [{"field": "from", "value": "ann@example.com"}]}),
    )
    .await;
    let listed = h
        .ok("list_mail", json!({"mailbox": "smart", "name": "from ann"}))
        .await;
    assert_eq!(thread_ids(&listed), ["t3"]);
    assert_eq!(
        h.run("list_mail", json!({"mailbox": "smart", "name": "Boats"}))
            .await,
        Err("There is no smart mailbox called Boats. There are: From Ann.".into())
    );
}
