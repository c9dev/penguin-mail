//! Application state that outlives any window: the core, the tray, the
//! content filter, identities, and the engine event loop.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use adw::prelude::*;
use gtk::{gio, glib};
use ksni::TrayMethods;
use mailrs_domain::{Account, AccountId, Address, ChangeEvent, system_label};
use mailrs_store::{messages, threads};

use crate::compose::Draft;
use crate::compose::Identity;
use crate::core::Core;
use crate::notify;
use crate::settings::{Change, ColorScheme, Effect, Effects, Settings};
use crate::tray::{MailTray, TrayCommand};
use crate::ui::autocomplete::Contacts;
use crate::ui::composer::{Composer, Remembered, Writing, spell};
use crate::ui::window::MainWindow;

const BLOCK_REMOTE_RULES: &str = r#"[
  {"trigger": {"url-filter": "^https?:"}, "action": {"type": "block"}},
  {"trigger": {"url-filter": "^wss?:"}, "action": {"type": "block"}},
  {"trigger": {"url-filter": "^ftp:"}, "action": {"type": "block"}}
]"#;

mod sending;

type AppAction = Box<dyn Fn(&Rc<App>)>;

pub struct App {
    pub gio: gio::Application,
    pub core: Rc<Core>,
    window: RefCell<Option<Rc<MainWindow>>>,
    filter: RefCell<Option<webkit::UserContentFilter>>,
    accounts: RefCell<Vec<Account>>,
    names: RefCell<HashMap<AccountId, String>>,
    tray: Arc<Mutex<Option<ksni::Handle<MailTray>>>>,
    open_requests: async_channel::Sender<(AccountId, String)>,
    skip_first_window: Cell<bool>,
    filter_requested: Cell<bool>,
    /// Main window plus open composers.
    open_windows: Cell<usize>,
    shed_generation: Cell<u64>,
    /// A message requested on the command line, opened on first activation.
    pending_compose: RefCell<Option<String>>,
    tray_started: Cell<bool>,
    settings: RefCell<Settings>,
    settings_path: std::path::PathBuf,
    /// People for recipient suggestions: the accounts' contacts, then the
    /// addresses mail turned up. Loading them reads every message, so the
    /// list reloads only after new mail arrives.
    contacts: Contacts,
    pub(crate) contacts_stale: Cell<bool>,
    /// Contact photos on disk, by lower-case address. Rows and the open
    /// conversation read it; it is filled whenever the suggestions load.
    photos: RefCell<HashMap<String, PathBuf>>,
    /// Messages waiting out the Undo Send delay.
    pending_sends: Cell<usize>,
    /// Hunspell dictionaries already read, by the languages they cover.
    /// Every composer shares them, because reading one is slow.
    dictionaries: RefCell<HashMap<Vec<String>, Rc<spell::Dictionaries>>>,
    /// Which languages a dictionary is installed for, read once.
    installed_dictionaries: RefCell<Option<Vec<String>>>,
    scheduler_running: Cell<bool>,
    _hold: gio::ApplicationHoldGuard,
}

