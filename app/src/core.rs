//! The bridge between GTK and the sync core. GTK owns the main thread; a
//! tokio runtime runs the engine and every database and network call. The
//! UI hands futures to `Core::call` and awaits the result on the main loop.

use std::cell::RefCell;
use std::future::Future;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Context, Result, anyhow, bail};
use mailrs_domain::{Account, AccountId, ChangeEvent, Filter, MessageBody, MessageMeta, Vacation};
use mailrs_gmail::{
    GMAIL_API_BASE, GmailError, HistoryPage, KeyringTokenStore, MessagePage, OAuthClient, Profile,
    RemoteLabel, TokenStore, authorize,
};
use mailrs_store::{Db, accounts};
use mailrs_sync::config::{Config, config_path, data_dir};
use mailrs_sync::{
    AccountClient, AccountSync, GmailApi, SavedDraft, SyncEngine, connect_account, now_millis,
};

use crate::demo::{self, DemoApi};

/// Gmail for real accounts, or the local stand-in for demo mode.
pub enum Api {
    Gmail(Box<AccountClient>),
    Demo(DemoApi),
}

macro_rules! delegate {
    ($self:ident, $method:ident($($arg:expr),*)) => {
        match $self {
            Api::Gmail(api) => api.$method($($arg),*).await,
            Api::Demo(api) => api.$method($($arg),*).await,
        }
    };
}

impl GmailApi for Api {
    async fn profile(&self) -> Result<Profile, GmailError> {
        delegate!(self, profile())
    }
    async fn labels(&self) -> Result<Vec<RemoteLabel>, GmailError> {
        delegate!(self, labels())
    }
    async fn list_messages(
        &self,
        query: &str,
        page_token: Option<&str>,
    ) -> Result<MessagePage, GmailError> {
        delegate!(self, list_messages(query, page_token))
    }
    async fn message_metadata(&self, id: &str) -> Result<MessageMeta, GmailError> {
        delegate!(self, message_metadata(id))
    }
    async fn thread_metadata(&self, thread_id: &str) -> Result<Vec<MessageMeta>, GmailError> {
        delegate!(self, thread_metadata(thread_id))
    }
    async fn message_body(&self, id: &str) -> Result<MessageBody, GmailError> {
        delegate!(self, message_body(id))
    }
    async fn history(
        &self,
        start: u64,
        page_token: Option<&str>,
    ) -> Result<HistoryPage, GmailError> {
        delegate!(self, history(start, page_token))
    }
    async fn modify_labels(
        &self,
        id: &str,
        add: &[String],
        remove: &[String],
    ) -> Result<(), GmailError> {
        delegate!(self, modify_labels(id, add, remove))
    }
    async fn trash(&self, id: &str) -> Result<(), GmailError> {
        delegate!(self, trash(id))
    }
    async fn untrash(&self, id: &str) -> Result<(), GmailError> {
        delegate!(self, untrash(id))
    }
    async fn send(&self, raw: &[u8], thread_id: Option<&str>) -> Result<String, GmailError> {
        delegate!(self, send(raw, thread_id))
    }
    async fn save_draft(
        &self,
        draft_id: Option<&str>,
        raw: &[u8],
        thread_id: Option<&str>,
    ) -> Result<SavedDraft, GmailError> {
        delegate!(self, save_draft(draft_id, raw, thread_id))
    }
    async fn send_draft(&self, draft_id: &str) -> Result<String, GmailError> {
        delegate!(self, send_draft(draft_id))
    }
    async fn delete_draft(&self, draft_id: &str) -> Result<(), GmailError> {
        delegate!(self, delete_draft(draft_id))
    }
    async fn draft_for_message(&self, message_id: &str) -> Result<Option<String>, GmailError> {
        delegate!(self, draft_for_message(message_id))
    }
    async fn display_name(&self) -> Result<Option<String>, GmailError> {
        delegate!(self, display_name())
    }
    async fn attachment(
        &self,
        message_id: &str,
        attachment_id: &str,
    ) -> Result<Vec<u8>, GmailError> {
        delegate!(self, attachment(message_id, attachment_id))
    }
    async fn signature(&self) -> Result<Option<String>, GmailError> {
        delegate!(self, signature())
    }
    async fn filters(&self) -> Result<Vec<Filter>, GmailError> {
        delegate!(self, filters())
    }
    async fn create_filter(&self, filter: &Filter) -> Result<Filter, GmailError> {
        delegate!(self, create_filter(filter))
    }
    async fn delete_filter(&self, id: &str) -> Result<(), GmailError> {
        delegate!(self, delete_filter(id))
    }
    async fn create_label(&self, name: &str) -> Result<RemoteLabel, GmailError> {
        delegate!(self, create_label(name))
    }
    async fn rename_label(&self, id: &str, name: &str) -> Result<RemoteLabel, GmailError> {
        delegate!(self, rename_label(id, name))
    }
    async fn delete_label(&self, id: &str) -> Result<(), GmailError> {
        delegate!(self, delete_label(id))
    }
    async fn vacation(&self) -> Result<Vacation, GmailError> {
        delegate!(self, vacation())
    }
    async fn set_vacation(&self, vacation: &Vacation) -> Result<(), GmailError> {
        delegate!(self, set_vacation(vacation))
    }
}

