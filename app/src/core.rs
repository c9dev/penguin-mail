//! The bridge between GTK and the sync core. GTK owns the main thread; a
//! tokio runtime runs the engine and every database and network call. The
//! UI hands futures to `Core::call` and awaits the result on the main loop.

use std::cell::RefCell;
use std::future::Future;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Context, Result, anyhow, bail};
use mailrs_domain::{Account, AccountId, ChangeEvent, Target};
use mailrs_gmail::{GMAIL_API_BASE, KeyringTokenStore, OAuthClient, TokenStore, authorize};
use mailrs_pgp::{Pgp, PgpError};
use mailrs_smime::{Smime, SmimeError};
use mailrs_store::{Db, accounts};
use mailrs_sync::config::{Config, config_path, data_dir, migrate_old_dirs};
use mailrs_sync::{
    AccountSettings, AccountSync, Accounts, AnyGmail, ContactBook, Failure, History, Invitations,
    MailAction, MailActions, Mailboxes, Outbox, Outcome, SyncEngine, Undone, connect_account,
    now_millis,
};

use crate::assistant::run::{Background, Modules};
use crate::demo::{self, DemoGmail};
use mailrs_domain::translate::{fill, gettext};

/// Gmail for real accounts, or the in-memory stand-in for demo mode.
/// `mailrs_sync::AnyGmail` holds both, since `GmailApi`'s `impl Future`
/// returns rule out one `dyn` object for the two.
pub type Api = AnyGmail;

pub type Engine = SyncEngine<Api>;
pub type Sync = AccountSync<Api>;
pub type Actions = MailActions<RunningEngine>;
/// Reads mailboxes for the window and the assistant alike.
pub type Lists = Mailboxes<RunningEngine>;
/// Changes an account's Gmail settings for the dialogs and the assistant
/// alike. See `mailrs_sync::AccountSettings`.
pub type GmailSettings = AccountSettings<RunningEngine>;
/// Reads the accounts' Google contacts. See `mailrs_sync::ContactBook`.
pub type Contacts = ContactBook<RunningEngine>;
/// Reads the invitations in mail and answers them. See
/// `mailrs_sync::Invitations`.
pub type Events = Invitations<RunningEngine>;
/// Holds the messages waiting to go out and sends them when it can. See
/// `mailrs_sync::Outbox`.
pub type Waiting = Outbox<RunningEngine>;

/// The engine that runs now. Changing the sync settings replaces it, so mail
/// actions look accounts up here rather than keep one engine.
#[derive(Default)]
pub struct RunningEngine(Mutex<Option<Arc<Engine>>>);

impl RunningEngine {
    fn current(&self) -> Option<Arc<Engine>> {
        self.lock().clone()
    }

    fn replace(&self, engine: Option<Arc<Engine>>) -> Option<Arc<Engine>> {
        std::mem::replace(&mut *self.lock(), engine)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Arc<Engine>>> {
        self.0.lock().expect("engine lock poisoned")
    }
}

impl Accounts for RunningEngine {
    type Api = Api;

