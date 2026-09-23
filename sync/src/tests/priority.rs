//! What a user action costs while accounts backfill. The in-memory Gmail
//! holds each account's budget here, so a call that arrives on an empty
//! bucket comes back rate limited, the way Gmail answers 429.

use std::sync::Arc;
use std::time::Duration;

use mailrs_domain::{AccountId, MessageMeta, Target};
use mailrs_gmail::AccountQuota;
use mailrs_store::threads::{self, ThreadFilter};
use mailrs_store::{Db, accounts};
use tokio::time::Instant;

use crate::fake::{FakeGmail, Usage, meta};
use crate::{
    AccountServices, EngineConfig, History, MailAction, MailActions, SyncEngine, TriageAction,
    now_millis,
};

/// Accounts syncing from nothing, each with a mailbox to backfill.
struct Backfilling {
    engine: Arc<SyncEngine>,
    fakes: Vec<Arc<FakeGmail>>,
    ids: Vec<AccountId>,
    db: Db,
    _dir: tempfile::TempDir,
}

/// `count` accounts of `messages` messages each, one message per thread,
/// with their sync loops running and their stores empty.
async fn backfilling(count: usize, messages: usize) -> Backfilling {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("mail.db")).unwrap();
    let (engine, _events) = SyncEngine::new(db.clone(), EngineConfig::default());
    let engine = Arc::new(engine);
    let now = now_millis();
    let (mut fakes, mut ids) = (Vec::new(), Vec::new());
    for account in 0..count {
        let email = format!("user{account}@example.com");
        let id = db
            .write(move |c| accounts::insert_account(c, &email, 0))
            .await
            .unwrap();
        let fake = Arc::new(FakeGmail::new().under_quota(Arc::new(AccountQuota::standalone())));
        fake.with(|s| s.page_size = 100);
        for message in 0..messages {
            let name = format!("a{account}m{message}");
            fake.seed(MessageMeta {
                account_id: id,
                ..meta(
                    &name,
                    &format!("a{account}t{message}"),
                    now - message as i64,
                    &["INBOX"],
                )
            });
        }
        engine.start_account(id, AccountServices::fake(Arc::clone(&fake)));
        fakes.push(fake);
        ids.push(id);
    }
    Backfilling {
        engine,
        fakes,
        ids,
        db,
        _dir: dir,
    }
}

impl Backfilling {
    /// Waits until every account's store holds `count` conversations, as
    /// the list does before the user can select any.
    async fn first_page(&self, count: usize) {
        for id in &self.ids {
            while self.held(*id, 1000).await < count {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    }

    async fn held(&self, id: AccountId, limit: i64) -> usize {
        self.db
            .read(move |c| {
                Ok(threads::list_threads(c, &ThreadFilter::account(id, "INBOX"), 0, limit)?.len())
            })
            .await
            .unwrap()
    }

    /// The newest `count` conversations of each account.
    async fn newest(&self, count: i64) -> Vec<Target> {
        let mut targets = Vec::new();
        for id in &self.ids {
            let id = *id;
            let held = self
                .db
                .read(move |c| {
                    threads::list_threads(c, &ThreadFilter::account(id, "INBOX"), 0, count)
                })
                .await
                .unwrap();
            targets.extend(held.into_iter().map(|t| Target::thread(id, t.id)));
        }
        targets
    }

    fn actions(&self) -> MailActions<SyncEngine> {
        MailActions::new(
            Arc::clone(&self.engine),
            self.db.clone(),
            crate::OneClick::Fake(Arc::default()),
        )
    }

    fn usage(&self) -> Usage {
        let mut sum = Usage::default();
        for fake in &self.fakes {
            let usage = fake.usage();
            sum.calls += usage.calls;
            sum.units += usage.units;
            sum.refused += usage.refused;
            sum.foreground_wait += usage.foreground_wait;
            for (method, count) in usage.by_method {
                *sum.by_method.entry(method).or_default() += count;
            }
        }
        sum
    }
}

/// What a bulk trash cost the person who asked for it.
struct Trashed {
    done: usize,
    failed: usize,
    /// How long the whole action took, which is what the user sits through.
    took: Duration,
}

/// Trashes `targets` and reports what it cost and how long the user waited.
async fn trash(busy: &Backfilling, targets: &[Target], what: &str) -> Trashed {
    let before = busy.usage().foreground_wait;
    let started = Instant::now();
    let outcome = busy
        .actions()
        .run(
            targets,
            MailAction::Triage(TriageAction::Trash),
            History::Record,
        )
        .await;
    let took = started.elapsed();
    let usage = busy.usage();
    eprintln!(
        "{what}: {} of {} conversations trashed in {took:?}, \
         {:?} of that waiting for budget, {} calls refused for quota",
        outcome.done.len(),
        targets.len(),
        usage.foreground_wait - before,
        usage.refused,
    );
    Trashed {
        done: outcome.done.len(),
        failed: outcome.failed.len(),
        took,
    }
}

#[tokio::test]
async fn a_bulk_trash_lands_while_one_account_backfills() {
    let busy = backfilling(1, 600).await;
    busy.first_page(100).await;
    let targets = busy.newest(20).await;

    let trashed = trash(&busy, &targets, "trash 20 while 1 account backfills").await;

    assert_eq!(trashed.failed, 0, "the user had to press Delete again");
    assert_eq!(trashed.done, 20);
    assert!(
        trashed.took < Duration::from_secs(1),
        "the user waited {:?} behind the backfill",
        trashed.took
    );
}

#[tokio::test]
async fn a_bulk_trash_lands_while_six_accounts_backfill() {
    let busy = backfilling(6, 600).await;
    busy.first_page(100).await;
    let targets = busy.newest(20).await;
    assert_eq!(targets.len(), 120);

    let trashed = trash(&busy, &targets, "trash 120 while 6 accounts backfill").await;

    assert_eq!(trashed.failed, 0, "the user had to press Delete again");
    assert_eq!(trashed.done, 120);
    assert!(
        trashed.took < Duration::from_secs(2),
        "the user waited {:?} behind six backfills",
        trashed.took
    );
}
