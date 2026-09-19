//! Application state that outlives any window: the core, the tray, the
//! content filter, identities, and the engine event loop.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
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
use crate::tray::{MailTray, TrayCommand};
use crate::ui::composer::{Composer, Identity};
use crate::ui::window::MainWindow;

const BLOCK_REMOTE_RULES: &str = r#"[
  {"trigger": {"url-filter": "^https?:"}, "action": {"type": "block"}},
  {"trigger": {"url-filter": "^wss?:"}, "action": {"type": "block"}},
  {"trigger": {"url-filter": "^ftp:"}, "action": {"type": "block"}}
]"#;

pub struct App {
    pub gtk: adw::Application,
    pub core: Rc<Core>,
    window: RefCell<Option<Rc<MainWindow>>>,
    filter: RefCell<Option<webkit::UserContentFilter>>,
    accounts: RefCell<Vec<Account>>,
    names: RefCell<HashMap<AccountId, String>>,
    tray: Arc<Mutex<Option<ksni::Handle<MailTray>>>>,
    open_requests: async_channel::Sender<(AccountId, String)>,
    skip_first_window: Cell<bool>,
    _hold: gio::ApplicationHoldGuard,
}

impl App {
    pub fn new(gtk: &adw::Application, core: Rc<Core>, background: bool) -> Rc<App> {
        let (open_requests, opened) = async_channel::unbounded();
        let app = Rc::new(App {
            gtk: gtk.clone(),
            core,
            window: RefCell::new(None),
            filter: RefCell::new(None),
            accounts: RefCell::new(Vec::new()),
            names: RefCell::new(HashMap::new()),
            tray: Arc::new(Mutex::new(None)),
            open_requests,
            skip_first_window: Cell::new(background),
            _hold: gtk.hold(),
        });
        app.install_actions();
        app.compile_filter();
        app.listen();
        app.listen_for_opens(opened);
        if !app.core.demo {
            app.start_tray();
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
        app
    }

    /// GApplication's activate: the first one is skipped with `--background`.
    pub fn activate(self: &Rc<Self>) {
        if self.skip_first_window.replace(false) {
            return;
        }
        self.show_window();
    }

    pub fn show_window(self: &Rc<Self>) -> Rc<MainWindow> {
        if let Some(window) = self.window.borrow().as_ref() {
            window.present();
            return Rc::clone(window);
        }
        let window = MainWindow::new(self);
        *self.window.borrow_mut() = Some(Rc::clone(&window));
        window.present();
        window.run_demo_script();
        window
    }

    pub fn forget_window(&self, window: &Rc<MainWindow>) {
        let mut slot = self.window.borrow_mut();
        if slot.as_ref().is_some_and(|w| Rc::ptr_eq(w, window)) {
            *slot = None;
        }
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

    pub fn compose(self: &Rc<Self>, draft: Draft) {
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
            return;
        }
        let this = Rc::downgrade(self);
        let composer = Composer::open(
            &self.gtk,
            Rc::clone(&self.core),
            identities,
            draft,
            move |account_id| {
                if let Some(app) = this.upgrade() {
                    app.core.poke(account_id);
                    if let Some(window) = app.window() {
                        window.toast_sent();
                    }
                }
            },
        );
        let keep = Rc::clone(&composer);
        composer_window(&composer).connect_destroy(move |_| {
            let _ = &keep;
        });
    }

    fn install_actions(self: &Rc<Self>) {
        let quit = gio::SimpleAction::new("quit", None);
        let gtk_app = self.gtk.clone();
        quit.connect_activate(move |_, _| gtk_app.quit());
        self.gtk.add_action(&quit);
        for (action, accels) in [
            ("app.quit", &["<Control>q"][..]),
            ("win.compose", &["<Control>n"][..]),
            ("win.search", &["<Control>f"][..]),
            ("win.check", &["F5", "<Control>r"][..]),
            ("win.shortcuts", &["<Control>question"][..]),
            ("window.close", &["<Control>w"][..]),
        ] {
            self.gtk.set_accels_for_action(action, accels);
        }
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
        if self.core.demo || self.window().is_some_and(|w| w.is_active()) {
            return;
        }
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
            if let Ok(found) = found
                && !found.is_empty()
            {
                notify::announce(found, this.open_requests.clone());
            }
        });
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
                Err(err) => tracing::info!(error = %err, "no system tray available"),
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
                    TrayCommand::Compose => {
                        let first = this.accounts.borrow().first().map(|a| a.id);
                        if let Some(account_id) = first {
                            this.compose(Draft::new(account_id, this.identity(account_id)));
                        }
                    }
                    TrayCommand::Check => this.core.poke_all(),
                    TrayCommand::Quit => this.gtk.quit(),
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
