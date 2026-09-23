//! What Penguin Mail spends at Gmail, counted rather than guessed. The
//! in-memory Gmail prices every call from Gmail's usage-limits table, so
//! these tests read as a bill: calls made and quota units charged for a
//! bulk delete, an idle minute, and a first sync.

use std::sync::Arc;
use std::time::Duration;

use mailrs_domain::{Account, AccountId, AccountState, Folder, MessageMeta, Target};
use mailrs_store::{Db, accounts};

use super::{Connected, harness};
use crate::fake::{FakeGmail, Usage, meta};
use crate::{
    AccountSync, History, MailAction, MailActions, Mailbox, Mailboxes, Scope, TriageAction, View,
    now_millis,
};

/// One connected account: its Gmail, its sync loop, and its id.
struct Synced {
    id: AccountId,
    fake: Arc<FakeGmail>,
    sync: Arc<AccountSync<FakeGmail>>,
}

/// `count` accounts, each holding `threads` conversations of `messages`
/// messages in the inbox, already synced.
async fn synced(db: &Db, count: usize, threads: usize, messages: usize) -> Vec<Synced> {
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
        all.push(Synced { id, fake, sync });
    }
    all
}

/// Runs every account's first sync to the end and returns what it cost.
async fn first_sync(all: &[Synced]) -> Usage {
    for mailbox in all {
        mailbox.sync.bootstrap().await.unwrap();
        while mailbox.sync.backfill_step().await.unwrap() {}
    }
    total(all)
}

