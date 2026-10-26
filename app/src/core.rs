//! The bridge between GTK and the sync core. GTK owns the main thread; a
//! tokio runtime runs the engine and every database and network call. The
//! UI hands futures to `Core::call` and awaits the result on the main loop.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Context, Result, anyhow, bail};
use mailrs_discover::{Found, JmapFound, Net, RealNet, Security, SrvRecord};
use mailrs_domain::{
    Account, AccountId, AccountState, ChangeEvent, Provider, SignInClient, Target,
};
use mailrs_pgp::{Pgp, PgpError};
use mailrs_smime::{Smime, SmimeError};
use mailrs_store::services::{FoundService, Miss, ServiceKind};
use mailrs_store::{Db, StoreError, accounts};
use mailrs_sync::config::{Config, config_path, data_dir, migrate_old_dirs};
use mailrs_sync::lock::{LockError, SyncLock};
use mailrs_sync::passwords::{PasswordStore, Secrets};
use mailrs_sync::read_inbox::InboxesRead;
use mailrs_sync::services::finding;
use mailrs_sync::sign_in::app_password::{
    NewGmailImap, check_gmail, gmail_imap_signed_in, read_pasted,
};
use mailrs_sync::sign_in::own_client::{OwnClient, saved_project};
use mailrs_sync::sign_in::{
    Browser, BrowserReady, BrowserSignInError, ClientSource, JmapBrowser, KeptClient, NewCalDav,
    NewImap, NewJmap, NewPop3, Wanted, caldav_signed_in, caldav_signed_in_again, calendar_consent,
    check_pop3, google_sign_in, imap_signed_in, in_browser, jmap_in_browser, jmap_signed_in,
    kept_own_client, pop3_signed_in,
};
use mailrs_sync::{
    AccountServices, AccountSettings, AccountSync, Accounts, BackendError, Clients, Connector,
    ContactBook, Failure, History, Invitations, MailAction, MailActions, Mailboxes, MovedFrom,
    OneClick, Outbox, Outcome, SyncEngine, SyncError, Undone, connect_caldav, connect_imap,
    connect_jmap, connect_pop3, now_millis, pop3_servers_for, servers_for,
};

use crate::add_account::lookup::{Check, Heard, check_of_url};
use crate::add_account::{Attempt, JmapAttempt};
use crate::assistant::run::{Background, Modules};
use crate::demo::{self, DemoMail};
use crate::starting::{self, Connected, Starting};
use mailrs_domain::translate::gettext;

mod goa;

