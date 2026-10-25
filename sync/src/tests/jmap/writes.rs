//! Moves, marks and Delete Forever over FakeJmap.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use mailrs_domain::Target;
use mailrs_domain::mailbox::keyword::{FLAGGED, SEEN};

use super::{fastmail, over, raw};
use crate::fake::FakeJmap;
use crate::{BackendError, History, MailAction, MailActions, MailBackend, MailOp, TriageAction};

fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[tokio::test]
async fn removing_the_last_mailbox_files_the_message_in_archive() {
    let h = fastmail().await;
    let id = h.fake.deliver("inbox", &raw("a1@x", "Archive me"), now());
    h.mail().mailboxes().await.unwrap();
    h.mail()
        .apply(
            std::slice::from_ref(&id),
            &[MailOp::RemoveFromMailbox(h.fake.mailbox("inbox"))],
        )
        .await
        .unwrap();
    assert_eq!(
        h.fake.with(|s| s.emails[&id].mailbox_ids.clone()),
        BTreeSet::from([h.fake.mailbox("archive")])
    );
}

#[tokio::test]
async fn archive_on_a_label_account_keeps_the_message_s_id() {
    let h = fastmail().await;
    let id = h.fake.deliver("inbox", &raw("a2@x", "Labels"), now());
    h.bootstrap_all().await;
    let thread = h.fake.with(|s| s.emails[&id].thread_id.clone());
    h.sync
        .triage_all(
            &[Target::thread(h.account_id, thread)],
            &TriageAction::Archive,
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        h.fake.with(|s| s.emails[&id].mailbox_ids.clone()),
        BTreeSet::from([h.fake.mailbox("archive")])
    );
    assert!(h.stored(&id).await);
}

#[tokio::test]
async fn archive_on_a_folder_account_moves_the_message() {
    let h = over(FakeJmap::new().mailboxes_per_email(Some(1))).await;
    let id = h.fake.deliver("inbox", &raw("a3@x", "Folders"), now());
    h.bootstrap_all().await;
    let thread = h.fake.with(|s| s.emails[&id].thread_id.clone());
    h.sync
        .triage_all(
            &[Target::thread(h.account_id, thread)],
            &TriageAction::Archive,
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        h.fake.with(|s| s.emails[&id].mailbox_ids.clone()),
        BTreeSet::from([h.fake.mailbox("archive")])
    );
}

#[tokio::test]
async fn a_flag_touches_only_the_flag() {
    let h = fastmail().await;
    let id = h.fake.deliver("inbox", &raw("f1@x", "Flag"), now());
    h.fake.set_keyword(&id, SEEN, true);
    h.mail().mailboxes().await.unwrap();
    h.mail()
        .apply(
            std::slice::from_ref(&id),
            &[MailOp::SetKeyword {
                keyword: FLAGGED.into(),
                on: true,
            }],
        )
        .await
        .unwrap();
    let marks = h.fake.with(|s| s.emails[&id].keywords.clone());
    assert_eq!(
        marks,
        BTreeSet::from([SEEN.to_string(), FLAGGED.to_string()])
    );
}

#[tokio::test]
async fn delete_forever_destroys_and_a_gone_message_counts_as_done() {
    let h = fastmail().await;
    let id = h.fake.deliver("trash", &raw("x1@x", "Bin"), now());
    h.mail().mailboxes().await.unwrap();
    h.mail()
        .apply(&[id.clone(), "gone".into()], &[MailOp::Destroy])
        .await
        .unwrap();
    assert!(h.fake.with(|s| !s.emails.contains_key(&id)));
}

/// A fake with no Archive mailbox, as a new Stalwart account has.
fn without_archive(fake: FakeJmap) -> FakeJmap {
    fake.with(|s| {
        let archive = s.role_id("archive").unwrap();
        s.mailboxes.remove(&archive);
    });
    fake
}

/// The mailbox the message `id` sits in, by name and role.
fn home_of(h: &super::Harness, id: &str) -> Vec<(String, Option<String>)> {
    h.fake.with(|s| {
        s.emails[id]
            .mailbox_ids
            .iter()
            .map(|m| (s.mailboxes[m].name.clone(), s.mailboxes[m].role.clone()))
            .collect()
    })
}