pub type Engine = SyncEngine<Api>;
pub type Sync = AccountSync<Api>;

pub struct Core {
    runtime: tokio::runtime::Runtime,
    pub db: Db,
    pub demo: bool,
    engine: RefCell<Option<Arc<Engine>>>,
    config: RefCell<Option<Config>>,
    tokens: Arc<dyn TokenStore>,
    events_tx: async_channel::Sender<ChangeEvent>,
    pub events: async_channel::Receiver<ChangeEvent>,
    in_flight: Arc<AtomicUsize>,
}

/// Counts a user operation until its task finishes, even if nobody awaits it.
struct InFlight(Arc<AtomicUsize>);

impl InFlight {
    fn new(counter: &Arc<AtomicUsize>) -> Self {
        counter.fetch_add(1, Ordering::SeqCst);
        InFlight(Arc::clone(counter))
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Core {
    /// Opens the store and, when a config exists, starts syncing every account.
    pub fn open(demo: bool) -> Result<Rc<Core>> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("mailrs-sync")
            .enable_all()
            .build()
            .context("could not start the async runtime")?;
        let dir = if demo {
            std::env::temp_dir().join(format!("mailrs-demo-{}", std::process::id()))
        } else {
            data_dir()?
        };
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("could not create {}", dir.display()))?;
        let db_path: PathBuf = dir.join("mailrs.db");
        if demo {
            let _ = std::fs::remove_file(&db_path);
        }
        let db = Db::open(&db_path)?;
        let config = if demo {
            Some(Config::new("demo", "demo"))
        } else {
            match Config::load(&config_path()?) {
                Ok(config) => Some(config),
                Err(err) if err.is_missing() => None,
                Err(err) => return Err(err.into()),
            }
        };
        if demo {
            runtime.block_on(db.write(|c| demo::seed(c, now_millis())))?;
        }
        let (events_tx, events) = async_channel::unbounded();
        let core = Rc::new(Core {
            runtime,
            db,
            demo,
            engine: RefCell::new(None),
            config: RefCell::new(config),
            tokens: Arc::new(KeyringTokenStore::new()),
            events_tx,
            events,
            in_flight: Arc::new(AtomicUsize::new(0)),
        });
        if core.has_config() {
            core.start_engine();
        }
        Ok(core)
    }

    pub fn has_config(&self) -> bool {
        self.config.borrow().is_some()
    }

    /// Writes `config.toml` and starts syncing.
    pub fn save_config(&self, config: Config) -> Result<()> {
        config.save(&config_path()?)?;
        *self.config.borrow_mut() = Some(config);
        self.start_engine();
        Ok(())
    }