    fn account(&self, account_id: AccountId) -> Option<Arc<Sync>> {
        self.current()?.account(account_id).ok()
    }
}

pub struct Core {
    runtime: tokio::runtime::Runtime,
    pub db: Db,
    pub demo: bool,
    /// The sample accounts' Gmail, in demo mode only.
    demo_gmail: Option<Arc<DemoGmail>>,
    engine: Arc<RunningEngine>,
    actions: Arc<Actions>,
    lists: Arc<Lists>,
    gmail_settings: Arc<GmailSettings>,
    contacts: Arc<Contacts>,
    invitations: Arc<Events>,
    outbox: Arc<Waiting>,
    config: RefCell<Option<Config>>,
    /// The person's own gpg, found once at startup. With none, every
    /// OpenPGP control stays out of the window rather than failing later.
    pgp: Option<Pgp>,
    /// Their gpgsm, found the same way, for the S/MIME half of the same
    /// controls.
    smime: Option<Smime>,
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
            std::env::temp_dir().join(format!("penguin-mail-demo-{}", std::process::id()))
        } else {
            migrate_old_dirs();
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
        let demo_gmail = if demo {
            let seeded = runtime.block_on(db.write(|c| demo::seed(c, now_millis())))?;
            Some(Arc::new(seeded))
        } else {
            None
        };
        let (events_tx, events) = async_channel::unbounded();
        let engine = Arc::new(RunningEngine::default());
        let actions = Arc::new(MailActions::new(Arc::clone(&engine), db.clone()));
        let lists = Arc::new(Mailboxes::new(Arc::clone(&engine), db.clone()));
        let gmail_settings = Arc::new(AccountSettings::new(Arc::clone(&engine), db.clone()));
        let photo_dir = contact_photo_dir(demo, &dir);
        if demo {
            let photos = photo_dir.clone();
            runtime.block_on(db.write(move |c| demo::seed_contacts(c, &photos)))?;
        }
        let contacts = Arc::new(ContactBook::new(Arc::clone(&engine), db.clone(), photo_dir));
        let invitations = Arc::new(Invitations::new(Arc::clone(&engine), db.clone()));
        let outbox = Arc::new(Outbox::new(Arc::clone(&engine), db.clone()));
        let core = Rc::new(Core {
            runtime,
            db,
            demo,
            demo_gmail,
            engine,
            actions,
            lists,
            gmail_settings,
            contacts,
            invitations,
            outbox,
            config: RefCell::new(config),
            pgp: Pgp::find().ok(),
            smime: Smime::find().ok(),
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
        if let Some(engine) = self.engine.replace(None) {
            engine.shutdown();
        }
        self.start_engine();
        Ok(())
    }

    fn oauth(&self) -> Result<OAuthClient> {
        let config = self.config.borrow();
        let config = config
            .as_ref()
            .ok_or_else(|| anyhow!("Penguin Mail has no OAuth client configured yet"))?;
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
        self.engine.replace(Some(Arc::clone(&engine)));
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
            self.demo_gmail.clone(),
            self.oauth().ok(),
        );
        self.runtime.spawn(async move {
            let Ok(all) = db.read(accounts::list_accounts).await else { return };
            for account in all {
                match connect(demo.as_deref(), oauth.clone(), Arc::clone(&tokens), &account).await {
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

    /// Whether this computer has a gpg to run. Without one the window
    /// offers nothing about OpenPGP.
    pub fn has_gpg(&self) -> bool {
        self.pgp.is_some()
    }

    /// Runs one call against the person's gpg and waits for it from the
    /// GTK loop. gpg puts a pinentry in front of them and waits as long as
    /// they take to type, so the call goes to a blocking thread rather
    /// than onto a runtime worker with the mail on it.
    pub async fn gpg<T, F>(&self, run: F) -> Result<T>
    where
        F: FnOnce(&Pgp) -> std::result::Result<T, PgpError> + Send + 'static,
        T: Send + 'static,
    {
        let pgp = self
            .pgp
            .clone()
            .ok_or_else(|| anyhow!("this computer has no gpg"))?;
        self.call(async move {
            let answered = tokio::task::spawn_blocking(move || run(&pgp)).await?;
            Ok::<_, anyhow::Error>(answered?)
        })
        .await
    }

    /// Whether this computer has a gpgsm to run. Without one the window
    /// offers nothing about S/MIME.
    pub fn has_gpgsm(&self) -> bool {
        self.smime.is_some()
    }

    /// Runs one call against the person's gpgsm, on a blocking thread for
    /// the reason [`Core::gpg`] gives.
    pub async fn gpgsm<T, F>(&self, run: F) -> Result<T>
    where
        F: FnOnce(&Smime) -> std::result::Result<T, SmimeError> + Send + 'static,
        T: Send + 'static,
    {
        let smime = self
            .smime
            .clone()
            .ok_or_else(|| anyhow!("this computer has no gpgsm"))?;
        self.call(async move {
            let answered = tokio::task::spawn_blocking(move || run(&smime)).await?;
            Ok::<_, anyhow::Error>(answered?)
        })
        .await
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
        self.engine.account(account_id)
    }

    /// Mail actions and the undo stack behind them. See
    /// `mailrs_sync::MailActions`.
    pub fn actions(&self) -> Arc<Actions> {
        Arc::clone(&self.actions)
    }

    /// One page of a mailbox, the sidebar counts, and fresh rows for the
    /// threads a change event named. See `mailrs_sync::Mailboxes`.
    pub fn lists(&self) -> Arc<Lists> {
        Arc::clone(&self.lists)
    }

    /// The Gmail settings of every account: automatic replies, rules,
    /// blocked senders, and hidden addresses.
    pub fn gmail_settings(&self) -> Arc<GmailSettings> {
        Arc::clone(&self.gmail_settings)
    }

    /// The accounts' Google contacts: names, photos, and the rest.
    pub fn contacts(&self) -> Arc<Contacts> {
        Arc::clone(&self.contacts)
    }

    /// The invitations in mail: what one says, and the answer the user
    /// sends back.
    pub fn invitations(&self) -> Arc<Events> {
        Arc::clone(&self.invitations)
    }

    /// The messages waiting to go out: Send Later, and whatever could not
    /// be sent when it was written. See `mailrs_sync::Outbox`.
    pub fn outbox(&self) -> Arc<Waiting> {
        Arc::clone(&self.outbox)
    }

    /// The modules the assistant's tools work through.
    pub fn modules(&self) -> Modules<RunningEngine> {
        Modules {
            mail: Arc::clone(&self.actions),
            lists: Arc::clone(&self.lists),
            gmail: Arc::clone(&self.gmail_settings),
            accounts: Arc::clone(&self.engine),
            db: self.db.clone(),
        }
    }

    /// Runs a mail action on the tokio runtime. See `MailActions::run`.
    pub async fn act(&self, targets: Vec<Target>, action: MailAction, history: History) -> Outcome {
        let (actions, given) = (Arc::clone(&self.actions), targets.clone());
        let outcome = self
            .call(async move {
                Ok::<_, std::convert::Infallible>(actions.run(&given, action, history).await)
            })
            .await;
        outcome.unwrap_or_else(|err| Outcome {
            done: vec![],
            failed: targets
                .into_iter()
                .map(|target| Failure {
                    target,
                    error: err.to_string(),
                })
                .collect(),
        })
    }

    /// Reverses the action on top of the undo stack, from the window or
    /// the assistant. `None` when the stack is empty.
    pub async fn undo(&self) -> Option<Undone> {
        let actions = Arc::clone(&self.actions);
        self.call(async move { Ok::<_, std::convert::Infallible>(actions.undo().await) })
            .await
            .ok()
            .flatten()
    }

    pub fn poke(&self, account_id: AccountId) {
        if let Some(engine) = self.engine.current() {
            engine.poke(account_id);
        }
    }

    /// Checks every account now instead of at its next tick. The kept
    /// Gmail search goes too, since asking for new mail means the folder
    /// on screen should be listed again rather than answered from memory.
    pub fn poke_all(&self) {
        self.forget_remote();
        if let Some(engine) = self.engine.current() {
            engine.poke_all();
        }
    }

    /// Drops the Gmail search the last folder or search listing kept, so
    /// the next listing asks Gmail again.
    pub fn forget_remote(&self) {
        self.lists.forget_remote();
    }

    /// Runs the browser consent flow, stores the refresh token, and starts
    /// syncing the account. `urls` receives the consent URL to open. When
    /// `expected` is set, the user must pick that account. `extra` names
    /// permissions to ask for beyond the ones sign-in always requests, such
    /// as `DELETE_SCOPE`; an account that already granted them keeps them.
    pub async fn authorize_account(
        &self,
        urls: async_channel::Sender<String>,
        expected: Option<String>,
        extra: &[&'static str],
    ) -> Result<Account> {
        if self.demo {
            bail!(gettext("Demo mode cannot add real accounts."));
        }
        let oauth = self.oauth()?;
        let engine = self
            .engine
            .current()
            .ok_or_else(|| anyhow!("sync is not running"))?;
        let (db, tokens) = (self.db.clone(), Arc::clone(&self.tokens));
        let extra = extra.to_vec();
        self.call(async move {
            let flow = authorize(&oauth, GMAIL_API_BASE, &extra, move |url: &str| {
                let _ = urls.try_send(url.to_string());
            });
            let authorized = tokio::time::timeout(std::time::Duration::from_secs(300), flow)
                .await
                .map_err(|_| {
                    anyhow!(gettext(
                        "Gave up waiting for the browser after five minutes."
                    ))
                })??;
            if let Some(expected) = expected
                && !expected.eq_ignore_ascii_case(&authorized.email)
            {
                bail!(fill(
                    &gettext(
                        "You signed in as {account}. Choose {wanted} to reconnect that \
                         account.",
                    ),
                    &[("account", &authorized.email), ("wanted", &expected)],
                ));
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
            let api = connect(None, Some(oauth), tokens, &account).await?;
            engine.start_account(account.id, Arc::new(api));
            Ok::<_, anyhow::Error>(account)
        })
        .await
    }

    /// Stops syncing an account and deletes its local mail and refresh token.
    pub async fn remove_account(&self, account: Account) -> Result<()> {
        if let Some(engine) = self.engine.current() {
            engine.stop_account(account.id);
        }
        // Nothing is left to reverse the account's actions through, and
        // its mail goes with it.
        self.actions.forget_account(account.id);
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

/// The assistant's tools run on the GTK thread and hand their store and
/// Gmail calls here, so they reach the same runtime and the same busy count
/// as the window's own calls.
impl Background for Core {
    fn start(&self, task: std::pin::Pin<Box<dyn Future<Output = ()> + Send>>) {
        let guard = InFlight::new(&self.in_flight);
        self.runtime.spawn(async move {
            let _guard = guard;
            task.await;
        });
    }
}

/// Where contact photos are kept: the cache directory, since Google
/// serves them again whenever they are wanted. Demo photos sit beside the
/// demo's throwaway store.
fn contact_photo_dir(demo: bool, data_dir: &std::path::Path) -> PathBuf {
    if demo {
        return data_dir.join("contact-photos");
    }
    gtk::glib::user_cache_dir()
        .join(mailrs_sync::config::DIR_NAME)
        .join("contact-photos")
}

/// Gmail for one account: the sample mailbox in demo mode, or the real
/// client signed in with the account's refresh token.
async fn connect(
    demo: Option<&DemoGmail>,
    oauth: Option<OAuthClient>,
    tokens: Arc<dyn TokenStore>,
    account: &Account,
) -> Result<Api> {
    if let Some(demo) = demo {
        let mailbox = demo
            .account(account.id)
            .ok_or_else(|| anyhow!("the demo has no mailbox for {}", account.email))?;
        return Ok(Api::Fake(mailbox));
    }
    let oauth = oauth.ok_or_else(|| anyhow!("Penguin Mail has no OAuth client configured yet"))?;
    Ok(Api::Real(Box::new(
        connect_account(oauth, tokens, account).await?,
    )))
}