impl App {
    pub fn new(
        gio_app: &gio::Application,
        core: Rc<Core>,
        background: bool,
        compose: Option<String>,
    ) -> Rc<App> {
        let (open_requests, opened) = async_channel::unbounded();
        let core_demo = core.demo;
        // Demo mode must not change the real preferences.
        let settings_path = if core.demo && std::env::var_os("MAILRS_SETTINGS").is_none() {
            std::env::temp_dir().join(format!(
                "penguin-mail-demo-{}-settings.toml",
                std::process::id()
            ))
        } else {
            Settings::default_path()
        };
        let app = Rc::new(App {
            gio: gio_app.clone(),
            core,
            window: RefCell::new(None),
            filter: RefCell::new(None),
            accounts: RefCell::new(Vec::new()),
            names: RefCell::new(HashMap::new()),
            tray: Arc::new(Mutex::new(None)),
            open_requests,
            skip_first_window: Cell::new(background),
            filter_requested: Cell::new(false),
            open_windows: Cell::new(0),
            shed_generation: Cell::new(0),
            pending_compose: RefCell::new(compose),
            tray_started: Cell::new(false),
            settings: RefCell::new(Settings {
                // The demo's contacts are already in its throwaway store,
                // so the switch shows what the mail on screen is using.
                contacts: core_demo,
                ..Settings::load(&settings_path)
            }),
            settings_path,
            contacts: Rc::new(RefCell::new(Rc::new(Vec::new()))),
            contacts_stale: Cell::new(true),
            photos: RefCell::new(HashMap::new()),
            pending_sends: Cell::new(0),
            dictionaries: RefCell::new(HashMap::new()),
            installed_dictionaries: RefCell::new(None),
            scheduler_running: Cell::new(false),
            _hold: gio_app.hold(),
        });
        app.install_actions();
        app.listen();
        app.listen_for_opens(opened);
        if !app.core.demo {
            app.watch_for_tray_host();
        }
        let weak = Rc::downgrade(&app);
        gio::NetworkMonitor::default().connect_network_available_notify(move |monitor| {
            if monitor.is_network_available()
                && let Some(app) = weak.upgrade()
            {
                app.core.poke_all();
            }
        });
        app.load_accounts();
        app.start_scheduler();
        app.watch_contacts();
        if !app.core.demo {
            crate::assistant::preload_keys();
        }
        app
    }

    /// GApplication's activate: the first one is skipped with `--background`.
    pub fn activate(self: &Rc<Self>) {
        if let Some(to) = self.pending_compose.borrow_mut().take() {
            self.skip_first_window.set(false);
            // Accounts load asynchronously; give them a moment first.
            let this = Rc::clone(self);
            glib::timeout_add_local_once(std::time::Duration::from_millis(600), move || {
                this.compose_to(&to)
            });
            return;
        }
        if self.skip_first_window.replace(false) {
            return;
        }
        self.show_window();
    }

    pub fn settings(&self) -> Settings {
        self.settings.borrow().clone()
    }

    /// Makes a named change, saves it, and applies its effects on screen.
    pub fn change_settings(self: &Rc<Self>, change: Change) -> Effects {
        let before = self.settings();
        let mut after = before.clone();
        let effects = change.apply(&mut after);
        self.commit_settings(&before, after, effects)
    }

    /// Changes preferences, saves them, and applies them to open windows.
    ///
    /// Prefer [`App::change_settings`]. This takes the changes that have no
    /// name yet, and works out their effects the same way.
    pub fn update_settings(self: &Rc<Self>, change: impl FnOnce(&mut Settings)) -> Effects {
        let before = self.settings();
        let mut after = before.clone();
        change(&mut after);
        let effects = Effects::between(&before, &after);
        self.commit_settings(&before, after, effects)
    }

    /// Saves the new preferences and hands their effects to the window.
    fn commit_settings(
        self: &Rc<Self>,
        before: &Settings,
        after: Settings,
        effects: Effects,
    ) -> Effects {
        if after == *before {
            return Effects::default();
        }
        if let Err(err) = after.save(&self.settings_path) {
            tracing::warn!(error = %err, "could not save preferences");
        }
        *self.settings.borrow_mut() = after;
        if effects.has(Effect::Theme) {
            self.apply_style();
        }
        if !effects.is_empty()
            && let Some(window) = self.window()
        {
            window.settings_changed(&effects);
        }
        effects
    }

    /// Follows the light or dark choice. Needs GTK, so it waits for a window.
    fn apply_style(&self) {
        if !gtk::is_initialized_main_thread() {
            return;
        }
        adw::StyleManager::default().set_color_scheme(match self.settings.borrow().color_scheme {
            ColorScheme::System => adw::ColorScheme::Default,
            ColorScheme::Light => adw::ColorScheme::ForceLight,
            ColorScheme::Dark => adw::ColorScheme::ForceDark,
        });
    }

