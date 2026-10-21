//! Application state that outlives any window: the core, the tray, the
//! content filter, identities, and the engine event loop.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use adw::prelude::*;
use gtk::{gio, glib};
use ksni::TrayMethods;
use mailrs_domain::translate::{fill, gettext};
use mailrs_domain::{Account, AccountId, Address, ChangeEvent, Label, Provider, Role};
use mailrs_store::{accounts, labels, messages};
use mailrs_sync::{History, MovedFrom};

use crate::app::announce::{announced, in_view, still_news};
use crate::compose::Identity;
use crate::core::Core;
use crate::notify;
use crate::permission::{Occasion, Permission};
use crate::settings::{Change, ColorScheme, Effect, Effects, Settings};
use crate::tray::{AccountUnread, MailTray, TrayCommand};
use crate::ui::autocomplete::Contacts;
use crate::ui::composer::spell;
use crate::ui::window::{MainWindow, Notice, Reveal};
use crate::unread::{Ask, Counted, Counter, Unread};
use mailrs_sync::mailbox::Counts;

const BLOCK_REMOTE_RULES: &str = r#"[
  {"trigger": {"url-filter": "^https?:"}, "action": {"type": "block"}},
  {"trigger": {"url-filter": "^wss?:"}, "action": {"type": "block"}},
  {"trigger": {"url-filter": "^ftp:"}, "action": {"type": "block"}}
]"#;

mod announce;
mod calendar_file;
mod composing;
mod dictionaries;
mod dock_badge;
mod hidden;
mod notifications;
mod reminders;
mod sending;
mod storage;
mod tray_feed;

pub use composing::Signature;
mod updates;

type AppAction = Box<dyn Fn(&Rc<App>)>;

pub struct App {
    pub gio: gio::Application,
    pub core: Rc<Core>,
    window: RefCell<Option<Rc<MainWindow>>>,
    filter: RefCell<Option<crate::web::Filter>>,
    /// The accounts in the store, in the order the store lists them, and
    /// each one's labels. The window's sidebar, list and dialogs all read
    /// this one copy.
    accounts: RefCell<Vec<Account>>,
    /// Whether `accounts` holds what the store does, rather than the empty
    /// list the app starts with.
    accounts_read: Cell<bool>,
    labels: RefCell<HashMap<AccountId, Vec<Label>>>,
    /// Where the person put each account's labels among their siblings.
    label_order: RefCell<HashMap<AccountId, HashMap<String, i64>>>,
    /// Each account's consent, read in the same pass as `accounts` so the
    /// Grant Access banner never waits on a round trip of its own.
    consent: RefCell<HashMap<AccountId, accounts::Consent>>,
    /// The JMAP accounts that signed in through the browser, as
    /// `reload_accounts` last read them, so signing in again says where.
    through_browser: RefCell<HashSet<AccountId>>,
    names: RefCell<HashMap<AccountId, String>>,
    tray: Arc<Mutex<Option<ksni::Handle<MailTray>>>>,
    /// The newest count for the tray, All Inboxes and each account's
    /// inbox, which the tray takes in order once it is up.
    tray_unread: tokio::sync::watch::Sender<(i64, Vec<AccountUnread>)>,
    /// What somebody picked on a new-mail notification.
    chosen: async_channel::Sender<notify::Request>,
    /// The new-mail notifications posted this run, by token, so a click
    /// acts only on mail this copy announced.
    issued: RefCell<notify::Issued>,
    notifications: notifications::Notifications,
    skip_first_window: Cell<bool>,
    filter_requested: Cell<bool>,
    /// Main window plus open composers.
    open_windows: Cell<usize>,
    shed_generation: Cell<u64>,
    /// A message requested on the command line, opened on first activation.
    pending_compose: RefCell<Option<String>>,
    /// A calendar file requested on the command line, opened on first
    /// activation.
    pending_file: RefCell<Option<std::path::PathBuf>>,
    tray_started: Cell<bool>,
    /// The unread counts the sidebar, the tray and the Dock badge share.
    unread: Rc<Unread<CoreCounts>>,
    dock_badge: dock_badge::Badge,
    /// Accounts whose waiting rule would replace a script of the person's
    /// own and were asked about it this run, so the minute timer asks once.
    asked_to_replace: RefCell<std::collections::HashSet<AccountId>>,
    settings: RefCell<Settings>,
    /// Writes each saved change off the main thread.
    settings_saver: crate::settings::Saver,
    /// Where an unreadable settings file went, until the first window says
    /// so.
    settings_broken: RefCell<Option<PathBuf>>,
    /// People for recipient suggestions: the accounts' contacts, then the
    /// addresses mail turned up. Loading them reads every message, so the
    /// list reloads only after new mail arrives.
    contacts: Contacts,
    pub(crate) contacts_stale: Cell<bool>,
    /// Contact photos, read from disk as rows and conversations ask. The
    /// files on disk are known whenever the suggestions load.
    photos: Rc<crate::photos::ContactPhotos>,
    /// Messages waiting out the Undo Send delay.
    pending_sends: Cell<usize>,
    /// Hunspell dictionaries already read, by the languages they cover.
    /// Every open composer shares them, because reading one is slow; they
    /// go when the last composer using them closes.
    dictionaries: dictionaries::Shared<spell::Dictionaries>,
    /// Which languages a dictionary is installed for, read once.
    installed_dictionaries: RefCell<Option<Vec<String>>>,
    scheduler_running: Cell<bool>,
    /// How full each account's server said it was, read hourly.
    storage: Rc<storage::ServerSpace<storage::CoreStorage>>,
    /// The timer for the next event reminder check. Each check replaces
    /// it with one set for the next reminder due, at most a minute away.
    reminder_wake: RefCell<Option<glib::SourceId>>,
    /// A reminder check is reading or posting; another waits for it.
    reminders_running: Cell<bool>,
    /// Finds and installs newer releases. None in the demo and in a cargo
    /// build, which never update.
    updater: Option<Rc<crate::update::Updater>>,
    _hold: gio::ApplicationHoldGuard,
}