    /// The sync section of `config.toml`.
    pub fn sync_config(&self) -> Option<mailrs_sync::config::SyncConfig> {
        self.config.borrow().as_ref().map(|c| c.sync.clone())
    }

    /// Saves new sync settings and restarts every account's loop with them.
    /// Demo mode keeps them in memory only.
    pub fn update_sync(&self, sync: mailrs_sync::config::SyncConfig) -> Result<()> {
        let updated = {
            let mut config = self.config.borrow_mut();
            let Some(config) = config.as_mut() else {
                return Ok(());
            };
            if config.sync == sync {
                return Ok(());
            }
            config.sync = sync;
            config.clone()
        };
        if !self.demo {
            updated.save(&config_path()?)?;
        }
        if let Some(engine) = self.engine.borrow_mut().take() {
            engine.shutdown();
        }
        self.start_engine();
        Ok(())
    }

    fn oauth(&self) -> Result<OAuthClient> {
        let config = self.config.borrow();
        let config = config
            .as_ref()
            .ok_or_else(|| anyhow!("mailrs has no OAuth client configured yet"))?;
        Ok(OAuthClient::new(
            &config.oauth.client_id,
            &config.oauth.client_secret,
        ))
    }

    fn start_engine(&self) {
        let Some(config) = self.config.borrow().clone() else {
            return;
        };
        let (engine, engine_events) = SyncEngine::new(self.db.clone(), config.engine_config());
        let engine = Arc::new(engine);
        *self.engine.borrow_mut() = Some(Arc::clone(&engine));
        let forward = self.events_tx.clone();
        self.runtime.spawn(async move {
            while let Ok(event) = engine_events.recv().await {
                if forward.send(event).await.is_err() {
                    break;
                }
            }
        });
        let (db, tokens, demo, oauth) = (
            self.db.clone(),
            Arc::clone(&self.tokens),
            self.demo,
            self.oauth().ok(),
        );
        self.runtime.spawn(async move {
            let Ok(all) = db.read(accounts::list_accounts).await else { return };
            for account in all {
                match connect(&db, demo, oauth.clone(), Arc::clone(&tokens), &account).await {
                    Ok(api) => engine.start_account(account.id, Arc::new(api)),
                    Err(err) => tracing::warn!(account = %account.email, error = %err, "could not start syncing"),
                }
            }
        });
    }

    /// Runs `future` on the tokio runtime and waits for it from the GTK loop.
    pub async fn call<T, E, F>(&self, future: F) -> Result<T>
    where
        F: Future<Output = std::result::Result<T, E>> + Send + 'static,
        T: Send + 'static,
        E: Into<anyhow::Error> + Send + 'static,
    {
        let guard = InFlight::new(&self.in_flight);
        let task = async move {
            let _guard = guard;
            future.await
        };
        match self.runtime.spawn(task).await {
            Ok(result) => result.map_err(Into::into),
            Err(err) => Err(anyhow!("background task failed: {err}")),
        }
    }

    /// User operations, such as a send or an archive, that have not finished.
    pub fn busy(&self) -> bool {
        self.in_flight.load(Ordering::SeqCst) > 0
    }