    /// `draft` with the signature of the address it comes from. Gmail keeps
    /// one per send-as address, so a reply from an alias is signed as that
    /// alias.
    pub fn signed(&self, mut draft: Draft) -> Draft {
        let account = self.account_email(draft.account_id);
        let settings = self.settings.borrow();
        draft.markdown = crate::compose::with_signature(
            &draft.markdown,
            settings.signature_for(&account, &draft.from.email),
        );
        draft
    }

    /// Opens a composer, addressed to `to` unless it is empty.
    pub fn compose_to(self: &Rc<Self>, to: &str) {
        let preferred = self.settings.borrow().default_account.clone();
        let first = {
            let accounts = self.accounts.borrow();
            preferred
                .and_then(|email| {
                    accounts
                        .iter()
                        .find(|a| a.email.eq_ignore_ascii_case(&email))
                        .map(|a| a.id)
                })
                .or_else(|| accounts.first().map(|a| a.id))
        };
        let Some(account_id) = first else {
            self.show_window();
            return;
        };
        let mut draft = Draft::new(account_id, self.identity(account_id));
        draft.to = crate::compose::parse_recipients(to);
        let draft = self.signed(draft);
        self.compose(draft);
    }

    pub fn show_window(self: &Rc<Self>) -> Rc<MainWindow> {
        crate::ensure_gtk();
        self.apply_style();
        if let Some(window) = self.window.borrow().as_ref() {
            window.present();
            return Rc::clone(window);
        }
        // WebKit starts its graphics stack when first used, which costs
        // tens of megabytes; waiting for the first window keeps a
        // background-only process small.
        if self.filter.borrow().is_none() && !self.filter_requested.replace(true) {
            self.compile_filter();
        }
        let window = MainWindow::new(self);
        *self.window.borrow_mut() = Some(Rc::clone(&window));
        self.window_opened();
        window.present();
        window.run_demo_script();
        window
    }

    pub fn forget_window(self: &Rc<Self>, window: &Rc<MainWindow>) {
        let forgotten = {
            let mut slot = self.window.borrow_mut();
            let same = slot.as_ref().is_some_and(|w| Rc::ptr_eq(w, window));
            if same {
                *slot = None;
            }
            same
        };
        if forgotten {
            self.window_closed();
        }
    }

    fn window_opened(&self) {
        self.open_windows.set(self.open_windows.get() + 1);
        self.shed_generation.set(self.shed_generation.get() + 1);
    }

    /// Once no window has been open for a minute, restarts the process in
    /// the background. GTK, the graphics drivers, and WebKit cannot be
    /// unloaded, so this is how a closed window gives its memory back.
    fn window_closed(self: &Rc<Self>) {
        let open = self.open_windows.get().saturating_sub(1);
        self.open_windows.set(open);
        if open > 0 || self.core.demo {
            return;
        }
        let generation = self.shed_generation.get() + 1;
        self.shed_generation.set(generation);
        let delay = std::env::var("MAILRS_SHED_AFTER")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(60);
        self.shed_after(generation, delay);
    }

    fn shed_after(self: &Rc<Self>, generation: u64, seconds: u32) {
        let weak = Rc::downgrade(self);
        glib::timeout_add_seconds_local_once(seconds, move || {
            let Some(app) = weak.upgrade() else { return };
            if app.shed_generation.get() != generation || app.open_windows.get() > 0 {
                return;
            }
            if app.core.busy() || app.pending_sends.get() > 0 {
                app.shed_after(generation, 10);
                return;
            }
            let Ok(exe) = std::env::current_exe() else {
                return;
            };
            tracing::info!("no window for a while; restarting in the background to return memory");
            let err = std::process::Command::new(exe).arg("--background").exec();
            tracing::warn!(error = %err, "could not restart in the background; staying as is");
        });
    }

    fn window(&self) -> Option<Rc<MainWindow>> {
        self.window.borrow().clone()
    }

