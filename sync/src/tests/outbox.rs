use std::collections::HashMap;
use std::sync::Arc;

use mailrs_domain::{Target, system_label};
use mailrs_gmail::GmailError;
use mailrs_store::outbox::{self, Queued};
use mailrs_store::{drafts, messages};

use super::{Connected, Harness, harness};
use crate::fake::meta;
use crate::{Cancelled, Outbox, Posted, now_millis, outbox_row};

/// Puts a draft's message in the store, as history replay does once the
/// draft reaches this computer. Nothing here reads thread rows, so the
/// thread is left unrefreshed.
async fn store_draft_message(h: &Harness, message_id: &str) {
    let message = meta(message_id, "t1", 1, &[system_label::DRAFT]);
    h.db.write(move |c| messages::upsert_message(c, &message, 1))
        .await
        .unwrap();
}

/// What the store holds for `message_id`, whatever its labels say.
async fn stored_pair(h: &Harness, message_id: &str) -> Option<String> {
    let (account_id, message_id) = (h.account_id, message_id.to_string());
    h.db.read(move |c| drafts::draft_of(c, account_id, &message_id))
        .await
        .unwrap()
}

#[tokio::test]
async fn sending_a_saved_draft_deletes_the_draft() {
    let h = harness().await;
    let draft = h
        .sync
        .save_draft(b"draft one".to_vec(), None, None)
        .await
        .unwrap()
        .draft_id;
    let same = h
        .sync
        .save_draft(b"draft two".to_vec(), None, Some(draft.clone()))
        .await
        .unwrap()
        .draft_id;
    assert_eq!(draft, same);
    assert_eq!(h.fake.with(|s| s.drafts[&draft].clone()), b"draft two");

    h.sync
        .send(b"final".to_vec(), Some("t1".into()), Some(draft.clone()))
        .await
        .unwrap();
    assert_eq!(
        h.fake.with(|s| s.sent.clone()),
        [(b"final".to_vec(), Some("t1".to_string()))]
    );
    assert!(h.fake.with(|s| s.drafts.is_empty()));
}

#[tokio::test]
async fn a_draft_already_gone_does_not_fail_the_send() {
    let h = harness().await;
    h.sync
        .send(b"final".to_vec(), None, Some("missing".into()))
        .await
        .unwrap();
    assert_eq!(h.fake.with(|s| s.sent.len()), 1);
}

#[tokio::test]
async fn saving_over_a_vanished_draft_creates_a_new_one() {
    let h = harness().await;
    let id = h
        .sync
        .save_draft(b"text".to_vec(), None, Some("gone".into()))
        .await
        .unwrap()
        .draft_id;
    assert_ne!(id, "gone");
    assert!(h.fake.with(|s| s.drafts.contains_key(&id)));
}

#[tokio::test]
async fn drafts_are_found_by_their_current_message() {
    let h = harness().await;
    let id = h
        .sync
        .save_draft(b"abc".to_vec(), None, None)
        .await
        .unwrap();
    let message = h.fake.with(|s| s.draft_messages[&id.draft_id].clone());
    assert_eq!(message, id.message_id);
    let id = id.draft_id;
    assert_eq!(h.sync.draft_id_for(&message).await.unwrap(), Some(id));
    assert_eq!(h.sync.draft_id_for("other").await.unwrap(), None);
}

