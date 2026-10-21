use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use mailrs_domain::{AccountId, MailSet, MessageMeta, RemoveSetting, Role, Target};
use mailrs_gmail::GmailError;
use mailrs_store::{Db, accounts};

use super::microsoft::outlook;
use super::pop3::pop3_harness;
use super::{Connected, Harness, harness};
use crate::fake::{FakeGmail, FakeMail, FakePop3, has_label, meta, pop3_mail};
use crate::read_inbox::READ_PAGE;
use crate::{
    AccountServices, AccountSync, History, MailAction, MailActions, Permitted, TriageAction,
    now_millis,
};

const DAY: i64 = 24 * 60 * 60 * 1000;

fn actions(accounts: &[(AccountId, &Arc<AccountSync>)], db: &Db) -> MailActions<Connected> {
    MailActions::new(
        Arc::new(Connected(
            accounts
                .iter()
                .map(|(id, sync)| (*id, Arc::clone(sync)))
                .collect::<HashMap<_, _>>(),
        )),
        db.clone(),
        crate::OneClick::Fake(Arc::default()),
    )
}

/// A second Gmail account in `h`'s store, synced, holding `seeded`.
async fn second_gmail(
    h: &Harness,
    seeded: &[MessageMeta],
) -> (AccountId, Arc<FakeGmail>, Arc<AccountSync>) {
    let id =
        h.db.write(|c| accounts::insert_account(c, "you@example.com", 0))
            .await
            .unwrap();
    let fake = Arc::new(FakeGmail::new());
    for message in seeded {
        fake.seed(MessageMeta {
            account_id: id,
            ..message.clone()
        });
    }
    fake.with(|s| s.page_size = 1000);
    let (sender, _events) = async_channel::unbounded();
    let sync = Arc::new(
        AccountSync::new(
            id,
            AccountServices::google(Arc::clone(&fake)),
            h.db.clone(),
            sender,
        )
        .with_retry_max(Duration::from_millis(10)),
    );
    sync.bootstrap().await.unwrap();
    (id, fake, sync)
}

/// Unread inbox mail goes read on Gmail in pages, including a message
/// older than the store holds, while read mail and unread mail outside
/// the inbox stay as they were.
#[tokio::test]
async fn the_inbox_goes_read_on_the_server_past_what_the_store_holds() {
    let h = harness().await;
    let now = now_millis();
    h.fake.seed(meta("a", "t1", now, &["INBOX", "UNREAD"]));
    h.fake.seed(meta("b", "t2", now - 1, &["INBOX", "UNREAD"]));
    h.fake.seed(meta("r", "t3", now - 2, &["INBOX"]));
    h.fake.seed(meta("x", "t4", now - 3, &["UNREAD"]));
    h.bootstrap_all().await;
    // Seeded after the first sync and with no history, as mail is that
    // the store never fetched.
    h.fake
        .seed(meta("old", "t5", now - 400 * DAY, &["INBOX", "UNREAD"]));
    assert!(
        !h.threads(MailSet::Role(Role::Inbox))
            .await
            .contains(&"t5".to_string())
    );

    let read = actions(&[(h.account_id, &h.sync)], &h.db)
        .read_inboxes_in_pages(&[h.account_id], History::Record, 2)
        .await;

    assert!(read.failed.is_empty(), "{:?}", read.failed);
    assert_eq!((read.read, read.refused), (3, 0));
    let unread = |id: &str| h.fake.with(|s| has_label(&s.messages[id], "UNREAD"));
    for id in ["a", "b", "old", "r"] {
        assert!(!unread(id), "{id} is read on Gmail");
    }
    assert!(unread("x"), "unread mail outside the inbox stays unread");
    assert!(!h.labels_of("a").await.contains(&"UNREAD".to_string()));
}

/// Outlook lists unread inbox mail on the server, following its next link
/// past a short page, so a message the store never fetched goes read too.
/// A POP3 account's mail lives in the store, which answers for it.
#[tokio::test]
async fn outlook_asks_the_server_and_pop3_the_store() {
    let ms = outlook().await;
    let inbox = ms.fake.folder_id("inbox");
    let kept = ms.fake.deliver(
        &inbox,
        FakeMail {
            at: now_millis(),
            ..FakeMail::default()
        },
    );
    ms.bootstrap_all().await;
    let unseen = ms.fake.deliver(
        &inbox,
        FakeMail {
            at: now_millis() - 400 * DAY,
            ..FakeMail::default()
        },
    );
    assert!(!ms.stored(&unseen).await);
    ms.fake.with(|s| s.listing_page = Some(1));

    let read = actions(&[(ms.account_id, &ms.sync)], &ms.db)
        .read_inboxes_in_pages(&[ms.account_id], History::Record, 2)
        .await;

    assert!(read.failed.is_empty(), "{:?}", read.failed);
    assert_eq!(read.read, 2);
    for id in [&kept, &unseen] {
        let is_read = ms.fake.with(|s| s.messages[id].message.is_read);
        assert_eq!(is_read, Some(true), "{id} is read on Outlook");
    }

    let pop = pop3_harness(FakePop3::default(), RemoveSetting::Never).await;
    let pop_id = pop.keep("u1", &pop3_mail(1), "inbox", now_millis()).await;
    let read = actions(&[(pop.account_id, &pop.sync)], &pop.db)
        .read_inboxes_in_pages(&[pop.account_id], History::Record, 2)
        .await;
    assert_eq!(read.read, 1);
    assert!(!pop.ids_in(MailSet::Unseen).await.contains(&pop_id));
}