#[tokio::test]
async fn without_an_archive_the_last_mailbox_makes_one() {
    let h = over(without_archive(FakeJmap::new())).await;
    let id = h.fake.deliver("inbox", &raw("n1@x", "Stay"), now());
    h.mail().mailboxes().await.unwrap();
    h.mail()
        .apply(
            std::slice::from_ref(&id),
            &[MailOp::RemoveFromMailbox(h.fake.mailbox("inbox"))],
        )
        .await
        .unwrap();
    assert_eq!(
        home_of(&h, &id),
        [("Archive".to_string(), Some("archive".to_string()))]
    );
    assert_eq!(
        h.mail().mailbox_for(mailrs_domain::Role::Archive),
        Some(h.fake.mailbox("archive"))
    );
}

#[tokio::test]
async fn archive_without_an_archive_mailbox_makes_one() {
    let h = over(without_archive(
        FakeJmap::new().mailboxes_per_email(Some(1)),
    ))
    .await;
    let id = h.fake.deliver("inbox", &raw("n2@x", "File me"), now());
    h.mail().mailboxes().await.unwrap();
    h.mail()
        .apply(
            std::slice::from_ref(&id),
            &[MailOp::MoveToRole(mailrs_domain::Role::Archive)],
        )
        .await
        .unwrap();
    assert_eq!(
        home_of(&h, &id),
        [("Archive".to_string(), Some("archive".to_string()))]
    );
}

#[tokio::test]
async fn a_server_that_refuses_the_role_gets_a_plain_archive_mailbox() {
    let h = over(without_archive(FakeJmap::new())).await;
    h.fake.with(|s| s.refuse_mailbox_roles = true);
    let id = h.fake.deliver("inbox", &raw("n3@x", "File me"), now());
    h.mail().mailboxes().await.unwrap();
    h.mail()
        .apply(
            std::slice::from_ref(&id),
            &[MailOp::RemoveFromMailbox(h.fake.mailbox("inbox"))],
        )
        .await
        .unwrap();
    assert_eq!(home_of(&h, &id), [("Archive".to_string(), None)]);
}

#[tokio::test]
async fn a_top_level_archive_without_the_role_counts_as_archive() {
    let h = over(without_archive(FakeJmap::new())).await;
    let plain = h.fake.add_mailbox("Archive", None);
    let id = h.fake.deliver("inbox", &raw("n4@x", "File me"), now());
    let listed = h.mail().mailboxes().await.unwrap();
    assert_eq!(
        listed.iter().find(|m| m.id == plain).and_then(|m| m.role),
        Some(mailrs_domain::Role::Archive)
    );
    h.fake.with(|s| s.calls.clear());
    h.mail()
        .apply(
            std::slice::from_ref(&id),
            &[MailOp::RemoveFromMailbox(h.fake.mailbox("inbox"))],
        )
        .await
        .unwrap();
    assert_eq!(
        h.fake.with(|s| s.emails[&id].mailbox_ids.clone()),
        BTreeSet::from([plain])
    );
    assert!(
        h.fake.with(|s| !s.calls.iter().any(|c| c == "Mailbox/set")),
        "the Archive there is used"
    );
}

#[tokio::test]
async fn when_no_archive_can_be_made_the_last_mailbox_stays_and_says_why() {
    let h = over(without_archive(FakeJmap::new())).await;
    h.fake.with(|s| s.refuse_mailbox_creates = true);
    let id = h.fake.deliver("inbox", &raw("n5@x", "Stay"), now());
    h.mail().mailboxes().await.unwrap();
    let refused = h
        .mail()
        .apply(
            std::slice::from_ref(&id),
            &[MailOp::RemoveFromMailbox(h.fake.mailbox("inbox"))],
        )
        .await
        .unwrap_err();
    assert_eq!(refused.taken, 0);
    let BackendError::Refused(why) = refused.error else {
        panic!("{:?}", refused.error)
    };
    assert!(why.contains("no Archive"), "{why}");
    assert_eq!(h.fake.with(|s| s.emails[&id].mailbox_ids.len()), 1);
}