/// Opening a draft used to page `drafts.list` over the whole account each
/// time, which an account with 200 drafts pays 2 calls and 10 units for.
/// Now the first open pays that and every later one pays nothing.
#[tokio::test]
async fn opening_a_draft_pages_gmail_once_for_the_whole_account() {
    let h = harness().await;
    // Gmail hands back 100 drafts a page.
    h.fake.with(|s| s.page_size = 100);
    for n in 0..200 {
        let (draft_id, message_id) = (format!("d{n}"), format!("m{n}"));
        h.fake.with(|s| {
            s.drafts.insert(draft_id.clone(), b"body".to_vec());
            s.draft_messages.insert(draft_id, message_id);
        });
        store_draft_message(&h, &format!("m{n}")).await;
    }
    h.fake.reset_usage();

    assert_eq!(
        h.sync.draft_id_for("m7").await.unwrap().as_deref(),
        Some("d7")
    );
    let paged = h.fake.usage();
    assert_eq!(paged.calls_to("users.drafts.list"), 2);
    assert_eq!(paged.units, 10);

    h.fake.reset_usage();
    for n in 0..200 {
        let found = h.sync.draft_id_for(&format!("m{n}")).await.unwrap();
        assert_eq!(found, Some(format!("d{n}")));
    }
    assert_eq!(h.fake.usage().calls, 0);
}

#[tokio::test]
async fn a_draft_this_app_saved_reopens_without_a_listing() {
    let h = harness().await;
    let saved = h
        .sync
        .save_draft(b"hello".to_vec(), None, None)
        .await
        .unwrap();
    store_draft_message(&h, &saved.message_id).await;
    h.fake.reset_usage();

    assert_eq!(
        h.sync.draft_id_for(&saved.message_id).await.unwrap(),
        Some(saved.draft_id)
    );
    assert_eq!(h.fake.usage().calls, 0);
}

#[tokio::test]
async fn a_draft_sent_elsewhere_stops_answering_for_its_message() {
    let h = harness().await;
    let saved = h
        .sync
        .save_draft(b"hello".to_vec(), None, None)
        .await
        .unwrap();
    store_draft_message(&h, &saved.message_id).await;
    // Gmail sent it from another client: the draft is gone and history
    // replay took the DRAFT label off the message it left behind.
    h.fake.with(|s| {
        s.drafts.remove(&saved.draft_id);
        s.draft_messages.remove(&saved.draft_id);
    });
    let (account_id, message_id) = (h.account_id, saved.message_id.clone());
    h.db.write(move |c| {
        messages::remove_labels(
            c,
            account_id,
            &message_id,
            &[system_label::DRAFT.to_string()],
        )
        .map(|_| ())
    })
    .await
    .unwrap();

    assert_eq!(h.sync.draft_id_for(&saved.message_id).await.unwrap(), None);
}

#[tokio::test]
async fn a_draft_edited_elsewhere_answers_for_its_new_message_alone() {
    let h = harness().await;
    let saved = h
        .sync
        .save_draft(b"first".to_vec(), None, None)
        .await
        .unwrap();
    store_draft_message(&h, &saved.message_id).await;
    // An edit in another client leaves the draft where it was and gives it
    // a new message; history replay drops the one it no longer holds.
    let moved = format!("{}-again", saved.message_id);
    h.fake.with(|s| {
        s.draft_messages
            .insert(saved.draft_id.clone(), moved.clone())
    });
    store_draft_message(&h, &moved).await;
    let (account_id, gone) = (h.account_id, saved.message_id.clone());
    h.db.write(move |c| messages::delete_message(c, account_id, &gone).map(|_| ()))
        .await
        .unwrap();

    assert_eq!(h.sync.draft_id_for(&saved.message_id).await.unwrap(), None);
    h.fake.reset_usage();
    assert_eq!(
        h.sync.draft_id_for(&moved).await.unwrap(),
        Some(saved.draft_id)
    );
    assert_eq!(h.fake.usage().calls, 0);
}

#[tokio::test]
async fn a_draft_that_leaves_gmail_takes_its_pair_with_it() {
    let h = harness().await;
    for send in [true, false] {
        let saved = h
            .sync
            .save_draft(b"bye".to_vec(), None, None)
            .await
            .unwrap();
        store_draft_message(&h, &saved.message_id).await;
        assert_eq!(
            stored_pair(&h, &saved.message_id).await,
            Some(saved.draft_id.clone())
        );
        match send {
            true => {
                h.sync.send_draft(&saved.draft_id).await.unwrap();
            }
            false => h.sync.delete_draft(&saved.draft_id).await.unwrap(),
        }
        assert_eq!(stored_pair(&h, &saved.message_id).await, None);
    }
}

