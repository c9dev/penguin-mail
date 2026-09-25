mod actions;
mod basics;
mod bootstrap;
mod calendar;
mod connect;
mod contacts;
mod engine;
pub(crate) mod heap;
mod export;
mod imap;
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
mod stored_listing;
mod structure;
mod thread_open;
mod triage;
mod undo;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use mailrs_domain::{
    AccountId, ChangeEvent, MailSet, MailboxKind, MessageMeta, RemoteMailbox, ThreadSummary,
};
use mailrs_store::threads::{self, ThreadFilter};
use mailrs_store::{Db, accounts, mailboxes, messages, remote_refs};

use crate::fake::{FakeGmail, FakeImap, FakeOneClick, FakeSmtp};
use crate::{AccountServices, AccountSync, Accounts, AnyMail, ImapSettings};

/// A stored message's memberships as Gmail labels, sorted.
pub(crate) fn labels_of(
    conn: &rusqlite::Connection,
    account_id: AccountId,
    message_id: &str,
) -> mailrs_store::Result<Vec<String>> {
    let held = messages::memberships_of(conn, account_id, &[message_id.to_string()])?;
    Ok(held
        .get(message_id)
        .map(mailrs_gmail::labels::labels)
        .unwrap_or_default())
}

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
    // The first listing has named Gmail's role mailboxes, as it has on any
    // account with mail in the store.
    db.write(move |c| {
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

    /// Thread ids `filter` lists, newest first. Trash and Spam are left
    /// out, as they are for every mail set `ThreadFilter::everything`
    /// starts from.
    async fn thread_ids(&self, filter: ThreadFilter) -> Vec<String> {
        self.db
            .read(move |c| threads::list_threads(c, &filter, 0, 1000))
            .await
            .unwrap()
            .into_iter()
            .map(|t| t.id)
            .collect()
    }

    /// Thread ids in `set`, newest first.
    pub async fn threads(&self, set: MailSet) -> Vec<String> {
        self.thread_ids(ThreadFilter::account(self.account_id, set))
            .await
    }

    /// Every thread in the account, inbox or archived, newest first.
    pub async fn all_threads(&self) -> Vec<String> {
        self.thread_ids(ThreadFilter::everything().in_account(self.account_id))
            .await
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
            .read(move |c| labels_of(c, account_id, &message_id))
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

    /// The history id the Google adapter keeps in the account's sync state.
    pub async fn history_id(&self) -> Option<u64> {
        let state = self.cursor().await.state?;
        serde_json::from_str::<serde_json::Value>(&state)
            .ok()?
            .get("history_id")?
            .as_u64()
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

pub(crate) struct ImapHarness {
    pub imap: Arc<FakeImap>,
    pub smtp: Arc<FakeSmtp>,
    pub sync: Arc<AccountSync>,
    pub db: Db,
    pub events: async_channel::Receiver<ChangeEvent>,
    pub account_id: AccountId,
    _dir: tempfile::TempDir,
}

/// Settings for a Fastmail account that files no copy of what it sends.
pub(crate) fn fake_settings() -> ImapSettings {
    ImapSettings {
        address: "me@example.com".into(),
        provider_name: "Fastmail".into(),
        files_sent_mail: false,
        window_days: crate::DEFAULT_WINDOW_DAYS,
    }
}

/// An IMAP account on a fresh fake server.
pub(crate) async fn imap_harness() -> ImapHarness {
    imap_harness_on(FakeImap::new(), fake_settings()).await
}

/// An IMAP account on `imap`, with `settings`. Every look at the feed
/// covers every synced mailbox, so a test sees a change in Sent or Archive
/// at the next `incremental` rather than after the slow poll.
pub(crate) async fn imap_harness_on(imap: FakeImap, settings: ImapSettings) -> ImapHarness {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("mail.db")).unwrap();
    let account_id = db
        .write(|c| accounts::insert_account(c, "me@example.com", 0))
        .await
        .unwrap();
    let (imap, smtp) = (Arc::new(imap), Arc::new(FakeSmtp::default()));
    let services =
        AccountServices::fake_imap_with(Arc::clone(&imap), Arc::clone(&smtp), settings);
    if let AnyMail::FakeImap(adapter) = &services.mail {
        adapter.look_at_every_mailbox();
    }
    let (sender, events) = async_channel::unbounded();
    let sync = Arc::new(
        AccountSync::new(account_id, services, db.clone(), sender)
            .with_retry_max(Duration::from_millis(10)),
    );
    ImapHarness {
        imap,
        smtp,
        sync,
        db,
        events,
        account_id,
        _dir: dir,
    }
}

impl ImapHarness {
    pub fn drain(&self) -> Vec<ChangeEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.events.try_recv() {
            events.push(event);
        }
        events
    }

    /// Loads the whole window, as a new account does, and drops the events
    /// that made.
    pub async fn bootstrap(&self) {
        self.sync.bootstrap().await.unwrap();
        while self.sync.backfill_step().await.unwrap() {}
        self.drain();
    }

    /// Every stored message id, sorted.
    pub async fn ids(&self) -> Vec<String> {
        let account_id = self.account_id;
        self.db
            .read(move |c| {
                let mut stmt =
                    c.prepare("SELECT id FROM messages WHERE account_id = ?1 ORDER BY id")?;
                let ids = stmt
                    .query_map([account_id], |row| row.get(0))?
                    .collect::<rusqlite::Result<Vec<String>>>()?;
                Ok(ids)
            })
            .await
            .unwrap()
    }

    /// The stored copy of message `id`.
    pub async fn stored(&self, id: &str) -> Option<MessageMeta> {
        let (account_id, id) = (self.account_id, id.to_string());
        self.db
            .read(move |c| messages::by_ids(c, account_id, &[id]))
            .await
            .unwrap()
            .pop()
    }

    /// The thread the store keeps message `id` in.
    pub async fn thread_of(&self, id: &str) -> Option<String> {
        let (account_id, id) = (self.account_id, id.to_string());
        self.db
            .read(move |c| messages::thread_id_of(c, account_id, &id))
            .await
            .unwrap()
    }

    /// Where the store says message `id` sits on the server now.
    pub async fn location(&self, id: &str) -> Option<String> {
        let (account_id, id) = (self.account_id, id.to_string());
        self.db
            .read(move |c| remote_refs::remotes_of(c, account_id, &[id]))
            .await
            .unwrap()
            .into_values()
            .next()
    }

    /// Whether the adapter still follows `mailbox`.
    pub fn is_followed(&self, mailbox: &str) -> bool {
        match &self.sync.services().mail {
            AnyMail::FakeImap(adapter) => adapter.is_followed(mailbox),
            _ => false,
        }
    }
}