    pub fn filter(&self) -> Option<webkit::UserContentFilter> {
        self.filter.borrow().clone()
    }

    /// The account's own Gmail address.
    pub fn account_email(&self, account_id: AccountId) -> String {
        self.accounts
            .borrow()
            .iter()
            .find(|a| a.id == account_id)
            .map(|a| a.email.clone())
            .unwrap_or_default()
    }

    /// The address and display name mail from this account is sent as.
    pub fn identity(&self, account_id: AccountId) -> Address {
        Address {
            name: self.names.borrow().get(&account_id).cloned(),
            email: self.account_email(account_id),
        }
    }

    /// Every address every account may send from, accounts in sidebar order
    /// and each account's own address first.
    pub fn identities(&self) -> Vec<Identity> {
        let settings = self.settings.borrow();
        let accounts = self.accounts.borrow();
        let emails: Vec<&str> = accounts.iter().map(|a| a.email.as_str()).collect();
        let mut identities = Vec::new();
        for email in settings.ordered(&emails) {
            let Some(account) = accounts.iter().find(|a| a.email == email) else {
                continue;
            };
            for sender in settings.senders(email) {
                let name = sender.name.clone().or_else(|| {
                    // Gmail gives no name for an alias it has none for; the
                    // account's own name is the right stand-in.
                    sender
                        .email
                        .eq_ignore_ascii_case(email)
                        .then(|| self.names.borrow().get(&account.id).cloned())
                        .flatten()
                });
                identities.push(Identity {
                    account_id: account.id,
                    account_email: email.to_string(),
                    signature: settings.signature_for(email, &sender.email).to_string(),
                    address: Address {
                        name,
                        email: sender.email.clone(),
                    },
                    default: sender.default,
                });
            }
        }
        identities
    }

    /// The addresses one account sends as, for picking a reply's sender.
    pub fn my_addresses(&self, account_id: AccountId) -> Vec<Address> {
        self.identities()
            .into_iter()
            .filter(|i| i.account_id == account_id)
            .map(|i| i.address)
            .collect()
    }