#[tokio::test]
async fn sending_from_a_draft_takes_its_pair_with_it() {
    let h = harness().await;
    let saved = h
        .sync
        .save_draft(b"draft".to_vec(), None, None)
        .await
        .unwrap();
    store_draft_message(&h, &saved.message_id).await;
    h.sync
        .send(b"final".to_vec(), None, Some(saved.draft_id))
        .await
        .unwrap();
    assert_eq!(stored_pair(&h, &saved.message_id).await, None);
}

#[tokio::test]
async fn search_returns_newest_first_without_storing() {
    let h = harness().await;
    let now = now_millis();
    h.fake.seed(meta("old", "t1", now - 5000, &["INBOX"]));
    h.fake.seed(meta("new", "t2", now, &["INBOX"]));
    h.fake.seed(meta("mid", "t3", now - 1000, &[]));
    let found = h.sync.search("snippet", 2).await.unwrap();
    let ids: Vec<&str> = found.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, ["new", "mid"]);
    assert!(h.threads("INBOX").await.is_empty());
}

#[tokio::test]
async fn attachments_and_identity_come_from_gmail() {
    let h = harness().await;
    h.fake.with(|s| {
        s.attachments
            .insert(("m1".into(), "a1".into()), vec![1, 2, 3]);
    });
    assert_eq!(h.sync.attachment("m1", "a1").await.unwrap(), vec![1, 2, 3]);
    assert!(matches!(
        h.sync.attachment("m1", "zz").await,
        Err(crate::SyncError::Gmail(GmailError::NotFound))
    ));
    assert_eq!(h.sync.display_name().await.unwrap().as_deref(), Some("Me"));
}

#[tokio::test]
async fn the_automatic_reply_and_signature_pass_through() {
    let h = harness().await;
    h.fake.with(|s| s.signature = Some("Me\nExample Co".into()));
    assert_eq!(
        h.sync.gmail_signature().await.unwrap().as_deref(),
        Some("Me\nExample Co")
    );
    assert!(!h.sync.vacation().await.unwrap().enabled);
    let away = mailrs_domain::Vacation {
        enabled: true,
        subject: "Away".into(),
        body: "Back Monday".into(),
        start: Some(1_700_000_000_000),
        ..Default::default()
    };
    h.sync.set_vacation(away.clone()).await.unwrap();
    assert_eq!(h.sync.vacation().await.unwrap(), away);
    h.fake
        .with(|s| s.failures.push_back(GmailError::MissingScope));
    assert!(matches!(
        h.sync.vacation().await,
        Err(crate::SyncError::Gmail(GmailError::MissingScope))
    ));
}

#[tokio::test]
async fn a_scheduled_draft_sends_once() {
    let h = harness().await;
    let saved = h
        .sync
        .save_draft(b"later".to_vec(), None, None)
        .await
        .unwrap();
    assert!(h.sync.send_draft(&saved.draft_id).await.unwrap().is_some());
    assert_eq!(h.fake.with(|s| s.sent.clone()), [(b"later".to_vec(), None)]);
    assert!(h.fake.with(|s| s.drafts.is_empty()));
    assert_eq!(h.sync.send_draft(&saved.draft_id).await.unwrap(), None);
}

#[tokio::test]
async fn filters_pass_through_and_a_gone_filter_deletes_quietly() {
    let h = harness().await;
    let created = h
        .sync
        .create_filter(mailrs_domain::Filter::block("pest@example.com"))
        .await
        .unwrap();
    let listed = h.sync.filters().await.unwrap();
    assert_eq!(listed, vec![created.clone()]);
    let id = created.id.unwrap();
    h.sync.delete_filter(&id).await.unwrap();
    h.sync.delete_filter(&id).await.unwrap();
    assert!(h.sync.filters().await.unwrap().is_empty());
}