/// Refused pages that fill more than one listing page of the server still
/// leave the mail behind them reachable, since the run lists the whole
/// inbox before it changes any of it. The refusal is reported.
#[tokio::test]
async fn refused_pages_longer_than_a_listing_page_do_not_hide_the_mail_after() {
    let h = harness().await;
    let now = now_millis();
    let ids = ["a", "b", "c", "d", "e"];
    for (i, id) in ids.iter().enumerate() {
        h.fake.seed(meta(
            id,
            &format!("t{i}"),
            now - i as i64,
            &["INBOX", "UNREAD"],
        ));
    }
    h.bootstrap_all().await;
    h.fake.with(|s| s.page_size = 2);
    // A refusal stops the batch, so the rest of the page fails with it.
    for _ in 0..2 {
        h.fake.fail_call(
            "users.messages.modify",
            0,
            GmailError::Http {
                status: 400,
                body: "no".into(),
            },
        );
    }

    let read = actions(&[(h.account_id, &h.sync)], &h.db)
        .read_inboxes_in_pages(&[h.account_id], History::Record, 2)
        .await;

    let unread = |id: &str| h.fake.with(|s| has_label(&s.messages[id], "UNREAD"));
    for id in &ids[..4] {
        assert!(unread(id), "{id} was in a refused page");
    }
    assert!(!unread("e"), "the mail past them goes read");
    assert_eq!((read.read, read.refused), (1, 4));
    assert_eq!(read.failed.len(), 1);
    assert_eq!(read.failed[0].0, h.account_id);
}

/// An account whose server cannot be reached is reported, and the other
/// accounts still go read.
#[tokio::test]
async fn an_account_that_cannot_list_is_reported_beside_the_ones_that_could() {
    let h = harness().await;
    h.fake
        .seed(meta("a", "t1", now_millis(), &["INBOX", "UNREAD"]));
    h.bootstrap_all().await;
    let (other_id, other_fake, other) =
        second_gmail(&h, &[meta("x", "t2", now_millis(), &["INBOX", "UNREAD"])]).await;
    other_fake.with(|s| s.offline = true);

    let read = actions(&[(h.account_id, &h.sync), (other_id, &other)], &h.db)
        .read_inboxes(&[h.account_id, other_id], History::Record)
        .await;

    assert_eq!(read.read, 1);
    assert!(!h.fake.with(|s| has_label(&s.messages["a"], "UNREAD")));
    let failed: Vec<AccountId> = read.failed.iter().map(|(id, _)| *id).collect();
    assert_eq!(failed, [other_id]);
}

/// Every page of every account goes into one undo entry, so one Undo
/// makes all of it unread again.
#[tokio::test]
async fn one_undo_takes_back_every_page_of_every_account() {
    let h = harness().await;
    let now = now_millis();
    let mine: Vec<String> = (0..=READ_PAGE).map(|i| format!("m{i}")).collect();
    for (i, id) in mine.iter().enumerate() {
        let at = now - i as i64;
        h.fake
            .seed(meta(id, &format!("t{i}"), at, &["INBOX", "UNREAD"]));
    }
    h.bootstrap_all().await;
    h.fake.with(|s| s.page_size = 1000);
    let (other_id, other_fake, other) =
        second_gmail(&h, &[meta("x", "tx", now, &["INBOX", "UNREAD"])]).await;
    let actions = actions(&[(h.account_id, &h.sync), (other_id, &other)], &h.db);
    let unread_mine = || {
        h.fake.with(|s| {
            mine.iter()
                .filter(|id| has_label(&s.messages[id.as_str()], "UNREAD"))
                .count()
        })
    };
    let theirs = || other_fake.with(|s| has_label(&s.messages["x"], "UNREAD"));

    let read = actions
        .read_inboxes(&[h.account_id, other_id], History::Record)
        .await;
    assert_eq!(read.read, READ_PAGE + 2, "{:?}", read.failed);
    assert_eq!(unread_mine(), 0);
    assert!(!theirs());

    actions.undo().await.expect("an undo");
    assert_eq!(unread_mine(), READ_PAGE + 1, "one undo reaches every page");
    assert!(theirs(), "and every account");
    assert!(actions.undo().await.is_none(), "it was one action");
}