    /// Runs a read query on the store's reader pool.
    pub async fn read<T, F>(&self, query: F) -> Result<T>
    where
        F: FnOnce(&rusqlite::Connection) -> mailrs_store::Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let db = self.db.clone();
        self.call(async move { db.read(query).await }).await
    }

    /// Runs a change on the store's writer thread.
    pub async fn write<T, F>(&self, change: F) -> Result<T>
    where
        F: FnOnce(&rusqlite::Connection) -> mailrs_store::Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let db = self.db.clone();
        self.call(async move { db.write(change).await }).await
    }

    /// Runs a change on the writer thread without waiting; failures are logged.
    pub fn spawn_write<F>(&self, change: F)
    where
        F: FnOnce(&rusqlite::Connection) -> mailrs_store::Result<()> + Send + 'static,
    {
        let db = self.db.clone();
        self.spawn(async move {
            if let Err(err) = db.write(change).await {
                tracing::warn!(error = %err, "could not update the store");
            }
        });
    }

    /// Runs `future` on the tokio runtime without waiting.
    pub fn spawn<F>(&self, future: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.runtime.spawn(future);
    }

    pub fn account(&self, account_id: AccountId) -> Option<Arc<Sync>> {
        self.engine
            .borrow()
            .as_ref()
            .and_then(|e| e.account(account_id).ok())
    }

    pub fn poke(&self, account_id: AccountId) {
        if let Some(engine) = self.engine.borrow().as_ref() {
            engine.poke(account_id);
        }
    }

    pub fn poke_all(&self) {
        if let Some(engine) = self.engine.borrow().as_ref() {
            engine.poke_all();
        }
    }

    /// Runs the browser consent flow, stores the refresh token, and starts
    /// syncing the account. `urls` receives the consent URL to open. When
    /// `expected` is set, the user must pick that account.
    pub async fn authorize_account(
        &self,
        urls: async_channel::Sender<String>,
        expected: Option<String>,
    ) -> Result<Account> {
        if self.demo {
            bail!("Demo mode cannot add real accounts.");
        }
        let oauth = self.oauth()?;
        let engine = self
            .engine
            .borrow()
            .clone()
            .ok_or_else(|| anyhow!("sync is not running"))?;
        let (db, tokens) = (self.db.clone(), Arc::clone(&self.tokens));
        self.call(async move {
            let flow = authorize(&oauth, GMAIL_API_BASE, move |url| {
                let _ = urls.try_send(url.to_string());
            });
            let authorized = tokio::time::timeout(std::time::Duration::from_secs(300), flow)
                .await
                .map_err(|_| anyhow!("Gave up waiting for the browser after five minutes."))??;
            if let Some(expected) = expected
                && !expected.eq_ignore_ascii_case(&authorized.email)
            {
                bail!(
                    "You signed in as {}. Choose {expected} to reconnect that account.",
                    authorized.email
                );
            }
            let (email, refresh) = (authorized.email.clone(), authorized.refresh_token.clone());
            let store = Arc::clone(&tokens);
            tokio::task::spawn_blocking(move || store.save(&email, &refresh)).await??;
            let email = authorized.email.clone();
            let id = db
                .write(move |c| accounts::insert_account(c, &email, now_millis()))
                .await?;
            let account = db
                .read(accounts::list_accounts)
                .await?
                .into_iter()
                .find(|a| a.id == id)
                .ok_or_else(|| anyhow!("the new account disappeared"))?;
            let api = connect(&db, false, Some(oauth), tokens, &account).await?;
            engine.start_account(account.id, Arc::new(api));
            Ok::<_, anyhow::Error>(account)
        })
        .await
    }

    /// Stops syncing an account and deletes its local mail and refresh token.
    pub async fn remove_account(&self, account: Account) -> Result<()> {
        if let Some(engine) = self.engine.borrow().as_ref() {
            engine.stop_account(account.id);
        }
        let (db, tokens, demo) = (self.db.clone(), Arc::clone(&self.tokens), self.demo);
        self.call(async move {
            db.write(move |c| accounts::delete_account(c, account.id))
                .await?;
            if !demo {
                tokio::task::spawn_blocking(move || tokens.delete(&account.email)).await??;
            }
            Ok::<_, anyhow::Error>(())
        })
        .await
    }
}

async fn connect(
    db: &Db,
    demo: bool,
    oauth: Option<OAuthClient>,
    tokens: Arc<dyn TokenStore>,
    account: &Account,
) -> Result<Api> {
    if demo {
        return Ok(Api::Demo(DemoApi {
            db: db.clone(),
            account_id: account.id,
        }));
    }
    let oauth = oauth.ok_or_else(|| anyhow!("mailrs has no OAuth client configured yet"))?;
    Ok(Api::Gmail(Box::new(
        connect_account(oauth, tokens, account).await?,
    )))
}