// ---- The outbox: what waits here, and what goes to the person ------------

fn queue(h: &super::Harness) -> Outbox<Connected> {
    let connected = HashMap::from([(h.account_id, Arc::clone(&h.sync))]);
    Outbox::new(Arc::new(Connected(connected)), h.db.clone())
}

fn message(account_id: mailrs_domain::AccountId, subject: &str) -> Queued {
    Queued {
        account_id,
        subject: subject.into(),
        recipients: "ann@example.com".into(),
        send_at: now_millis(),
        raw: Some(format!("Subject: {subject}\r\n\r\nhello").into_bytes()),
        composer: "{}".into(),
        ..Queued::default()
    }
}

fn http(status: u16) -> GmailError {
    GmailError::Http {
        status,
        body: "no".into(),
    }
}

#[tokio::test]
async fn a_message_that_cannot_go_out_now_waits_with_its_bytes() {
    let h = harness().await;
    h.fake.fail_next(GmailError::Network("offline".into()));
    let posted = queue(&h)
        .post(message(h.account_id, "Report"))
        .await
        .unwrap();
    let Posted::Waiting(id) = posted else {
        panic!("expected it to wait, got {posted:?}");
    };

    let waiting =
        h.db.read(move |c| outbox::find(c, id))
            .await
            .unwrap()
            .unwrap();
    assert_eq!(waiting.attempts, 1);
    assert!(waiting.problem.is_some(), "the row says what went wrong");
    assert_eq!(
        waiting.raw.as_deref(),
        Some(&b"Subject: Report\r\n\r\nhello"[..]),
        "the bytes are here, not in Gmail"
    );
    assert!(
        waiting.send_at > now_millis(),
        "the next try is a wait away"
    );
    assert!(h.fake.with(|s| s.sent.is_empty()));
}

#[tokio::test]
async fn a_refused_message_goes_back_to_the_person_and_not_into_the_outbox() {
    let h = harness().await;
    h.fake.fail_next(http(400));
    let posted = queue(&h)
        .post(message(h.account_id, "Bad address"))
        .await
        .unwrap();
    assert!(
        matches!(posted, Posted::Refused(_)),
        "a refused recipient is the person's to fix, got {posted:?}"
    );
    assert!(h.db.read(outbox::list).await.unwrap().is_empty());
}

#[tokio::test]
async fn the_outbox_sends_what_is_due_and_clears_the_draft_behind_it() {
    let h = harness().await;
    let draft = h
        .sync
        .save_draft(b"early".to_vec(), None, None)
        .await
        .unwrap()
        .draft_id;
    let mut waiting = message(h.account_id, "Report");
    waiting.draft_id = Some(draft);
    waiting.problem = Some("Network error".into());
    waiting.attempts = 1;
    h.db.write(move |c| outbox::put(c, &waiting).map(|_| ()))
        .await
        .unwrap();

    let drained = queue(&h).send_due(now_millis()).await.unwrap();
    assert_eq!(drained.sent.len(), 1);
    assert!(drained.stuck.is_empty());
    assert!(drained.changed);
    assert_eq!(h.fake.with(|s| s.sent.len()), 1, "it landed at Gmail");
    assert!(h.fake.with(|s| s.drafts.is_empty()), "the draft is gone");
    assert!(h.db.read(outbox::list).await.unwrap().is_empty());
}