#[tokio::test]
async fn a_category_is_refused_in_words() {
    let h = fastmail().await;
    let id = h.fake.deliver("inbox", &raw("c1@x", "Cat"), now());
    let refused = h
        .mail()
        .apply(
            &[id],
            &[MailOp::SetCategory {
                category: "social".into(),
                on: true,
            }],
        )
        .await
        .unwrap_err();
    assert!(matches!(refused.error, BackendError::Refused(_)));
}

fn flag(on: bool) -> [MailOp; 1] {
    [MailOp::SetKeyword {
        keyword: FLAGGED.into(),
        on,
    }]
}

fn deliver_many(h: &super::Harness, count: usize) -> Vec<String> {
    (0..count)
        .map(|n| {
            h.fake.deliver(
                "inbox",
                &raw(&format!("m{n}@x"), &format!("Many {n}")),
                now() + n as i64,
            )
        })
        .collect()
}

#[tokio::test]
async fn a_write_is_split_to_the_server_s_set_limit() {
    let h = over(FakeJmap::new().objects_in_set(2)).await;
    let ids = deliver_many(&h, 5);
    h.mail().mailboxes().await.unwrap();
    h.mail().apply(&ids, &flag(true)).await.unwrap();
    assert!(
        h.fake
            .with(|s| ids.iter().all(|id| s.emails[id].keywords.contains(FLAGGED)))
    );
}

#[tokio::test]
async fn a_refusal_mid_chunk_counts_the_messages_before_it() {
    let h = fastmail().await;
    let ids = deliver_many(&h, 3);
    h.fake.with(|s| s.refused.insert(ids[1].clone()));
    h.mail().mailboxes().await.unwrap();
    let refused = h.mail().apply(&ids, &flag(true)).await.unwrap_err();
    assert_eq!(refused.taken, 1);
    assert!(matches!(refused.error, BackendError::Refused(_)));
}

#[tokio::test]
async fn a_refused_destroy_counts_the_messages_before_it() {
    let h = fastmail().await;
    let ids = deliver_many(&h, 3);
    h.fake.with(|s| s.refused.insert(ids[2].clone()));
    h.mail().mailboxes().await.unwrap();
    let refused = h.mail().apply(&ids, &[MailOp::Destroy]).await.unwrap_err();
    assert_eq!(refused.taken, 2);
}

#[tokio::test]
async fn a_second_chunk_that_fails_leaves_the_first_counted() {
    let h = over(FakeJmap::new().objects_in_set(2)).await;
    let ids = deliver_many(&h, 5);
    h.fake.with(|s| s.refused.insert(ids[2].clone()));
    h.mail().mailboxes().await.unwrap();
    let refused = h.mail().apply(&ids, &flag(true)).await.unwrap_err();
    assert_eq!(refused.taken, 2);
}

#[tokio::test]
async fn a_message_in_two_mailboxes_losing_one_gets_no_archive() {
    let h = fastmail().await;
    let id = h.fake.deliver("inbox", &raw("t1@x", "Two"), now());
    let junk = h.fake.mailbox("junk");
    h.fake.with(|s| {
        s.emails
            .get_mut(&id)
            .unwrap()
            .mailbox_ids
            .insert(junk.clone())
    });
    h.mail().mailboxes().await.unwrap();
    h.mail()
        .apply(
            std::slice::from_ref(&id),
            &[MailOp::RemoveFromMailbox(h.fake.mailbox("inbox"))],
        )
        .await
        .unwrap();
    assert_eq!(
        h.fake.with(|s| s.emails[&id].mailbox_ids.clone()),
        BTreeSet::from([junk])
    );
}