/// Undo leaves mail that left the inbox since alone, as the ordinary
/// Undo does, and still takes back the rest.
#[tokio::test]
async fn undo_leaves_mail_archived_since_where_it_is() {
    let h = harness().await;
    let now = now_millis();
    h.fake.seed(meta("a", "t1", now, &["INBOX", "UNREAD"]));
    h.fake.seed(meta("b", "t2", now - 1, &["INBOX", "UNREAD"]));
    h.bootstrap_all().await;
    let actions = actions(&[(h.account_id, &h.sync)], &h.db);
    let read = actions.read_inboxes(&[h.account_id], History::Record).await;
    assert_eq!(read.read, 2, "{:?}", read.failed);
    actions
        .run(
            &[Target::thread(h.account_id, "t1")],
            MailAction::Triage(TriageAction::Archive),
            History::Skip,
        )
        .await;

    let undone = actions
        .undo_this(read.recorded.expect("recorded"))
        .await
        .expect("an undo");

    let unread = |id: &str| h.fake.with(|s| has_label(&s.messages[id], "UNREAD"));
    assert!(!unread("a"), "the archived message stays read");
    assert!(unread("b"), "the one still in the inbox goes unread");
    assert_eq!(undone.outcome.failed.len(), 1);
    assert_eq!(undone.outcome.failed[0].target.thread_id, "t1");
}

/// Delete Forever and Empty Trash take their mail out of the staged Undo,
/// and the entry goes once nothing it changed is left.
#[tokio::test]
async fn erased_mail_leaves_the_staged_undo() {
    let h = harness().await;
    let now = now_millis();
    h.fake.seed(meta("a", "t1", now, &["INBOX", "UNREAD"]));
    h.fake.seed(meta("b", "t2", now - 1, &["INBOX", "UNREAD"]));
    h.bootstrap_all().await;
    let actions = actions(&[(h.account_id, &h.sync)], &h.db);
    let read = actions.read_inboxes(&[h.account_id], History::Record).await;
    assert_eq!(read.read, 2, "{:?}", read.failed);
    let both = [
        Target::thread(h.account_id, "t1"),
        Target::thread(h.account_id, "t2"),
    ];
    actions
        .run(
            &both,
            MailAction::Triage(TriageAction::Trash),
            History::Skip,
        )
        .await;

    let erased = actions.erase(&both[..1]).await.unwrap();
    assert!(matches!(erased, Permitted::Done(o) if o.done.len() == 1));
    let Permitted::Done(emptied) = actions.empty_trash(h.account_id).await.unwrap() else {
        panic!("permission is granted");
    };
    assert_eq!(emptied.deleted, 1);

    assert!(
        actions.undo().await.is_none(),
        "nothing is left to take back"
    );
}

/// Emptying the Trash erases one message of a conversation, and Undo
/// still reaches the reply of it that stayed in the inbox.
#[tokio::test]
async fn emptying_the_trash_keeps_the_inbox_half_of_a_conversation_in_undo() {
    let h = harness().await;
    let now = now_millis();
    h.fake.seed(meta("a", "t1", now, &["INBOX", "UNREAD"]));
    h.fake.seed(meta("z", "t1", now - 1, &["TRASH"]));
    h.bootstrap_all().await;
    let actions = actions(&[(h.account_id, &h.sync)], &h.db);
    let read = actions.read_inboxes(&[h.account_id], History::Record).await;
    assert_eq!(read.read, 1, "{:?}", read.failed);

    let Permitted::Done(emptied) = actions.empty_trash(h.account_id).await.unwrap() else {
        panic!("permission is granted");
    };
    assert_eq!(emptied.deleted, 1);

    actions
        .undo()
        .await
        .expect("the inbox message is still to undo");
    assert!(h.fake.with(|s| has_label(&s.messages["a"], "UNREAD")));
}

/// An account with no unread mail puts nothing on the undo entry, so the
/// entry goes with the only account it changed.
#[tokio::test]
async fn an_account_with_nothing_unread_leaves_no_undo_behind() {
    let h = harness().await;
    h.fake
        .seed(meta("a", "t1", now_millis(), &["INBOX", "UNREAD"]));
    h.bootstrap_all().await;
    let (other_id, _, other) = second_gmail(&h, &[meta("r", "t2", now_millis(), &["INBOX"])]).await;
    let actions = actions(&[(h.account_id, &h.sync), (other_id, &other)], &h.db);
    let read = actions
        .read_inboxes(&[h.account_id, other_id], History::Record)
        .await;
    assert_eq!(read.read, 1, "{:?}", read.failed);

    actions.forget_account(h.account_id);

    assert!(actions.undo().await.is_none());
}