#[tokio::test]
async fn every_failed_try_widens_the_wait_before_the_next_one() {
    let h = harness().await;
    h.fake.fail_next(GmailError::Network("offline".into()));
    let Posted::Waiting(id) = queue(&h)
        .post(message(h.account_id, "Report"))
        .await
        .unwrap()
    else {
        panic!("expected it to wait");
    };

    let mut waits = Vec::new();
    for _ in 0..3 {
        h.fake.fail_next(GmailError::Network("offline".into()));
        h.db.write(move |c| outbox::try_now(c, now_millis()))
            .await
            .unwrap();
        queue(&h).send_due(now_millis()).await.unwrap();
        let row =
            h.db.read(move |c| outbox::find(c, id))
                .await
                .unwrap()
                .unwrap();
        waits.push(row.send_at - now_millis());
    }
    assert!(
        waits.windows(2).all(|w| w[1] > w[0]),
        "each wait is longer than the last: {waits:?}"
    );
}

#[tokio::test]
async fn a_message_nothing_would_fix_stops_coming_round() {
    let h = harness().await;
    let mut waiting = message(h.account_id, "Too big");
    waiting.problem = Some("Network error".into());
    waiting.attempts = 1;
    h.db.write(move |c| outbox::put(c, &waiting).map(|_| ()))
        .await
        .unwrap();

    h.fake.fail_next(http(413));
    let drained = queue(&h).send_due(now_millis()).await.unwrap();
    assert_eq!(drained.stuck.len(), 1, "the person hears about it");
    let stuck = h.db.read(outbox::stuck).await.unwrap();
    assert_eq!(stuck.len(), 1);
    assert!(stuck[0].problem.as_deref().unwrap().contains("413"));

    let again = queue(&h).send_due(now_millis()).await.unwrap();
    assert!(!again.changed, "the outbox leaves it alone now");
    assert!(h.fake.with(|s| s.sent.is_empty()));
}

#[tokio::test]
async fn sending_a_stuck_message_by_hand_ignores_the_wait_it_was_serving() {
    let h = harness().await;
    let mut waiting = message(h.account_id, "Report");
    waiting.problem = Some("Network error".into());
    waiting.attempts = 4;
    waiting.send_at = now_millis() + 60 * 60 * 1000;
    let id = h.db.write(move |c| outbox::put(c, &waiting)).await.unwrap();

    let drained = queue(&h).send_due(now_millis()).await.unwrap();
    assert!(drained.sent.is_empty(), "its wait has not run out");
    let posted = queue(&h).send_one(id).await.unwrap();
    assert!(matches!(posted, Posted::Sent(_)), "got {posted:?}");
    assert!(h.db.read(outbox::list).await.unwrap().is_empty());
}

