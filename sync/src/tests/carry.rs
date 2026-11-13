//! Carrying mail between accounts: a message read from one server,
//! landed on another with its date and marks, and its original taken
//! away only once the copy is safe. Undo runs the same road backwards.
//! Both accounts here are Gmail fakes over one store, because the road,
//! not the protocol, is what these tests measure.

use std::collections::HashMap;
use std::sync::Arc;

use mailrs_domain::{ChangeEvent, MailSet, MailboxKind, RemoteMailbox, Role, Target};
use mailrs_store::{accounts, mailboxes};

use super::{Connected, Harness, harness};
use crate::fake::{FakeGmail, meta};
use crate::mailbox::Mailbox;
use crate::{AccountServices, AccountSync, History, MailAction, MailActions, MovedFrom};

/// A second Gmail account sharing the harness's store, bootstrapped
/// empty, with its fake kept for the test to read.
struct Other {
    fake: Arc<FakeGmail>,
    sync: Arc<AccountSync>,
    /// Held so the sync's change events have somewhere to go.
    _events: async_channel::Receiver<ChangeEvent>,
}

async fn other(h: &Harness) -> Other {
    let account_id = h
        .db
        .write(|c| accounts::insert_account(c, "you@example.org", 0))
        .await
        .unwrap();
    assert_eq!(account_id, 2, "the second account is the second stored");
    h.db
        .write(move |c| {
            for (id, role) in mailrs_gmail::labels::ROLES {
                let mailbox = RemoteMailbox {
                    id: id.into(),
                    name: id.into(),
                    kind: MailboxKind::System,
                    role: Some(role),
                    color: None,
                    hidden: false,
                };
                mailboxes::upsert(c, account_id, &mailbox)?;
            }
            Ok(())
        })
        .await
        .unwrap();
    let fake = Arc::new(FakeGmail::new());
    let (sender, _events) = async_channel::unbounded();
    let sync = Arc::new(AccountSync::new(
        account_id,
        AccountServices::google(Arc::clone(&fake)),
        h.db.clone(),
        sender,
    ));
    sync.bootstrap().await.unwrap();
    Other {
        fake,
        sync,
        _events,
    }
}

fn actions(h: &Harness, other: &Other) -> MailActions<Connected> {
    let connected =
        HashMap::from([(h.account_id, Arc::clone(&h.sync)), (2, other.sync.clone())]);
    MailActions::new(
        Arc::new(Connected(connected)),
        h.db.clone(),
        crate::OneClick::Fake(Arc::default()),
    )
}

/// The destination: a label on the other account.
fn travel() -> Mailbox {
    Mailbox::Label {
        account_id: 2,
        label_id: "Travel".into(),
        name: "Travel".into(),
    }
}

/// What the fake holds on each of its messages, as `id: labels, date`.
fn held(fake: &FakeGmail) -> Vec<String> {
    fake.with(|s| {
        let mut lines: Vec<String> = s
            .messages
            .iter()
            .map(|(id, m)| {
                let mut labels = mailrs_gmail::labels::label_ids(m);
                labels.sort();
                format!("{id}: {}, {}", labels.join("+"), m.date)
            })
            .collect();
        lines.sort();
        lines
    })
}

