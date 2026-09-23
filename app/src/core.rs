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
use mailrs_domain::{Account, AccountId, AccountState, ChangeEvent, Target};
use mailrs_gmail::{
    GMAIL_API_BASE, KeyringTokenStore, OAuthClient, TokenStore, authorize, built_in_client,
};
use mailrs_pgp::{Pgp, PgpError};
use mailrs_smime::{Smime, SmimeError};
use mailrs_store::{Db, accounts};
use mailrs_sync::config::{Config, config_path, data_dir, migrate_old_dirs};
use mailrs_sync::lock::{LockError, SyncLock};
use mailrs_sync::sign_in::{account_client, signed_in};
use mailrs_sync::{
    AccountServices, AccountSettings, AccountSync, Accounts, ContactBook, Failure, History,
    Invitations, MailAction, MailActions, Mailboxes, OneClick, Outbox, Outcome, SyncEngine, Undone,
    connect_account, now_millis,
};

use crate::assistant::run::{Background, Modules};
use crate::demo::{self, DemoGmail};
use mailrs_domain::translate::{fill, gettext};

pub type Engine = SyncEngine;
pub type Sync = AccountSync;
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
    calendar: Arc<mailrs_sync::Calendar<RunningEngine>>,
    outbox: Arc<Waiting>,
    /// The sync settings and, for accounts added through the old setup
    /// page, their own Google client.
    config: RefCell<Config>,
    /// The person's own gpg, found once at startup. With none, every
    /// OpenPGP control stays out of the window rather than failing later.
    pgp: Option<Pgp>,
    /// Their gpgsm, found the same way, for the S/MIME half of the same
    /// controls.
    smime: Option<Smime>,
    /// What the engines said about signed messages this run, so reopening
    /// one does not start gpg again. Memory only.
    pub verdicts: RefCell<crate::protection::remembered::Verdicts>,
    tokens: Arc<dyn TokenStore>,
    events_tx: async_channel::Sender<ChangeEvent>,
    pub events: async_channel::Receiver<ChangeEvent>,
    in_flight: Arc<AtomicUsize>,
    /// Keeps `penguin-mail-cli sync` off this store while the app runs.
    /// Changing the sync settings restarts the engine in this process, so
    /// the lock stays with the core rather than with one engine.
    /// The demo takes none.
    _sync_lock: Option<SyncLock>,
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
    /// Opens the store and starts syncing every account.
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
            mailrs_sync::config::secure_dirs();
            data_dir()?
        };
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("could not create {}", dir.display()))?;
        // The demo's store is its own and new each run, so nothing else
        // could be syncing it.
        let sync_lock = match (!demo).then(|| SyncLock::take(&dir)).transpose() {
            Ok(lock) => lock,
            Err(LockError::Held) => bail!(gettext(
                "Another copy of Penguin Mail, or penguin-mail-cli sync, is syncing your \
                 mail. Stop it, then open Penguin Mail again."
            )),
            Err(err) => return Err(err.into()),
        };
        let db_path: PathBuf = dir.join("mailrs.db");
        if demo {
            let _ = std::fs::remove_file(&db_path);
        }
        let db = Db::open(&db_path)?;
        // A first run has no config file and needs none: new accounts sign
        // in with the client the build carries.
        let config = if demo {
            Config::default()
        } else {
            match Config::load(&config_path()?) {
                Ok(config) => config,
                Err(err) if err.is_missing() => Config::default(),
                Err(err) => return Err(err.into()),
            }
        };
        let demo_gmail = if demo {
            let seeded = runtime.block_on(demo::seed(&db, now_millis()))?;
            Some(Arc::new(seeded))
        } else {
            None
        };
        let (events_tx, events) = async_channel::unbounded();
        let engine = Arc::new(RunningEngine::default());
        // The demo's newsletters point at addresses nobody owns, so its
        // one-click requests go to a fake.
        let one_click = match demo {
            true => OneClick::Fake(Arc::default()),
            false => OneClick::Web,
        };
        let actions = Arc::new(MailActions::new(Arc::clone(&engine), db.clone(), one_click));
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
        let calendar = Arc::new(mailrs_sync::Calendar::new(Arc::clone(&engine)));
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
            calendar,
            outbox,
            config: RefCell::new(config),
            pgp: Pgp::find().ok(),
            smime: Smime::find().ok(),
            verdicts: RefCell::default(),
            tokens: Arc::new(KeyringTokenStore::new()),
            events_tx,
            events,
            in_flight: Arc::new(AtomicUsize::new(0)),
            _sync_lock: sync_lock,
        });
        core.start_engine();
        Ok(core)
    }

    /// Whether this copy was built with the Google client that every
    /// sign-in goes through. A copy built from source without the release
    /// values has none and cannot add a Google account.
    pub fn built_with_google_sign_in(&self) -> bool {
        built_in_client().is_some()
    }

    /// The sync section of `config.toml`.
    pub fn sync_config(&self) -> mailrs_sync::config::SyncConfig {
        self.config.borrow().sync.clone()
    }

    /// Saves new sync settings and restarts every account's loop with them.
    /// Demo mode keeps them in memory only.
    pub fn update_sync(&self, sync: mailrs_sync::config::SyncConfig) -> Result<()> {
        let updated = {
            let mut config = self.config.borrow_mut();
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

    fn start_engine(&self) {
        let config = self.config.borrow().clone();
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
        let (db, tokens, demo, events) = (
            self.db.clone(),
            Arc::clone(&self.tokens),
            self.demo_gmail.clone(),
            self.events_tx.clone(),
        );
        self.runtime.spawn(async move {
            let Ok(all) = db.read(accounts::list_accounts).await else { return };
            for account in all {
                // The demo's accounts talk to the sample mailbox and need
                // no Google client.
                let oauth = if demo.is_some() {
                    None
                } else {
                    match account_client(&db, &config, built_in_client(), &account).await {
                        Ok(Some(oauth)) => Some(oauth),
                        // The store now says the account needs a new
                        // sign-in; the sidebar hears it here, since the
                        // engine never runs the account to report it.
                        Ok(None) => {
                            tracing::warn!(account = %account.email, "no Google client for this account");
                            let state = AccountState::NeedsReauth;
                            let _ = events
                                .send(ChangeEvent::AccountStateChanged { account_id: account.id, state })
                                .await;
                            continue;
                        }
                        Err(err) => {
                            tracing::warn!(account = %account.email, error = %err, "could not read the account's Google client");
                            continue;
                        }
                    }
                };
                match connect(demo.as_deref(), oauth, Arc::clone(&tokens), &account).await {
                    Ok(services) => engine.start_account(account.id, services),
                    Err(err) => tracing::warn!(account = %account.email, error = %err, "could not start syncing"),
                }
            }
        });
    }

    /// The tokio runtime, for a port that has to start its own work there.
    pub fn runtime(&self) -> tokio::runtime::Handle {
        self.runtime.handle().clone()
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
            .ok_or_else(|| anyhow!(crate::pgp::explain(&PgpError::NoGpg)))?;
        self.call(async move {
            let answered = tokio::task::spawn_blocking(move || run(&pgp)).await?;
            // The engine's own words are for a log. Whatever reaches a
            // person from here, a toast or a line on the card, is theirs.
            answered.map_err(|err| anyhow!(crate::pgp::explain(&err)))
        })
        .await
    }

    /// When the keyring the engine for `opening` reads against last
    /// changed, which is how long a remembered answer holds.
    pub fn keyring_stamp(
        &self,
        opening: crate::protection::Engine,
    ) -> Option<std::time::SystemTime> {
        match opening {
            crate::protection::Engine::Pgp(_) => self.pgp.as_ref()?.keyring_stamp(),
            crate::protection::Engine::Smime(_) => self.smime.as_ref()?.keyring_stamp(),
        }
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
            .ok_or_else(|| anyhow!(crate::smime::explain(&SmimeError::NoGpgsm)))?;
        self.call(async move {
            let answered = tokio::task::spawn_blocking(move || run(&smime)).await?;
            answered.map_err(|err| anyhow!(crate::smime::explain(&err)))
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
            calendar: Arc::clone(&self.calendar),
            invitations: Arc::clone(&self.invitations),
            contacts: Arc::clone(&self.contacts),
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
    /// Every sign-in, first or again, goes through the build's client and
    /// records it for the account.
    pub async fn authorize_account(
        &self,
        urls: async_channel::Sender<String>,
        expected: Option<String>,
        extra: &[&'static str],
    ) -> Result<Account> {
        if self.demo {
            bail!(gettext("Demo mode cannot add real accounts."));
        }
        let oauth = built_in_client().ok_or_else(|| {
            anyhow!(gettext(
                "This copy of Penguin Mail was built without Google sign-in. \
                 Get a release from github.com/c9dev/penguin-mail/releases."
            ))
        })?;
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
            let account = signed_in(&db, &authorized.email, now_millis()).await?;
            let services = connect(None, Some(oauth), tokens, &account).await?;
            engine.start_account(account.id, services);
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

/// The services for one account: the sample mailbox in demo mode, or
/// Google signed in with the account's refresh token.
async fn connect(
    demo: Option<&DemoGmail>,
    oauth: Option<OAuthClient>,
    tokens: Arc<dyn TokenStore>,
    account: &Account,
) -> Result<AccountServices> {
    if let Some(demo) = demo {
        let mailbox = demo
            .account(account.id)
            .ok_or_else(|| anyhow!("the demo has no mailbox for {}", account.email))?;
        return Ok(AccountServices::fake(mailbox));
    }
    let oauth = oauth.ok_or_else(|| anyhow!("Penguin Mail has no OAuth client configured yet"))?;
    Ok(AccountServices::google(
        connect_account(oauth, tokens, account).await?,
    ))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::Core;

    #[test]
    fn every_tool_run_shares_one_calendar() {
        let core = Core::open(true).expect("the demo core opens");
        assert!(Arc::ptr_eq(
            &core.modules().calendar,
            &core.modules().calendar
        ));
    }
}
