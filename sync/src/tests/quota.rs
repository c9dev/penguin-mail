//! What Penguin Mail spends at Gmail, counted rather than guessed. The
//! in-memory Gmail prices every call from Gmail's usage-limits table, so
//! these tests read as a bill: calls made and quota units charged for a
//! bulk delete, an idle minute, and a first sync.

use std::sync::Arc;
use std::time::Duration;

use mailrs_domain::{AccountId, MessageMeta, Target};
use mailrs_store::{Db, accounts};

use super::{Connected, harness};
use crate::fake::{FakeGmail, Usage, meta};
use crate::{AccountSync, History, MailAction, MailActions, TriageAction, now_millis};

/// One connected account: its Gmail, its sync loop, and its id.
struct Mailbox {
    id: AccountId,
    fake: Arc<FakeGmail>,
    sync: Arc<AccountSync<FakeGmail>>,
}

/// `count` accounts, each holding `threads` conversations of `messages`
/// messages in the inbox, already synced.
async fn mailboxes(db: &Db, count: usize, threads: usize, messages: usize) -> Vec<Mailbox> {
    let now = now_millis();
    let mut all = Vec::new();
    for account in 0..count {
        let email = format!("user{account}@example.com");
        let id = db
            .write(move |c| accounts::insert_account(c, &email, 0))
            .await
            .unwrap();
        let fake = Arc::new(FakeGmail::new());
        fake.with(|s| s.page_size = 100);
        for thread in 0..threads {
            for message in 0..messages {
                let ids = format!("a{account}t{thread}m{message}");
                fake.seed(MessageMeta {
                    account_id: id,
                    ..meta(&ids, &format!("a{account}t{thread}"), now, &["INBOX"])
                });
            }
        }
        let (sender, _events) = async_channel::unbounded();
        let sync = Arc::new(
            AccountSync::new(id, Arc::clone(&fake), db.clone(), sender)
                .with_retry_max(Duration::from_millis(10)),
        );
        all.push(Mailbox { id, fake, sync });
    }
    all
}

/// Runs every account's first sync to the end and returns what it cost.
async fn first_sync(all: &[Mailbox]) -> Usage {
    for mailbox in all {
        mailbox.sync.bootstrap().await.unwrap();
        while mailbox.sync.backfill_step().await.unwrap() {}
    }
    total(all)
}

fn total(all: &[Mailbox]) -> Usage {
    let mut sum = Usage::default();
    for mailbox in all {
        let usage = mailbox.fake.usage();
        sum.calls += usage.calls;
        sum.units += usage.units;
        for (method, count) in usage.by_method {
            *sum.by_method.entry(method).or_default() += count;
        }
    }
    sum
}

fn reset(all: &[Mailbox]) {
    for mailbox in all {
        mailbox.fake.reset_usage();
    }
}

fn actions(all: &[Mailbox], db: &Db) -> MailActions<Connected> {
    let connected = all
        .iter()
        .map(|m| (m.id, Arc::clone(&m.sync)))
        .collect::<std::collections::HashMap<_, _>>();
    MailActions::new(Arc::new(Connected(connected)), db.clone())
}

/// Every conversation in every account, in the order the list shows them.
fn everything(all: &[Mailbox], threads: usize) -> Vec<Target> {
    let mut targets = Vec::new();
    for thread in 0..threads {
        for (account, mailbox) in all.iter().enumerate() {
            targets.push(Target::thread(mailbox.id, format!("a{account}t{thread}")));
        }
    }
    targets
}

fn report(what: &str, usage: &Usage) {
    let methods: Vec<String> = usage
        .by_method
        .iter()
        .map(|(m, n)| format!("{m} x{n}"))
        .collect();
    eprintln!(
        "{what}: {} calls, {} units [{}]",
        usage.calls,
        usage.units,
        methods.join(", ")
    );
}

#[tokio::test]
async fn trashing_two_hundred_conversations_takes_one_call() {
    let h = harness().await;
    let all = mailboxes(&h.db, 1, 200, 2).await;
    first_sync(&all).await;
    reset(&all);

    let targets = everything(&all, 200);
    let outcome = actions(&all, &h.db)
        .run(
            &targets,
            MailAction::Triage(TriageAction::Trash),
            History::Record,
        )
        .await;

    assert_eq!(outcome.done.len(), 200);
    assert!(outcome.failed.is_empty());
    let usage = total(&all);
    report("trash 200 conversations, 1 account", &usage);
    assert_eq!(usage.calls_to("users.messages.batchModify"), 1);
    assert_eq!(usage.calls, 1, "one batch for the account");
    assert_eq!(usage.units, 50);
}

#[tokio::test]
async fn six_accounts_spend_one_batch_each() {
    let h = harness().await;
    let all = mailboxes(&h.db, 6, 34, 2).await;
    first_sync(&all).await;
    reset(&all);

    let targets = everything(&all, 34);
    assert_eq!(targets.len(), 204);
    let outcome = actions(&all, &h.db)
        .run(
            &targets,
            MailAction::Triage(TriageAction::Trash),
            History::Record,
        )
        .await;

    assert_eq!(outcome.done.len(), 204);
    let usage = total(&all);
    report("trash 204 conversations, 6 accounts", &usage);
    assert_eq!(usage.calls, 6, "one batch per account, whatever the order");
    assert_eq!(usage.units, 300);
}

#[tokio::test]
async fn undoing_a_bulk_action_takes_one_call_per_account() {
    let h = harness().await;
    let all = mailboxes(&h.db, 6, 34, 2).await;
    first_sync(&all).await;
    let targets = everything(&all, 34);
    let actions = actions(&all, &h.db);
    actions
        .run(
            &targets,
            MailAction::Triage(TriageAction::Archive),
            History::Record,
        )
        .await;
    reset(&all);

    let undone = actions.undo().await.expect("an undo");

    assert_eq!(undone.done.len(), 204);
    let usage = total(&all);
    report("undo 204 conversations, 6 accounts", &usage);
    assert_eq!(usage.calls, 6);
}

#[tokio::test]
async fn an_idle_minute_costs_two_history_calls_per_account() {
    let h = harness().await;
    let all = mailboxes(&h.db, 6, 5, 1).await;
    first_sync(&all).await;
    reset(&all);

    // The engine polls every 30 seconds, so a minute is two ticks.
    for _ in 0..2 {
        for mailbox in &all {
            mailbox.sync.incremental().await.unwrap();
        }
    }

    let usage = total(&all);
    report("idle minute, 6 accounts", &usage);
    assert_eq!(usage.calls, 12);
    assert_eq!(usage.units, 24);
}

#[tokio::test]
async fn a_first_sync_costs_five_units_a_message() {
    let h = harness().await;
    let all = mailboxes(&h.db, 6, 50, 2).await;

    let usage = first_sync(&all).await;

    report("first sync, 6 accounts, 100 messages each", &usage);
    // Per account: the profile and the label list, one listing call per
    // page of 100, and one metadata fetch per message. Gmail has no batch
    // get, so the metadata dominates and only a smaller window trims it.
    assert_eq!(usage.calls_to("users.messages.get"), 600);
    assert_eq!(usage.units, 6 * (1 + 1 + 5 + 100 * 5));
}