#[tokio::test]
async fn only_the_messages_left_in_no_mailbox_go_to_archive() {
    let h = fastmail().await;
    let alone = h.fake.deliver("inbox", &raw("t2@x", "Alone"), now());
    let shared = h.fake.deliver("inbox", &raw("t3@x", "Shared"), now() + 1);
    let junk = h.fake.mailbox("junk");
    h.fake.with(|s| {
        s.emails
            .get_mut(&shared)
            .unwrap()
            .mailbox_ids
            .insert(junk.clone())
    });
    h.mail().mailboxes().await.unwrap();
    h.mail()
        .apply(
            &[alone.clone(), shared.clone()],
            &[MailOp::RemoveFromMailbox(h.fake.mailbox("inbox"))],
        )
        .await
        .unwrap();
    assert_eq!(
        h.fake.with(|s| s.emails[&alone].mailbox_ids.clone()),
        BTreeSet::from([h.fake.mailbox("archive")])
    );
    assert_eq!(
        h.fake.with(|s| s.emails[&shared].mailbox_ids.clone()),
        BTreeSet::from([junk])
    );
}

#[tokio::test]
async fn adding_a_mailbox_and_removing_it_again_still_files_in_archive() {
    let h = fastmail().await;
    let id = h.fake.deliver("inbox", &raw("t4@x", "Undo"), now());
    let junk = h.fake.mailbox("junk");
    h.mail().mailboxes().await.unwrap();
    let ops = [
        MailOp::AddToMailbox(junk.clone()),
        MailOp::RemoveFromMailbox(h.fake.mailbox("inbox")),
        MailOp::RemoveFromMailbox(junk),
    ];
    h.mail()
        .apply(std::slice::from_ref(&id), &ops)
        .await
        .unwrap();
    assert_eq!(
        h.fake.with(|s| s.emails[&id].mailbox_ids.clone()),
        BTreeSet::from([h.fake.mailbox("archive")])
    );
}

#[tokio::test]
async fn empty_trash_keeps_every_successful_deletion_in_a_refused_batch() {
    for refused in 0..3 {
        let h = fastmail().await;
        let ids: Vec<_> = (0..3)
            .map(|n| {
                h.fake.deliver(
                    "trash",
                    &raw(&format!("trash{n}@example.com"), "Old mail"),
                    1,
                )
            })
            .collect();
        h.bootstrap_all().await;
        let actions = MailActions::new(
            Arc::new(crate::tests::Connected(HashMap::from([(
                h.account_id,
                Arc::clone(&h.sync),
            )]))),
            h.db.clone(),
            crate::OneClick::Fake(Arc::default()),
        );
        let targets: Vec<_> = ids
            .iter()
            .map(|id| Target {
                account_id: h.account_id,
                thread_id: h.fake.with(|s| s.emails[id].thread_id.clone()),
                message_id: Some(id.clone()),
            })
            .collect();
        let marked = actions
            .run(
                &targets,
                MailAction::Triage(TriageAction::MarkRead),
                History::Record,
            )
            .await;
        assert_eq!(marked.done.len(), 3);
        assert!(marked.failed.is_empty());
        h.fake.with(|s| s.refused.insert(ids[refused].clone()));
        let crate::Permitted::Done(outcome) = actions.empty_trash(h.account_id).await.unwrap()
        else {
            panic!("granted")
        };
        assert_eq!(outcome.deleted, 2, "refused position {refused}");
        assert_eq!(outcome.failed, 1);
        for (at, id) in ids.iter().enumerate() {
            assert_eq!(h.fake.with(|s| s.emails.contains_key(id)), at == refused);
            assert_eq!(h.stored(id).await, at == refused);
        }
        h.fake.with(|s| s.refused.clear());
        let undone = actions
            .undo()
            .await
            .expect("the refused message keeps its Undo");
        assert_eq!(undone.outcome.done, [targets[refused].clone()]);
        assert!(undone.outcome.failed.is_empty());
        assert!(actions.undo().await.is_none());
    }
}

#[tokio::test]
async fn empty_trash_rejects_a_restore_between_the_jmap_check_and_delete() {
    let h = fastmail().await;
    let id = h
        .fake
        .deliver("trash", &raw("restore@example.com", "Keep me"), 1);
    h.bootstrap_all().await;
    h.fake
        .with(|s| s.move_before_set = Some((id.clone(), "inbox".into())));
    let crate::Permitted::Done(result) = h.sync.empty_trash(|_| {}).await.unwrap() else {
        panic!("granted")
    };
    assert_eq!(result.deleted, 0);
    assert_eq!(result.failed, 1);
    assert!(h.fake.with(|s| s.emails.contains_key(&id)));
}