fn total(all: &[Synced]) -> Usage {
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

fn reset(all: &[Synced]) {
    for mailbox in all {
        mailbox.fake.reset_usage();
    }
}

fn actions(all: &[Synced], db: &Db) -> MailActions<Connected> {
    let connected = all
        .iter()
        .map(|m| (m.id, Arc::clone(&m.sync)))
        .collect::<std::collections::HashMap<_, _>>();
    MailActions::new(Arc::new(Connected(connected)), db.clone())
}

/// Every conversation in every account, in the order the list shows them.
fn everything(all: &[Synced], threads: usize) -> Vec<Target> {
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
    let all = synced(&h.db, 1, 200, 2).await;
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
    let all = synced(&h.db, 6, 34, 2).await;
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
    let all = synced(&h.db, 6, 34, 2).await;
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

    assert_eq!(undone.outcome.done.len(), 204);
    let usage = total(&all);
    report("undo 204 conversations, 6 accounts", &usage);
    assert_eq!(usage.calls, 6);
}

#[tokio::test]
async fn an_idle_minute_costs_two_history_calls_per_account() {
    let h = harness().await;
    let all = synced(&h.db, 6, 5, 1).await;
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

/// Junk mail sits outside the synced window, so the store holds none of
/// it and every row costs a metadata call.
async fn junk(all: &[Synced], messages: usize) {
    let now = now_millis();
    for (account, mailbox) in all.iter().enumerate() {
        for message in 0..messages {
            let ids = format!("a{account}junk{message}");
            mailbox.fake.seed(MessageMeta {
                account_id: mailbox.id,
                ..meta(&ids, &format!("a{account}junkt{message}"), now, &["SPAM"])
            });
        }
    }
}

fn lists_over(all: &[Synced], db: &Db) -> Mailboxes<Connected> {
    let connected = all
        .iter()
        .map(|m| (m.id, Arc::clone(&m.sync)))
        .collect::<std::collections::HashMap<_, _>>();
    Mailboxes::new(Arc::new(Connected(connected)), db.clone())
}

fn scope(all: &[Synced]) -> Scope {
    Scope::over(all.iter().map(|m| Account {
        id: m.id,
        email: format!("user{}@example.com", m.id),
        state: AccountState::Ok,
    }))
}

#[tokio::test]
async fn opening_junk_pays_for_the_rows_it_shows() {
    let h = harness().await;
    let all = synced(&h.db, 1, 1, 1).await;
    junk(&all, 100).await;
    first_sync(&all).await;
    reset(&all);
    let lists = lists_over(&all, &h.db);
    let folder = Mailbox::Folder {
        account_id: Some(all[0].id),
        folder: Folder::Junk,
    };

    let listing = lists
        .list(&folder, &scope(&all), &View::default(), 0)
        .await
        .unwrap();

    assert_eq!(listing.rows.len(), 25);
    assert!(listing.more, "the other 75 wait until the reader scrolls");
    let usage = total(&all);
    report("open Junk, 100 messages, 1 account", &usage);
    // One listing call for the ids, then metadata for the rows on screen.
    assert_eq!(usage.calls_to("users.messages.list"), 1);
    assert_eq!(usage.calls_to("users.messages.get"), 25);
    assert_eq!(usage.units, 5 + 25 * 5);
}

#[tokio::test]
async fn opening_junk_again_asks_gmail_nothing() {
    let h = harness().await;
    let all = synced(&h.db, 6, 1, 1).await;
    junk(&all, 100).await;
    first_sync(&all).await;
    reset(&all);
    let lists = lists_over(&all, &h.db);
    let folder = Mailbox::Folder {
        account_id: None,
        folder: Folder::Junk,
    };
    let (scope, view) = (scope(&all), View::default());
    lists.list(&folder, &scope, &view, 0).await.unwrap();
    let first = total(&all);
    report("open Junk, 100 messages, 6 accounts", &first);
    assert_eq!(first.units, 6 * (5 + 25 * 5));
    reset(&all);

    // The reload every change event used to trigger.
    lists.list(&folder, &scope, &view, 0).await.unwrap();

    let again = total(&all);
    report("open Junk again within the minute", &again);
    assert_eq!(again.calls, 0);
}

#[tokio::test]
async fn scrolling_junk_pays_only_for_the_next_rows() {
    let h = harness().await;
    let all = synced(&h.db, 1, 1, 1).await;
    junk(&all, 100).await;
    first_sync(&all).await;
    let lists = lists_over(&all, &h.db);
    let folder = Mailbox::Folder {
        account_id: Some(all[0].id),
        folder: Folder::Junk,
    };
    let (scope, view) = (scope(&all), View::default());
    lists.list(&folder, &scope, &view, 0).await.unwrap();
    reset(&all);

    let more = lists.list(&folder, &scope, &view, 25).await.unwrap();

    assert_eq!(
        more.rows.len(),
        25,
        "only the rows the list has yet to show"
    );
    assert!(more.more, "50 of the 100 messages are still unread");
    let usage = total(&all);
    report("scroll Junk to the next 25 rows", &usage);
    assert_eq!(usage.calls_to("users.messages.list"), 0, "the ids are kept");
    assert_eq!(usage.calls_to("users.messages.get"), 25);
}

#[tokio::test]
async fn a_search_reuses_the_metadata_the_store_holds() {
    let h = harness().await;
    let all = synced(&h.db, 1, 40, 1).await;
    first_sync(&all).await;
    reset(&all);

    // Every hit is inbox mail the first sync already stored.
    let found = all[0].sync.search("in:inbox", 25).await.unwrap();

    assert_eq!(found.len(), 25);
    let usage = total(&all);
    report("search 25 stored messages", &usage);
    assert_eq!(usage.calls_to("users.messages.get"), 0);
    assert_eq!(usage.units, 5);
}

#[tokio::test]
async fn a_first_sync_fetches_each_conversation_in_one_call() {
    let h = harness().await;
    let all = synced(&h.db, 6, 50, 2).await;

    let usage = first_sync(&all).await;

    report("first sync, 6 accounts, 50 conversations of 2 each", &usage);
    // Per account: the profile and the label list, one listing call per
    // page of 100, and one `threads.get` per conversation. Two messages
    // cost the same 10 units either way; one call instead of two halves
    // the round trips.
    assert_eq!(usage.calls_to("users.messages.get"), 0);
    assert_eq!(usage.calls_to("users.threads.get"), 300);
    assert_eq!(usage.units, 6 * (1 + 1 + 5 + 50 * 10));
}

/// Conversation sizes for a mailbox of 300 messages in 120 threads: half
/// of them a lone message, the rest replies of two, five and eight.
const SIZES: [(usize, usize); 4] = [(60, 1), (30, 2), (20, 5), (10, 8)];

/// Seeds that mailbox into one fresh account, a conversation every few
/// hours across the window and its replies minutes apart, and pages the
/// listing at 100 as the real client does.
async fn realistic(db: &Db) -> Synced {
    const HOUR: i64 = 60 * 60 * 1000;
    let id = db
        .write(|c| accounts::insert_account(c, "busy@example.com", 0))
        .await
        .unwrap();
    let fake = Arc::new(FakeGmail::new());
    fake.with(|s| s.page_size = 100);
    let now = now_millis();
    let mut thread = 0;
    for (count, size) in SIZES {
        for _ in 0..count {
            // Interleave the sizes so every page holds a mix.
            let started = now - ((thread * 37) % 120) as i64 * 5 * HOUR;
            for reply in 0..size {
                fake.seed(MessageMeta {
                    account_id: id,
                    ..meta(
                        &format!("t{thread}m{reply}"),
                        &format!("t{thread}"),
                        started + reply as i64 * 10 * 60 * 1000,
                        &["INBOX"],
                    )
                });
            }
            thread += 1;
        }
    }
    let (sender, _events) = async_channel::unbounded();
    let sync = Arc::new(AccountSync::new(id, Arc::clone(&fake), db.clone(), sender));
    Synced { id, fake, sync }
}

#[tokio::test]
async fn a_first_sync_of_a_busy_mailbox_spends_by_conversation() {
    let h = harness().await;
    let all = vec![realistic(&h.db).await];

    let usage = first_sync(&all).await;

    report("first sync, 300 messages in 120 conversations", &usage);
    let stored =
        h.db.read(|c| {
            Ok(c.query_row("SELECT COUNT(*) FROM messages", [], |row| {
                row.get::<_, i64>(0)
            })?)
        })
        .await
        .unwrap();
    assert_eq!(stored, 300, "every message in the window, and no more");
    // A message at a time this cost 305 calls and 1517 units: the profile,
    // the labels, three pages, and 300 `messages.get`. Now the 60 lone
    // messages still cost one each, and the other 60 conversations cost a
    // `threads.get` each, plus one more for a conversation the page break
    // splits.
    assert_eq!(usage.calls_to("users.messages.list"), 3);
    assert_eq!(usage.calls_to("users.messages.get"), 60);
    assert_eq!(usage.calls_to("users.threads.get"), 61);
    assert_eq!(usage.calls, 126);
    assert_eq!(usage.units, 1 + 1 + 3 * 5 + 60 * 5 + 61 * 10);
}

/// Trash older than the window, which the store never holds: `count`
/// conversations of one message each, then `pairs` of two.
fn old_trash(mailbox: &Synced, count: usize, pairs: usize) -> Vec<Target> {
    let long_ago = now_millis() - 200 * 86_400_000;
    let mut targets = Vec::new();
    for thread in 0..count + pairs {
        let size = if thread < count { 1 } else { 2 };
        let thread_id = format!("trash{thread}");
        for message in 0..size {
            mailbox.fake.seed(MessageMeta {
                account_id: mailbox.id,
                ..meta(
                    &format!("trash{thread}m{message}"),
                    &thread_id,
                    long_ago + thread as i64 * 1000 + message as i64,
                    &["TRASH"],
                )
            });
        }
        targets.push(Target::thread(mailbox.id, thread_id));
    }
    targets
}

#[tokio::test]
async fn deleting_two_hundred_conversations_forever_takes_one_call() {
    let h = harness().await;
    let all = synced(&h.db, 1, 1, 1).await;
    let targets = old_trash(&all[0], 180, 20);
    first_sync(&all).await;
    all[0].fake.with(|s| s.page_size = 500);
    let trash = Mailbox::Folder {
        account_id: Some(all[0].id),
        folder: Folder::Trash,
    };
    let view = View {
        limit: Some(250),
        ..View::default()
    };
    // The reader has the Trash open, which listed every row.
    lists_over(&all, &h.db)
        .list(&trash, &scope(&all), &view, 0)
        .await
        .unwrap();
    reset(&all);

    let erased = actions(&all, &h.db).erase(&targets).await.unwrap();

    let outcome = erased.done().expect("the permission is there");
    assert_eq!(outcome.done.len(), 200);
    assert!(outcome.failed.is_empty(), "{:?}", outcome.failed);
    let usage = total(&all);
    report("delete forever 200 conversations, 1 account", &usage);
    // One `threads.get` and one `batchDelete` per conversation cost 400
    // calls and 12,000 units. The listing already named every message.
    assert_eq!(usage.calls_to("users.messages.batchDelete"), 1);
    assert_eq!(usage.calls, 1);
    assert_eq!(usage.units, 50);
    assert!(
        all[0]
            .fake
            .with(|s| s.messages.keys().all(|id| !id.starts_with("trash")))
    );
}

#[tokio::test]
async fn deleting_forever_what_nobody_listed_asks_for_each_conversation_once() {
    let h = harness().await;
    let all = synced(&h.db, 1, 1, 1).await;
    let targets = old_trash(&all[0], 3, 0);
    first_sync(&all).await;
    reset(&all);

    let erased = actions(&all, &h.db).erase(&targets).await.unwrap();

    assert_eq!(erased.done().map(|o| o.done.len()), Some(3));
    let usage = total(&all);
    assert_eq!(usage.calls_to("users.threads.get"), 3);
    assert_eq!(usage.calls_to("users.messages.batchDelete"), 1);
}

#[tokio::test]
async fn deleting_more_than_a_thousand_messages_forever_splits_the_batch() {
    let h = harness().await;
    let all = synced(&h.db, 1, 1100, 1).await;
    all[0].fake.with(|s| s.page_size = 2000);
    first_sync(&all).await;
    reset(&all);
    let targets = everything(&all, 1100);

    let erased = actions(&all, &h.db).erase(&targets).await.unwrap();

    assert_eq!(erased.done().map(|o| o.done.len()), Some(1100));
    let usage = total(&all);
    assert_eq!(usage.calls_to("users.messages.batchDelete"), 2);
    assert_eq!(usage.calls, 2);
}

/// The inbox check needs Gmail's ids and nothing else, so it asks for 500
/// a page, Gmail's most, for the same 5 units a call as 100.
#[tokio::test]
async fn the_inbox_check_lists_five_hundred_ids_a_call() {
    let h = harness().await;
    let all = synced(&h.db, 1, 1200, 1).await;
    // The fake answers each listing with as many ids as it asked for.
    all[0].fake.with(|s| s.page_size = 10_000);
    first_sync(&all).await;
    reset(&all);

    all[0].sync.reconcile_inbox().await.unwrap();

    let usage = total(&all);
    report("inbox check, 1,200 messages", &usage);
    // At 100 a page this was 12 calls and 60 units.
    assert_eq!(usage.calls_to("users.messages.list"), 3);
    assert_eq!(usage.units, 15);
}

/// History names each new message with its thread, so replies arriving
/// three to a conversation come in one `threads.get` per conversation.
#[tokio::test]
async fn history_fetches_new_replies_a_conversation_at_a_time() {
    let h = harness().await;
    let all = synced(&h.db, 1, 10, 1).await;
    first_sync(&all).await;
    let now = now_millis();
    for thread in 0..10 {
        for reply in 0..3 {
            all[0].fake.deliver(MessageMeta {
                account_id: all[0].id,
                ..meta(
                    &format!("a0t{thread}r{reply}"),
                    &format!("a0t{thread}"),
                    now + reply,
                    &["INBOX", "UNREAD"],
                )
            });
        }
    }
    reset(&all);

    all[0].sync.incremental().await.unwrap();

    let usage = total(&all);
    report("history with 30 replies in 10 conversations", &usage);
    // A message at a time this was 30 calls and 150 units.
    assert_eq!(usage.calls_to("users.messages.get"), 0);
    assert_eq!(usage.calls_to("users.threads.get"), 10);
    let stored = h
        .db
        .read(|c| Ok(c.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get::<_, i64>(0))?))
        .await
        .unwrap();
    assert_eq!(stored, 40, "every reply is stored, and nothing else");
}
