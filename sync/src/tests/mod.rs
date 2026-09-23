mod actions;
mod basics;
mod bootstrap;
mod calendar;
mod connect;
mod contacts;
mod engine;
mod export;
mod incremental;
mod invitations;
mod labels;
mod mailbox;
mod newsletters;
mod outbox;
mod parity;
mod priority;
mod quota;
mod search;
mod senders;
mod settings;
mod thread_open;
mod triage;
mod undo;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use mailrs_domain::{AccountId, ChangeEvent, ThreadSummary};
use mailrs_store::threads::{self, ThreadFilter};
use mailrs_store::{Db, accounts, messages};

use crate::fake::{FakeGmail, FakeOneClick};
use crate::{AccountServices, AccountSync, Accounts};

/// The accounts a test connects, by id.
pub(crate) struct Connected(pub HashMap<AccountId, Arc<AccountSync>>);

impl Accounts for Connected {
    fn account(&self, account_id: AccountId) -> Option<Arc<AccountSync>> {
        self.0.get(&account_id).cloned()
    }
}

pub(crate) struct Harness {
    pub fake: Arc<FakeGmail>,
    pub one_click: Arc<FakeOneClick>,
    pub sync: Arc<AccountSync>,
    pub db: Db,
    pub events: async_channel::Receiver<ChangeEvent>,
    pub account_id: AccountId,
    /// The other end of `events`, for a second sync over the same account.
    sender: async_channel::Sender<ChangeEvent>,
    _dir: tempfile::TempDir,
}

pub(crate) async fn harness() -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("mail.db")).unwrap();
    let account_id = db
        .write(|c| accounts::insert_account(c, "me@example.com", 0))
        .await
        .unwrap();
    assert_eq!(account_id, 1, "fake messages belong to account 1");
    let fake = Arc::new(FakeGmail::new());
    let (sender, events) = async_channel::unbounded();
    let sync = Arc::new(
        AccountSync::new(
            account_id,
            AccountServices::fake(Arc::clone(&fake)),
            db.clone(),
            sender.clone(),
        )
        .with_retry_max(Duration::from_millis(10)),
    );
    Harness {
        fake,
        one_click: Arc::new(FakeOneClick::default()),
        sync,
        db,
        events,
        account_id,
        sender,
        _dir: dir,
    }
}

impl Harness {
    /// Another sync over the same account and the same Gmail, which gives
    /// up on a busy Gmail after `ceiling` rather than after a minute.
    pub fn sync_with(&self, ceiling: Duration) -> AccountSync {
        AccountSync::new(
            self.account_id,
            AccountServices::fake(Arc::clone(&self.fake)),
            self.db.clone(),
            self.sender.clone(),
        )
        .with_retry_max(Duration::from_millis(10))
        .with_wait_ceiling(ceiling)
    }

    pub fn drain(&self) -> Vec<ChangeEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.events.try_recv() {
            events.push(event);
        }
        events
    }

    /// Thread ids under `label`, newest first.
    pub async fn threads(&self, label: &str) -> Vec<String> {
        let filter = ThreadFilter::account(self.account_id, label);
        self.db
            .read(move |c| threads::list_threads(c, &filter, 0, 1000))
            .await
            .unwrap()
            .into_iter()
            .map(|t| t.id)
            .collect()
    }

    pub async fn thread(&self, thread_id: &str) -> Option<ThreadSummary> {
        let (account_id, thread_id) = (self.account_id, thread_id.to_string());
        self.db
            .read(move |c| threads::get_thread(c, account_id, &thread_id))
            .await
            .unwrap()
    }

    pub async fn labels_of(&self, message_id: &str) -> Vec<String> {
        let (account_id, message_id) = (self.account_id, message_id.to_string());
        self.db
            .read(move |c| messages::labels_of(c, account_id, &message_id))
            .await
            .unwrap()
    }

    pub async fn cursor(&self) -> accounts::SyncCursor {
        let account_id = self.account_id;
        self.db
            .read(move |c| accounts::sync_cursor(c, account_id))
            .await
            .unwrap()
    }

    /// When a cached body was last read.
    pub async fn accessed_at(&self, message_id: &str) -> i64 {
        let (account_id, message_id) = (self.account_id, message_id.to_string());
        self.db
            .read(move |c| {
                Ok(c.query_row(
                    "SELECT accessed_at FROM bodies WHERE account_id = ?1 AND message_id = ?2",
                    (account_id, message_id),
                    |row| row.get(0),
                )?)
            })
            .await
            .unwrap()
    }

    /// Bootstraps with one page big enough for the whole fake mailbox, then
    /// drops the events that produced.
    pub async fn bootstrap_all(&self) {
        let page_size = self
            .fake
            .with(|s| std::mem::replace(&mut s.page_size, 1000));
        self.sync.bootstrap().await.unwrap();
        self.fake.with(|s| s.page_size = page_size);
        self.drain();
    }
}