#[tokio::test]
async fn send_later_with_no_way_to_gmail_keeps_its_hour_and_its_bytes() {
    let h = harness().await;
    let at = now_millis() + 24 * 60 * 60 * 1000;
    let mut later = message(h.account_id, "Monday");
    later.send_at = at;
    h.fake.fail_next(GmailError::Network("offline".into()));

    let posted = queue(&h).schedule(later).await.unwrap();
    assert!(matches!(posted, Posted::Waiting(_)), "got {posted:?}");
    let scheduled = h.db.read(outbox::scheduled).await.unwrap();
    assert_eq!(
        scheduled.len(),
        1,
        "it is a Send Later message, not a stuck one"
    );
    assert_eq!(scheduled[0].send_at, at, "the hour the writer chose stands");
    assert!(
        scheduled[0].draft_id.is_none(),
        "Gmail holds no draft for it"
    );
    assert!(scheduled[0].raw.is_some(), "so the bytes stay here");
    assert!(h.db.read(outbox::stuck).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_scheduled_send_that_fails_moves_into_the_outbox() {
    let h = harness().await;
    let draft = h
        .sync
        .save_draft(b"monday".to_vec(), None, None)
        .await
        .unwrap()
        .draft_id;
    let account_id = h.account_id;
    h.db.write(move |c| {
        outbox::put(
            c,
            &Queued {
                account_id,
                draft_id: Some(draft),
                subject: "Monday".into(),
                recipients: "ann@example.com".into(),
                send_at: now_millis(),
                ..Queued::default()
            },
        )
        .map(|_| ())
    })
    .await
    .unwrap();
    assert_eq!(h.db.read(outbox::scheduled).await.unwrap().len(), 1);

    h.fake.fail_next(http(503));
    queue(&h).send_due(now_millis()).await.unwrap();
    assert!(h.db.read(outbox::scheduled).await.unwrap().is_empty());
    assert_eq!(h.db.read(outbox::stuck).await.unwrap().len(), 1);
}

#[tokio::test]
async fn an_account_that_is_not_connected_yet_costs_a_message_nothing() {
    let h = harness().await;
    let mut waiting = message(h.account_id, "Report");
    waiting.problem = Some("Network error".into());
    waiting.attempts = 3;
    let id = h.db.write(move |c| outbox::put(c, &waiting)).await.unwrap();
    let nobody = Outbox::new(Arc::new(Connected(HashMap::new())), h.db.clone());

    let drained = nobody.send_due(now_millis()).await.unwrap();
    assert!(!drained.changed, "nothing happened to it");
    let row =
        h.db.read(move |c| outbox::find(c, id))
            .await
            .unwrap()
            .unwrap();
    assert_eq!(row.attempts, 3, "the try was never made, so it is not one");
    assert_eq!(row.problem.as_deref(), Some("Network error"));
}

#[tokio::test]
async fn a_message_queued_before_a_restart_goes_out_after_one() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mail.db");
    let fake = Arc::new(crate::fake::FakeGmail::new());
    let (sender, _events) = async_channel::unbounded();
    let outbox_over = |db: &mailrs_store::Db| {
        let sync = Arc::new(crate::AccountSync::new(
            1,
            Arc::clone(&fake),
            db.clone(),
            sender.clone(),
        ));
        Outbox::new(Arc::new(Connected(HashMap::from([(1, sync)]))), db.clone())
    };

    let db = mailrs_store::Db::open(&path).unwrap();
    db.write(|c| mailrs_store::accounts::insert_account(c, "me@example.com", 0))
        .await
        .unwrap();
    fake.fail_next(GmailError::Network("offline".into()));
    let posted = outbox_over(&db).post(message(1, "Report")).await.unwrap();
    assert!(matches!(posted, Posted::Waiting(_)), "got {posted:?}");
    drop(db);

    // A new run of the app, over the same file the last one left behind.
    let db = mailrs_store::Db::open(&path).unwrap();
    let later = now_millis() + 24 * 60 * 60 * 1000;
    let drained = outbox_over(&db).send_due(later).await.unwrap();
    assert_eq!(drained.sent.len(), 1, "the message was still here");
    assert_eq!(
        fake.with(|s| s.sent.clone()),
        [(b"Subject: Report\r\n\r\nhello".to_vec(), None)],
        "and went out byte for byte as it was written"
    );
    assert!(db.read(outbox::list).await.unwrap().is_empty());
}

#[tokio::test]
async fn the_network_coming_back_brings_every_stuck_message_forward() {
    let h = harness().await;
    let mut waiting = message(h.account_id, "Report");
    waiting.problem = Some("Network error".into());
    waiting.attempts = 6;
    waiting.send_at = now_millis() + 30 * 60 * 1000;
    h.db.write(move |c| outbox::put(c, &waiting).map(|_| ()))
        .await
        .unwrap();

    let outbox = queue(&h);
    assert!(outbox.send_due(now_millis()).await.unwrap().sent.is_empty());
    outbox.try_now().await.unwrap();
    assert_eq!(outbox.send_due(now_millis()).await.unwrap().sent.len(), 1);
}

#[tokio::test]
async fn cancelling_send_later_stops_the_named_messages_and_keeps_their_drafts() {
    let h = harness().await;
    let outbox = queue(&h);
    let at = now_millis() + 24 * 60 * 60 * 1000;
    let later = |subject: &str| {
        let mut later = message(h.account_id, subject);
        later.send_at = at;
        later
    };
    outbox.schedule(later("Monday")).await.unwrap();
    outbox.schedule(later("Tuesday")).await.unwrap();
    h.fake.fail_next(GmailError::Network("offline".into()));
    let Posted::Waiting(offline) = outbox.schedule(later("Wednesday")).await.unwrap() else {
        panic!("Send Later keeps a message it could not hand to Gmail");
    };
    let rows = h.db.read(outbox::scheduled).await.unwrap();
    let row = |subject: &str| rows.iter().find(|r| r.subject == subject).unwrap().clone();
    let (monday, tuesday) = (row("Monday"), row("Tuesday"));

    // The list names the first by its Gmail thread, and the one Gmail has
    // never seen by its place in the table.
    let targets = [
        Target::thread(h.account_id, monday.thread_id.clone().unwrap()),
        Target::thread(h.account_id, outbox_row(offline)),
    ];
    assert_eq!(
        outbox.cancel_scheduled(&targets).await.unwrap(),
        Cancelled {
            in_drafts: 2,
            unsaved: vec![]
        },
        "the one Gmail never saw goes to Drafts now that Gmail answers"
    );
    let left = h.db.read(outbox::scheduled).await.unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].subject, "Tuesday");
    assert_eq!(
        h.fake.with(|s| s.drafts.len()),
        3,
        "the Gmail drafts stay in Drafts, and Wednesday joins them"
    );

    // An open draft names its message rather than its thread.
    let by_message = Target {
        account_id: h.account_id,
        thread_id: "another thread".into(),
        message_id: tuesday.message_id.clone(),
    };
    assert_eq!(
        outbox.cancel_scheduled(&[by_message]).await.unwrap(),
        Cancelled {
            in_drafts: 1,
            unsaved: vec![]
        }
    );
    assert!(h.db.read(outbox::scheduled).await.unwrap().is_empty());
}

