use mailrs_gmail::GmailError;

use super::harness;
use crate::fake::meta;
use crate::now_millis;

#[tokio::test]
async fn sending_a_saved_draft_deletes_the_draft() {
    let h = harness().await;
    let draft = h
        .sync
        .save_draft(b"draft one".to_vec(), None, None)
        .await
        .unwrap();
    let same = h
        .sync
        .save_draft(b"draft two".to_vec(), None, Some(draft.clone()))
        .await
        .unwrap();
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
        .unwrap();
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
    let message = h.fake.with(|s| s.draft_messages[&id].clone());
    assert_eq!(h.sync.draft_id_for(&message).await.unwrap(), Some(id));
    assert_eq!(h.sync.draft_id_for("other").await.unwrap(), None);
}

#[tokio::test]
async fn search_returns_newest_first_without_storing() {
    let h = harness().await;
    let now = now_millis();
    h.fake.seed(meta("old", "t1", now - 5000, &["INBOX"]));
    h.fake.seed(meta("new", "t2", now, &["INBOX"]));
    h.fake.seed(meta("mid", "t3", now - 1000, &[]));
    let found = h.sync.search("anything", 2).await.unwrap();
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