impl App {
    pub fn new(
        gio_app: &gio::Application,
        core: Rc<Core>,
        background: bool,
        compose: Option<String>,
        calendar_file: Option<std::path::PathBuf>,
    ) -> Rc<App> {
        let (chosen, picked) = async_channel::unbounded();
        let core_demo = core.demo;
        // Demo mode must not change the real preferences.
        let demo_settings = core
            .demo_folder()
            .filter(|_| std::env::var_os("MAILRS_SETTINGS").is_none());
        let settings_path = if let Some(folder) = demo_settings {
            folder.join("settings.toml")
        } else {
            Settings::default_path()
        };
        let opened = Settings::open(&settings_path);
        let counted_by = Rc::clone(&core);
        let storage = storage::ServerSpace::new(storage::CoreStorage(Rc::clone(&core)));
        let app_id = gio_app
            .application_id()
            .map_or_else(|| crate::APP_ID.to_string(), |id| id.to_string());
        let app = Rc::new(App {
            gio: gio_app.clone(),
            core,
            window: RefCell::new(None),
            filter: RefCell::new(None),
            accounts: RefCell::new(Vec::new()),
            accounts_read: Cell::new(false),
            labels: RefCell::new(HashMap::new()),
            label_order: RefCell::new(HashMap::new()),
            consent: RefCell::new(HashMap::new()),
            through_browser: RefCell::default(),
            names: RefCell::new(HashMap::new()),
            tray: Arc::new(Mutex::new(None)),
            tray_unread: tokio::sync::watch::Sender::new((0, Vec::new())),
            chosen,
            issued: RefCell::default(),
            notifications: notifications::Notifications::new(gio_app, core_demo),
            skip_first_window: Cell::new(background),
            filter_requested: Cell::new(false),
            open_windows: Cell::new(0),
            shed_generation: Cell::new(0),
            pending_compose: RefCell::new(compose),
            pending_file: RefCell::new(calendar_file),
            tray_started: Cell::new(false),
            unread: Unread::new(CoreCounts(counted_by)),
            dock_badge: dock_badge::Badge::new(&app_id),
            asked_to_replace: RefCell::default(),
            settings: RefCell::new(Settings {
                // The demo's contacts are already in its throwaway store,
                // so the switch shows what the mail on screen is using.
                contacts: core_demo,
                ..opened.settings
            }),
            settings_saver: crate::settings::Saver::new(settings_path),
            settings_broken: RefCell::new(opened.broken),
            contacts: Rc::new(RefCell::new(Rc::new(Vec::new()))),
            contacts_stale: Cell::new(true),
            photos: Rc::new(crate::photos::ContactPhotos::new()),
            pending_sends: Cell::new(0),
            dictionaries: dictionaries::Shared::default(),
            installed_dictionaries: RefCell::new(None),
            scheduler_running: Cell::new(false),
            storage,
            reminder_wake: RefCell::new(None),
            reminders_running: Cell::new(false),
            updater: crate::update::Updater::for_this_copy(core_demo).map(Rc::new),
            _hold: gio_app.hold(),
        });
        crate::locale_time::set_week_start_setting(app.settings.borrow().week_start);
        app.install_actions();
        app.listen();
        app.count_unread();
        app.listen_for_notifications(picked);
        // The tray's Quit and a session logout can end the process
        // through the application without ever calling `App::quit`, so
        // this is the one signal every way out fires. `Holding::drain`
        // makes a second call, from the window's own close request, a
        // no-op.
        let weak = Rc::downgrade(&app);
        gio_app.connect_shutdown(move |_| {
            let Some(app) = weak.upgrade() else { return };
            if let Some(window) = app.window() {
                window.calendar.commit_all_now();
            }
            // A Linux dock keeps the last count it heard, so the badge
            // would outlive the app.
            app.dock_badge.hide_now();
        });
        // The tray is a D-Bus service of Linux desktops. macOS has no
        // session bus, and GIO answers a watch on it with a vanished
        // callback that holds no connection.
        if !app.core.demo && cfg!(target_os = "linux") {
            app.watch_for_tray_host();
        }
        let weak = Rc::downgrade(&app);
        let monitor = gio::NetworkMonitor::default();
        app.core.set_network(monitor.is_network_available());
        monitor.connect_network_available_notify(move |monitor| {
            let Some(app) = weak.upgrade() else { return };
            let available = monitor.is_network_available();
            // The engine pauses or wakes each account's loop itself.
            app.core.set_network(available);
            if let Some(window) = app.window() {
                window.calendar.network_changed();
                if available {
                    window.network_back();
                }
            }
            if available {
                // The folder on screen may have changed on Gmail while
                // the network was gone.
                app.core.forget_remote();
                // Whatever is waiting in the outbox has a widening
                // interval to sit out; the network coming back is better
                // news than any of it.
                app.wake_outbox();
                app.refresh_calendars();
            }
        });
        app.load_accounts();
        app.start_scheduler();
        app.start_storage_checks();
        app.start_event_reminders();
        app.watch_contacts();
        app.watch_calendars();
        app.watch_rules();
        app.start_update_checks();
        if !app.core.demo {
            crate::assistant::preload_keys();
            crate::assistant::sources::skills::shell::clear_leftovers();
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
        if let Some(path) = self.pending_file.borrow_mut().take() {
            self.skip_first_window.set(false);
            // Accounts load asynchronously; give them a moment first, so
            // the window knows which have a calendar.
            let (this, hold) = (Rc::clone(self), self.gio.hold());
            glib::timeout_add_local_once(std::time::Duration::from_millis(600), move || {
                this.open_calendar_file(&path);
                drop(hold);
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

    /// What `read` makes of the preferences, without copying them all.
    /// `read` must not change the settings.
    pub fn settings_with<R>(&self, read: impl FnOnce(&Settings) -> R) -> R {
        read(&self.settings.borrow())
    }

    /// Makes a named change, saves it, and applies its effects on screen.
    pub fn change_settings(self: &Rc<Self>, change: Change) -> Effects {
        let before = self.settings();
        let mut after = before.clone();
        let effects = change.apply(&mut after);
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
        self.settings_saver.save(&after);
        *self.settings.borrow_mut() = after;
        self.apply_effects(&effects);
        effects
    }

    /// Carries out what a saved change leaves to do: the app's own part
    /// here, and the window's part in [`MainWindow::settings_changed`].
    fn apply_effects(self: &Rc<Self>, effects: &Effects) {
        if effects.has(Effect::Theme) {
            self.apply_style();
        }
        if effects.has(Effect::Calendar) {
            crate::locale_time::set_week_start_setting(self.settings.borrow().week_start);
        }
        if !effects.is_empty() {
            self.tell_window(Notice::SettingsChanged(effects));
        }
        let books = effects.address_books();
        // Reading is when Google asks for the permission, so the account
        // just switched on gets its dialog.
        if books.read {
            self.refresh_contacts(true);
        }
        for email in &books.forget {
            self.forget_contacts(email);
        }
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

    /// Marks the unread mail in every account's inbox read, the tray's
    /// Mark All as Read. Each page asks the accounts' servers, so mail
    /// older than this computer keeps goes read too.
    fn read_all(self: &Rc<Self>) {
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let accounts: Vec<_> = this.mail_accounts().into_iter().map(|a| a.id).collect();
            let read = this.core.read_inboxes(accounts).await;
            if read.read > 0 {
                this.tell_window(Notice::InboxesRead);
            }
            for (account, error) in &read.failed {
                tracing::warn!(account, error, "could not mark all inbox mail read");
            }
            if let Some((_, reason)) = read.failed.first() {
                this.tell_window(Notice::Toast(fill(
                    &gettext("Could not mark all mail as read: {reason}"),
                    &[("reason", reason)],
                )));
            }
        });
    }

    /// Shows the window on `account_id`'s inbox, as the sidebar's Inbox row
    /// under that account does. A tray line can outlive its account, and
    /// then the window opens as it is.
    pub fn show_inbox_of(self: &Rc<Self>, account_id: AccountId) {
        let window = self.show_window();
        if self.accounts.borrow().iter().any(|a| a.id == account_id) {
            window.show_inbox_of(account_id);
        }
    }

    pub fn show_window(self: &Rc<Self>) -> Rc<MainWindow> {
        crate::ensure_gtk();
        self.apply_style();
        self.core.set_window_open(true);
        if let Some(window) = self.window.borrow().as_ref() {
            window.present();
            window.check_keyring_plug();
            return Rc::clone(window);
        }
        // WebKit starts its graphics stack when first used, which costs
        // tens of megabytes; waiting for the first window keeps a
        // background-only process small.
        if self.filter.borrow().is_none() && !self.filter_requested.replace(true) {
            self.compile_filter();
        }
        let window = MainWindow::new(self);
        if let Some(updater) = &self.updater {
            window.notice(Notice::Update(&updater.state()));
        }
        *self.window.borrow_mut() = Some(Rc::clone(&window));
        self.window_opened();
        window.present();
        window.run_demo_script();
        if let Some(aside) = self.settings_broken.borrow_mut().take() {
            let name = aside
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            window.notice(Notice::Toast(fill(
                &gettext(
                    "Your preferences could not be read, so Penguin Mail started from the defaults. The old file is kept as {file}.",
                ),
                &[("file", &name)],
            )));
        }
        window
    }

    /// What has to happen before this process ends or turns into another
    /// one: the MCP servers it started stop, since nothing else would stop
    /// a stdio server, and the last saved preferences reach the disk.
    pub(crate) fn before_leaving(&self) {
        crate::assistant::sources::mcp::registry().stop_all();
        self.settings_saver.flush();
    }

    /// Replaces this process with `command`, as the idle restart and an
    /// update do, and says why when that fails.
    pub(crate) fn exec_into(&self, mut command: std::process::Command) -> std::io::Error {
        self.before_leaving();
        command.exec()
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
    /// With no accounts there is nothing to keep running for, and the
    /// process ends instead.
    fn window_closed(self: &Rc<Self>) {
        let open = self.open_windows.get().saturating_sub(1);
        self.open_windows.set(open);
        if open == 0 {
            self.core.set_window_open(false);
        }
        // macOS brings a closed app back from the Dock, which takes the
        // Cocoa event loop GDK runs; a copy restarted in the background
        // runs none and could never be reopened.
        if open > 0 || cfg!(target_os = "macos") {
            return;
        }
        let accounts = self
            .accounts_read
            .get()
            .then(|| self.accounts.borrow().len());
        match after_last_window(self.core.demo, accounts) {
            AfterLastWindow::Stay => return,
            AfterLastWindow::Quit => {
                tracing::info!("the window closed with no accounts; quitting");
                self.quit();
                return;
            }
            AfterLastWindow::Shed => {}
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
            let Ok(exe) = crate::exe::launcher() else {
                return;
            };
            tracing::info!("no window for a while; restarting in the background to return memory");
            let mut command = std::process::Command::new(exe);
            command.arg("--background");
            let err = app.exec_into(command);
            tracing::warn!(error = %err, "could not restart in the background; staying as is");
        });
    }

    pub(crate) fn window(&self) -> Option<Rc<MainWindow>> {
        self.window.borrow().clone()
    }

    /// Passes news to the window, when one is open.
    pub(crate) fn tell_window(&self, notice: Notice<'_>) {
        if let Some(window) = self.window() {
            window.notice(notice);
        }
    }

    pub fn filter(&self) -> Option<crate::web::Filter> {
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
    fn identity(&self, account_id: AccountId) -> Address {
        self.identities()
            .into_iter()
            .find(|identity| identity.account_id == account_id)
            .map(|identity| identity.address)
            .unwrap_or_else(|| Address {
                name: None,
                email: self.account_email(account_id),
            })
    }

    /// Every address every account may send from, accounts in sidebar order
    /// and each account's own address first.
    pub fn identities(&self) -> Vec<Identity> {
        let settings = self.settings.borrow();
        // A calendar account sends nothing, so it has no place in From.
        let accounts = self.mail_accounts();
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
                    suffixes: sender.suffixes,
                    account_id: account.id,
                    account_email: email.to_string(),
                    signature: settings.signature_for(email, &sender.email).to_string(),
                    address: Address {
                        name,
                        email: sender.email.clone(),
                    },
                    default: sender.default,
                    masks: self
                        .core
                        .account(account.id)
                        .is_some_and(|sync| sync.services().offers().masked_email),
                });
            }
        }
        identities
    }

    /// The addresses one account sends as, for picking a reply's sender.
    pub fn my_addresses(
        &self,
        account_id: AccountId,
        original: &mailrs_domain::MessageMeta,
    ) -> Vec<Address> {
        let account = self.account_email(account_id);
        let mut senders = self.settings.borrow().senders(&account);
        for sender in &mut senders {
            if sender.name.is_none() && sender.email.eq_ignore_ascii_case(&account) {
                sender.name = self.names.borrow().get(&account_id).cloned();
            }
        }
        let mail = self
            .core
            .account(account_id)
            .map(|sync| sync.services().mail.clone());
        let masked = |email: &str| {
            mail.as_ref()
                .is_some_and(|mail| mail.is_masked(email) == Some(true))
        };
        crate::compose::reply_addresses(&senders, original, masked)
    }

    /// The dictionaries a composer for `account_id` should check against.
    ///
    /// Reading a Hunspell dictionary takes long enough to stutter a window,
    /// so this hands back a future: the composer opens straight away and the
    /// squiggles appear a moment later. Every composer that asks for the same
    /// languages while another still holds them gets the same dictionaries
    /// back.
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
        if let Some(loaded) = self.dictionaries.get(&languages) {
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
            this.dictionaries.keep(key, &loaded);
            loaded
        })
    }

    /// Every language a dictionary is installed for. The list only changes
    /// when a package is installed, so it is read once.
    pub fn installed_dictionaries(&self) -> Vec<String> {
        let mut cache = self.installed_dictionaries.borrow_mut();
        cache.get_or_insert_with(spell::installed_languages).clone()
    }

    /// Asks Gmail which addresses each of `accounts` may send as and keeps
    /// the answer, skipping an account asked within the day and one whose
    /// server keeps no send-as addresses. Composers open on what was
    /// stored last time, so this never holds a window up.
    fn refresh_send_as(self: &Rc<Self>, accounts: &[Account]) {
        let now = mailrs_sync::now_millis();
        for account in accounts {
            if !crate::offered::reads_send_as(account) {
                continue;
            }
            if !self.settings_with(|s| s.send_as_due(&account.email, now)) {
                continue;
            }
            if self.core.account(account.id).is_none() {
                continue;
            }
            let settings = self.core.gmail_settings();
            let (this, email, id) = (Rc::clone(self), account.email.clone(), account.id);
            glib::spawn_future_local(async move {
                let Ok(addresses) = this
                    .core
                    .call(async move { settings.send_as(id).await })
                    .await
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
                        ..Default::default()
                    })
                    .collect();
                if addresses.is_empty() {
                    return;
                }
                this.change_settings(Change::SendAsAddresses {
                    account: email.clone(),
                    addresses,
                    at: mailrs_sync::now_millis(),
                });
                if let Some(name) = this.settings_with(|s| s.display_name(&email)) {
                    this.names.borrow_mut().insert(id, name);
                }
            });
        }
    }

    /// Asks Gmail at once for the send-as addresses of an account that
    /// signed in again. Signing in again is how a person repairs an
    /// account, and an alias they added meanwhile would otherwise wait for
    /// the daily check. A new account is asked when it first loads.
    pub fn signed_in(self: &Rc<Self>, account: &Account) {
        if self.account(account.id).is_none() {
            return;
        }
        self.change_settings(Change::SignedInAgain {
            account: account.email.clone(),
        });
        self.refresh_send_as(std::slice::from_ref(account));
    }

    /// Every account in the order chosen in Preferences or the sidebar.
    pub fn accounts(&self) -> Vec<Account> {
        let mut accounts = self.accounts.borrow().clone();
        let settings = self.settings.borrow();
        accounts.sort_by_key(|account| settings.account_rank(&account.email));
        accounts
    }

    /// The accounts with mail in sidebar order, which leaves out a
    /// calendar account: what the mail window and the mail pages of
    /// Preferences list.
    pub fn mail_accounts(&self) -> Vec<Account> {
        mailrs_domain::mail_accounts(&self.accounts())
    }

    pub fn account(&self, account_id: AccountId) -> Option<Account> {
        self.accounts
            .borrow()
            .iter()
            .find(|a| a.id == account_id)
            .cloned()
    }

    /// Every account's labels.
    pub fn labels(&self) -> HashMap<AccountId, Vec<Label>> {
        self.labels.borrow().clone()
    }

    /// One account's labels, system labels included.
    pub fn labels_of(&self, account_id: AccountId) -> Vec<Label> {
        self.labels
            .borrow()
            .get(&account_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Where the person put each of the account's labels among its
    /// siblings, by label id, as `reload_accounts` last read it.
    pub fn label_order(&self, account_id: AccountId) -> HashMap<String, i64> {
        self.label_order
            .borrow()
            .get(&account_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Reads the accounts and their labels from the store and keeps them.
    /// Hands back each account with its labels, for the sidebar. Reads
    /// every account's consent in the same pass, so the Grant Access
    /// banner reads it from `consent` rather than a round trip of its
    /// own: a late one, arriving after the window looks settled, has
    /// shown up as a stray unnamed control to a screen reader already
    /// walking the page.
    pub async fn reload_accounts(self: &Rc<Self>) -> anyhow::Result<Vec<(Account, Vec<Label>)>> {
        let (mut loaded, order, consent, through_browser) = self
            .core
            .read(|c| {
                let mut out: Vec<(Account, Vec<Label>)> = Vec::new();
                let mut order = HashMap::new();
                let mut through_browser = HashSet::new();
                for account in accounts::list_accounts(c)? {
                    let account_labels = labels::list_labels(c, account.id)?;
                    order.insert(account.id, labels::positions(c, account.id)?);
                    if account.provider == Provider::Jmap
                        && mailrs_store::jmap::load(c, account.id)?
                            .is_some_and(|saved| saved.auth == mailrs_store::jmap::JmapAuth::OAuth)
                    {
                        through_browser.insert(account.id);
                    }
                    out.push((account, account_labels));
                }
                let consent = accounts::all_consent(c)?;
                Ok((out, order, consent, through_browser))
            })
            .await?;
        loaded.sort_by_key(|(account, _)| self.settings.borrow().account_rank(&account.email));
        *self.label_order.borrow_mut() = order;
        *self.through_browser.borrow_mut() = through_browser;
        let known: Vec<AccountId> = self.accounts.borrow().iter().map(|a| a.id).collect();
        *self.accounts.borrow_mut() = loaded.iter().map(|(a, _)| a.clone()).collect();
        // A calendar account has no inbox for the tray and the Dock to count.
        let with_mail = mailrs_domain::mail_accounts(&self.accounts.borrow());
        self.unread
            .set_accounts(with_mail.iter().map(|a| a.id).collect());
        self.storage
            .set_accounts(loaded.iter().map(|(a, _)| a.id).collect());
        self.accounts_read.set(true);
        *self.labels.borrow_mut() = loaded.iter().map(|(a, l)| (a.id, l.clone())).collect();
        *self.consent.borrow_mut() = consent;
        // A settings change that touches the accounts reloads them too, and
        // only an account this run has not seen needs Gmail asked about it.
        let arrived: Vec<Account> = loaded
            .iter()
            .map(|(a, _)| a.clone())
            .filter(|a| !known.contains(&a.id))
            .collect();
        self.remember_accounts(&arrived);
        Ok(loaded)
    }

    /// Whether `account_id` is a JMAP account that signed in through the
    /// browser, as `reload_accounts` last read it.
    pub fn signs_in_through_browser(&self, account_id: AccountId) -> bool {
        self.through_browser.borrow().contains(&account_id)
    }

    /// Every account's consent, as `reload_accounts` last read it: whether
    /// it was ever asked for every scope, and which scopes it granted.
    pub fn consent(&self) -> HashMap<AccountId, accounts::Consent> {
        self.consent.borrow().clone()
    }

    /// Finds a display name and the send-as addresses for each account
    /// that just arrived, and updates the tray. The name comes from the
    /// send-as addresses Preferences keeps when they hold one, so a restart
    /// asks Gmail only for what it has never told this computer.
    fn remember_accounts(self: &Rc<Self>, arrived: &[Account]) {
        // The name is the one mail goes out under, and a calendar account
        // sends none.
        for account in &mailrs_domain::mail_accounts(arrived) {
            if self.names.borrow().contains_key(&account.id) {
                continue;
            }
            if let Some(name) = self.settings_with(|s| s.display_name(&account.email)) {
                self.names.borrow_mut().insert(account.id, name);
                continue;
            }
            if self.core.account(account.id).is_none() {
                continue;
            }
            let settings = self.core.gmail_settings();
            let (this, id) = (Rc::clone(self), account.id);
            glib::spawn_future_local(async move {
                if let Ok(Some(name)) = this
                    .core
                    .call(async move { settings.display_name(id).await })
                    .await
                {
                    this.names.borrow_mut().insert(id, name);
                }
            });
        }
        self.refresh_send_as(arrived);
        self.list_masked(arrived);
    }

    /// Lists the masked addresses of each account in `arrived` whose server
    /// makes them, so a reply to mail sent to one goes out from it. The
    /// account's adapter keeps the list. Until it has one, or when the
    /// listing fails, a reply comes from the account's own address.
    fn list_masked(self: &Rc<Self>, arrived: &[Account]) {
        for account in arrived {
            let Some(sync) = self.core.account(account.id) else {
                continue;
            };
            if !sync.services().offers().masked_email {
                continue;
            }
            let (core, settings, id) = (
                Rc::clone(&self.core),
                self.core.gmail_settings(),
                account.id,
            );
            glib::spawn_future_local(async move {
                if core
                    .call(async move { settings.masked_addresses(id).await })
                    .await
                    .is_err()
                {
                    tracing::warn!(account = id, "could not list the masked addresses");
                }
            });
        }
    }

    fn load_accounts(self: &Rc<Self>) {
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            // Give the engine a moment to connect before asking for display names.
            glib::timeout_future(std::time::Duration::from_millis(500)).await;
            if let Ok(loaded) = this.reload_accounts().await {
                // Contacts were one switch for every account before, which
                // only ever asked the first account for its permission. This
                // turns that into each account's own, once, and asks every
                // account that still lacks it.
                // The fold reads every address book as an effect of the
                // change, asking for the permission where it is missing.
                if this.settings.borrow().contacts {
                    let emails = loaded.iter().map(|(a, _)| a.email.clone()).collect();
                    this.change_settings(Change::AllContacts(emails));
                } else {
                    this.refresh_contacts(false);
                }
            }
        });
    }

    /// Known correspondents, shared with composers and search.
    pub fn contacts(self: &Rc<Self>) -> Contacts {
        self.reload_contacts();
        Rc::clone(&self.contacts)
    }

    /// Loads the suggestions again after the person added contacts.
    pub fn contacts_added(self: &Rc<Self>) {
        self.contacts_stale.set(true);
        self.reload_contacts();
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
            let files: Vec<(String, String)> = found
                .iter()
                .filter_map(|p| Some((p.email.clone(), p.photo_file.clone()?)))
                .collect();
            let on_disk = gio::spawn_blocking(move || crate::photos::on_disk(dir, files))
                .await
                .unwrap_or_default();
            this.photos.set_files(on_disk);
            *this.contacts.borrow_mut() = Rc::new(found);
            this.tell_window(Notice::ContactsLoaded);
        });
    }

    /// The contact photos rows and conversations show.
    pub fn photos(&self) -> Rc<crate::photos::ContactPhotos> {
        Rc::clone(&self.photos)
    }

    /// The photo of `email`, when a contact has one on this computer.
    pub fn photo(&self, email: &str) -> Option<PathBuf> {
        self.photos.path(email)
    }

    /// The contact photos of `addresses` already in memory, as `data:`
    /// URIs by lower-case address. The conversation page loads nothing
    /// from disk or the network, so a photo travels inline or not at all.
    /// The rest are read off the main thread, and the window hears when
    /// they arrive.
    pub fn sender_photos(
        &self,
        addresses: impl Iterator<Item = String>,
    ) -> HashMap<String, Rc<str>> {
        addresses
            .filter_map(|address| {
                let uri = self.photos.uri(&address)?;
                Some((address.trim().to_lowercase(), uri))
            })
            .collect()
    }

    /// Deletes one account's contacts and photos from this computer and
    /// leaves the other accounts' alone. Turning its contacts off does this.
    fn forget_contacts(self: &Rc<Self>, email: &str) {
        let Some(account_id) = self
            .accounts
            .borrow()
            .iter()
            .find(|a| a.email.eq_ignore_ascii_case(email))
            .map(|a| a.id)
        else {
            return;
        };
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let book = this.core.contacts();
            if let Err(err) = this
                .core
                .call(async move { book.forget(account_id).await })
                .await
            {
                tracing::warn!(error = %err, "could not delete the stored contacts");
            }
            this.photos.set_files(HashMap::new());
            this.contacts_stale.set(true);
            this.reload_contacts();
        });
    }

    /// Reads the accounts' Google contacts when the preference is on.
    /// An address book read within the last few hours costs nothing, so
    /// this is safe to call on a timer. With `ask`, a missing permission
    /// puts the Grant Access dialog on screen instead of a log line.
    pub fn refresh_contacts(self: &Rc<Self>, ask: bool) {
        let accounts: Vec<AccountId> = {
            let settings = self.settings.borrow();
            self.accounts
                .borrow()
                .iter()
                .filter(|a| settings.reads_contacts(&a.email))
                .map(|a| a.id)
                .collect()
        };
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
                Ok(refreshed) => {
                    // Each account Google refused asks for itself, so the
                    // person grants the one they just switched on.
                    if let (true, Some(window)) = (ask, this.window()) {
                        for account_id in &refreshed.needs_permission {
                            window.ask_permission(
                                *account_id,
                                Permission::Contacts,
                                Occasion::Needed,
                            );
                        }
                    }
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
                Err(err) => {
                    // A People API switched off in the Google Cloud project
                    // refuses before Google can ask for the permission, so
                    // the person has to hear what to turn on.
                    if let (true, Some((service, url))) = (ask, api_off(&err)) {
                        this.tell_window(Notice::ApiOff {
                            service: &service,
                            enable_url: &url,
                        });
                    }
                    tracing::warn!(error = %err, "could not read the address book");
                }
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

    /// Sends one account's queued calendar changes now rather than at
    /// the next tick, then reloads the calendar so pending marks clear.
    /// Offline, the queue waits for the network to return.
    pub fn push_calendar(self: &Rc<Self>, account_id: AccountId) {
        if !self.core.network() {
            return;
        }
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let copy = this.core.calendar_copy();
            let sent = this
                .core
                .call(async move { copy.send(account_id).await })
                .await;
            let Some(window) = this.window() else { return };
            match sent {
                Ok(turned_down) => {
                    window.calendar_refreshed(&mailrs_sync::calendar_copy::Refreshed {
                        turned_down,
                        ..mailrs_sync::calendar_copy::Refreshed::default()
                    })
                }
                Err(err) => tracing::info!(%err, "calendar changes wait for the next try"),
            }
        });
    }

    /// Keeps the calendar copy fresh. Runs on a short timer and on a
    /// Refresh press alike; the copy reads an account only when its
    /// minute (window open) or five minutes (tray only) are up, so most
    /// ticks cost nothing. Nothing runs while the network is gone, and
    /// an open calendar's offline line and Refresh debounce follow
    /// whatever this pass finds.
    pub fn refresh_calendars(self: &Rc<Self>) {
        if !self.core.network() {
            if let Some(window) = self.window() {
                window.calendar.refresh_done();
                window.calendar.network_changed();
            }
            return;
        }
        let accounts: Vec<AccountId> = self.accounts.borrow().iter().map(|a| a.id).collect();
        if accounts.is_empty() {
            if let Some(window) = self.window() {
                window.calendar.refresh_done();
            }
            return;
        }
        let window_open = self.core.window_open();
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let copy = this.core.calendar_copy();
            let now = mailrs_sync::now_millis();
            let read = this
                .core
                .call(async move { copy.refresh_due(&accounts, now, window_open).await })
                .await;
            if let Some(window) = this.window() {
                window.calendar.refresh_done();
                window.calendar.synced(read.is_ok());
            }
            match read {
                Ok(refreshed) => {
                    for turned_down in &refreshed.turned_down {
                        tracing::info!(event = %turned_down.event, "a calendar change was turned down");
                    }
                    if !refreshed.needs_permission.is_empty() {
                        tracing::info!(
                            accounts = ?refreshed.needs_permission,
                            "calendar permission missing"
                        );
                    }
                    if let Some(window) = this.window() {
                        window.calendar_refreshed(&refreshed);
                        if refreshed.events > 0 {
                            window.calendar_read_for_threads();
                        }
                    }
                    // A change read from Google may bring a reminder
                    // closer than the next minute's check.
                    if refreshed.events > 0 {
                        this.check_event_reminders();
                    }
                }
                Err(err) => tracing::warn!(%err, "could not read the calendars"),
            }
        });
    }

    /// Ticks the calendar copy every 15 seconds; `CalendarCopy` itself
    /// decides which accounts are actually due, at the window-open or
    /// tray-only cadence.
    fn watch_calendars(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        glib::timeout_add_seconds_local(15, move || match weak.upgrade() {
            Some(app) => {
                app.refresh_calendars();
                glib::ControlFlow::Continue
            }
            None => glib::ControlFlow::Break,
        });
        self.refresh_calendars();
    }

    /// Application actions, also reachable over D-Bus, for example:
    /// `gdbus call --session --dest io.github.c9dev.PenguinMail --object-path /io/github/c9dev/PenguinMail
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
        add("compose", Box::new(|app| app.compose_to("")));
        add("check", Box::new(|app| app.core.poke_all()));
        // GTK's native macOS menu names these application actions, even
        // when the main window is closed or a composer has the focus.
        #[cfg(target_os = "macos")]
        for name in ["preferences", "about"] {
            add(
                name,
                Box::new(move |app| {
                    let window = app.show_window();
                    let _ = window.window.activate_action(&format!("win.{name}"), None);
                }),
            );
        }
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
        // A byte string, so a file name that is not UTF-8 arrives whole.
        let open_file =
            gio::SimpleAction::new("open-calendar-file", Some(glib::VariantTy::BYTE_STRING));
        let weak = Rc::downgrade(self);
        open_file.connect_activate(move |_, parameter| {
            if let (Some(app), Some(path)) =
                (weak.upgrade(), parameter.and_then(crate::path_from_variant))
            {
                app.open_calendar_file(&path);
            }
        });
        self.gio.add_action(&open_file);
        add("quit", Box::new(|app| app.quit()));
    }

    /// Quits the whole process, tray included.
    pub fn quit(&self) {
        self.gio.quit();
    }

    /// Starts Penguin Mail again the way it was started, and quits this
    /// copy. The Language preference needs it: GTK and gettext both read
    /// the locale as the process starts.
    pub fn restart(&self) {
        let Ok(exe) = crate::exe::launcher() else {
            return;
        };
        let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
        let mut command = std::process::Command::new(exe);
        command.args(args);
        if crate::exe::restarts_in_place() {
            let err = self.exec_into(command);
            tracing::warn!(error = %err, "could not restart in place");
            return;
        }
        match command.spawn() {
            Ok(_) => self.quit(),
            Err(err) => tracing::warn!(error = %err, "could not start the new copy"),
        }
    }

    fn compile_filter(self: &Rc<Self>) {
        let dir = glib::user_cache_dir()
            .join(mailrs_sync::config::DIR_NAME)
            .join("content-filters");
        let _ = std::fs::create_dir_all(&dir);
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            match crate::web::compile_filter(&dir, "block-remote", BLOCK_REMOTE_RULES).await {
                Ok(filter) => {
                    *this.filter.borrow_mut() = Some(filter.clone());
                    this.tell_window(Notice::FilterReady(filter));
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
                this.unread.heard(&event);
                this.storage.heard(&event);
                this.tell_window(Notice::Engine(&event));
                match &event {
                    ChangeEvent::NewMail {
                        account_id,
                        message_ids,
                    } => {
                        this.contacts_stale.set(true);
                        this.after_rules(*account_id, message_ids.clone());
                    }
                    ChangeEvent::ThreadsChanged {
                        account_id,
                        thread_ids,
                    } => this.settle_notifications(*account_id, thread_ids),
                    _ => {}
                }
            }
        });
    }

    fn listen_for_notifications(self: &Rc<Self>, picked: async_channel::Receiver<notify::Request>) {
        let action = gio::SimpleAction::new(notify::CHOSEN, Some(glib::VariantTy::STRING));
        let weak = Rc::downgrade(self);
        action.connect_activate(move |_, parameter| {
            let Some(app) = weak.upgrade() else { return };
            let request = parameter
                .and_then(|p| p.get::<String>())
                .and_then(|p| app.issued.borrow().request(&p));
            if let Some(request) = request {
                let _ = app.chosen.try_send(request);
            }
        });
        self.gio.add_action(&action);
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            while let Ok(request) = picked.recv().await {
                this.carry_out(request).await;
            }
        });
    }

    /// Posts the new-mail notifications for `messages`. Each gets a fresh
    /// token: clicks carry it back, and the app withdraws by it once the
    /// mail is read.
    pub(crate) fn announce_mail(
        &self,
        messages: Vec<mailrs_domain::MessageMeta>,
        previews: bool,
        buttons: Vec<notify::Button>,
    ) {
        use rand::distr::{Alphanumeric, SampleString};
        for notice in notify::notices(messages, previews, buttons) {
            let token = Alphanumeric.sample_string(&mut rand::rng(), 24);
            let evicted = if let Some(target) = &notice.target {
                self.issued.borrow_mut().issue(
                    token.clone(),
                    target.clone(),
                    notice.buttons.clone(),
                )
            } else {
                self.issued
                    .borrow_mut()
                    .issue_summary(token.clone(), notice.counted.clone())
            };
            // One pushed out of the list would stay on screen with clicks
            // that do nothing, so it goes now.
            if let Some(evicted) = evicted {
                self.notifications.withdraw(&evicted);
            }
            self.notifications.mail(&token, &notice);
        }
    }

    /// Takes back the notifications for mail in `thread_ids` that was read,
    /// archived or trashed since it arrived, here or on another device.
    /// The desktop keeps a notification until the app withdraws it, long
    /// after the mail it announced was read in the window. Only the
    /// messages still waited on in those threads are asked about, as ids
    /// alone: a summary after days away waits on thousands, and the
    /// store answers from its membership tables without building a row.
    /// Each that settled drops out, and a notification goes once none is
    /// left.
    fn settle_notifications(self: &Rc<Self>, account_id: AccountId, thread_ids: &[String]) {
        let asked = self.issued.borrow().in_threads(account_id, thread_ids);
        if asked.is_empty() {
            return;
        }
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let ids: Vec<String> = asked.iter().flat_map(|(_, ids)| ids.clone()).collect();
            let still = this
                .core
                .read(move |c| messages::unread_in_inbox(c, account_id, &ids))
                .await;
            let still = match still {
                Ok(still) => still,
                Err(err) => {
                    tracing::warn!(error = %err, "could not read the mail behind a notification");
                    return;
                }
            };
            for (token, ids) in asked {
                let settled = notify::settled_among(&ids, &still);
                if this.issued.borrow_mut().settle(&token, &settled) {
                    this.notifications.withdraw(&token);
                }
            }
        });
    }

    /// Does what somebody picked on a notification. The mail may have been
    /// read, archived or trashed since it arrived, so the store decides
    /// whether the button still has work to do; one that does not is
    /// dropped rather than put through.
    async fn carry_out(self: &Rc<Self>, request: notify::Request) {
        let notify::Request { target, choice } = request;
        let (account_id, thread_id) = (target.account_id, target.thread_id.clone());
        let button = match choice {
            notify::Choice::Open => {
                self.show_window()
                    .reveal(account_id, thread_id, Reveal::Read);
                return;
            }
            notify::Choice::Button(button) => button,
        };
        let Some(message_id) = target.message_id.clone() else {
            return;
        };
        let message = self
            .core
            .read(move |c| Ok(messages::by_ids(c, account_id, &[message_id])?.pop()))
            .await;
        // Mail the store no longer holds leaves nothing to act on.
        let Ok(Some(message)) = message else { return };
        if !notify::still_applies(button, &message) {
            return;
        }
        let Some(action) = button.action() else {
            self.show_window()
                .reveal(account_id, thread_id, Reveal::Reply);
            return;
        };
        let outcome = self
            .core
            .act(
                vec![target],
                action.clone(),
                History::Record,
                MovedFrom::nowhere(),
            )
            .await;
        self.tell_window(Notice::MailChanged {
            action: &action,
            outcome: &outcome,
        });
        if let Some(error) = outcome.first_error() {
            tracing::warn!(
                error,
                button = button.label(),
                "a notification's button failed"
            );
        }
    }

    /// Runs local rules over new mail before it is announced, so mail a
    /// rule files away or marks read raises no notification. An account
    /// whose rules run on the server announces at once.
    fn after_rules(self: &Rc<Self>, account_id: AccountId, message_ids: Vec<String>) {
        if !self.core.rules_here(account_id) {
            return self.announce(account_id, message_ids);
        }
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            if let Err(err) = this.core.run_rules(account_id).await {
                tracing::warn!(account = account_id, %err, "local rules did not run");
            }
            // The rows read here are the ones announced, rather than
            // their ids read again: thousands can arrive in one event
            // after days away, and each row is built from its JSON.
            match this
                .core
                .read(move |c| announced(c, account_id, &message_ids))
                .await
            {
                Ok(found) => this.announce_found(account_id, found),
                Err(err) => {
                    tracing::warn!(account = account_id, %err, "could not read the mail behind a notification")
                }
            }
        });
    }

    /// Every minute: local rules over mail no new-mail event names, which
    /// is what an account's first listing brought and what arrived already
    /// read, and rule changes that waited for a ManageSieve server. Ten
    /// seconds after start first, once the accounts have started.
    fn watch_rules(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        glib::timeout_add_seconds_local_once(10, {
            let weak = weak.clone();
            move || {
                if let Some(app) = weak.upgrade() {
                    app.rules_tick();
                }
            }
        });
        glib::timeout_add_seconds_local(60, move || match weak.upgrade() {
            Some(app) => {
                app.rules_tick();
                glib::ControlFlow::Continue
            }
            None => glib::ControlFlow::Break,
        });
    }

    fn rules_tick(self: &Rc<Self>) {
        let accounts: Vec<AccountId> = self
            .accounts
            .borrow()
            .iter()
            .filter(|a| matches!(a.provider, Provider::Imap | Provider::Pop3 | Provider::Jmap))
            .map(|a| a.id)
            .collect();
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            for account_id in accounts {
                if this.core.rules_here(account_id) {
                    if let Err(err) = this.core.run_rules(account_id).await {
                        tracing::warn!(account = account_id, %err, "local rules did not run");
                    }
                    continue;
                }
                if !this.core.network() {
                    continue;
                }
                match this.core.send_rule_changes(account_id).await {
                    Ok(sent) => {
                        for words in sent.refused {
                            this.tell_window(Notice::RuleRefused(words));
                        }
                        if let Some(script) = sent.would_replace {
                            this.ask_to_replace(account_id, &script);
                        }
                    }
                    Err(err) => {
                        tracing::info!(account = account_id, %err, "rule changes still wait")
                    }
                }
            }
        });
    }

    /// Asks once a run whether a rule that waited may take the place of
    /// `script`, the person's own rules on a server that runs one script.
    /// The rule keeps waiting, and shows in Rules, until they say yes;
    /// then it goes out at once.
    fn ask_to_replace(self: &Rc<Self>, account_id: AccountId, script: &str) {
        let (Some(window), Some(account)) = (self.window(), self.account(account_id)) else {
            return;
        };
        if !self.asked_to_replace.borrow_mut().insert(account_id) {
            return;
        }
        let dialog = crate::ui::rules::replace_question(&account, script);
        let this = Rc::clone(self);
        dialog.connect_response(None, move |_, response| {
            if response != "replace" {
                return;
            }
            let this = Rc::clone(&this);
            glib::spawn_future_local(async move {
                let settings = this.core.gmail_settings();
                let sent = this
                    .core
                    .call(async move {
                        settings.take_over_rules(account_id).await?;
                        settings.send_rule_changes(account_id).await
                    })
                    .await;
                match sent {
                    Ok(sent) => {
                        for words in sent.refused {
                            this.tell_window(Notice::RuleRefused(words));
                        }
                    }
                    Err(err) => this.tell_window(Notice::RuleRefused(err.to_string())),
                }
            });
        });
        dialog.present(Some(&window.window));
    }

    /// Reads the mail behind `message_ids` and announces what deserves
    /// it. An account whose rules run here goes through
    /// [`Self::announce_found`] instead, with the rows it read already.
    fn announce(self: &Rc<Self>, account_id: AccountId, message_ids: Vec<String>) {
        if self.core.demo || !self.settings().notifications {
            return;
        }
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            match this
                .core
                .read(move |c| announced(c, account_id, &message_ids))
                .await
            {
                Ok(found) => this.announce_found(account_id, found),
                Err(err) => {
                    tracing::warn!(account = account_id, %err, "could not read the mail behind a notification")
                }
            }
        });
    }

    /// Announces the new mail among `found`, as the store held it when
    /// read, then asks the store again for the threads announced. Mail
    /// read elsewhere between that read and the notifications
    /// registering had its change event arrive with nothing to settle,
    /// and this second look is what withdraws its notification.
    fn announce_found(
        self: &Rc<Self>,
        account_id: AccountId,
        mut found: Vec<mailrs_domain::MessageMeta>,
    ) {
        let settings = self.settings();
        if self.core.demo || !settings.notifications {
            return;
        }
        // Mail read or filed between the engine's word and the read would
        // get a notification nothing withdraws, so it gets none. Nor does
        // mail in the conversation on screen, which the person is looking
        // at; the rest is announced even while the window has focus, as
        // the mail that came while the app was closed arrives right after
        // it opens.
        let viewed = self.window().and_then(|w| w.viewed());
        found.retain(|m| {
            still_news(m.in_role(Role::Inbox), m.is_unread())
                && (!settings.notify_vips_only
                    || m.from.as_ref().is_some_and(|a| settings.is_vip(&a.email)))
                && !in_view(m, viewed.as_ref())
        });
        if found.is_empty() {
            return;
        }
        let threads: Vec<String> = found
            .iter()
            .map(|m| m.thread_id.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        self.announce_mail(
            found,
            settings.notification_previews,
            settings.notification_buttons.clone(),
        );
        self.settle_notifications(account_id, &threads);
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
            can_update: self.updater.is_some(),
            update: None,
            icon_theme_path: crate::tray::installed_icons(),
        };
        let slot = Arc::clone(&self.tray);
        // The icon comes up on the runtime, then takes the newest count and
        // each one after it, in order.
        let counts = self.tray_unread.subscribe();
        // The restart that returns memory execs in place and keeps the pid,
        // so the default name, StatusNotifierItem-<pid>-1, comes back while
        // the AppIndicators extension is still timing out the old owner. When
        // the new process registers inside that 500 ms window, the extension
        // sometimes destroys the indicator after accepting the registration,
        // and the icon stays gone. A unique connection name never repeats.
        self.core.spawn(async move {
            match tray.disable_dbus_name(true).spawn().await {
                Ok(handle) => {
                    *slot.lock().expect("tray slot poisoned") = Some(handle.clone());
                    tray_feed::follow(counts, move |(total, accounts)| {
                        let handle = handle.clone();
                        async move {
                            handle
                                .update(move |tray: &mut MailTray| {
                                    tray.unread = total;
                                    tray.accounts = accounts;
                                })
                                .await;
                        }
                    })
                    .await;
                }
                Err(err) => tracing::warn!(error = %err, "could not add the tray icon"),
            }
        });
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            while let Ok(command) = received.recv().await {
                match command {
                    TrayCommand::ReadAll => this.read_all(),
                    TrayCommand::Toggle => match this.window() {
                        Some(window) if window.is_active() => window.window.close(),
                        _ => {
                            this.show_window();
                        }
                    },
                    TrayCommand::Open => {
                        this.show_window();
                    }
                    TrayCommand::OpenInbox(id) => this.show_inbox_of(id),
                    TrayCommand::Compose => this.compose_to(""),
                    TrayCommand::Check => this.core.poke_all(),
                    TrayCommand::CheckForUpdates => this.check_for_updates(true),
                    TrayCommand::InstallUpdate => this.install_update(),
                    TrayCommand::WhatsNew => this.open_release_notes(),
                    TrayCommand::Quit => this.quit(),
                }
            }
        });
    }

    /// Starts the unread counts, wakes them on each server answer and on
    /// each account that starts, and hands every count to the tray and the
    /// Dock badge.
    fn count_unread(self: &Rc<Self>) {
        glib::spawn_future_local(self.unread.start(glib::timeout_future));
        let mut answers = self.core.lists().counts_changed();
        let weak = Rc::downgrade(&self.unread);
        glib::spawn_future_local(async move {
            while answers.changed().await.is_ok() {
                let Some(unread) = weak.upgrade() else { break };
                unread.answered();
            }
        });
        let (started, weak) = (self.core.started.clone(), Rc::downgrade(&self.unread));
        glib::spawn_future_local(async move {
            while let Ok(account_id) = started.recv().await {
                let Some(unread) = weak.upgrade() else { break };
                unread.started(account_id);
            }
        });
        let weak = Rc::downgrade(self);
        self.unread.listen(Box::new(move |counted| {
            if let Some(app) = weak.upgrade() {
                app.show_unread(counted);
            }
        }));
    }

    /// Puts a count on the tray and the Dock badge: All Inboxes, and each
    /// account's inbox on its line of the tray menu.
    fn show_unread(&self, counted: &Counted) {
        let total = counted.all_inboxes();
        self.dock_badge.show(total);
        let accounts: Vec<AccountUnread> = mailrs_domain::mail_accounts(&self.accounts.borrow())
            .into_iter()
            .map(|account| AccountUnread {
                id: account.id,
                unread: counted.inbox(account.id),
                email: account.email,
            })
            .collect();
        // One task hands the counts to the tray, so an older one cannot
        // land after this one, as it could from a task of its own.
        self.tray_unread.send_replace((total, accounts));
    }

    /// The unread counts, for a window to show them too.
    pub(crate) fn unread(&self) -> &Rc<Unread<CoreCounts>> {
        &self.unread
    }
}