#[tokio::test]
async fn cancelling_a_message_gmail_never_had_while_gmail_is_away_keeps_it() {
    let h = harness().await;
    let outbox = queue(&h);
    let mut later = message(h.account_id, "Wednesday");
    later.send_at = now_millis() + 24 * 60 * 60 * 1000;
    h.fake.fail_next(GmailError::Network("offline".into()));
    let Posted::Waiting(offline) = outbox.schedule(later).await.unwrap() else {
        panic!("Send Later keeps a message it could not hand to Gmail");
    };

    h.fake.fail_next(GmailError::NeedsReauth);
    let cancelled = outbox
        .cancel_scheduled(&[Target::thread(h.account_id, outbox_row(offline))])
        .await
        .unwrap();
    assert_eq!(cancelled.in_drafts, 0);
    assert_eq!(
        cancelled.unsaved.len(),
        1,
        "the app reopens it for the writer"
    );
    assert_eq!(cancelled.unsaved[0].subject, "Wednesday");
    assert_eq!(
        h.db.read(outbox::scheduled).await.unwrap().len(),
        1,
        "nothing is lost until the writer has it open again"
    );
}

#[tokio::test]
async fn a_reopened_draft_finds_the_hour_send_later_gave_it() {
    let h = harness().await;
    let outbox = queue(&h);
    let mut later = message(h.account_id, "Monday");
    later.send_at = now_millis() + 60_000;
    let at = later.send_at;
    outbox.schedule(later).await.unwrap();
    let draft_id = h.fake.with(|s| s.drafts.keys().next().unwrap().clone());

    let found = outbox.find_draft(h.account_id, &draft_id).await.unwrap();
    assert_eq!(found.map(|q| q.send_at), Some(at));
    assert_eq!(outbox.find_draft(h.account_id, "nope").await.unwrap(), None);
}