/// The store's message ids for one account, each with its date.
async fn stored(h: &Harness, account_id: i64) -> Vec<(String, i64)> {
    h.db
        .read(move |c| {
            let mut stmt = c.prepare(
                "SELECT id, date FROM messages WHERE account_id = ?1 ORDER BY id",
            )?;
            let rows = stmt
                .query_map([account_id], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn a_carried_message_keeps_its_date_and_marks_and_leaves_nothing_behind() {
    let h = harness().await;
    let at = crate::now_millis() - 24 * 60 * 60 * 1000;
    h.fake.seed(meta("a", "t1", at, &["INBOX"]));
    h.bootstrap_all().await;
    let other = other(&h).await;
    let actions = actions(&h, &other);
    let outcome = actions
        .run_from(
            &[Target::thread(1, "t1")],
            MailAction::MoveTo {
                at: Box::new(travel()),
            },
            History::Record,
            &MovedFrom::one(1, MailSet::Role(Role::Inbox)),
        )
        .await;
    assert!(outcome.failed.is_empty(), "{:?}", outcome.failed);
    assert_eq!(outcome.done, [Target::thread(1, "t1")]);
    // The copy sits on the other server with the date it arrived under
    // at home, filed under the label it was dropped on, and read, since
    // it was read at home.
    assert_eq!(held(&other.fake), [format!("moved1: Travel, {at}")]);
    assert_eq!(
        other.fake.with(|s| s.messages["moved1"].held.keywords.clone()),
        ["$seen"],
        "its marks ride along"
    );
    let raws = other.fake.with(|s| s.raws["moved1"].clone());
    assert!(
        raws.windows(b"Subject: Subject a".len()).any(|w| w == b"Subject: Subject a"),
        "the bytes landed as they arrived: {}",
        String::from_utf8_lossy(&raws)
    );
    // The original is gone from home: server and store both.
    assert!(held(&h.fake).is_empty());
    assert_eq!(stored(&h, 1).await, []);
    // And the destination's own sync brought its new copy's row.
    assert_eq!(stored(&h, 2).await, [("moved1".to_string(), at)]);
}

#[tokio::test]
async fn undoing_a_carry_brings_the_message_home_with_its_place_and_marks() {
    let h = harness().await;
    let at = crate::now_millis() - 24 * 60 * 60 * 1000;
    h.fake.seed(meta("a", "t1", at, &["INBOX"]));
    h.bootstrap_all().await;
    let other = other(&h).await;
    let actions = actions(&h, &other);
    let outcome = actions
        .run_from(
            &[Target::thread(1, "t1")],
            MailAction::MoveTo {
                at: Box::new(travel()),
            },
            History::Record,
            &MovedFrom::one(1, MailSet::Role(Role::Inbox)),
        )
        .await;
    assert!(outcome.done.len() == 1, "{:?}", outcome.failed);
    let undone = actions.undo().await.expect("the carry was recorded");
    assert!(undone.outcome.failed.is_empty(), "{:?}", undone.outcome);
    assert_eq!(undone.outcome.done, [Target::thread(1, "t1")]);
    // Home again: filed where it was picked from, read as it was, under
    // the date it arrived, and the copy over there is taken back.
    assert_eq!(held(&h.fake), [format!("moved1: INBOX, {at}")]);
    assert!(held(&other.fake).is_empty());
    // Both accounts' stores followed their servers.
    assert_eq!(stored(&h, 1).await, [("moved1".to_string(), at)]);
    assert_eq!(stored(&h, 2).await, []);
}

#[tokio::test]
async fn a_carry_that_cannot_erase_the_original_says_the_mail_is_in_both_places() {
    let h = harness().await;
    let at = crate::now_millis() - 24 * 60 * 60 * 1000;
    h.fake.seed(meta("a", "t1", at, &["INBOX"]));
    h.bootstrap_all().await;
    let other = other(&h).await;
    h.fake.fail_call(
        "users.messages.batchDelete",
        0,
        mailrs_gmail::GmailError::Http {
            status: 500,
            body: "no".into(),
        },
    );
    let actions = actions(&h, &other);
    let outcome = actions
        .run_from(
            &[Target::thread(1, "t1")],
            MailAction::MoveTo {
                at: Box::new(travel()),
            },
            History::Record,
            &MovedFrom::one(1, MailSet::Role(Role::Inbox)),
        )
        .await;
    assert!(outcome.done.is_empty());
    let error = &outcome.failed[0].error;
    assert!(
        error.contains("Moved to Travel, but a copy stayed behind"),
        "{error}"
    );
    // Both places hold it, which is exactly what the words say.
    assert_eq!(held(&h.fake), [format!("a: INBOX, {at}")]);
    assert_eq!(held(&other.fake), [format!("moved1: Travel, {at}")]);
    // And nothing was recorded for Undo: undoing half a carry would
    // put a third copy in the world.
    assert!(actions.newest().is_none());
}

#[tokio::test]
async fn a_drop_across_accounts_moves_its_own_account_rows_by_label() {
    let h = harness().await;
    let at = crate::now_millis() - 24 * 60 * 60 * 1000;
    h.fake.seed(meta("a", "t1", at, &["INBOX"]));
    h.bootstrap_all().await;
    let other = other(&h).await;
    other
        .fake
        .seed(meta("b", "t2", at, &["INBOX", "UNREAD"]));
    other.sync.look_for_mail().await.unwrap();
    let actions = actions(&h, &other);
    let outcome = actions
        .run_from(
            &[Target::thread(1, "t1"), Target::thread(2, "t2")],
            MailAction::MoveTo {
                at: Box::new(travel()),
            },
            History::Record,
            &MovedFrom::nowhere(),
        )
        .await;
    assert!(outcome.failed.is_empty(), "{:?}", outcome.failed);
    assert_eq!(
        outcome.done,
        [Target::thread(1, "t1"), Target::thread(2, "t2")]
    );
    // The account's own row took the ordinary label change, unread as
    // it was; the foreign row was carried.
    assert_eq!(held(&other.fake), [
        format!("b: INBOX+Travel+UNREAD, {at}"),
        format!("moved2: Travel, {at}")
    ]);
    assert!(held(&h.fake).is_empty());
}

/// The marks a carried message wears are spelled the way the account
/// it lands in keeps them: Gmail stars with STARRED and calls unread
/// what the others call a missing `$seen`.
#[tokio::test]
async fn a_carry_spells_its_marks_the_way_the_destination_keeps_them() {
    let h = harness().await;
    let at = crate::now_millis() - 24 * 60 * 60 * 1000;
    h.fake.seed(meta("a", "t1", at, &["INBOX", "STARRED"]));
    h.fake.seed(meta("b", "t2", at, &["INBOX", "UNREAD"]));
    h.bootstrap_all().await;
    let other = other(&h).await;
    let actions = actions(&h, &other);
    let outcome = actions
        .run_from(
            &[Target::thread(1, "t1"), Target::thread(1, "t2")],
            MailAction::MoveTo {
                at: Box::new(travel()),
            },
            History::Record,
            &MovedFrom::one(1, MailSet::Role(Role::Inbox)),
        )
        .await;
    assert!(outcome.failed.is_empty(), "{:?}", outcome.failed);
    // The starred one arrives starred and read, the unread one unread:
    // no stray flag spellings, and unread said by its own label.
    assert_eq!(held(&other.fake), [
        format!("moved1: STARRED+Travel, {at}"),
        format!("moved2: Travel+UNREAD, {at}")
    ]);
}

/// One conversation carried, one message of it refused by the
/// destination: the message that made the whole road is undoable on
/// its own, and the one that stayed home is the failure the person
/// hears about.
#[tokio::test]
async fn a_thread_half_carried_undoes_the_messages_that_made_the_trip() {
    let h = harness().await;
    let at = crate::now_millis() - 24 * 60 * 60 * 1000;
    h.fake.seed(meta("a", "t1", at, &["INBOX"]));
    h.fake.seed(meta("b", "t1", at + 1000, &["INBOX"]));
    h.bootstrap_all().await;
    let other = other(&h).await;
    // The destination takes the first copy and refuses the second.
    other.fake.fail_call(
        "users.messages.insert",
        1,
        mailrs_gmail::GmailError::Http {
            status: 500,
            body: "no".into(),
        },
    );
    let actions = actions(&h, &other);
    let outcome = actions
        .run_from(
            &[Target::thread(1, "t1")],
            MailAction::MoveTo {
                at: Box::new(travel()),
            },
            History::Record,
            &MovedFrom::one(1, MailSet::Role(Role::Inbox)),
        )
        .await;
    assert!(outcome.done.is_empty(), "{:?}", outcome.done);
    // Half the conversation travelled and half stayed. Whichever
    // message the destination refused, the other is safe over there.
    assert_eq!(held(&other.fake).len(), 1, "one copy arrived");
    assert_eq!(held(&h.fake).len(), 1, "one original stayed");
    // The refused message earns no Undo for itself, but the message
    // that made the whole road does, so the run is on the stack.
    assert!(actions.newest().is_some(), "the clean carry is recorded");
    let undone = actions.undo().await.expect("the carry was recorded");
    assert!(undone.outcome.failed.is_empty(), "{:?}", undone.outcome);
    // The traveller is home again and the destination holds nothing.
    assert_eq!(held(&h.fake).len(), 2);
    assert!(held(&other.fake).is_empty());
}

/// Deleting something else for good leaves a carry's Undo standing:
/// nothing that carry moved is anywhere near the erased thread.
#[tokio::test]
async fn deleting_other_mail_for_good_leaves_a_carries_undo_in_place() {
    let h = harness().await;
    let at = crate::now_millis() - 24 * 60 * 60 * 1000;
    h.fake.seed(meta("a", "t1", at, &["INBOX"]));
    h.fake.seed(meta("spam", "t9", at, &["INBOX"]));
    h.bootstrap_all().await;
    let other = other(&h).await;
    let actions = actions(&h, &other);
    let outcome = actions
        .run_from(
            &[Target::thread(1, "t1")],
            MailAction::MoveTo {
                at: Box::new(travel()),
            },
            History::Record,
            &MovedFrom::one(1, MailSet::Role(Role::Inbox)),
        )
        .await;
    assert!(outcome.done.len() == 1, "{:?}", outcome.failed);
    let erased = actions
        .erase(&[Target::thread(1, "t9")])
        .await
        .unwrap()
        .done()
        .expect("the fake erases");
    assert_eq!(erased.done, [Target::thread(1, "t9")]);
    // The carry's entry survived the erase, and still brings its
    // message back.
    let undone = actions.undo().await.expect("still on the stack");
    assert_eq!(undone.outcome.done, [Target::thread(1, "t1")]);
    assert!(held(&other.fake).is_empty());
    assert_eq!(held(&h.fake).len(), 1, "the carried message is home");
}

/// The window asks what each account offers before it lets a drop
/// land, so the offer must read the server honestly: Gmail, JMAP and
/// IMAP take a copy, Microsoft does not.
#[test]
fn accounts_offer_a_landing_where_their_server_takes_one() {
    use crate::fake::{FakeGmail, FakeGraph, FakeJmap};
    assert!(AccountServices::google(Arc::new(FakeGmail::new())).offers().lands_mail);
    let jmap = AccountServices::jmap(
        Arc::new(FakeJmap::new()),
        crate::JmapSettings {
            address: "me@fastmail.com".into(),
            provider_name: "Fastmail".into(),
            window_days: 30,
            labels: true,
            capabilities: vec![mailrs_jmap::MAIL.into()],
            auto_reply_needs_sign_in: false,
        },
    );
    assert!(jmap.offers().lands_mail);
    let graph = AccountServices::microsoft(
        Arc::new(FakeGraph::new()),
        crate::MicrosoftSettings {
            shared: false,
            address: "me@outlook.com".into(),
            provider_name: "Outlook".into(),
            window_days: 30,
        },
    );
    assert!(!graph.offers().lands_mail, "Graph cannot file into a folder");
}