/// The store could not be updated to this version, and the copy taken
/// before the attempt was put back. Startup shows this with a way to
/// report it, since only a fixed release can open the store again.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct StoreNotUpdated {
    pub message: String,
}

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
/// Keeps the local copy of every account's calendars fresh. See
/// `mailrs_sync::calendar_copy::CalendarCopy`. Built over `RunningEngine`,
/// not `SyncEngine`, like every other module here: a changed sync setting
/// replaces the engine, and a copy holding the old one would go stale.
pub type CalendarCopy = mailrs_sync::calendar_copy::CalendarCopy<RunningEngine>;

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
    /// Where the mail store lives, for About's backup line.
    pub store_path: PathBuf,
    /// The sample accounts' servers, in demo mode only.
    demo_mail: Option<Arc<DemoMail>>,
    engine: Arc<RunningEngine>,
    actions: Arc<Actions>,
    lists: Arc<Lists>,
    gmail_settings: Arc<GmailSettings>,
    contacts: Arc<Contacts>,
    invitations: Arc<Events>,
    calendar: Arc<mailrs_sync::Calendar<RunningEngine>>,
    calendar_copy: Arc<CalendarCopy>,
    outbox: Arc<Waiting>,
    rules: Arc<mailrs_sync::rules::RulesEngine<RunningEngine>>,
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
    /// Why the last search found no calendar or contacts server for an
    /// account, read from the store at startup and kept by each search, so
    /// Preferences words its Not Available lines without waiting.
    misses: RefCell<HashMap<(AccountId, ServiceKind), Miss>>,
    /// The IMAP accounts whose server also speaks JMAP, with the session
    /// to sign in to, as `look_for_jmap` found them.
    switch_offers: RefCell<HashMap<AccountId, JmapFound>>,
    /// The desktop's Online Accounts, or a stand-in in the demo.
    goa: Arc<dyn mailrs_sync::goa::OnlineAccounts>,
    /// What the app last read from it.
    goa_state: Arc<goa::GoaState>,
    /// The watch on it, while one runs.
    goa_watch: RefCell<Option<tokio::task::JoinHandle<()>>>,
    /// Whether an account reads through Online Accounts.
    goa_linked: Cell<bool>,
    /// The accounts `look_for_jmap` has looked at this run, found or not.
    switch_looked: RefCell<HashSet<AccountId>>,
    /// Where each kind of account keeps what signs it in: the keyring, or
    /// memory in the demo, which never touches the person's keyring.
    secrets: Secrets,
    events_tx: async_channel::Sender<ChangeEvent>,
    pub events: async_channel::Receiver<ChangeEvent>,
    started_tx: async_channel::Sender<AccountId>,
    /// Each account whose sync started, once per start.
    pub started: async_channel::Receiver<AccountId>,
    in_flight: Arc<AtomicUsize>,
    /// Whether the computer has a network, as the network monitor last
    /// said. A new engine, after a change to the sync settings, starts
    /// from this rather than from the engine's own guess.
    network: Cell<bool>,
    /// Whether the main window is open, as `set_window_open` last said.
    /// False until the first `show_window`, so a process that starts in
    /// the tray with `--background` polls at the tray's own pace rather
    /// than the window's; a new engine, after a change to the sync
    /// settings, starts from this rather than reopening as the window.
    window_open: Cell<bool>,
    /// Keeps `penguin-mail-cli sync` off this store while the app runs.
    /// Changing the sync settings restarts the engine in this process, so
    /// the lock stays with the core rather than with one engine.
    /// The demo takes none.
    _sync_lock: Option<SyncLock>,
    /// The demo's own folder, holding its store and settings, removed
    /// when the core closes. Last, so the store closes before it goes.
    demo_folder: Option<crate::demo::folder::DemoFolder>,
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
    /// The demo's own folder, where its settings belong too. None outside
    /// the demo.
    pub fn demo_folder(&self) -> Option<&Path> {
        self.demo_folder.as_ref().map(|folder| folder.path())
    }

    /// Opens the store and starts syncing every account.
    pub fn open(demo: bool) -> Result<Rc<Core>> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("mailrs-sync")
            .thread_stack_size(mailrs_sync::WORKER_STACK)
            .enable_all()
            .build()
            .context("could not start the async runtime")?;
        let demo_folder = demo
            .then(crate::demo::folder::DemoFolder::make)
            .transpose()
            .context("could not make the demo's folder")?;
        let dir = if let Some(folder) = &demo_folder {
            folder.path().to_path_buf()
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
        let db = match Db::open(&db_path) {
            Ok(db) => db,
            Err(err @ StoreError::Migration { .. }) => {
                tracing::error!(error = %err, "the mail store could not be updated");
                return Err(StoreNotUpdated {
                    message: gettext(
                        "Penguin Mail could not update its mail store, and kept the old one.",
                    ),
                }
                .into());
            }
            Err(err) => return Err(err.into()),
        };
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
        let demo_mail = if demo {
            let seeded = runtime.block_on(demo::seed(&db, now_millis()))?;
            Some(Arc::new(seeded))
        } else {
            None
        };
        let (events_tx, events) = async_channel::bounded(mailrs_sync::events::CHANNEL_EVENTS);
        let (started_tx, started) = async_channel::unbounded();
        let engine = Arc::new(RunningEngine::default());
        // The demo's newsletters point at addresses nobody owns, so its
        // one-click requests go to a fake.
        let one_click = match demo {
            true => OneClick::Fake(Arc::default()),
            false => OneClick::Web,
        };
        let actions = Arc::new(MailActions::new(Arc::clone(&engine), db.clone(), one_click));
        let lists = Arc::new(Mailboxes::new(Arc::clone(&engine), db.clone()));
        let rules = Arc::new(mailrs_sync::rules::RulesEngine::new(
            Arc::clone(&engine),
            db.clone(),
            Arc::clone(&actions),
        ));
        let gmail_settings = Arc::new(AccountSettings::new(Arc::clone(&engine), db.clone()));
        let photo_dir = contact_photo_dir(demo, &dir);
        if demo {
            let photos = photo_dir.clone();
            runtime.block_on(db.write(move |c| demo::seed_contacts(c, &photos)))?;
        }
        let contacts = Arc::new(ContactBook::new(Arc::clone(&engine), db.clone(), photo_dir));
        let outbox = Arc::new(Outbox::new(Arc::clone(&engine), db.clone()));
        let calendar_copy = Arc::new(CalendarCopy::new(Arc::clone(&engine), db.clone()));
        let invitations = Arc::new(Invitations::new(
            Arc::clone(&engine),
            db.clone(),
            Arc::clone(&calendar_copy),
        ));
        // A change still waiting on its Undo toast when the app last
        // stopped has no toast left to close over it, so this run queues
        // it before anything reads or sends the account it belongs to.
        runtime.block_on(calendar_copy.recover_holds())?;
        let calendar = Arc::new(mailrs_sync::Calendar::new(
            Arc::clone(&engine),
            db.clone(),
            Arc::clone(&calendar_copy),
        ));
        let misses = runtime
            .block_on(db.read(mailrs_store::services::all_misses))?
            .into_iter()
            .map(|(account, kind, miss)| ((account, kind), miss))
            .collect();
        let core = Rc::new(Core {
            runtime,
            db,
            demo,
            store_path: db_path.clone(),
            demo_mail,
            engine,
            actions,
            lists,
            gmail_settings,
            contacts,
            invitations,
            calendar,
            calendar_copy,
            outbox,
            rules,
            config: RefCell::new(config),
            pgp: Pgp::find().ok(),
            smime: Smime::find().ok(),
            verdicts: RefCell::default(),
            misses: RefCell::new(misses),
            switch_offers: RefCell::default(),
            goa: online_accounts(demo),
            goa_state: Arc::default(),
            goa_watch: RefCell::new(None),
            goa_linked: Cell::new(false),
            switch_looked: RefCell::default(),
            secrets: match demo {
                true => Secrets::memory(),
                false => Secrets::keyring(),
            },
            events_tx,
            events,
            started_tx,
            started,
            in_flight: Arc::new(AtomicUsize::new(0)),
            network: Cell::new(true),
            window_open: Cell::new(false),
            _sync_lock: sync_lock,
            demo_folder,
        });
        core.start_engine();
        Ok(core)
    }

    /// Whether this copy has a client for a sign-in on `browser`'s page:
    /// the build's own, or for Google an own client the environment names,
    /// or the one an account signing in again kept, which `own_account`
    /// says it has. A copy built from source without the release values
    /// has no built-in client. Half an own client counts as one, so the
    /// sign-in goes on to say which value is missing.
    pub fn built_with_sign_in(&self, browser: Browser, own_account: bool) -> bool {
        let clients = Clients::current();
        match browser {
            Browser::Google => {
                own_account
                    || !matches!(
                        google_sign_in(&clients, None),
                        Err(BrowserSignInError::NoClient(_))
                    )
            }
            Browser::Microsoft => clients.microsoft.is_some(),
        }
    }

    /// Where the client of a Google sign-in started now would come from,
    /// with `chosen` the setup's client, or why it cannot start. The
    /// environment is read again on each call. An account's kept client is
    /// not looked up here, so a sign-in again that would take it reads as
    /// the built-in one.
    pub fn google_sign_in_client(
        &self,
        chosen: Option<&OwnClient>,
    ) -> Result<ClientSource, BrowserSignInError> {
        google_sign_in(&Clients::current().with_chosen(chosen.cloned()), None)
            .map(|choice| choice.source)
    }

    /// What connecting, signing in and forgetting an account needs, with
    /// the settings as they are now.
    fn connector(&self) -> Connector {
        Connector {
            db: self.db.clone(),
            config: self.config.borrow().clone(),
            clients: Clients::current(),
            secrets: self.secrets.clone(),
            goa: self.goa.clone(),
        }
    }

    /// The sync section of `config.toml`.
    pub fn sync_config(&self) -> mailrs_sync::config::SyncConfig {
        self.config.borrow().sync.clone()
    }

    /// Where the client of a Google sign-in for `address` would come
    /// from, with `chosen` the setup's client: as
    /// [`Core::google_sign_in_client`], and the account's kept client when
    /// `address` is an account that signed in through its own project.
    /// Unlike that one it never fails: a sign-in that cannot start says
    /// so itself.
    pub async fn google_sign_in_source(
        &self,
        chosen: Option<&OwnClient>,
        address: Option<String>,
    ) -> ClientSource {
        if let Ok(source @ (ClientSource::Environment | ClientSource::Setup)) =
            self.google_sign_in_client(chosen)
        {
            return source;
        }
        let Some(address) = address else {
            return ClientSource::BuiltIn;
        };
        let own = self
            .db
            .read(move |c| {
                let Some(account) = accounts::account_by_email(c, &address)? else {
                    return Ok(false);
                };
                Ok(accounts::google_sign_ins(c)?
                    .get(&account.id)
                    .is_some_and(|sign_in| sign_in.client() == SignInClient::Own))
            })
            .await
            .unwrap_or(false);
        if own {
            ClientSource::Kept
        } else {
            ClientSource::BuiltIn
        }
    }

    /// Google Cloud's page of the project the Gmail account at `address`
    /// signs in through, for Preferences' link.
    pub async fn own_project_page(&self, address: String) -> String {
        let connector = self.connector();
        let kept = self
            .call(async move {
                let Connector {
                    db,
                    config,
                    secrets,
                    ..
                } = &connector;
                kept_own_client(db, config, secrets, &address).await
            })
            .await;
        let project = match kept {
            Ok(KeptClient::Found(own)) => own.project_id().map(str::to_string),
            _ => None,
        };
        crate::own_project::project_page(project.as_deref())
    }

    /// The Google project kept from an earlier setup, if there is one.
    /// The demo keeps none.
    pub async fn saved_project(&self) -> Option<OwnClient> {
        if self.demo {
            return None;
        }
        let secrets = self.secrets.clone();
        self.call(async move { Ok::<_, anyhow::Error>(saved_project(&secrets).await) })
            .await
            .ok()
            .flatten()
    }

    /// The step of the own Google project setup the person reached.
    pub fn google_setup_step(&self) -> Option<u8> {
        self.config.borrow().google_setup_step
    }

    /// Remembers the setup step reached, or `None` once the setup ends.
    /// Writes `config.toml`; the demo keeps it in memory. Unlike
    /// `update_sync` it leaves the engine running.
    pub fn set_google_setup_step(&self, step: Option<u8>) -> Result<()> {
        let updated = {
            let mut config = self.config.borrow_mut();
            if config.google_setup_step == step {
                return Ok(());
            }
            config.google_setup_step = step;
            config.clone()
        };
        if !self.demo {
            updated.save(&config_path()?)?;
        }
        Ok(())
    }

    /// Forgets the saved setup step after the setup's client signed an
    /// account in. A file that will not write is logged; the sign-in holds.
    fn finish_google_setup(&self) {
        if let Err(err) = self.set_google_setup_step(None) {
            tracing::warn!(%err, "could not clear the saved Google setup step");
        }
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
        engine.set_network(self.network.get());
        engine.set_window_open(self.window_open.get());
        self.engine.replace(Some(Arc::clone(&engine)));
        let forward = self.events_tx.clone();
        self.runtime.spawn(async move {
            while let Ok(event) = engine_events.recv().await {
                if forward.send(event).await.is_err() {
                    break;
                }
            }
        });
        let reach = Arc::new(Reach {
            connector: Connector {
                db: self.db.clone(),
                config,
                clients: Clients::built_in(),
                secrets: self.secrets.clone(),
                goa: self.goa.clone(),
            },
            demo: self.demo_mail.clone(),
        });
        let (running, events, started) = (
            Arc::clone(&self.engine),
            self.events_tx.clone(),
            self.started_tx.clone(),
        );
        let db = self.db.clone();
        self.runtime.spawn(async move {
            let Ok(all) = db.read(accounts::list_accounts).await else {
                return;
            };
            let port = Arc::new(EnginePort {
                engine,
                running,
                db,
                events,
                started,
            });
            let connect = move |account: Account| {
                let reach = Arc::clone(&reach);
                async move { reach.connect(&account).await }
            };
            starting::start_each(all, connect, port, starting::Waits::APP).await;
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

    /// Runs an account's call without losing its provider when a server
    /// asks us to wait. Keep the original error as the source for callers
    /// that still need to inspect it.
    pub async fn call_for<T, E, F>(&self, account_id: AccountId, future: F) -> Result<T>
    where
        F: Future<Output = std::result::Result<T, E>> + Send + 'static,
        T: Send + 'static,
        E: Into<anyhow::Error> + Send + 'static,
    {
        let result = self.call(future).await;
        let Err(err) = result else {
            return result;
        };
        if !matches!(
            err.downcast_ref::<SyncError>(),
            Some(SyncError::Backend(BackendError::RateLimited(_)))
        ) {
            return Err(err);
        }
        let provider = self
            .db
            .read(move |c| accounts::account(c, account_id))
            .await
            .ok()
            .flatten()
            .map(|account| mailrs_discover::resolved_provider_name(account.provider_name()))
            .unwrap_or_default();
        if provider.is_empty() {
            return Err(err);
        }
        Err(err.context(SyncError::asked_to_wait(&provider)))
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

    /// Whether the account's rules, or some of them, run on this
    /// computer.
    pub fn rules_here(&self, account_id: AccountId) -> bool {
        self.account(account_id)
            .and_then(|sync| sync.services().rules.as_ref().map(|r| r.runs_here()))
            .unwrap_or(false)
    }

    /// Runs the account's local rules over its new Inbox mail.
    pub async fn run_rules(&self, account_id: AccountId) -> Result<mailrs_sync::rules::Ran> {
        let rules = Arc::clone(&self.rules);
        self.call(async move { rules.run_due(account_id).await })
            .await
    }

    /// Sends rule changes that waited for a ManageSieve server.
    pub async fn send_rule_changes(&self, account_id: AccountId) -> Result<mailrs_sync::SentRules> {
        let settings = Arc::clone(&self.gmail_settings);
        self.call(async move { settings.send_rule_changes(account_id).await })
            .await
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

    /// The account's calendars and the events on them, for what adds an
    /// event on the person's word, such as a calendar file.
    pub fn calendar(&self) -> Arc<mailrs_sync::Calendar<RunningEngine>> {
        Arc::clone(&self.calendar)
    }

    /// The messages waiting to go out: Send Later, and whatever could not
    /// be sent when it was written. See `mailrs_sync::Outbox`.
    pub fn outbox(&self) -> Arc<Waiting> {
        Arc::clone(&self.outbox)
    }

    /// The local copy of every account's calendars, kept fresh on a
    /// timer. See `mailrs_sync::calendar_copy::CalendarCopy`.
    pub fn calendar_copy(&self) -> Arc<CalendarCopy> {
        Arc::clone(&self.calendar_copy)
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

    /// Runs a mail action on the tokio runtime, on mail picked from
    /// `from`. See `MailActions::run_from`.
    pub async fn act(
        &self,
        targets: Vec<Target>,
        action: MailAction,
        history: History,
        from: MovedFrom,
    ) -> Outcome {
        let (actions, given) = (Arc::clone(&self.actions), targets.clone());
        let outcome = self
            .call(async move {
                let outcome = actions.run_from(&given, action, history, &from).await;
                Ok::<_, std::convert::Infallible>(outcome)
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
            recorded: None,
        })
    }

    /// Marks every unread message in each account's inbox read, on its
    /// server as well as here. See `MailActions::read_inboxes`.
    pub async fn read_inboxes(&self, accounts: Vec<AccountId>) -> InboxesRead {
        let actions = Arc::clone(&self.actions);
        self.call(async move {
            Ok::<_, std::convert::Infallible>(
                actions.read_inboxes(&accounts, History::Record).await,
            )
        })
        .await
        .unwrap_or_else(|err| {
            tracing::warn!(error = %err, "marking the inboxes read stopped");
            InboxesRead::default()
        })
    }

    /// Reverses the one action `recorded` names, wherever it sits on the
    /// undo stack. `None` when it is no longer there.
    pub async fn undo_this(&self, recorded: mailrs_sync::Recorded) -> Option<Undone> {
        let actions = Arc::clone(&self.actions);
        self.call(
            async move { Ok::<_, std::convert::Infallible>(actions.undo_this(recorded).await) },
        )
        .await
        .ok()
        .flatten()
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

    /// Tells sync whether the computer has a network, so the accounts wait
    /// while it is gone and check for mail as soon as it returns. The
    /// demo's accounts read a sample mailbox and ignore the network.
    pub fn set_network(&self, available: bool) {
        if self.demo {
            return;
        }
        self.network.set(available);
        if let Some(engine) = self.engine.current() {
            engine.set_network(available);
        }
    }

    /// Whether the computer has a network, as `set_network` last said.
    /// Always true in the demo, which never talks to a real network.
    pub fn network(&self) -> bool {
        self.network.get()
    }

    /// Tells sync whether the main window is open, so an account looks at
    /// mailboxes other than its inbox less often while only the tray runs.
    pub fn set_window_open(&self, open: bool) {
        self.window_open.set(open);
        if let Some(engine) = self.engine.current() {
            engine.set_window_open(open);
        }
    }

    /// Whether the main window is open, as `set_window_open` last said.
    pub fn window_open(&self) -> bool {
        self.window_open.get()
    }

    /// Drops the Gmail search the last folder or search listing kept, so
    /// the next listing asks Gmail again.
    pub fn forget_remote(&self) {
        self.lists.forget_remote();
    }

    /// Runs `browser`'s sign-in, keeps the account and its refresh token,
    /// and starts syncing it. `urls` receives the page to open. Each
    /// sign-in asks for every permission Penguin Mail uses; the provider
    /// keeps what the account already granted, so signing in again asks
    /// nothing new. Answers the account and the name the provider knows
    /// the person by. Dropping the sender behind `cancel` stops the wait
    /// for the browser, as Cancel on the waiting page does.
    pub async fn sign_in_in_browser(
        &self,
        browser: Browser,
        urls: async_channel::Sender<String>,
        wanted: Wanted,
        cancel: async_channel::Receiver<()>,
        chosen: Option<OwnClient>,
    ) -> Result<(Account, Option<String>)> {
        if self.demo {
            bail!(gettext("Demo mode cannot add real accounts."));
        }
        let engine = self
            .engine
            .current()
            .ok_or_else(|| anyhow!("sync is not running"))?;
        let mut connector = self.connector();
        connector.clients = connector.clients.with_chosen(chosen);
        let setup = connector.clients.chosen_google.is_some();
        let calendar_copy = self.calendar_copy();
        let db = self.db.clone();
        let signed = self
            .call(async move {
                let run = in_browser(&connector, browser, wanted, move |url: &str| {
                    let _ = urls.try_send(url.to_string());
                });
                let waited = tokio::select! {
                    waited = tokio::time::timeout(BROWSER_WAIT, run) => waited,
                    _ = cancel.recv() => bail!(gettext("Canceled.")),
                };
                let signed = waited.map_err(|_| BrowserTimedOut)??;
                match signed.services {
                    mailrs_sync::Connected::Ready(services) => {
                        engine.start_account(signed.account.id, services)
                    }
                    // A Gmail account whose mail route still lacks its secret
                    // waits for it, as at any start.
                    mailrs_sync::Connected::NeedsSignIn(_) => {
                        let id = signed.account.id;
                        db.write(move |c| accounts::set_state(c, id, AccountState::NeedsReauth))
                            .await?;
                    }
                }
                // The sign-in may have granted the calendar permission, so the
                // copy stops waiting out an earlier refusal.
                calendar_copy.permission_changed(signed.account.id);
                Ok::<_, anyhow::Error>((signed.account, signed.name))
            })
            .await?;
        if setup {
            self.finish_google_setup();
        }
        Ok(signed)
    }

    /// Signs in to a Gmail account with the app password `pasted` holds:
    /// Gmail's servers take the login first, then the account, its servers
    /// and its password are kept and the account starts. Nothing is kept
    /// when the login fails.
    pub async fn sign_in_with_app_password(
        &self,
        address: String,
        pasted: String,
    ) -> Result<Account> {
        if self.demo {
            bail!(gettext("Demo mode cannot add real accounts."));
        }
        let engine = self
            .engine
            .current()
            .ok_or_else(|| anyhow!("sync is not running"))?;
        let (db, secrets) = (self.db.clone(), self.secrets.clone());
        let reach = Reach {
            connector: self.connector(),
            demo: self.demo_mail.clone(),
        };
        let account = self
            .call(async move {
                let pasted = read_pasted(&pasted);
                let checked = check_gmail(&address, pasted.secret()).await?;
                let new = NewGmailImap {
                    address: address.trim().to_lowercase(),
                    servers: checked.servers,
                    password: pasted.secret().to_string(),
                };
                let account = gmail_imap_signed_in(&db, &secrets, new, now_millis()).await?;
                start_again(&reach, &engine, &db, &account, false).await?;
                Ok::<_, anyhow::Error>(account)
            })
            .await?;
        Ok(account)
    }

    /// The servers for `address`, found the way discovery goes: the
    /// provider table, then DNS, the domain's own files and a probe. The
    /// demo reads the table alone, so it sends nothing anywhere. Each
    /// question discovery sends out, and its answer, goes to `heard` for
    /// the lookup page.
    pub async fn discover(
        &self,
        address: String,
        heard: async_channel::Sender<Heard>,
    ) -> Result<(Found, Option<JmapFound>)> {
        let demo = self.demo;
        self.call(async move {
            // Both lookups share one watched network, so the lookup page
            // ticks off what either of them asks.
            let found = match demo {
                true => {
                    let net = Watched {
                        net: Offline,
                        heard,
                    };
                    tokio::join!(
                        mailrs_discover::find(&net, &address),
                        mailrs_discover::find_jmap(&net, &address)
                    )
                }
                false => {
                    let net = Watched {
                        net: RealNet::new()?,
                        heard,
                    };
                    tokio::join!(
                        mailrs_discover::find(&net, &address),
                        mailrs_discover::find_jmap(&net, &address)
                    )
                }
            };
            Ok::<_, anyhow::Error>(found)
        })
        .await
    }

    /// Signs in to an IMAP account: tries the login on both servers, then
    /// keeps the account, its servers and its password, and starts
    /// syncing it. Nothing is kept when the login fails. An account
    /// signing in again keeps its mail and starts over with the new
    /// password.
    pub async fn sign_in_imap(&self, attempt: Attempt) -> Result<Account> {
        if self.demo {
            bail!(gettext("Demo mode cannot add real accounts."));
        }
        let engine = self
            .engine
            .current()
            .ok_or_else(|| anyhow!("sync is not running"))?;
        let (db, passwords) = (self.db.clone(), Arc::clone(&self.secrets.passwords));
        let window_days = self.config.borrow().engine_config().window_days;
        self.call(async move {
            let Attempt {
                address,
                provider_name,
                incoming,
                smtp,
                incoming_login,
                smtp_login,
                password,
                smtp_password,
                ..
            } = attempt;
            // `check` reports which server refused, as a `CheckError` the
            // dialog reads back out of the `anyhow::Error`.
            let checked = mailrs_imap::check(
                &incoming,
                &smtp,
                &incoming_login,
                &smtp_login,
                &password,
                smtp_password.as_deref().unwrap_or(&password),
            )
            .await?;
            let new = NewImap {
                address,
                provider_name,
                servers: servers_for(&incoming, &checked.imap_user, &smtp, &checked.smtp_user),
                password,
                smtp_password,
            };
            let account = imap_signed_in(&db, Arc::clone(&passwords), new, now_millis()).await?;
            let services = connect_imap(&db, passwords, &account, window_days).await?;
            engine.start_account(account.id, services);
            Ok::<_, anyhow::Error>(account)
        })
        .await
    }

    /// Adds a calendar account: tries the login on the CalDAV server at
    /// `url`, then keeps the account, the server and the password, and
    /// starts it, so its calendars are read at the next tick. Nothing is
    /// kept when the login fails.
    pub async fn sign_in_calendar(
        &self,
        url: String,
        user_name: String,
        password: String,
    ) -> Result<Account> {
        let engine = self.calendar_engine()?;
        let (db, passwords, calendar_copy) = (
            self.db.clone(),
            Arc::clone(&self.secrets.passwords),
            self.calendar_copy(),
        );
        self.call(async move {
            let new = NewCalDav {
                url,
                user_name,
                password,
            };
            // A failure is a `CalDavSignInError`, which the dialog reads
            // back out of the `anyhow::Error` to word what went wrong.
            let account = caldav_signed_in(
                &finding::RealProbe,
                &db,
                Arc::clone(&passwords),
                new,
                now_millis(),
            )
            .await?;
            engine.start_account(account.id, connect_caldav(&db, passwords, &account).await?);
            // The same login added again signs its account in again, which
            // the copy reads at once rather than when it is next due.
            calendar_copy.permission_changed(account.id);
            Ok::<_, anyhow::Error>(account)
        })
        .await
    }

    /// Signs calendar account `account_id` in again, as its Sign In Again
    /// asks, and starts it over with the new password. It keeps its
    /// calendars; another server or user is refused.
    pub async fn sign_in_calendar_again(
        &self,
        account_id: AccountId,
        url: String,
        user_name: String,
        password: String,
    ) -> Result<Account> {
        let engine = self.calendar_engine()?;
        let (db, passwords, calendar_copy) = (
            self.db.clone(),
            Arc::clone(&self.secrets.passwords),
            self.calendar_copy(),
        );
        self.call(async move {
            let new = NewCalDav {
                url,
                user_name,
                password,
            };
            let account = caldav_signed_in_again(
                &finding::RealProbe,
                &db,
                Arc::clone(&passwords),
                account_id,
                new,
            )
            .await?;
            engine.start_account(account.id, connect_caldav(&db, passwords, &account).await?);
            // The copy stops waiting out the read the old password failed.
            calendar_copy.permission_changed(account.id);
            Ok::<_, anyhow::Error>(account)
        })
        .await
    }

    /// The engine a calendar sign-in starts its account in. The demo adds
    /// no real account.
    fn calendar_engine(&self) -> Result<Arc<Engine>> {
        if self.demo {
            bail!(gettext("Demo mode cannot add real accounts."));
        }
        self.engine
            .current()
            .ok_or_else(|| anyhow!("sync is not running"))
    }

    /// Signs in to a POP3 account: signs in to the POP3 server, which
    /// must answer UIDL, then to the SMTP server, then keeps the account,
    /// its servers, its removal setting and its password, and starts
    /// downloading. Nothing is kept when a sign-in fails; a failure comes
    /// back as a `CheckError` naming the server, as for IMAP.
    pub async fn sign_in_pop3(&self, attempt: Attempt) -> Result<Account> {
        if self.demo {
            bail!(gettext("Demo mode cannot add real accounts."));
        }
        let engine = self
            .engine
            .current()
            .ok_or_else(|| anyhow!("sync is not running"))?;
        let (db, passwords) = (self.db.clone(), Arc::clone(&self.secrets.passwords));
        self.call(async move {
            let Attempt {
                address,
                provider_name,
                incoming,
                smtp,
                incoming_login,
                smtp_login,
                password,
                smtp_password,
                pop3_remove,
                ..
            } = attempt;
            let pop3_user = check_pop3(&incoming, &incoming_login, &password).await?;
            let outgoing = smtp_password.as_deref().unwrap_or(&password);
            let smtp_user =
                mailrs_imap::check_smtp(&smtp, &pop3_user, &smtp_login, outgoing).await?;
            let new = NewPop3 {
                address,
                provider_name,
                servers: pop3_servers_for(&incoming, &pop3_user, &smtp, &smtp_user),
                remove: pop3_remove,
                password,
                smtp_password,
            };
            let account = pop3_signed_in(&db, Arc::clone(&passwords), new, now_millis()).await?;
            let services = connect_pop3(&db, passwords, &account).await?;
            engine.start_account(account.id, services);
            Ok::<_, anyhow::Error>(account)
        })
        .await
    }

    /// Where a JMAP well-known URL leads, followed without credentials,
    /// for a bare host typed in Server Settings. `None` when nothing
    /// answers in time; the demo asks nobody.
    pub async fn locate_jmap(&self, url: String) -> Option<String> {
        if self.demo {
            return None;
        }
        self.call(async move {
            let net = RealNet::new()?;
            let led = tokio::time::timeout(mailrs_discover::STEP_LIMIT, net.locate(&url)).await;
            Ok::<_, anyhow::Error>(led.ok().flatten())
        })
        .await
        .ok()
        .flatten()
    }

    /// Signs in to a JMAP account: checks the session with the password or
    /// the token, then keeps the account, its session row and its secret,
    /// and starts syncing it. Nothing is kept when the check fails; the
    /// failure comes back as a `JmapError` the dialog reads. The dialog
    /// has the person's yes for a session outside their domain before it
    /// calls this.
    pub async fn sign_in_jmap(&self, attempt: JmapAttempt) -> Result<Account> {
        if self.demo {
            bail!(gettext("Demo mode cannot add real accounts."));
        }
        let engine = self
            .engine
            .current()
            .ok_or_else(|| anyhow!("sync is not running"))?;
        let (db, secrets) = (self.db.clone(), self.secrets.clone());
        let window_days = self.config.borrow().engine_config().window_days;
        self.call(async move {
            let JmapAttempt {
                address,
                provider_name,
                session_url,
                token,
                user,
                secret,
                dav_password,
            } = attempt;
            let auth = match token {
                true => mailrs_jmap::Auth::Bearer(secret.clone()),
                false => mailrs_jmap::Auth::Basic {
                    user: user.clone(),
                    password: secret.clone(),
                },
            };
            let client = mailrs_jmap::JmapClient::new(&session_url, auth)?;
            // The account takes the address the person typed, whatever the
            // server names: Cyrus names none, and signing in again must
            // find the account it already is.
            let checked = mailrs_jmap::check(&client).await?;
            let session = &checked.session;
            let saved = mailrs_store::jmap::JmapSaved {
                session_url,
                auth: match token {
                    true => mailrs_store::jmap::JmapAuth::Bearer,
                    false => mailrs_store::jmap::JmapAuth::Basic,
                },
                user_name: (!token && !user.eq_ignore_ascii_case(&address)).then_some(user),
                labels: session.mailboxes_per_email().is_none_or(|most| most > 1),
                capabilities: session
                    .capabilities
                    .keys()
                    .filter(|c| session.offers(c))
                    .cloned()
                    .collect(),
                oauth: None,
                owner: None,
            };
            let new = NewJmap {
                address,
                provider_name,
                saved,
                secret,
                dav_password,
                signed_as: mailrs_sync::sign_in::SignedAs::of(&checked),
            };
            let account = jmap_signed_in(&db, &secrets, new, now_millis()).await?;
            let services = connect_jmap(&db, &secrets, &account, window_days).await?;
            engine.start_account(account.id, services);
            Ok::<_, anyhow::Error>(account)
        })
        .await
    }

    /// Whether the JMAP server behind `offer` takes a sign-in in the
    /// browser: its OAuth metadata reads and offers registration. The demo
    /// asks nobody.
    pub async fn jmap_browser_ready(&self, offer: JmapBrowser) -> bool {
        if self.demo {
            return false;
        }
        self.call(async move {
            Ok::<_, anyhow::Error>(mailrs_sync::sign_in::browser_ready(&offer).await)
        })
        .await
        .is_ok_and(|ready| ready != BrowserReady::NotOffered)
    }

    /// Signs in to a JMAP server on its own page in the browser, keeps the
    /// account with its refresh token, and starts its services afresh, so
    /// nothing keeps the session of an earlier grant. `urls` receives each
    /// page to open: a second one comes when the server forgot the client
    /// it had for the account. Dropping the sender behind `cancel` ends
    /// the wait for the browser. Once the browser came back, the sign-in
    /// runs to its end however the dialog moves, so an account is never
    /// left half kept. `dav_password` is the app password the person gave
    /// for a calendar server that refused the browser sign-in.
    pub async fn sign_in_jmap_browser(
        &self,
        offer: JmapBrowser,
        address: String,
        dav_password: Option<String>,
        urls: async_channel::Sender<String>,
        cancel: async_channel::Receiver<()>,
    ) -> Result<Account> {
        if self.demo {
            bail!(gettext("Demo mode cannot add real accounts."));
        }
        let engine = self
            .engine
            .current()
            .ok_or_else(|| anyhow!("sync is not running"))?;
        let connector = self.connector();
        self.call(async move {
            let open = move |url: &str| {
                let _ = urls.try_send(url.to_string());
            };
            let canceled = async move {
                let _ = cancel.recv().await;
            };
            let signed =
                jmap_in_browser(&connector, offer, &address, dav_password, open, canceled).await?;
            if let mailrs_sync::Connected::Ready(services) = signed.services {
                engine.start_account(signed.account.id, services);
            }
            Ok::<_, anyhow::Error>(signed.account)
        })
        .await
    }

    /// Looks for the account's calendar, contacts and Sieve servers
    /// (calendar and contacts alone for a POP3 account) and keeps what it
    /// finds. A JMAP account keeps none for what its session gives. One
    /// signed in through the browser with no app password takes its
    /// provider's servers from the table instead of looking; any other
    /// that keeps no password such a server takes looks for nothing. A
    /// new confirmed server restarts the account's services, so the
    /// calendar and contacts show without a restart. The demo asks nobody.
    pub async fn find_services(&self, account: Account) -> Result<ServicesFound> {
        if self.demo {
            return Ok(ServicesFound::default());
        }
        let (db, secrets, engine) = (self.db.clone(), self.secrets.clone(), self.engine.current());
        let connector = self.connector();
        let account_id = account.id;
        let found = self
            .call(async move {
                let session = match account.provider {
                    Provider::Jmap => {
                        db.read(move |c| mailrs_store::jmap::load(c, account_id))
                            .await?
                    }
                    _ => None,
                };
                let offers = |capability: &str| {
                    session
                        .as_ref()
                        .is_some_and(|saved| saved.offers(capability))
                };
                let offers_contacts = offers(mailrs_jmap::CONTACTS);
                let Some((host, user, password)) = dav_login(&db, &secrets, &account).await? else {
                    // An account signed in through the browser has no password
                    // to search with: it takes its provider's servers from the
                    // table, on the session's own site, and sends them its token.
                    let Some(saved) = session
                        .as_ref()
                        .filter(|saved| saved.auth == mailrs_store::jmap::JmapAuth::OAuth)
                    else {
                        return Ok(ServicesFound {
                            contacts: offers_contacts,
                            ..ServicesFound::default()
                        });
                    };
                    let (mut found, changed) =
                        finding::keep_oauth_table(&db, &account, &saved.session_url).await?;
                    if saved.offers(mailrs_jmap::CONTACTS) {
                        found.carddav = None;
                        found.carddav_missed = None;
                    }
                    if saved.offers(mailrs_jmap::CALENDARS) {
                        found.caldav = None;
                        found.caldav_missed = None;
                    }
                    if changed {
                        restart_services(&connector, engine, &account).await?;
                    }
                    return Ok(ServicesFound {
                        calendar: found.caldav.is_some(),
                        contacts: found.carddav.is_some() || offers_contacts,
                        rules_on_server: false,
                        calendar_missed: found.caldav_missed,
                        contacts_missed: found.carddav_missed,
                    });
                };
                let net = RealNet::new()?;
                let mut found = finding::find_services(
                    &net,
                    &finding::RealProbe,
                    &account.email,
                    account.provider_name(),
                    &host,
                    &user,
                    &password,
                )
                .await;
                // POP3 mail never reaches a server's rules, so a ManageSieve
                // host found beside a POP3 server is not kept.
                if account.provider == Provider::Pop3 {
                    found.sieve = None;
                }
                // Contacts and rules over JMAP win over a server found beside
                // it, so nothing comes twice.
                if offers_contacts {
                    found.carddav = None;
                    found.carddav_missed = None;
                }
                if offers(mailrs_jmap::SIEVE) {
                    found.sieve = None;
                }
                let changed = finding::keep_found(&db, account.id, &found).await?;
                if changed {
                    restart_services(&connector, engine, &account).await?;
                }
                Ok::<_, anyhow::Error>(ServicesFound {
                    calendar: found.caldav.is_some_and(|f| f.confirmed),
                    contacts: found.carddav.is_some_and(|f| f.confirmed) || offers_contacts,
                    rules_on_server: found.sieve.is_some(),
                    calendar_missed: found.caldav_missed,
                    contacts_missed: found.carddav_missed,
                })
            })
            .await?;
        let mut misses = self.misses.borrow_mut();
        for (kind, missed) in [
            (ServiceKind::CalDav, found.calendar_missed),
            (ServiceKind::CardDav, found.contacts_missed),
        ] {
            match missed {
                Some(miss) => misses.insert((account_id, kind), miss),
                None => misses.remove(&(account_id, kind)),
            };
        }
        Ok(found)
    }

    /// Why the last search found no server for `missing`, a calendar or
    /// contacts, on `account_id`.
    pub fn missed(&self, account_id: AccountId, missing: mailrs_sync::Missing) -> Option<Miss> {
        let kind = match missing {
            mailrs_sync::Missing::Calendar => ServiceKind::CalDav,
            mailrs_sync::Missing::Contacts => ServiceKind::CardDav,
            _ => return None,
        };
        self.misses.borrow().get(&(account_id, kind)).copied()
    }

    /// What was found for the account, for the Preferences rows.
    pub async fn services_found(&self, account_id: AccountId) -> Result<Vec<FoundService>> {
        let db = self.db.clone();
        self.call(async move {
            db.read(move |c| mailrs_store::services::load(c, account_id))
                .await
        })
        .await
    }

    /// Checks a CalDAV or CardDAV address the person typed and uses it.
    pub async fn use_typed_server(
        &self,
        account: Account,
        kind: ServiceKind,
        url: String,
    ) -> Result<()> {
        let (db, secrets, engine) = (self.db.clone(), self.secrets.clone(), self.engine.current());
        let connector = self.connector();
        self.call(async move {
            let Some((_, user, password)) = dav_login(&db, &secrets, &account).await? else {
                bail!(gettext(
                    "This account keeps no password a calendar or contacts server takes."
                ));
            };
            finding::use_typed(
                &finding::RealProbe,
                &db,
                account.id,
                kind,
                &url,
                &user,
                &password,
            )
            .await?;
            restart_services(&connector, engine, &account).await
        })
        .await
    }

    /// The person said yes to a server found outside their domain.
    pub async fn confirm_server(&self, account: Account, kind: ServiceKind) -> Result<()> {
        let (db, secrets, engine) = (self.db.clone(), self.secrets.clone(), self.engine.current());
        let connector = self.connector();
        self.call(async move {
            let Some((_, _, password)) = dav_login(&db, &secrets, &account).await? else {
                bail!(gettext(
                    "This account keeps no password a calendar or contacts server takes."
                ));
            };
            finding::confirm_found(&finding::RealProbe, &db, account.id, kind, &password).await?;
            restart_services(&connector, engine, &account).await
        })
        .await
    }

    /// Why the account's calendar and contacts servers last refused the
    /// login, for the Preferences rows.
    pub fn login_refusals(&self, account_id: AccountId) -> (Option<String>, Option<String>) {
        let Some(sync) = self.account(account_id) else {
            return (None, None);
        };
        let services = sync.services();
        (
            services.calendar.as_ref().and_then(|c| c.login_refused()),
            services.contacts.as_ref().and_then(|c| c.login_refused()),
        )
    }

    /// Stops syncing an account and deletes its local mail and what signs
    /// it in: a Google or Microsoft account's refresh token, an IMAP account's
    /// password.
    pub async fn remove_account(&self, account: Account) -> Result<()> {
        if let Some(engine) = self.engine.current() {
            engine.stop_account(account.id);
        }
        // Nothing is left to reverse the account's actions through, and
        // its mail goes with it.
        self.actions.forget_account(account.id);
        self.calendar_copy.forget(account.id);
        let (db, secrets, demo) = (self.db.clone(), self.secrets.clone(), self.demo);
        self.call(async move {
            let id = account.id;
            // Read while the account's row is still there.
            let grant = match demo {
                true => None,
                false => mailrs_sync::jmap_grant(&db, &account).await,
            };
            // A calendar sign-in writing this account's password finishes
            // first: once the row goes, the next account added may take
            // its id, and must not find this one's password under it.
            let _turn = mailrs_sync::sign_in::secrets_turn().await;
            db.write(move |c| accounts::delete_account(c, id)).await?;
            // Only now that the row is gone: retiring the session stops it
            // saving, and the token it answers is the one to revoke.
            let grant = match grant {
                Some(row) => row.retire(&secrets).await,
                None => None,
            };
            // The demo's secrets live in memory, so this never reaches the
            // person's keyring.
            secrets.forget(&account).await?;
            // On its own task: the removal never waits on the server.
            if let Some(grant) = grant {
                tokio::spawn(grant.revoke());
            }
            Ok::<_, anyhow::Error>(())
        })
        .await
    }
}

/// The IMAP accounts in `accounts` not looked at yet this run, each noted
/// in `looked`. Offline, only those the table answers for: discovery would
/// find nothing, and the account would never be looked at again.
fn to_look(accounts: Vec<Account>, looked: &mut HashSet<AccountId>, online: bool) -> Vec<Account> {
    accounts
        .into_iter()
        .filter(|a| a.provider == Provider::Imap && (online || table_offer(a).is_some()))
        .filter(|a| looked.insert(a.id))
        .collect()
}

/// The table's JMAP session for an IMAP account whose provider it names,
/// with no network at all.
fn table_offer(account: &Account) -> Option<JmapFound> {
    if account.provider != Provider::Imap {
        return None;
    }
    let name = mailrs_discover::resolved_provider_name(account.provider_name());
    let provider = mailrs_discover::jmap_named(&name)?;
    let session_url = mailrs_discover::jmap_session_named(&name)?;
    Some(JmapFound {
        session_url,
        source: mailrs_discover::Source::Table,
        confirm: false,
        provider: Some(provider),
    })
}

impl Core {
    /// The IMAP accounts whose server also speaks JMAP, as far as the
    /// looks so far found.
    pub fn switchable(&self) -> HashSet<AccountId> {
        self.switch_offers.borrow().keys().copied().collect()
    }

    /// The JMAP session `account` can switch to: the one a look found, or
    /// the table's before the look has run.
    pub fn offer_of(&self, account: &Account) -> Option<JmapFound> {
        self.switch_offers
            .borrow()
            .get(&account.id)
            .cloned()
            .or_else(|| table_offer(account))
    }

    /// Looks once a run for a JMAP session for each IMAP account: the
    /// table first, then discovery on the address's domain. Answers
    /// whether it found a new one. The demo asks nobody. A session outside
    /// the address's domain that the table does not name is no offer: the
    /// switch dialog has no place to ask whether the person trusts it.
    pub async fn look_for_jmap(&self, accounts: Vec<Account>) -> bool {
        let online = self.demo || self.network.get();
        let fresh = to_look(accounts, &mut self.switch_looked.borrow_mut(), online);
        let mut found_any = false;
        for account in fresh {
            let found = match table_offer(&account) {
                Some(found) => Some(found),
                None if self.demo => None,
                None => {
                    let email = account.email.clone();
                    let looked = self
                        .call(async move {
                            let net = RealNet::new()?;
                            Ok::<_, anyhow::Error>(mailrs_discover::find_jmap(&net, &email).await)
                        })
                        .await;
                    // A look that could not start says nothing about the
                    // server, so the account is looked at again later.
                    if looked.is_err() {
                        self.switch_looked.borrow_mut().remove(&account.id);
                    }
                    looked.ok().flatten().filter(|found| !found.confirm)
                }
            };
            if let Some(found) = found {
                self.switch_offers.borrow_mut().insert(account.id, found);
                found_any = true;
            }
        }
        found_any
    }

    /// Switches `account` to JMAP. Its outgoing mail waits and its loop
    /// stops while the switch runs; afterwards the account starts again on
    /// whatever the store names, JMAP or still IMAP, whether or not the
    /// switch went through.
    pub async fn switch_to_jmap(
        &self,
        account: Account,
        how: mailrs_sync::switch::SwitchSignIn,
    ) -> Result<mailrs_sync::switch::Switched> {
        let engine = self
            .engine
            .current()
            .ok_or_else(|| anyhow!("sync is not running"))?;
        let (db, secrets, outbox, demo) = (
            self.db.clone(),
            self.secrets.clone(),
            Arc::clone(&self.outbox),
            self.demo,
        );
        let reach = Reach {
            connector: self.connector(),
            demo: self.demo_mail.clone(),
        };
        let switched = self
            .call(async move {
                let id = account.id;
                // A send already under way finishes first; anything due
                // later waits for the account the switch leaves.
                let _held = outbox.hold(id).await;
                // Kept past the stop: it reads from the IMAP server the
                // drafts Send Later messages name.
                let imap = engine.account(id).ok();
                engine.stop_account(id);
                let switched = match demo {
                    true => Err(anyhow!(gettext("The demo cannot switch accounts."))),
                    false => mailrs_sync::switch::to_jmap(&db, &secrets, &account, imap.as_deref(), how)
                        .await
                        .map_err(anyhow::Error::from),
                };
                drop(imap);
                let now = match &switched {
                    Ok(switched) => Some(switched.account.clone()),
                    Err(_) => db.read(move |c| accounts::account(c, id)).await.ok().flatten(),
                };
                let moved = switched.is_ok();
                if let Err(err) = start_again(&reach, &engine, &db, &now.unwrap_or(account), moved).await {
                    tracing::warn!(account = id, error = %err, "could not start the account again after the switch");
                }
                switched
            })
            .await?;
        let id = switched.account.id;
        self.switch_offers.borrow_mut().remove(&id);
        // Undo would reverse an action through the IMAP ids the switch
        // replaced.
        self.actions.forget_account(id);
        Ok(switched)
    }
}

impl Core {
    /// Whether the calendar consent of `account` meets Google through the
    /// built-in client. False in the demo and when the store cannot say.
    pub async fn consent_through_built_in(&self, account: Account) -> bool {
        if self.demo {
            return false;
        }
        let connector = self.connector();
        self.call(async move {
            mailrs_sync::sign_in::consent_through_built_in(&connector, &account).await
        })
        .await
        .unwrap_or(false)
    }

    /// Asks Google, through the built-in client, for the calendar,
    /// contacts and Drive access of `account`, a Gmail account whose mail
    /// comes over IMAP, and starts the account again with them. Its mail
    /// route stays as it was. `urls` gets Google's page, and closing the
    /// channel behind `cancel` ends the wait and leaves the account as it
    /// was.
    pub async fn connect_calendar(
        &self,
        account: Account,
        urls: async_channel::Sender<String>,
        cancel: async_channel::Receiver<()>,
    ) -> Result<Account> {
        if self.demo {
            bail!(gettext("The demo cannot connect a calendar."));
        }
        let engine = self
            .engine
            .current()
            .ok_or_else(|| anyhow!("sync is not running"))?;
        let db = self.db.clone();
        let connector = self.connector();
        let reach = Reach {
            connector: self.connector(),
            demo: self.demo_mail.clone(),
        };
        self.call(async move {
            let open = move |url: &str| {
                let _ = urls.try_send(url.to_string());
            };
            let consent = calendar_consent(&connector, &account, open);
            let signed = tokio::select! {
                waited = tokio::time::timeout(BROWSER_WAIT, consent) => waited
                    .map_err(|_| BrowserTimedOut)??,
                _ = cancel.recv() => bail!(gettext("Canceled.")),
            };
            // The account's services read the new token once its loop starts again.
            engine.stop_account(signed.id);
            if let Err(err) = start_again(&reach, &engine, &db, &signed, false).await {
                tracing::warn!(account = signed.id, error = %err, "could not start the account again after its calendar consent");
            }
            Ok::<_, anyhow::Error>(signed)
        })
        .await
    }
}

impl Core {
    /// Signs in on the JMAP server's own page in the browser for Switch to
    /// JMAP, and hands back the grant; nothing is kept until the switch
    /// runs. The account goes on syncing over IMAP while the browser is
    /// open. `urls` and `cancel` work as for [`Core::sign_in_jmap_browser`].
    pub async fn switch_grant(
        &self,
        offer: JmapBrowser,
        address: String,
        urls: async_channel::Sender<String>,
        cancel: async_channel::Receiver<()>,
    ) -> Result<mailrs_sync::sign_in::BrowserGrant> {
        if self.demo {
            bail!(gettext("The demo cannot switch accounts."));
        }
        self.call(async move {
            let open = move |url: &str| {
                let _ = urls.try_send(url.to_string());
            };
            let canceled = async move {
                let _ = cancel.recv().await;
            };
            Ok::<_, anyhow::Error>(
                mailrs_sync::sign_in::browser_grant(offer, &address, open, canceled).await?,
            )
        })
        .await
    }
}

/// Nobody came back from the browser within [`BROWSER_WAIT`]. Its own type
/// lets the own-project setup tell a wait that ran out from other errors.
#[derive(Debug, thiserror::Error)]
#[error("{}", gettext("Gave up waiting for the browser after five minutes."))]
pub struct BrowserTimedOut;

/// Starts `account`'s loop on the services the store names for it now,
/// or marks it as needing a sign-in when nothing signs it in. Once a
/// switch went through, its local rules move into the server's Sieve
/// script first, where the server runs one and takes them.
async fn start_again(
    reach: &Reach,
    engine: &SyncEngine,
    db: &Db,
    account: &Account,
    switched: bool,
) -> Result<()> {
    match reach.connect(account).await? {
        Connected::Ready(mut services) => {
            if switched {
                mailrs_sync::switch::move_rules(&mut services).await;
            }
            engine.start_account(account.id, services);
        }
        Connected::NeedsSignIn => {
            let id = account.id;
            db.write(move |c| accounts::set_state(c, id, AccountState::NeedsReauth))
                .await?;
        }
    }
    Ok(())
}

/// What a search for an IMAP or POP3 account's other servers found in use.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ServicesFound {
    pub calendar: bool,
    pub contacts: bool,
    pub rules_on_server: bool,
    /// Why no calendar server was found, when none was.
    pub calendar_missed: Option<Miss>,
    /// Why no contacts server was found, when none was.
    pub contacts_missed: Option<Miss>,
}

impl Core {
    /// The outgoing password `account_id` keeps apart from its incoming
    /// one, for Server Settings to hold when the account signs in again,
    /// so signing in leaves it in place. `None` when it has none or the
    /// keyring would not say.
    pub async fn outgoing_password(&self, account_id: AccountId) -> Option<String> {
        let passwords = Arc::clone(&self.secrets.passwords);
        let read = self
            .call(async move {
                tokio::task::spawn_blocking(move || passwords.load_outgoing(account_id))
                    .await
                    .map_err(anyhow::Error::from)?
                    .map_err(anyhow::Error::from)
            })
            .await;
        read.unwrap_or_else(|err| {
            tracing::warn!(account = account_id, error = %err, "could not read the outgoing password");
            None
        })
    }
}

/// What a DAV or ManageSieve server takes for `account`: the host to look
/// beside, the user name and the password. An IMAP or POP3 account gives
/// its incoming server's host, the user name it took and its password. A
/// JMAP account looks beside its provider's IMAP host, or its session's,
/// and signs in with the app password kept for DAV, or its own password
/// over Basic. `None` when a JMAP account keeps no password such a server
/// would take.
pub(crate) async fn dav_login(
    db: &Db,
    secrets: &Secrets,
    account: &Account,
) -> anyhow::Result<Option<(String, String, String)>> {
    let id = account.id;
    if account.provider != Provider::Jmap {
        let saved = db
            .read(move |c| {
                Ok(match mailrs_store::servers::load(c, id)? {
                    Some(servers) => Some(servers.imap),
                    None => mailrs_store::servers::load_pop3(c, id)?.map(|servers| servers.pop3),
                })
            })
            .await?
            .ok_or_else(|| anyhow!("no servers kept"))?;
        let passwords = Arc::clone(&secrets.passwords);
        let password = tokio::task::spawn_blocking(move || passwords.load(id))
            .await??
            .ok_or_else(|| anyhow!("no password kept"))?;
        return Ok(Some((saved.host, saved.user_name, password)));
    }
    let saved = db
        .read(move |c| mailrs_store::jmap::load(c, id))
        .await?
        .ok_or_else(|| anyhow!("no session kept"))?;
    let host = mailrs_discover::jmap_named(account.provider_name())
        .and_then(|p| p.imap_host)
        .or_else(|| {
            url::Url::parse(&saved.session_url)
                .ok()
                .and_then(|u| u.host_str().map(str::to_string))
        })
        .unwrap_or_default();
    let user = saved
        .user_name
        .clone()
        .unwrap_or_else(|| account.email.clone());
    let (dav, jmap) = (Arc::clone(&secrets.passwords), Arc::clone(&secrets.jmap));
    let password = match tokio::task::spawn_blocking(move || dav.load(id)).await?? {
        Some(password) => Some(password),
        None if saved.auth == mailrs_store::jmap::JmapAuth::Basic => {
            tokio::task::spawn_blocking(move || jmap.load(id)).await??
        }
        None => None,
    };
    Ok(password.map(|password| (host, user, password)))
}

/// Builds the account's services again from the store and restarts its
/// loop on them, so a server found or typed serves at once.
async fn restart_services(
    connector: &Connector,
    engine: Option<Arc<SyncEngine>>,
    account: &Account,
) -> anyhow::Result<()> {
    let Some(engine) = engine else { return Ok(()) };
    match connector.connect(account).await? {
        mailrs_sync::Connected::Ready(services) => engine.start_account(account.id, services),
        mailrs_sync::Connected::NeedsSignIn(_) => {
            return Err(SyncError::Backend(BackendError::NeedsReauth).into());
        }
    }
    Ok(())
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

/// The desktop's Online Accounts. The demo holds one Google account of its
/// own, so the pages that use it can be seen without touching the person's.
fn online_accounts(demo: bool) -> Arc<dyn mailrs_sync::goa::OnlineAccounts> {
    if !demo {
        return mailrs_sync::goa::session();
    }
    let fake = mailrs_sync::goa::fake::FakeAccounts::new();
    fake.add(mailrs_sync::goa::GoaAccount {
        id: "account_1_0".into(),
        identity: "dana@example.com".into(),
        presentation: "dana@example.com".into(),
        mail_disabled: false,
        attention_needed: false,
    });
    fake
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

/// What connecting an account at startup needs: the store, the settings,
/// the build's clients and the secrets, or the demo's sample servers.
struct Reach {
    connector: Connector,
    demo: Option<Arc<DemoMail>>,
}

impl Reach {
    /// The account's services, or that it needs to sign in again. A
    /// missing token or password is recorded in the store by the connect
    /// that finds it.
    async fn connect(&self, account: &Account) -> Result<Connected<AccountServices>> {
        if let Some(demo) = self.demo.as_deref() {
            // `MAILRS_DEMO_STUCK_KEYRING` names an account whose start
            // waits on a keyring that never answers, to see what the
            // sidebar says then.
            if std::env::var("MAILRS_DEMO_STUCK_KEYRING")
                .is_ok_and(|id| id == account.id.to_string())
            {
                return Err(SyncError::NoAnswer(mailrs_sync::KEYRING).into());
            }
            // The demo's accounts talk to their sample servers and need no
            // client and no secret.
            return demo
                .services(account.id)
                .map(Connected::Ready)
                .ok_or_else(|| anyhow!("the demo has no mailbox for {}", account.email));
        }
        Ok(match self.connector.connect(account).await? {
            mailrs_sync::Connected::Ready(services) => Connected::Ready(services),
            mailrs_sync::Connected::NeedsSignIn(_) => Connected::NeedsSignIn,
        })
    }
}

/// Where starting an account lands in the app: the engine it was meant
/// for, the store, and the window through the change events.
struct EnginePort {
    engine: Arc<SyncEngine>,
    running: Arc<RunningEngine>,
    db: Db,
    events: async_channel::Sender<ChangeEvent>,
    started: async_channel::Sender<AccountId>,
}

impl EnginePort {
    /// Whether the engine this start was meant for still runs. Changing
    /// the sync settings replaces it, and the new one starts every
    /// account itself.
    fn current(&self) -> bool {
        self.running
            .current()
            .is_some_and(|now| Arc::ptr_eq(&now, &self.engine))
    }
}

impl Starting<AccountServices> for EnginePort {
    fn start(&self, account: AccountId, services: AccountServices) {
        if self.current() {
            self.engine.start_account(account, services);
            let _ = self.started.try_send(account);
        }
    }

    /// The engine never runs an account that did not start, so the store
    /// and the sidebar hear its state here.
    async fn report(&self, account_id: AccountId, state: AccountState) {
        let marked = self
            .db
            .write(move |c| accounts::set_state(c, account_id, state))
            .await;
        if let Err(err) = marked {
            tracing::warn!(account = account_id, %err, "could not record the account's state");
        }
        let _ = self
            .events
            .send(ChangeEvent::AccountStateChanged { account_id, state })
            .await;
    }

    async fn wanted(&self, account_id: AccountId) -> bool {
        self.current()
            && matches!(
                self.db
                    .read(move |c| accounts::account(c, account_id))
                    .await,
                Ok(Some(_))
            )
    }
}

/// A network that answers nothing, for the demo: discovery then finds
/// what the provider table knows and sends nothing anywhere.
struct Offline;

impl Net for Offline {
    async fn mx(&self, _domain: &str) -> Vec<String> {
        Vec::new()
    }

    async fn srv(&self, _name: &str) -> Vec<SrvRecord> {
        Vec::new()
    }

    async fn get(&self, _url: &str) -> Option<String> {
        None
    }

    async fn reaches(&self, _host: &str, _port: u16, _security: Security) -> bool {
        false
    }
}

/// How long a browser sign-in waits for the person to finish on the
/// provider's page. The waiting page counts it down.
pub const BROWSER_WAIT: std::time::Duration = std::time::Duration::from_secs(300);

/// A network that tells the lookup page each time discovery asks it
/// something and each time the answer comes back. The page reads nothing
/// else from it, so the answers themselves never leave discovery.
struct Watched<N> {
    net: N,
    heard: async_channel::Sender<Heard>,
}

impl<N: Net> Watched<N> {
    async fn asking<T>(
        &self,
        check: Check,
        ask: impl Future<Output = T>,
        found: impl Fn(&T) -> bool,
    ) -> T {
        let _ = self.heard.try_send(Heard::Asked(check));
        let answer = ask.await;
        let _ = self.heard.try_send(Heard::Answered(check, found(&answer)));
        answer
    }
}

impl<N: Net> Net for Watched<N> {
    async fn mx(&self, domain: &str) -> Vec<String> {
        self.asking(Check::Mx, self.net.mx(domain), |hosts| !hosts.is_empty())
            .await
    }

    async fn srv(&self, name: &str) -> Vec<SrvRecord> {
        self.asking(Check::Common, self.net.srv(name), |records| {
            !records.is_empty()
        })
        .await
    }

    async fn get(&self, url: &str) -> Option<String> {
        self.asking(check_of_url(url), self.net.get(url), Option::is_some)
            .await
    }

    async fn locate(&self, url: &str) -> Option<String> {
        self.asking(check_of_url(url), self.net.locate(url), Option::is_some)
            .await
    }

    async fn reaches(&self, host: &str, port: u16, security: Security) -> bool {
        self.asking(
            Check::Common,
            self.net.reaches(host, port, security),
            |ok| *ok,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex, PoisonError};

    use super::Core;
    use mailrs_domain::{GmailSignIn, Provider};
    use mailrs_store::accounts;
    use mailrs_sync::sign_in::ClientSource;
    use mailrs_sync::sign_in::own_client::OwnClient;

    /// Each `Core::open(true)` makes a demo folder of its own
    /// (`DemoFolder::make` numbers them), so the tests here share no file.
    /// Each core also starts a runtime and seeds a whole demo store, so the
    /// tests hold this for as long as a core lives and the run keeps one
    /// in memory at a time.
    static DEMO: Mutex<()> = Mutex::new(());

    #[test]
    fn an_account_call_keeps_its_provider_and_original_error() {
        let _demo = DEMO.lock().unwrap_or_else(PoisonError::into_inner);
        let core = Core::open(true).expect("the demo core opens");
        let account = running_fastmail(&core);
        let err = core
            .runtime
            .block_on(core.call_for(account.id, async {
                Err::<(), _>(mailrs_sync::SyncError::Backend(
                    mailrs_sync::BackendError::RateLimited(None),
                ))
            }))
            .unwrap_err();
        assert_eq!(err.to_string(), "Fastmail asked Penguin Mail to wait");
        assert!(matches!(
            err.downcast_ref::<mailrs_sync::SyncError>(),
            Some(mailrs_sync::SyncError::Backend(
                mailrs_sync::BackendError::RateLimited(_)
            ))
        ));
        let unknown = core
            .runtime
            .block_on(core.call_for(-1, async {
                Err::<(), _>(mailrs_sync::SyncError::Backend(
                    mailrs_sync::BackendError::RateLimited(None),
                ))
            }))
            .unwrap_err();
        assert_eq!(unknown.to_string(), "The server asked Penguin Mail to wait");
    }

    #[tokio::test]
    async fn an_account_without_a_calendar_password_skips_the_search() {
        let dir = tempfile::tempdir().unwrap();
        let db = mailrs_store::Db::open(&dir.path().join("mail.db")).unwrap();
        let secrets = mailrs_sync::passwords::Secrets::memory();
        let account = db
            .write(|c| {
                let id = mailrs_store::accounts::insert_jmap_account(
                    c,
                    "me@fastmail.com",
                    "Fastmail",
                    0,
                )?
                .unwrap();
                mailrs_store::jmap::save(
                    c,
                    id,
                    &mailrs_store::jmap::JmapSaved {
                        session_url: "https://api.fastmail.com/jmap/session".into(),
                        auth: mailrs_store::jmap::JmapAuth::Bearer,
                        user_name: None,
                        labels: true,
                        capabilities: vec![mailrs_jmap::MAIL.into()],
                        oauth: None,
                        owner: None,
                    },
                )?;
                Ok(mailrs_store::accounts::account(c, id)?.unwrap())
            })
            .await
            .unwrap();
        assert_eq!(
            super::dav_login(&db, &secrets, &account).await.unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn a_token_account_signs_in_to_dav_with_its_app_password_beside_the_imap_host() {
        use mailrs_sync::passwords::PasswordStore;

        let dir = tempfile::tempdir().unwrap();
        let db = mailrs_store::Db::open(&dir.path().join("mail.db")).unwrap();
        let secrets = mailrs_sync::passwords::Secrets::memory();
        let account = db
            .write(|c| {
                let id = mailrs_store::accounts::insert_jmap_account(
                    c,
                    "me@fastmail.com",
                    "Fastmail",
                    0,
                )?
                .unwrap();
                mailrs_store::jmap::save(
                    c,
                    id,
                    &mailrs_store::jmap::JmapSaved {
                        session_url: "https://api.fastmail.com/jmap/session".into(),
                        auth: mailrs_store::jmap::JmapAuth::Bearer,
                        user_name: None,
                        labels: true,
                        capabilities: vec![mailrs_jmap::MAIL.into()],
                        oauth: None,
                        owner: None,
                    },
                )?;
                Ok(mailrs_store::accounts::account(c, id)?.unwrap())
            })
            .await
            .unwrap();
        secrets.jmap.save(account.id, "token").unwrap();
        secrets.passwords.save(account.id, "app").unwrap();
        let login = super::dav_login(&db, &secrets, &account).await.unwrap();
        assert_eq!(
            login,
            Some((
                "imap.fastmail.com".into(),
                "me@fastmail.com".into(),
                "app".into()
            ))
        );
    }

    #[test]
    fn the_setup_step_is_kept_in_memory_in_the_demo_and_cleared_by_none() {
        let _demo = DEMO.lock().unwrap_or_else(PoisonError::into_inner);
        let core = Core::open(true).expect("the demo core opens");
        assert_eq!(core.google_setup_step(), None);
        core.set_google_setup_step(Some(3)).unwrap();
        assert_eq!(core.google_setup_step(), Some(3));
        core.finish_google_setup();
        assert_eq!(core.google_setup_step(), None);
    }

    #[test]
    fn a_sign_in_again_of_an_own_account_goes_through_its_kept_client() {
        let _demo = DEMO.lock().unwrap_or_else(PoisonError::into_inner);
        let core = Core::open(true).expect("the demo core opens");
        let accounts = core
            .runtime
            .block_on(core.db.read(mailrs_store::accounts::list_accounts))
            .expect("the demo lists its accounts");
        let gmail = accounts
            .into_iter()
            .find(|a| a.provider == Provider::Gmail)
            .expect("the demo has a Gmail account");
        let (id, email) = (gmail.id, gmail.email.clone());
        core.runtime
            .block_on(core.db.write(move |c| {
                accounts::set_gmail_sign_in(c, id, &GmailSignIn::OwnProject)?;
                Ok(())
            }))
            .unwrap();
        let source = |chosen: Option<&OwnClient>, address: Option<&str>| {
            core.runtime
                .block_on(core.google_sign_in_source(chosen, address.map(str::to_string)))
        };
        assert_eq!(source(None, Some(&email)), ClientSource::Kept);
        let setup = OwnClient::new("123-abc.apps.googleusercontent.com", "x");
        assert_eq!(source(Some(&setup), Some(&email)), ClientSource::Setup);
        assert_eq!(
            source(None, Some("nobody@gmail.example")),
            ClientSource::BuiltIn
        );
        assert_eq!(source(None, None), ClientSource::BuiltIn);
    }

    #[test]
    fn the_demo_keeps_no_google_project() {
        let _demo = DEMO.lock().unwrap_or_else(PoisonError::into_inner);
        let core = Core::open(true).expect("the demo core opens");
        assert!(core.runtime.block_on(core.saved_project()).is_none());
    }

    #[test]
    fn every_tool_run_shares_one_calendar() {
        let _demo = DEMO.lock().unwrap_or_else(PoisonError::into_inner);
        let core = Core::open(true).expect("the demo core opens");
        assert!(Arc::ptr_eq(
            &core.modules().calendar,
            &core.modules().calendar
        ));
    }

    /// A core built for the tray, as `--background` starts one, never
    /// told the window is open. A settings change restarts the engine and
    /// must not wake it back up.
    #[test]
    fn a_core_that_never_opened_a_window_starts_its_engine_closed() {
        let _demo = DEMO.lock().unwrap_or_else(PoisonError::into_inner);
        let core = Core::open(true).expect("the demo core opens");
        assert!(
            !core
                .engine
                .current()
                .expect("an engine is running")
                .window_open(),
            "nobody has shown a window yet"
        );

        core.update_sync(mailrs_sync::config::SyncConfig {
            poll_seconds: Some(60),
            ..Default::default()
        })
        .expect("the demo keeps new settings in memory");

        assert!(
            !core
                .engine
                .current()
                .expect("an engine is running")
                .window_open(),
            "restarting the engine for new settings must not open it"
        );
    }

    /// Once a window has shown, the state survives a settings restart, so
    /// the account does not fall back to the tray's slower pace while the
    /// window is still on screen.
    #[test]
    fn window_open_survives_a_settings_restart() {
        let _demo = DEMO.lock().unwrap_or_else(PoisonError::into_inner);
        let core = Core::open(true).expect("the demo core opens");
        core.set_window_open(true);

        core.update_sync(mailrs_sync::config::SyncConfig {
            poll_seconds: Some(60),
            ..Default::default()
        })
        .expect("the demo keeps new settings in memory");

        assert!(
            core.engine
                .current()
                .expect("an engine is running")
                .window_open(),
            "the new engine keeps the window open"
        );
    }

    /// Offline, discovery cannot answer, so an account that needs it is
    /// not marked as looked at, and the look runs once the network is back.
    #[test]
    fn an_account_looked_for_offline_is_looked_for_again_online() {
        let imap = |id, email: &str, name: Option<&str>| mailrs_domain::Account {
            id,
            email: email.into(),
            state: mailrs_domain::AccountState::Ok,
            provider: mailrs_domain::Provider::Imap,
            provider_name: name.map(str::to_string),
        };
        let accounts = vec![
            imap(1, "me@fastmail.com", Some("Fastmail")),
            imap(2, "me@example.org", None),
        ];
        let mut looked = std::collections::HashSet::new();
        let ids = |list: Vec<mailrs_domain::Account>| list.iter().map(|a| a.id).collect::<Vec<_>>();
        assert_eq!(
            ids(super::to_look(accounts.clone(), &mut looked, false)),
            [1],
            "the table needs no network"
        );
        assert_eq!(
            ids(super::to_look(accounts.clone(), &mut looked, true)),
            [2]
        );
        assert!(
            super::to_look(accounts, &mut looked, true).is_empty(),
            "once a run"
        );
    }

    #[test]
    fn fastmail_is_switchable_from_the_table_alone() {
        let account = mailrs_domain::Account {
            id: 1,
            email: "me@fastmail.com".into(),
            state: mailrs_domain::AccountState::Ok,
            provider: mailrs_domain::Provider::Imap,
            provider_name: Some("Fastmail".into()),
        };
        let found = super::table_offer(&account).expect("the table names Fastmail's session");
        assert_eq!(found.session_url, "https://api.fastmail.com/jmap/session");
        let gmail = mailrs_domain::Account {
            provider: mailrs_domain::Provider::Gmail,
            ..account
        };
        assert!(
            super::table_offer(&gmail).is_none(),
            "only an IMAP account switches"
        );
    }

    /// The demo's Fastmail account, once its loop runs.
    fn running_fastmail(core: &Core) -> mailrs_domain::Account {
        let accounts = core
            .runtime
            .block_on(core.db.read(mailrs_store::accounts::list_accounts))
            .expect("the demo lists its accounts");
        let fastmail = accounts
            .into_iter()
            .find(|a| a.email == "dana@fastmail.example")
            .expect("the demo has a Fastmail account");
        for _ in 0..200 {
            if core
                .engine
                .current()
                .is_some_and(|e| e.account(fastmail.id).is_ok())
            {
                return fastmail;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        panic!("the Fastmail account never started");
    }

    #[test]
    fn a_failed_switch_leaves_the_account_running_as_imap() {
        let _demo = DEMO.lock().unwrap_or_else(PoisonError::into_inner);
        let core = Core::open(true).expect("the demo core opens");
        let fastmail = running_fastmail(&core);
        let before = core
            .engine
            .current()
            .and_then(|e| e.account(fastmail.id).ok())
            .expect("it runs");
        let to = mailrs_sync::switch::SwitchTo {
            session_url: "https://api.fastmail.com/jmap/session".into(),
            token: true,
            user: fastmail.email.clone(),
            secret: "not a token".into(),
        };

        let switched = futures::executor::block_on(core.switch_to_jmap(
            fastmail.clone(),
            mailrs_sync::switch::SwitchSignIn::Typed(to),
        ));

        assert!(switched.is_err(), "the demo switches nothing");
        let after = core
            .engine
            .current()
            .and_then(|e| e.account(fastmail.id).ok())
            .expect("it runs again");
        assert!(
            !Arc::ptr_eq(&before, &after),
            "the switch stopped it and started it again"
        );
        let id = fastmail.id;
        let stored = core
            .runtime
            .block_on(
                core.db
                    .read(move |c| mailrs_store::accounts::account(c, id)),
            )
            .unwrap()
            .unwrap();
        assert_eq!(stored.provider, mailrs_domain::Provider::Imap);
    }
}