/// The unread counts, read through the core's runtime.
pub(crate) struct CoreCounts(Rc<Core>);

impl Counter for CoreCounts {
    async fn count(&self, ask: Ask) -> Result<Counts, String> {
        let lists = self.0.lists();
        self.0
            .call(async move { crate::unread::read(&lists, ask).await })
            .await
            .map_err(|err| err.to_string())
    }
}

/// The API and its enable page, when `err` is Google saying the Cloud
/// project has that API switched off.
fn api_off(err: &anyhow::Error) -> Option<(String, String)> {
    use mailrs_gmail::GmailError;
    use mailrs_sync::{BackendError, SyncError};
    match err.downcast_ref::<SyncError>() {
        Some(SyncError::Backend(BackendError::ApiDisabled {
            service,
            enable_url,
        })) => Some((service.clone(), enable_url.clone())),
        // A call that went to Google without the sync crate in between,
        // such as signing in, still answers in Gmail's own type.
        _ => match err.downcast_ref::<GmailError>()? {
            GmailError::ApiDisabled {
                service,
                enable_url,
            } => Some((service.clone(), enable_url.clone())),
            _ => None,
        },
    }
}

/// What the app does once its last window has closed.
#[derive(Debug, PartialEq, Eq)]
enum AfterLastWindow {
    /// Nothing to sync and nothing to show in the tray: the process ends.
    Quit,
    /// Keep syncing in the background, and restart within a minute to
    /// give the window's memory back.
    Shed,
    /// The demo stays as it is.
    Stay,
}

/// `accounts` is how many accounts the store holds, `None` until the app
/// has read them.
fn after_last_window(demo: bool, accounts: Option<usize>) -> AfterLastWindow {
    match (demo, accounts) {
        (true, _) => AfterLastWindow::Stay,
        (false, Some(0)) => AfterLastWindow::Quit,
        (false, _) => AfterLastWindow::Shed,
    }
}

#[cfg(test)]
mod tests {
    use super::{AfterLastWindow, after_last_window};

    #[test]
    fn closing_the_window_with_no_accounts_quits() {
        assert_eq!(after_last_window(false, Some(0)), AfterLastWindow::Quit);
    }

    #[test]
    fn closing_the_window_with_accounts_keeps_syncing() {
        assert_eq!(after_last_window(false, Some(2)), AfterLastWindow::Shed);
    }

    #[test]
    fn a_window_closed_before_the_accounts_are_read_does_not_quit() {
        assert_eq!(after_last_window(false, None), AfterLastWindow::Shed);
    }

    #[test]
    fn the_demo_stays_open_in_the_background() {
        assert_eq!(after_last_window(true, Some(0)), AfterLastWindow::Stay);
    }
}