#[tokio::test]
async fn a_scheduled_draft_saved_again_keeps_its_hour_under_its_new_message() {
    let h = harness().await;
    let outbox = queue(&h);
    let mut later = message(h.account_id, "Monday");
    later.send_at = now_millis() + 60_000;
    let at = later.send_at;
    outbox.schedule(later).await.unwrap();
    let before = h.db.read(outbox::scheduled).await.unwrap().remove(0);
    let draft_id = before.draft_id.clone().expect("Gmail holds its draft");

    let saved = h
        .sync
        .save_draft(
            b"Subject: Monday\r\n\r\nEdited".to_vec(),
            None,
            Some(draft_id),
        )
        .await
        .unwrap();
    assert_ne!(Some(&saved.message_id), before.message_id.as_ref());
    outbox
        .draft_saved(h.account_id, saved.clone())
        .await
        .unwrap();

    let after = h.db.read(outbox::scheduled).await.unwrap();
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].send_at, at, "the hour stands");
    assert_eq!(after[0].message_id, Some(saved.message_id));
    assert_eq!(after[0].thread_id, Some(saved.thread_id));

    // A draft nothing waits on leaves the table alone.
    let other = h
        .sync
        .save_draft(b"Subject: Other\r\n\r\nHi".to_vec(), None, None)
        .await
        .unwrap();
    outbox.draft_saved(h.account_id, other).await.unwrap();
    assert_eq!(h.db.read(outbox::scheduled).await.unwrap(), after);
}

/// A list row finds its message in either mailbox: a Send Later row by its
/// Gmail thread, an Outbox row by its place in the table.
#[tokio::test]
async fn a_row_names_its_waiting_message_in_send_later_and_the_outbox() {
    let h = harness().await;
    let outbox = queue(&h);
    let mut later = message(h.account_id, "Monday");
    later.send_at = now_millis() + 24 * 60 * 60 * 1000;
    outbox.schedule(later).await.unwrap();
    let mut stuck = message(h.account_id, "Stuck");
    stuck.problem = Some("Gmail said no".into());
    let stuck = h.db.write(move |c| outbox::put(c, &stuck)).await.unwrap();
    let monday = h.db.read(outbox::scheduled).await.unwrap()[0].clone();

    let named = outbox
        .named(&[
            Target::thread(h.account_id, monday.thread_id.clone().unwrap()),
            Target::thread(h.account_id, outbox_row(stuck)),
            Target::thread(h.account_id, "someone else's thread"),
        ])
        .await
        .unwrap();
    let subjects: Vec<&str> = named.iter().map(|m| m.subject.as_str()).collect();
    assert_eq!(subjects.len(), 2);
    assert!(subjects.contains(&"Monday") && subjects.contains(&"Stuck"));
}

/// A new hour for Send Later changes only the hour. A message in the
/// Outbox keeps the time of its next try.
#[tokio::test]
async fn reschedule_moves_a_send_later_message_and_leaves_the_outbox_alone() {
    let h = harness().await;
    let outbox = queue(&h);
    let day = 24 * 60 * 60 * 1000;
    let mut later = message(h.account_id, "Monday");
    later.send_at = now_millis() + day;
    let Posted::Waiting(id) = outbox.schedule(later).await.unwrap() else {
        panic!("Send Later keeps the message");
    };
    let at = now_millis() + 2 * day;
    let moved = outbox.reschedule(id, at).await.unwrap().expect("it moves");
    assert_eq!(moved.send_at, at);
    let stored =
        h.db.read(move |c| outbox::find(c, id))
            .await
            .unwrap()
            .unwrap();
    assert_eq!(stored, moved);

    let mut stuck = message(h.account_id, "Stuck");
    stuck.problem = Some("Gmail said no".into());
    let before = stuck.send_at;
    let stuck = h.db.write(move |c| outbox::put(c, &stuck)).await.unwrap();
    assert_eq!(outbox.reschedule(stuck, at).await.unwrap(), None);
    let kept =
        h.db.read(move |c| outbox::find(c, stuck))
            .await
            .unwrap()
            .unwrap();
    assert_eq!(kept.send_at, before);
    assert_eq!(outbox.reschedule(9999, at).await.unwrap(), None);
}
