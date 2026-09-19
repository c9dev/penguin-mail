//! Application state that outlives any window: the core, the tray, the
//! content filter, identities, and the engine event loop.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::os::unix::process::CommandExt;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use adw::prelude::*;
use gtk::{gio, glib};
use ksni::TrayMethods;
use mailrs_domain::{Account, AccountId, Address, ChangeEvent};
use mailrs_store::{messages, threads};

use crate::compose::Draft;
use crate::core::Core;
use crate::notify;
use crate::settings::{ColorScheme, Settings};
use crate::tray::{MailTray, TrayCommand};
use crate::ui::autocomplete::Contacts;
use crate::ui::composer::{Composer, Identity};
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
    /// Correspondents for recipient suggestions, reloaded per composer.
    contacts: Contacts,
    /// Messages waiting out the Undo Send delay.
    pending_sends: Cell<usize>,
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
        // Demo mode must not change the real preferences.
        let settings_path = if core.demo && std::env::var_os("MAILRS_SETTINGS").is_none() {
            std::env::temp_dir().join(format!("mailrs-demo-{}-settings.toml", std::process::id()))
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
            settings: RefCell::new(Settings::load(&settings_path)),
            settings_path,
            contacts: Rc::new(RefCell::new(Rc::new(Vec::new()))),
            pending_sends: Cell::new(0),
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

    /// Changes preferences, saves them, and applies them to open windows.
    pub fn update_settings(self: &Rc<Self>, change: impl FnOnce(&mut Settings)) {
        let before = self.settings();
        let mut after = before.clone();
        change(&mut after);
        if after == before {
            return;
        }
        if let Err(err) = after.save(&self.settings_path) {
            tracing::warn!(error = %err, "could not save preferences");
        }
        *self.settings.borrow_mut() = after.clone();
        self.apply_style();
        if let Some(window) = self.window() {
            window.settings_changed(&before, &after);
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

    /// `draft` with its account's signature added.
    pub fn signed(&self, mut draft: Draft) -> Draft {
        let settings = self.settings.borrow();
        draft.markdown =
            crate::compose::with_signature(&draft.markdown, settings.signature(&draft.from.email));
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

    /// The address and display name mail from this account is sent as.
    pub fn identity(&self, account_id: AccountId) -> Address {
        let email = self
            .accounts
            .borrow()
            .iter()
            .find(|a| a.id == account_id)
            .map(|a| a.email.clone())
            .unwrap_or_default();
        Address {
            name: self.names.borrow().get(&account_id).cloned(),
            email,
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
        self.update_tray();
    }

    fn load_accounts(self: &Rc<Self>) {
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            if let Ok(accounts) = this.core.read(mailrs_store::accounts::list_accounts).await {
                // Give the engine a moment to connect before asking for display names.
                glib::timeout_future(std::time::Duration::from_millis(500)).await;
                this.remember_accounts(&accounts);
            }
        });
    }

    pub fn compose(self: &Rc<Self>, draft: Draft) -> Option<Rc<Composer>> {
        crate::ensure_gtk();
        self.apply_style();
        let identities: Vec<Identity> = self
            .accounts
            .borrow()
            .iter()
            .map(|a| Identity {
                account_id: a.id,
                address: self.identity(a.id),
            })
            .collect();
        if identities.is_empty() {
            return None;
        }
        self.reload_contacts();
        let this = Rc::downgrade(self);
        let composer = Composer::open(
            Rc::clone(&self.core),
            identities,
            Rc::clone(&self.contacts),
            draft,
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

    /// Refreshes the suggestions composers offer. Open composers see the
    /// new list once it loads.
    fn reload_contacts(self: &Rc<Self>) {
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            match this.core.read(mailrs_store::contacts::list_contacts).await {
                Ok(found) => *this.contacts.borrow_mut() = Rc::new(found),
                Err(err) => tracing::warn!(error = %err, "could not load contacts"),
            }
        });
    }

    /// Application actions, also reachable over D-Bus, for example:
    /// `gdbus call --session --dest dev.mailrs.Mailrs --object-path /dev/mailrs/Mailrs
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
            .join("mailrs")
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
                    } => this.announce(*account_id, message_ids.clone()),
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
    /// it later shows the icon without restarting mailrs.
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
                            &threads::ThreadFilter::account(account.id, "INBOX"),
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