    /// The dictionaries a composer for `account_id` should check against.
    ///
    /// Reading a Hunspell dictionary takes long enough to stutter a window,
    /// so this hands back a future: the composer opens straight away and the
    /// squiggles appear a moment later. Every composer that asks for the same
    /// languages gets the same dictionaries back.
    fn dictionaries(
        self: &Rc<Self>,
        account_id: AccountId,
    ) -> futures::future::LocalBoxFuture<'static, Rc<spell::Dictionaries>> {
        let account = self.account_email(account_id);
        let (languages, words) = {
            let settings = self.settings.borrow();
            let wanted = settings
                .spell_languages
                .get(&account.to_lowercase())
                .cloned()
                .unwrap_or_default();
            let installed = self.installed_dictionaries();
            (
                spell::languages_to_load(&wanted, &spell::locale_language(), &installed),
                settings.spell_words.clone(),
            )
        };
        if let Some(loaded) = self.dictionaries.borrow().get(&languages) {
            let loaded = Rc::clone(loaded);
            for word in &words {
                loaded.remember(word);
            }
            return Box::pin(async move { loaded });
        }
        let this = Rc::clone(self);
        Box::pin(async move {
            let key = languages.clone();
            let loaded = gio::spawn_blocking(move || spell::Dictionaries::load(&languages, &words))
                .await
                .map(Rc::new)
                .unwrap_or_else(|_| Rc::new(spell::Dictionaries::load(&[], &[])));
            this.dictionaries
                .borrow_mut()
                .insert(key, Rc::clone(&loaded));
            loaded
        })
    }

    /// Every language a dictionary is installed for. The list only changes
    /// when a package is installed, so it is read once.
    pub fn installed_dictionaries(&self) -> Vec<String> {
        let mut cache = self.installed_dictionaries.borrow_mut();
        cache.get_or_insert_with(spell::installed_languages).clone()
    }

    /// Asks Gmail which addresses each account may send as and keeps the
    /// answer. Composers open on what was stored last time, so this never
    /// holds a window up.
    fn refresh_send_as(self: &Rc<Self>) {
        for account in self.accounts.borrow().iter() {
            let Some(sync) = self.core.account(account.id) else {
                continue;
            };
            let (this, email) = (Rc::clone(self), account.email.clone());
            glib::spawn_future_local(async move {
                let Ok(addresses) = this.core.call(async move { sync.send_as().await }).await
                else {
                    return;
                };
                let addresses: Vec<crate::compose::SendAsAddress> = addresses
                    .into_iter()
                    .map(|a| crate::compose::SendAsAddress {
                        email: a.email,
                        name: a.name,
                        signature: a.signature,
                        default: a.default,
                    })
                    .collect();
                if addresses.is_empty() {
                    return;
                }
                this.change_settings(Change::SendAsAddresses {
                    account: email,
                    addresses,
                });
            });
        }
    }

    pub fn remember_accounts(self: &Rc<Self>, accounts: &[Account]) {
        *self.accounts.borrow_mut() = accounts.to_vec();
        for account in accounts {
            if self.names.borrow().contains_key(&account.id) {
                continue;
            }
            let Some(sync) = self.core.account(account.id) else {
                continue;
            };
            let (this, id) = (Rc::clone(self), account.id);
            glib::spawn_future_local(async move {
                if let Ok(Some(name)) = this
                    .core
                    .call(async move { sync.display_name().await })
                    .await
                {
                    this.names.borrow_mut().insert(id, name);
                }
            });
        }
        self.refresh_send_as();
        self.update_tray();
    }

    fn load_accounts(self: &Rc<Self>) {
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            if let Ok(accounts) = this.core.read(mailrs_store::accounts::list_accounts).await {
                // Give the engine a moment to connect before asking for display names.
                glib::timeout_future(std::time::Duration::from_millis(500)).await;
                this.remember_accounts(&accounts);
                this.refresh_contacts(false);
            }
        });
    }

    pub fn compose(self: &Rc<Self>, draft: Draft) -> Option<Rc<Composer>> {
        crate::ensure_gtk();
        self.apply_style();
        let identities = self.identities();
        if identities.is_empty() {
            return None;
        }
        self.reload_contacts();
        let writing = Writing {
            identities,
            last_used: self
                .settings
                .borrow()
                .last_sender
                .iter()
                .map(|(account, email)| (account.clone(), email.clone()))
                .collect(),
            dictionaries: self.dictionaries(draft.account_id),
            remember: {
                let app = Rc::downgrade(self);
                Rc::new(move |learned| {
                    let Some(app) = app.upgrade() else { return };
                    app.change_settings(match learned {
                        Remembered::SentFrom { account, email } => {
                            Change::LastSender { account, email }
                        }
                        Remembered::Word(word) => Change::KeepWord(word),
                    });
                })
            },
        };
        let this = Rc::downgrade(self);
        let format = self.settings.borrow().compose_format;
        let composer = Composer::open(
            Rc::clone(&self.core),
            writing,
            Rc::clone(&self.contacts),
            draft,
            format,
            move |draft, when| {
                if let Some(app) = this.upgrade() {
                    app.send(draft, when);
                }
            },
        );
        self.window_opened();
        let keep = Rc::clone(&composer);
        let app = Rc::downgrade(self);
        composer_window(&composer).connect_destroy(move |_| {
            let _ = &keep;
            if let Some(app) = app.upgrade() {
                app.window_closed();
            }
        });
        Some(composer)
    }

    /// Known correspondents, shared with composers and search.
    pub fn contacts(self: &Rc<Self>) -> Contacts {
        self.reload_contacts();
        Rc::clone(&self.contacts)
    }

    /// Refreshes the suggestions composers offer. Open composers see the
    /// new list once it loads.
    fn reload_contacts(self: &Rc<Self>) {
        if !self.contacts_stale.replace(false) {
            return;
        }
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let found = match this.core.read(mailrs_store::contacts::suggestions).await {
                Ok(found) => found,
                Err(err) => return tracing::warn!(error = %err, "could not load contacts"),
            };
            let dir = this.core.contacts().photo_dir().to_path_buf();
            *this.photos.borrow_mut() = found
                .iter()
                .filter_map(|person| {
                    let file = dir.join(person.photo_file.as_ref()?);
                    file.exists().then(|| (person.email.to_lowercase(), file))
                })
                .collect();
            *this.contacts.borrow_mut() = Rc::new(found);
            if let Some(window) = this.window() {
                window.contacts_loaded();
            }
        });
    }

    /// Every contact photo on disk, by lower-case address.
    pub fn photos(&self) -> HashMap<String, PathBuf> {
        self.photos.borrow().clone()
    }

    /// The photo of `email`, when a contact has one on this computer.
    pub fn photo(&self, email: &str) -> Option<PathBuf> {
        self.photos
            .borrow()
            .get(&email.trim().to_lowercase())
            .cloned()
    }

    /// The contact photos of `addresses`, as `data:` URIs by lower-case
    /// address. The conversation page loads nothing from disk or the
    /// network, so a photo travels inline or not at all.
    pub fn sender_photos(
        &self,
        addresses: impl Iterator<Item = String>,
    ) -> HashMap<String, String> {
        use base64::Engine;
        let mut found = HashMap::new();
        for address in addresses {
            let key = address.trim().to_lowercase();
            if key.is_empty() || found.contains_key(&key) {
                continue;
            }
            let Some(path) = self.photo(&key) else {
                continue;
            };
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let data = base64::engine::general_purpose::STANDARD.encode(&bytes);
            found.insert(key, format!("data:image/jpeg;base64,{data}"));
        }
        found
    }

    /// Turns contacts on or off. Turning them on reads each account's
    /// address book, which is when Google asks for the permission; turning
    /// them off deletes every contact and photo from this computer.
    pub fn set_contacts(self: &Rc<Self>, on: bool) {
        self.change_settings(Change::Contacts(on));
        if on {
            self.refresh_contacts(true);
            return;
        }
        let accounts: Vec<AccountId> = self.accounts.borrow().iter().map(|a| a.id).collect();
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let book = this.core.contacts();
            for account_id in accounts {
                let book = Arc::clone(&book);
                if let Err(err) = this
                    .core
                    .call(async move { book.forget(account_id).await })
                    .await
                {
                    tracing::warn!(error = %err, "could not delete the stored contacts");
                }
            }
            this.photos.borrow_mut().clear();
            this.contacts_stale.set(true);
            this.reload_contacts();
        });
    }

    /// Reads the accounts' Google contacts when the preference is on.
    /// An address book read within the last few hours costs nothing, so
    /// this is safe to call on a timer. With `ask`, a missing permission
    /// puts the Grant Access dialog on screen instead of a log line.
    pub fn refresh_contacts(self: &Rc<Self>, ask: bool) {
        if !self.settings.borrow().contacts {
            return;
        }
        let accounts: Vec<AccountId> = self.accounts.borrow().iter().map(|a| a.id).collect();
        if accounts.is_empty() {
            return;
        }
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let book = this.core.contacts();
            let now = mailrs_sync::now_millis();
            let read = {
                let accounts = accounts.clone();
                this.core
                    .call(async move { book.refresh_stale(&accounts, now).await })
                    .await
            };
            match read {
                Ok(mailrs_sync::Permitted::Done(refreshed)) => {
                    if refreshed.contacts == 0 && refreshed.photos == 0 {
                        return;
                    }
                    tracing::info!(
                        contacts = refreshed.contacts,
                        photos = refreshed.photos,
                        "read the address book"
                    );
                    this.contacts_stale.set(true);
                    this.reload_contacts();
                }
                Ok(mailrs_sync::Permitted::NeedsPermission) => {
                    if let (true, Some(window), Some(first)) =
                        (ask, this.window(), accounts.first().copied())
                    {
                        window.ask_for_contacts_access(first);
                    }
                }
                Err(err) => tracing::warn!(error = %err, "could not read the address book"),
            }
        });
    }

    /// Reads the address books at startup and every hour after that.
    /// `ContactBook` leaves the ones it read recently alone, so a tick
    /// with nothing to do costs one store read per account.
    fn watch_contacts(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        glib::timeout_add_seconds_local(60 * 60, move || match weak.upgrade() {
            Some(app) => {
                app.refresh_contacts(false);
                glib::ControlFlow::Continue
            }
            None => glib::ControlFlow::Break,
        });
    }

    /// Application actions, also reachable over D-Bus, for example:
    /// `gdbus call --session --dest dev.penguinmail.PenguinMail --object-path /dev/penguinmail/PenguinMail
    /// --method org.gtk.Actions.Activate hide-window [] {}`
    fn install_actions(self: &Rc<Self>) {
        let add = |name: &str, run: AppAction| {
            let action = gio::SimpleAction::new(name, None);
            let weak = Rc::downgrade(self);
            action.connect_activate(move |_, _| {
                if let Some(app) = weak.upgrade() {
                    run(&app);
                }
            });
            self.gio.add_action(&action);
        };
        add(
            "show-window",
            Box::new(|app| {
                app.show_window();
            }),
        );
        add(
            "hide-window",
            Box::new(|app| {
                if let Some(window) = app.window() {
                    window.window.close();
                }
            }),
        );
        add(
            "compose",
            Box::new(|app| {
                let first = app.accounts.borrow().first().map(|a| a.id);
                if let Some(account_id) = first {
                    app.compose(Draft::new(account_id, app.identity(account_id)));
                }
            }),
        );
        add("check", Box::new(|app| app.core.poke_all()));
        let compose_to = gio::SimpleAction::new("compose-to", Some(glib::VariantTy::STRING));
        let weak = Rc::downgrade(self);
        compose_to.connect_activate(move |_, parameter| {
            if let (Some(app), Some(to)) =
                (weak.upgrade(), parameter.and_then(|p| p.get::<String>()))
            {
                app.compose_to(&to);
            }
        });
        self.gio.add_action(&compose_to);
        add("quit", Box::new(|app| app.quit()));
    }

    /// Quits the whole process, tray included.
    pub fn quit(&self) {
        self.gio.quit();
    }

    fn compile_filter(self: &Rc<Self>) {
        let dir = glib::user_cache_dir()
            .join(mailrs_sync::config::DIR_NAME)
            .join("content-filters");
        let _ = std::fs::create_dir_all(&dir);
        let store = webkit::UserContentFilterStore::new(&dir.to_string_lossy());
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            match store
                .save_future(
                    "block-remote",
                    &glib::Bytes::from_static(BLOCK_REMOTE_RULES.as_bytes()),
                )
                .await
            {
                Ok(filter) => {
                    *this.filter.borrow_mut() = Some(filter.clone());
                    if let Some(window) = this.window() {
                        window.install_filter(filter);
                    }
                }
                Err(err) => {
                    tracing::error!(error = %err, "could not compile the remote content filter; the page policy still blocks remote loads")
                }
            }
        });
    }

    fn listen(self: &Rc<Self>) {
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            while let Ok(event) = this.core.events.recv().await {
                if let Some(window) = this.window() {
                    window.handle(&event);
                }
                match &event {
                    ChangeEvent::NewMail {
                        account_id,
                        message_ids,
                    } => {
                        this.contacts_stale.set(true);
                        this.announce(*account_id, message_ids.clone());
                    }
                    ChangeEvent::ThreadsChanged { .. }
                    | ChangeEvent::AccountStateChanged { .. } => this.update_tray(),
                    _ => {}
                }
            }
        });
    }

    fn listen_for_opens(self: &Rc<Self>, opened: async_channel::Receiver<(AccountId, String)>) {
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            while let Ok((account_id, thread_id)) = opened.recv().await {
                this.show_window().reveal(account_id, thread_id);
            }
        });
    }

    fn announce(self: &Rc<Self>, account_id: AccountId, message_ids: Vec<String>) {
        let settings = self.settings();
        if self.core.demo || !settings.notifications || self.window().is_some_and(|w| w.is_active())
        {
            return;
        }
        let previews = settings.notification_previews;
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let found = this
                .core
                .read(move |c| {
                    let mut found = Vec::new();
                    for id in &message_ids {
                        if let Some(thread) = messages::thread_id_of(c, account_id, id)? {
                            found.extend(
                                messages::thread_messages(c, account_id, &thread)?
                                    .into_iter()
                                    .filter(|m| &m.id == id),
                            );
                        }
                    }
                    Ok(found)
                })
                .await;
            let found = found.map(|found| {
                found
                    .into_iter()
                    .filter(|m| {
                        !settings.notify_vips_only
                            || m.from.as_ref().is_some_and(|a| settings.is_vip(&a.email))
                    })
                    .collect::<Vec<_>>()
            });
            if let Ok(found) = found
                && !found.is_empty()
            {
                notify::announce(found, previews, this.open_requests.clone());
            }
        });
    }

    /// Registers the tray icon whenever a tray host appears on the session
    /// bus. On Ubuntu that host is the AppIndicators extension, so enabling
    /// it later shows the icon without restarting Penguin Mail.
    fn watch_for_tray_host(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        // The watch lasts for the life of the process; the id is not needed.
        let _ = gio::bus_watch_name(
            gio::BusType::Session,
            "org.kde.StatusNotifierWatcher",
            gio::BusNameWatcherFlags::NONE,
            move |_, _, _| {
                if let Some(app) = weak.upgrade()
                    && !app.tray_started.replace(true)
                {
                    app.start_tray();
                }
            },
            |_, _| tracing::info!("the tray host went away; the icon returns when it does"),
        );
    }

    fn start_tray(self: &Rc<Self>) {
        let (commands, received) = async_channel::unbounded();
        let tray = MailTray {
            unread: 0,
            accounts: Vec::new(),
            commands,
        };
        let slot = Arc::clone(&self.tray);
        self.core.spawn(async move {
            match tray.spawn().await {
                Ok(handle) => *slot.lock().expect("tray slot poisoned") = Some(handle),
                Err(err) => tracing::warn!(error = %err, "could not add the tray icon"),
            }
        });
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            while let Ok(command) = received.recv().await {
                match command {
                    TrayCommand::Toggle => match this.window() {
                        Some(window) if window.is_active() => window.window.close(),
                        _ => {
                            this.show_window();
                        }
                    },
                    TrayCommand::Open => {
                        this.show_window();
                    }
                    TrayCommand::Compose => this.compose_to(""),
                    TrayCommand::Check => this.core.poke_all(),
                    TrayCommand::Quit => this.quit(),
                }
            }
        });
    }

    fn update_tray(self: &Rc<Self>) {
        let Some(handle) = self.tray.lock().expect("tray slot poisoned").clone() else {
            return;
        };
        let accounts = self.accounts.borrow().clone();
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let counts = this
                .core
                .read(move |c| {
                    let mut counts = Vec::new();
                    for account in accounts {
                        let unread = threads::unread_threads(
                            c,
                            &threads::ThreadFilter::account(account.id, system_label::INBOX),
                        )?;
                        counts.push((account.email, unread));
                    }
                    Ok(counts)
                })
                .await;
            let Ok(counts) = counts else { return };
            this.core.spawn(async move {
                handle
                    .update(move |tray: &mut MailTray| {
                        tray.unread = counts.iter().map(|(_, n)| n).sum();
                        tray.accounts = counts;
                    })
                    .await;
            });
        });
    }
}

fn composer_window(composer: &Rc<Composer>) -> adw::Window {
    composer.window()
}
