//! The main window: sidebar, thread list, and conversation, plus the
//! first-run pages. It reacts to engine events and turns user actions into
//! calls on the sync core.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use base64::Engine;
use gtk::{gdk, gio, glib};
use mailrs_domain::{
    Account, AccountId, AccountState, ChangeEvent, Label, MessageBody, MessageMeta, ThreadSummary,
};
use mailrs_store::{accounts, labels, messages, threads};
use mailrs_sync::TriageAction;

use super::conversation::{Action, ConversationView, OpenThread};
use super::sidebar::Sidebar;
use super::thread_list::ThreadList;
use super::{Mailbox, summarize_search, welcome};
use crate::app::App;
use crate::compose::{self, Draft, OutgoingAttachment, ReplyKind};
use crate::core::Core;

/// Largest inline image embedded into a page.
const INLINE_IMAGE_LIMIT: usize = 5 * 1024 * 1024;

type WindowAction = Box<dyn Fn(&Rc<MainWindow>)>;
type AccountAction = Box<dyn Fn(&Rc<MainWindow>, Account)>;

pub struct MainWindow {
    pub window: adw::Window,
    actions: gio::SimpleActionGroup,
    app: Weak<App>,
    core: Rc<Core>,
    toasts: adw::ToastOverlay,
    stack: gtk::Stack,
    split: adw::OverlaySplitView,
    nav: adw::NavigationSplitView,
    sidebar: Rc<Sidebar>,
    list: Rc<ThreadList>,
    conversation: Rc<ConversationView>,
    first_account: gtk::Button,
    mailbox: RefCell<Mailbox>,
    before_search: RefCell<Mailbox>,
    accounts: RefCell<Vec<Account>>,
    refresh_queued: Cell<bool>,
    list_generation: Cell<u64>,
    authorizing: Cell<bool>,
}

impl MainWindow {
    pub fn new(app: &Rc<App>) -> Rc<MainWindow> {
        let window = Rc::new_cyclic(|weak: &Weak<MainWindow>| {
            let w = weak.clone();
            let sidebar = Sidebar::new(move |mailbox| {
                if let Some(win) = w.upgrade() {
                    win.show_mailbox(mailbox);
                }
            });
            let (w, s) = (weak.clone(), weak.clone());
            let list = ThreadList::new(
                move |thread| {
                    if let Some(win) = w.upgrade() {
                        win.open_thread(thread);
                    }
                },
                move |query| {
                    if let Some(win) = s.upgrade() {
                        win.search(query);
                    }
                },
            );
            let w = weak.clone();
            let conversation = ConversationView::new(move |action| {
                if let Some(win) = w.upgrade() {
                    win.act(action);
                }
            });
            let nav = adw::NavigationSplitView::builder()
                .sidebar(&list.page)
                .content(&conversation.page)
                .min_sidebar_width(300.0)
                .max_sidebar_width(420.0)
                .sidebar_width_fraction(0.34)
                .build();
            let split = adw::OverlaySplitView::builder()
                .sidebar(&sidebar.page)
                .content(&nav)
                .min_sidebar_width(220.0)
                .max_sidebar_width(290.0)
                .sidebar_width_fraction(0.22)
                .build();
            split
                .bind_property("collapsed", &list.sidebar_button, "visible")
                .sync_create()
                .build();
            split
                .bind_property("show-sidebar", &list.sidebar_button, "active")
                .bidirectional()
                .sync_create()
                .build();

            let w = weak.clone();
            let setup = welcome::setup_page(move |id, secret| {
                if let Some(win) = w.upgrade() {
                    win.save_config(id, secret);
                }
            });
            let w = weak.clone();
            let (first_page, first_account) = welcome::first_account_page(move || {
                if let Some(win) = w.upgrade() {
                    win.authorize(None);
                }
            });
            let stack = gtk::Stack::builder()
                .transition_type(gtk::StackTransitionType::Crossfade)
                .build();
            stack.add_named(&split, Some("mail"));
            stack.add_named(&setup, Some("setup"));
            stack.add_named(&first_page, Some("first-account"));
            let toasts = adw::ToastOverlay::new();
            toasts.set_child(Some(&stack));
            let window = adw::Window::builder()
                .title(if app.core.demo {
                    "mailrs (demo)"
                } else {
                    "mailrs"
                })
                .default_width(1320)
                .default_height(840)
                .width_request(360)
                .height_request(480)
                .content(&toasts)
                .build();
            let medium = adw::Breakpoint::new(
                adw::BreakpointCondition::parse("max-width: 960sp").expect("valid breakpoint"),
            );
            medium.add_setter(&split, "collapsed", Some(&true.to_value()));
            medium.add_setter(&split, "show-sidebar", Some(&false.to_value()));
            let narrow = adw::Breakpoint::new(
                adw::BreakpointCondition::parse("max-width: 620sp").expect("valid breakpoint"),
            );
            narrow.add_setter(&split, "collapsed", Some(&true.to_value()));
            narrow.add_setter(&split, "show-sidebar", Some(&false.to_value()));
            narrow.add_setter(&nav, "collapsed", Some(&true.to_value()));
            let (on, off) = (Rc::clone(&conversation), Rc::clone(&conversation));
            narrow.connect_apply(move |_| on.set_compact(true));
            narrow.connect_unapply(move |_| off.set_compact(false));
            window.add_breakpoint(medium);
            window.add_breakpoint(narrow);

            let actions = gio::SimpleActionGroup::new();
            window.insert_action_group("win", Some(&actions));
            MainWindow {
                window,
                actions,
                app: Rc::downgrade(app),
                core: Rc::clone(&app.core),
                toasts,
                stack,
                split,
                nav,
                sidebar,
                list,
                conversation,
                first_account,
                mailbox: RefCell::new(Mailbox::Unified("INBOX")),
                before_search: RefCell::new(Mailbox::Unified("INBOX")),
                accounts: RefCell::new(Vec::new()),
                refresh_queued: Cell::new(false),
                list_generation: Cell::new(0),
                authorizing: Cell::new(false),
            }
        });
        window.install_actions();
        window.install_menu();
        window.install_keys();
        let weak = Rc::downgrade(&window);
        window.list.search_button.connect_toggled(move |button| {
            let Some(win) = weak.upgrade() else { return };
            if !button.is_active() && matches!(*win.mailbox.borrow(), Mailbox::Search { .. }) {
                let back = win.before_search.borrow().clone();
                win.sidebar.select(&back);
                win.show_mailbox(back);
            }
        });
        let weak = Rc::downgrade(&window);
        window.list.banner.connect_button_clicked(move |_| {
            let Some(win) = weak.upgrade() else { return };
            let email = win
                .accounts
                .borrow()
                .iter()
                .find(|a| a.state == AccountState::NeedsReauth)
                .map(|a| a.email.clone());
            win.authorize(email);
        });
        let weak = Rc::downgrade(&window);
        window.sidebar.add_account.connect_clicked(move |_| {
            if let Some(win) = weak.upgrade() {
                win.authorize(None);
            }
        });
        if let Some(filter) = app.filter() {
            window.conversation.set_filter(filter);
        }
        window.refresh_accounts();
        window
    }

    pub fn present(&self) {
        self.window.present();
    }

    pub fn is_active(&self) -> bool {
        self.window.is_active() && self.window.is_visible()
    }

    pub fn install_filter(&self, filter: webkit::UserContentFilter) {
        self.conversation.set_filter(filter);
    }

    fn toast(&self, text: &str) {
        self.toasts.add_toast(
            adw::Toast::builder()
                .title(glib::markup_escape_text(text))
                .timeout(4)
                .build(),
        );
    }

    // ---- Engine events -------------------------------------------------

    pub fn handle(self: &Rc<Self>, event: &ChangeEvent) {
        match event {
            ChangeEvent::AccountStateChanged { .. } | ChangeEvent::LabelsChanged { .. } => {
                self.refresh_accounts()
            }
            ChangeEvent::ThreadsChanged { .. } | ChangeEvent::NewMail { .. } => {
                self.queue_refresh()
            }
            ChangeEvent::WriteFailed { message, .. } => self.toast(message),
        }
    }

    /// Coalesces bursts of change events into one refresh.
    fn queue_refresh(self: &Rc<Self>) {
        if self.refresh_queued.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(std::time::Duration::from_millis(150), move || {
            if let Some(win) = weak.upgrade() {
                win.refresh_queued.set(false);
                win.refresh_counts();
                win.reload_list();
                win.refresh_open_thread();
            }
        });
    }

    fn refresh_accounts(self: &Rc<Self>) {
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let loaded = this
                .core
                .read(|c| {
                    let mut out: Vec<(Account, Vec<Label>)> = Vec::new();
                    for account in accounts::list_accounts(c)? {
                        let account_labels = labels::list_labels(c, account.id)?;
                        out.push((account, account_labels));
                    }
                    Ok(out)
                })
                .await;
            let data = match loaded {
                Ok(data) => data,
                Err(err) => return this.toast(&format!("Could not read accounts: {err}")),
            };
            *this.accounts.borrow_mut() = data.iter().map(|(a, _)| a.clone()).collect();
            let page = if !this.core.has_config() {
                "setup"
            } else if data.is_empty() {
                "first-account"
            } else {
                "mail"
            };
            this.stack.set_visible_child_name(page);
            let mailbox = this.mailbox.borrow().clone();
            let still_exists = match &mailbox {
                Mailbox::Label { account_id, .. } => data.iter().any(|(a, _)| a.id == *account_id),
                _ => true,
            };
            if !still_exists {
                *this.mailbox.borrow_mut() = Mailbox::Unified("INBOX");
            }
            if !matches!(mailbox, Mailbox::Search { .. }) {
                this.sidebar.rebuild(&data, &this.mailbox.borrow());
            }
            this.list
                .set_show_accounts(this.mailbox.borrow().account().is_none() && data.len() > 1);
            let reauth: Vec<&str> = data
                .iter()
                .filter(|(a, _)| a.state == AccountState::NeedsReauth)
                .map(|(a, _)| a.email.as_str())
                .collect();
            match reauth.first() {
                Some(email) => {
                    this.list
                        .banner
                        .set_title(&format!("Sign in again to keep {email} syncing"));
                    this.list.banner.set_button_label(Some("Sign In"));
                    this.list.banner.set_revealed(true);
                }
                None => this.list.banner.set_revealed(false),
            }
            if let Some(app) = this.app.upgrade() {
                app.remember_accounts(&this.accounts.borrow());
            }
            this.refresh_counts();
            this.reload_list();
        });
    }

    fn refresh_counts(self: &Rc<Self>) {
        let mailboxes = self.sidebar.mailboxes();
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let counts = this
                .core
                .read(move |c| {
                    let mut counts = HashMap::new();
                    for mailbox in mailboxes {
                        let Some(filter) = mailbox.filter() else {
                            continue;
                        };
                        let count = if mailbox.counts_unread() {
                            threads::unread_threads(c, &filter)?
                        } else {
                            threads::count_threads(c, &filter)?
                        };
                        counts.insert(mailbox, count);
                    }
                    Ok(counts)
                })
                .await;
            if let Ok(counts) = counts {
                this.sidebar.set_counts(&counts);
            }
        });
    }

    // ---- Mailboxes and the thread list ---------------------------------

    fn show_mailbox(self: &Rc<Self>, mailbox: Mailbox) {
        if !matches!(mailbox, Mailbox::Search { .. }) && self.list.search_open() {
            *self.before_search.borrow_mut() = mailbox.clone();
            *self.mailbox.borrow_mut() = mailbox.clone();
            self.list.close_search();
        }
        *self.mailbox.borrow_mut() = mailbox.clone();
        self.list
            .set_show_accounts(mailbox.account().is_none() && self.accounts.borrow().len() > 1);
        self.list.set_title(&mailbox.title(), "");
        self.list.unselect();
        self.conversation.clear();
        self.nav.set_show_content(false);
        if self.split.is_collapsed() {
            self.split.set_show_sidebar(false);
        }
        self.reload_list();
    }

    fn reload_list(self: &Rc<Self>) {
        let mailbox = self.mailbox.borrow().clone();
        let Some(filter) = mailbox.filter() else {
            return;
        };
        let generation = self.list_generation.get() + 1;
        self.list_generation.set(generation);
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let loaded = this
                .core
                .read(move |c| {
                    Ok((
                        threads::list_threads(c, &filter, 0, 10_000)?,
                        threads::unread_threads(c, &filter)?,
                    ))
                })
                .await;
            if this.list_generation.get() != generation {
                return;
            }
            match loaded {
                Ok((rows, unread)) => {
                    let (title, icon) = empty_state(&mailbox);
                    this.list.set_rows(rows, title, icon);
                    let subtitle = if unread > 0 {
                        format!("{unread} unread")
                    } else {
                        String::new()
                    };
                    this.list.set_title(&mailbox.title(), &subtitle);
                }
                Err(err) => this.toast(&format!("Could not load mail: {err}")),
            }
        });
    }

    fn search(self: &Rc<Self>, query: String) {
        let current = self.mailbox.borrow().clone();
        let scope = current.account();
        if !matches!(current, Mailbox::Search { .. }) {
            *self.before_search.borrow_mut() = current;
        }
        *self.mailbox.borrow_mut() = Mailbox::Search {
            query: query.clone(),
            account_id: scope,
        };
        self.list_generation.set(self.list_generation.get() + 1);
        let generation = self.list_generation.get();
        self.sidebar.clear_selection();
        self.list.set_title("Search", &query);
        self.list.show_loading();
        self.conversation.clear();
        let targets: Vec<Account> = self
            .accounts
            .borrow()
            .iter()
            .filter(|a| scope.is_none_or(|id| a.id == id))
            .cloned()
            .collect();
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let searches = targets.iter().map(|account| {
                let (core, query) = (Rc::clone(&this.core), query.clone());
                let sync = core.account(account.id);
                async move {
                    match sync {
                        Some(sync) => {
                            core.call(async move { sync.search(&query, 50).await })
                                .await
                        }
                        None => Ok(Vec::new()),
                    }
                }
            });
            let results = futures::future::join_all(searches).await;
            if this.list_generation.get() != generation {
                return;
            }
            let mut hits = Vec::new();
            for (account, result) in targets.iter().zip(results) {
                match result {
                    Ok(found) => hits.extend(found),
                    Err(err) => this.toast(&format!("Search failed for {}: {err}", account.email)),
                }
            }
            this.list.set_rows(
                summarize_search(hits),
                "No Results",
                "system-search-symbolic",
            );
        });
    }

    // ---- Opening threads -------------------------------------------------

    fn addresses_for(&self, account_id: AccountId) -> Vec<String> {
        self.accounts
            .borrow()
            .iter()
            .filter(|a| a.id == account_id)
            .map(|a| a.email.clone())
            .collect()
    }

    fn open_thread(self: &Rc<Self>, summary: ThreadSummary) {
        self.nav.set_show_content(true);
        if self
            .conversation
            .is_showing(summary.account_id, &summary.id)
        {
            return;
        }
        let (account_id, thread_id) = (summary.account_id, summary.id.clone());
        let me = self.addresses_for(account_id);
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let key = thread_id.clone();
            let local = this
                .core
                .read(move |c| {
                    let found = messages::thread_messages(c, account_id, &key)?;
                    let mut cached = HashMap::new();
                    for meta in &found {
                        if let Some(body) = read_cached_body(c, account_id, &meta.id)? {
                            cached.insert(meta.id.clone(), body);
                        }
                    }
                    Ok((found, cached))
                })
                .await;
            let (found, cached) = local.unwrap_or_default();
            let expanded = default_expanded(&found);
            let thread = OpenThread {
                account_id,
                thread_id: thread_id.clone(),
                subject: found
                    .first()
                    .map(|m| m.subject.clone())
                    .unwrap_or(summary.subject.clone()),
                messages: found,
                bodies: cached
                    .into_iter()
                    .map(|(id, body)| (id, Ok(body)))
                    .collect(),
                expanded,
                images_allowed: false,
                me,
                inline_images: HashMap::new(),
            };
            this.conversation.show(thread, true);
            this.complete_thread(account_id, thread_id).await;
        });
    }

    /// Fetches the whole thread and any missing bodies, then marks it read.
    async fn complete_thread(self: &Rc<Self>, account_id: AccountId, thread_id: String) {
        let Some(sync) = self.core.account(account_id) else {
            return;
        };
        let (s, t) = (sync.clone(), thread_id.clone());
        if let Err(err) = self
            .core
            .call(async move { s.ensure_thread(&t).await })
            .await
        {
            tracing::info!(error = %err, "showing the stored copy of the thread");
        }
        if !self.conversation.is_showing(account_id, &thread_id) {
            return;
        }
        let key = thread_id.clone();
        let fresh = self
            .core
            .read(move |c| messages::thread_messages(c, account_id, &key))
            .await
            .unwrap_or_default();
        let missing: Vec<String> = self
            .conversation
            .with_open(|open| {
                for meta in &fresh {
                    if !open.messages.iter().any(|m| m.id == meta.id) && meta.is_unread() {
                        open.expanded.insert(meta.id.clone());
                    }
                }
                if !fresh.is_empty() {
                    open.messages = fresh.clone();
                }
                open.messages
                    .iter()
                    .filter(|m| !open.bodies.contains_key(&m.id))
                    .map(|m| m.id.clone())
                    .collect()
            })
            .unwrap_or_default();
        let fetches = missing.into_iter().map(|id| {
            let (core, sync) = (Rc::clone(&self.core), sync.clone());
            async move {
                let key = id.clone();
                let result = core.call(async move { sync.body(&key).await }).await;
                (id, result.map_err(|e| e.to_string()))
            }
        });
        let loaded = futures::future::join_all(fetches).await;
        let images = self.inline_images(&sync, &loaded).await;
        if !self.conversation.is_showing(account_id, &thread_id) {
            return;
        }
        let unread = self
            .conversation
            .with_open(|open| {
                open.bodies.extend(loaded);
                open.inline_images.extend(images);
                open.unread()
            })
            .unwrap_or(false);
        self.conversation.render(false);
        if unread {
            self.run_triage(account_id, thread_id, TriageAction::MarkRead, false);
        }
    }

    /// Downloads `cid:` images that HTML bodies reference, as `data:` URIs.
    async fn inline_images(
        &self,
        sync: &std::sync::Arc<crate::core::Sync>,
        loaded: &[(String, Result<MessageBody, String>)],
    ) -> HashMap<String, HashMap<String, String>> {
        let mut out = HashMap::new();
        for (message_id, body) in loaded {
            let Ok(body) = body else { continue };
            if !body.html.as_deref().is_some_and(|h| h.contains("cid:")) {
                continue;
            }
            let mut images = HashMap::new();
            for attachment in &body.attachments {
                let (Some(cid), Some(attachment_id)) =
                    (&attachment.content_id, &attachment.attachment_id)
                else {
                    continue;
                };
                if !attachment.mime_type.starts_with("image/")
                    || attachment.size as usize > INLINE_IMAGE_LIMIT
                {
                    continue;
                }
                let (s, m, a) = (sync.clone(), message_id.clone(), attachment_id.clone());
                if let Ok(bytes) = self
                    .core
                    .call(async move { s.attachment(&m, &a).await })
                    .await
                {
                    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
                    images.insert(
                        cid.clone(),
                        format!("data:{};base64,{encoded}", attachment.mime_type),
                    );
                }
            }
            out.insert(message_id.clone(), images);
        }
        out
    }

    /// Picks up label changes and new messages in the open thread. Redraws
    /// only when the set of messages changed, so reading position survives.
    fn refresh_open_thread(self: &Rc<Self>) {
        let Some((account_id, thread_id)) = self
            .conversation
            .with_open(|o| (o.account_id, o.thread_id.clone()))
        else {
            return;
        };
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let key = thread_id.clone();
            let Ok(fresh) = this
                .core
                .read(move |c| messages::thread_messages(c, account_id, &key))
                .await
            else {
                return;
            };
            if !this.conversation.is_showing(account_id, &thread_id) {
                return;
            }
            if fresh.is_empty() {
                this.conversation.clear();
                return;
            }
            let changed = this
                .conversation
                .with_open(|open| {
                    let same = open
                        .messages
                        .iter()
                        .map(|m| &m.id)
                        .eq(fresh.iter().map(|m| &m.id));
                    open.messages = fresh;
                    !same
                })
                .unwrap_or(false);
            if changed {
                this.complete_thread(account_id, thread_id).await;
            } else {
                this.conversation.render_buttons();
            }
        });
    }

    // ---- Actions on the open thread --------------------------------------

    fn act(self: &Rc<Self>, action: Action) {
        match action {
            Action::Reply(kind) => self.reply(kind),
            Action::EditDraft => self.edit_draft(),
            Action::Archive => self.triage_open(TriageAction::Archive),
            Action::Trash => self.triage_open(TriageAction::Trash),
            Action::ToggleStar => {
                let starred = self
                    .conversation
                    .with_open(|o| o.starred())
                    .unwrap_or(false);
                self.triage_open(if starred {
                    TriageAction::Unstar
                } else {
                    TriageAction::Star
                });
            }
            Action::ToggleRead => {
                let unread = self.conversation.with_open(|o| o.unread()).unwrap_or(false);
                self.triage_open(if unread {
                    TriageAction::MarkRead
                } else {
                    TriageAction::MarkUnread
                });
            }
            Action::LoadImages => {
                self.conversation.with_open(|o| o.images_allowed = true);
                self.conversation.render(false);
            }
            Action::SaveAttachment { message_id, index } => self.save_attachment(message_id, index),
            Action::Mailto(address) => {
                let account_id = self.default_account();
                if let (Some(account_id), Some(app)) = (account_id, self.app.upgrade()) {
                    let mut draft = Draft::new(account_id, app.identity(account_id));
                    draft.to = compose::parse_recipients(&address);
                    app.compose(draft);
                }
            }
        }
    }

    fn default_account(&self) -> Option<AccountId> {
        self.conversation
            .with_open(|o| o.account_id)
            .or_else(|| self.mailbox.borrow().account())
            .or_else(|| self.accounts.borrow().first().map(|a| a.id))
    }

    fn triage_open(self: &Rc<Self>, action: TriageAction) {
        let key = self
            .conversation
            .with_open(|o| (o.account_id, o.thread_id.clone()));
        let Some((account_id, thread_id)) = key else {
            return;
        };
        let leaves = matches!(action, TriageAction::Archive | TriageAction::Trash);
        if leaves {
            let next = self.list.neighbour_of_selected();
            self.conversation.clear();
            match next {
                Some(next) => self.list.select(next.account_id, &next.id),
                None => self.nav.set_show_content(false),
            }
        }
        self.run_triage(account_id, thread_id, action, true);
    }

    fn run_triage(
        self: &Rc<Self>,
        account_id: AccountId,
        thread_id: String,
        action: TriageAction,
        announce: bool,
    ) {
        let Some(sync) = self.core.account(account_id) else {
            return;
        };
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let (s, t, a) = (sync.clone(), thread_id.clone(), action.clone());
            match this
                .core
                .call(async move { s.triage_thread(&t, &a).await })
                .await
            {
                Ok(()) if announce && action == TriageAction::Archive => {
                    let toast = adw::Toast::builder()
                        .title("Archived")
                        .button_label("Undo")
                        .timeout(5)
                        .build();
                    let weak = Rc::downgrade(&this);
                    toast.connect_button_clicked(move |_| {
                        if let Some(win) = weak.upgrade() {
                            win.run_triage(
                                account_id,
                                thread_id.clone(),
                                TriageAction::AddLabel("INBOX".into()),
                                false,
                            );
                        }
                    });
                    this.toasts.add_toast(toast);
                }
                Ok(()) if announce && action == TriageAction::Trash => this.toast("Moved to Trash"),
                Ok(()) => {}
                Err(err) => this.toast(&format!("{} failed: {err}", action.describe())),
            }
        });
    }

    fn reply(self: &Rc<Self>, kind: ReplyKind) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let prepared = self.conversation.with_open(|open| {
            let target = open.reply_target()?.clone();
            let text = match open.bodies.get(&target.id) {
                Some(Ok(body)) => compose::body_text(body),
                _ => target.snippet.clone(),
            };
            let attachments = match open.bodies.get(&target.id) {
                Some(Ok(body)) if kind == ReplyKind::Forward => body.attachments.clone(),
                _ => Vec::new(),
            };
            Some((
                open.account_id,
                target,
                text,
                open.messages.clone(),
                attachments,
            ))
        });
        let Some(Some((account_id, target, text, thread, attachments))) = prepared else {
            return;
        };
        let me = app.identity(account_id);
        let mut draft = compose::respond(kind, account_id, &me, &target, &text, &thread);
        if attachments.is_empty() {
            app.compose(draft);
            return;
        }
        let Some(sync) = self.core.account(account_id) else {
            return;
        };
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            for attachment in attachments {
                let Some(attachment_id) = attachment.attachment_id.clone() else {
                    continue;
                };
                let (s, m) = (sync.clone(), target.id.clone());
                match this
                    .core
                    .call(async move { s.attachment(&m, &attachment_id).await })
                    .await
                {
                    Ok(data) => draft.attachments.push(OutgoingAttachment {
                        filename: attachment.filename,
                        mime_type: attachment.mime_type,
                        data,
                    }),
                    Err(err) => {
                        this.toast(&format!("Could not include {}: {err}", attachment.filename))
                    }
                }
            }
            app.compose(draft);
        });
    }

    fn edit_draft(self: &Rc<Self>) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let found = self.conversation.with_open(|open| {
            let draft = open
                .messages
                .iter()
                .rev()
                .find(|m| m.has_label("DRAFT"))?
                .clone();
            let body = match open.bodies.get(&draft.id) {
                Some(Ok(body)) => compose::body_text(body),
                _ => String::new(),
            };
            Some((
                open.account_id,
                open.thread_id.clone(),
                open.messages.len() > 1,
                draft,
                body,
            ))
        });
        let Some(Some((account_id, thread_id, in_thread, message, markdown))) = found else {
            return;
        };
        let Some(sync) = self.core.account(account_id) else {
            return;
        };
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let (s, m) = (sync.clone(), message.id.clone());
            let draft_id = this
                .core
                .call(async move { s.draft_id_for(&m).await })
                .await
                .ok()
                .flatten();
            let mut draft = Draft::new(account_id, app.identity(account_id));
            draft.to = message.to.clone();
            draft.cc = message.cc.clone();
            draft.subject = message.subject.clone();
            draft.markdown = markdown;
            draft.thread_id = in_thread.then_some(thread_id);
            draft.draft_id = draft_id;
            app.compose(draft);
        });
    }

    fn save_attachment(self: &Rc<Self>, message_id: String, index: usize) {
        let found = self.conversation.with_open(|open| {
            let body = open.bodies.get(&message_id)?.as_ref().ok()?;
            Some((open.account_id, body.attachments.get(index)?.clone()))
        });
        let Some(Some((account_id, attachment))) = found else {
            return;
        };
        let (Some(sync), Some(attachment_id)) = (
            self.core.account(account_id),
            attachment.attachment_id.clone(),
        ) else {
            return;
        };
        let downloads =
            glib::user_special_dir(glib::UserDirectory::Downloads).unwrap_or_else(glib::home_dir);
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            this.toast(&format!("Downloading {}…", attachment.filename));
            let filename = attachment.filename.clone();
            let saved = this
                .core
                .call(async move {
                    let data = sync.attachment(&message_id, &attachment_id).await?;
                    let path = unique_path(&downloads, &filename);
                    let target = path.clone();
                    tokio::task::spawn_blocking(move || std::fs::write(&target, data)).await??;
                    Ok::<PathBuf, anyhow::Error>(path)
                })
                .await;
            match saved {
                Ok(path) => {
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let toast = adw::Toast::builder()
                        .title(glib::markup_escape_text(&format!(
                            "Saved {name} to Downloads"
                        )))
                        .button_label("Open")
                        .timeout(6)
                        .build();
                    let window = this.window.clone();
                    toast.connect_button_clicked(move |_| {
                        let file = gio::File::for_path(&path);
                        gtk::FileLauncher::new(Some(&file)).launch(
                            Some(&window),
                            gio::Cancellable::NONE,
                            |_| {},
                        );
                    });
                    this.toasts.add_toast(toast);
                }
                Err(err) => this.toast(&format!("Could not save {}: {err}", attachment.filename)),
            }
        });
    }

    // ---- Accounts ----------------------------------------------------------

    fn save_config(self: &Rc<Self>, client_id: String, client_secret: String) {
        match self
            .core
            .save_config(mailrs_sync::config::Config::new(client_id, client_secret))
        {
            Ok(()) => self.refresh_accounts(),
            Err(err) => self.toast(&format!("Could not save the settings: {err}")),
        }
    }

    fn authorize(self: &Rc<Self>, expected: Option<String>) {
        if self.authorizing.replace(true) {
            return;
        }
        let (urls, opened) = async_channel::unbounded::<String>();
        let window = self.window.clone();
        glib::spawn_future_local(async move {
            while let Ok(url) = opened.recv().await {
                gtk::UriLauncher::new(&url).launch(Some(&window), gio::Cancellable::NONE, |_| {});
            }
        });
        self.first_account.set_sensitive(false);
        self.first_account.set_label("Waiting for Your Browser…");
        self.sidebar.add_account.set_sensitive(false);
        self.toast("Continue in your browser");
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            match this.core.authorize_account(urls, expected).await {
                Ok(account) => {
                    this.toast(&format!("Added {}. Downloading mail…", account.email));
                    this.refresh_accounts();
                }
                Err(err) => this.toast(&err.to_string()),
            }
            this.authorizing.set(false);
            this.first_account.set_sensitive(true);
            this.first_account.set_label("Sign In with Google");
            this.sidebar.add_account.set_sensitive(true);
        });
    }

    fn account(&self, account_id: AccountId) -> Option<Account> {
        self.accounts
            .borrow()
            .iter()
            .find(|a| a.id == account_id)
            .cloned()
    }

    fn confirm_remove(self: &Rc<Self>, account: Account) {
        let dialog = adw::AlertDialog::new(
            Some(&format!("Remove {}?", account.email)),
            Some(
                "Its downloaded mail and saved sign-in are deleted from this computer. Nothing changes in Gmail.",
            ),
        );
        dialog.add_responses(&[("cancel", "Cancel"), ("remove", "Remove")]);
        dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
        dialog.set_close_response("cancel");
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            if dialog.choose_future(Some(&this.window)).await != "remove" {
                return;
            }
            if this.conversation.with_open(|o| o.account_id) == Some(account.id) {
                this.conversation.clear();
            }
            let email = account.email.clone();
            match this.core.remove_account(account).await {
                Ok(()) => this.toast(&format!("Removed {email}")),
                Err(err) => this.toast(&format!("Could not remove {email}: {err}")),
            }
            this.refresh_accounts();
        });
    }

    // ---- Actions, menu, and keys -------------------------------------------

    fn install_actions(self: &Rc<Self>) {
        let add = |name: &str, run: WindowAction| {
            let action = gio::SimpleAction::new(name, None);
            let weak = Rc::downgrade(self);
            action.connect_activate(move |_, _| {
                if let Some(win) = weak.upgrade() {
                    run(&win);
                }
            });
            self.actions.add_action(&action);
        };
        add("compose", Box::new(|win| win.compose_new()));
        add("search", Box::new(|win| win.list.open_search()));
        add(
            "check",
            Box::new(|win| {
                win.core.poke_all();
                win.toast("Checking for mail");
            }),
        );
        add("add-account", Box::new(|win| win.authorize(None)));
        add("shortcuts", Box::new(|win| win.show_shortcuts()));
        add("reply", Box::new(|win| win.reply(ReplyKind::Reply)));
        add("reply-all", Box::new(|win| win.reply(ReplyKind::ReplyAll)));
        add("forward", Box::new(|win| win.reply(ReplyKind::Forward)));
        add(
            "archive",
            Box::new(|win| win.triage_open(TriageAction::Archive)),
        );
        add(
            "trash",
            Box::new(|win| win.triage_open(TriageAction::Trash)),
        );
        add("toggle-star", Box::new(|win| win.act(Action::ToggleStar)));
        add("toggle-read", Box::new(|win| win.act(Action::ToggleRead)));
        add("about", Box::new(|win| win.show_about()));
        add(
            "quit",
            Box::new(|win| {
                if let Some(app) = win.app.upgrade() {
                    app.quit();
                }
            }),
        );

        let with_account = |name: &str, run: AccountAction| {
            let action = gio::SimpleAction::new(name, Some(glib::VariantTy::INT64));
            let weak = Rc::downgrade(self);
            action.connect_activate(move |_, parameter| {
                let (Some(win), Some(id)) =
                    (weak.upgrade(), parameter.and_then(|p| p.get::<i64>()))
                else {
                    return;
                };
                if let Some(account) = win.account(id) {
                    run(&win, account);
                }
            });
            self.actions.add_action(&action);
        };
        with_account(
            "account-check",
            Box::new(|win, account| win.core.poke(account.id)),
        );
        with_account(
            "account-reconnect",
            Box::new(|win, account| win.authorize(Some(account.email))),
        );
        with_account(
            "account-remove",
            Box::new(|win, account| win.confirm_remove(account)),
        );

        let shortcuts = gtk::ShortcutController::new();
        shortcuts.set_scope(gtk::ShortcutScope::Global);
        for (trigger, action) in [
            ("<Control>n", "win.compose"),
            ("<Control>f", "win.search"),
            ("F5", "win.check"),
            ("<Control>r", "win.check"),
            ("<Control>question", "win.shortcuts"),
            ("<Control>q", "win.quit"),
            ("<Control>w", "window.close"),
        ] {
            shortcuts.add_shortcut(gtk::Shortcut::new(
                gtk::ShortcutTrigger::parse_string(trigger),
                Some(gtk::NamedAction::new(action)),
            ));
        }
        self.window.add_controller(shortcuts);

        let weak = Rc::downgrade(self);
        self.window.connect_close_request(move |_| {
            if let (Some(win), Some(app)) =
                (weak.upgrade(), weak.upgrade().and_then(|w| w.app.upgrade()))
            {
                app.forget_window(&win);
            }
            glib::Propagation::Proceed
        });
    }

    fn install_menu(&self) {
        let menu = gio::Menu::new();
        let first = gio::Menu::new();
        first.append(Some("Check for Mail"), Some("win.check"));
        first.append(Some("Add Account…"), Some("win.add-account"));
        menu.append_section(None, &first);
        let second = gio::Menu::new();
        second.append(Some("Keyboard Shortcuts"), Some("win.shortcuts"));
        second.append(Some("About mailrs"), Some("win.about"));
        second.append(Some("Quit"), Some("win.quit"));
        menu.append_section(None, &second);
        self.sidebar.header.pack_end(
            &gtk::MenuButton::builder()
                .icon_name("open-menu-symbolic")
                .menu_model(&menu)
                .primary(true)
                .tooltip_text("Main Menu")
                .build(),
        );
    }

    fn install_keys(self: &Rc<Self>) {
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, key, _, modifiers| {
            let Some(win) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            let blocked = gdk::ModifierType::CONTROL_MASK
                | gdk::ModifierType::ALT_MASK
                | gdk::ModifierType::SUPER_MASK;
            if modifiers.intersects(blocked)
                || win.typing()
                || win.stack.visible_child_name().as_deref() != Some("mail")
            {
                return glib::Propagation::Proceed;
            }
            match key.to_unicode() {
                Some('j') => win.list.step(1),
                Some('k') => win.list.step(-1),
                Some('e') => win.triage_open(TriageAction::Archive),
                Some('#') => win.triage_open(TriageAction::Trash),
                Some('s') => win.act(Action::ToggleStar),
                Some('u') => win.act(Action::ToggleRead),
                Some('r') => win.reply(ReplyKind::Reply),
                Some('a') => win.reply(ReplyKind::ReplyAll),
                Some('f') => win.reply(ReplyKind::Forward),
                Some('c') => win.compose_new(),
                Some('/') => win.list.open_search(),
                _ => return glib::Propagation::Proceed,
            }
            glib::Propagation::Stop
        });
        self.window.add_controller(keys);
    }

    fn typing(&self) -> bool {
        let Some(focus) = GtkWindowExt::focus(&self.window) else {
            return false;
        };
        focus.is::<gtk::Text>()
            || focus.is::<gtk::TextView>()
            || focus.dynamic_cast_ref::<gtk::Editable>().is_some()
    }

    fn compose_new(self: &Rc<Self>) {
        let (Some(app), Some(account_id)) = (self.app.upgrade(), self.default_account()) else {
            return self.toast("Add an account first");
        };
        app.compose(Draft::new(account_id, app.identity(account_id)));
    }

    /// Opens a thread from outside the window, such as a notification.
    pub fn reveal(self: &Rc<Self>, account_id: AccountId, thread_id: String) {
        let inbox = Mailbox::Unified("INBOX");
        if *self.mailbox.borrow() != inbox {
            self.sidebar.select(&inbox);
            self.show_mailbox(inbox);
        }
        let this = Rc::clone(self);
        glib::timeout_add_local_once(std::time::Duration::from_millis(250), move || {
            this.list.select(account_id, &thread_id);
        });
    }

    /// Screenshot hooks, honoured only in demo mode: `MAILRS_DEMO_OPEN`
    /// opens a thread by id, `MAILRS_DEMO_SEARCH` runs a search,
    /// `MAILRS_DEMO_COMPOSE=reply` opens a reply to the open thread, and
    /// `MAILRS_DEMO_ACTION` activates a window action such as `shortcuts`.
    pub fn run_demo_script(self: &Rc<Self>) {
        if !self.core.demo {
            return;
        }
        let this = Rc::clone(self);
        glib::timeout_add_local_once(std::time::Duration::from_millis(900), move || {
            if let Ok(action) = std::env::var("MAILRS_DEMO_ACTION") {
                let _ = WidgetExt::activate_action(&this.window, &format!("win.{action}"), None);
            }
            if let Ok(query) = std::env::var("MAILRS_DEMO_SEARCH") {
                this.list.open_search();
                this.list.search_entry.set_text(&query);
                this.search(query);
            }
            if let Ok(thread_id) = std::env::var("MAILRS_DEMO_OPEN") {
                let rows = this.accounts.borrow().clone();
                let finder = Rc::clone(&this);
                glib::spawn_future_local(async move {
                    let key = thread_id.clone();
                    let found = finder
                        .core
                        .read(move |c| {
                            Ok(rusqlite::OptionalExtension::optional(c.query_row(
                                "SELECT account_id FROM threads WHERE id = ?1",
                                [&key],
                                |r| r.get::<_, i64>(0),
                            ))?)
                        })
                        .await;
                    if let Ok(Some(account_id)) = found {
                        let _ = rows;
                        finder.list.select(account_id, &thread_id);
                        if std::env::var("MAILRS_DEMO_COMPOSE").as_deref() == Ok("reply") {
                            let replier = Rc::clone(&finder);
                            glib::timeout_add_local_once(
                                std::time::Duration::from_millis(900),
                                move || {
                                    replier.reply(ReplyKind::Reply);
                                },
                            );
                        }
                    }
                });
            }
        });
    }

    pub fn toast_sent(&self) {
        self.toast("Message sent");
    }

    fn show_shortcuts(&self) {
        let dialog = adw::ShortcutsDialog::new();
        let groups: [(&str, &[(&str, &str)]); 3] = [
            (
                "Reading",
                &[
                    ("Next conversation", "j"),
                    ("Previous conversation", "k"),
                    ("Search", "slash"),
                    ("Check for mail", "F5"),
                ],
            ),
            (
                "Triage",
                &[
                    ("Archive", "e"),
                    ("Move to trash", "numbersign"),
                    ("Star or unstar", "s"),
                    ("Mark read or unread", "u"),
                ],
            ),
            (
                "Writing",
                &[
                    ("New message", "c"),
                    ("Reply", "r"),
                    ("Reply all", "a"),
                    ("Forward", "f"),
                    ("Send", "<Control>Return"),
                    ("Save draft", "<Control>s"),
                ],
            ),
        ];
        for (title, items) in groups {
            let section = adw::ShortcutsSection::new(Some(title));
            for (label, accel) in items {
                section.add(adw::ShortcutsItem::new(label, accel));
            }
            dialog.add(section);
        }
        dialog.present(Some(&self.window));
    }

    fn show_about(&self) {
        let about = adw::AboutDialog::builder()
            .application_name("mailrs")
            .application_icon(crate::APP_ID)
            .version(env!("CARGO_PKG_VERSION"))
            .developer_name("David Santos")
            .comments("A fast, private Gmail client for the GNOME desktop. Mail stays on your computer and your own Google Cloud project.")
            .build();
        about.present(Some(&self.window));
    }
}

fn read_cached_body(
    c: &rusqlite::Connection,
    account_id: AccountId,
    message_id: &str,
) -> mailrs_store::Result<Option<MessageBody>> {
    // Readers are read-only, so this peeks at the cache without touching
    // its access time; the body fetch that follows records the access.
    let mut stmt = c.prepare_cached(
        "SELECT html, text FROM bodies WHERE account_id = ?1 AND message_id = ?2",
    )?;
    let row: Option<(Option<String>, Option<String>)> = rusqlite::OptionalExtension::optional(
        stmt.query_row(rusqlite::params![account_id, message_id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        }),
    )?;
    let Some((html, text)) = row else {
        return Ok(None);
    };
    let mut stmt = c.prepare_cached(
        "SELECT part_id, filename, mime_type, size, attachment_id, content_id FROM attachments \
         WHERE account_id = ?1 AND message_id = ?2 ORDER BY part_id",
    )?;
    let attachments = stmt
        .query_map(rusqlite::params![account_id, message_id], |r| {
            Ok(mailrs_domain::Attachment {
                part_id: r.get(0)?,
                filename: r.get(1)?,
                mime_type: r.get(2)?,
                size: r.get(3)?,
                attachment_id: r.get(4)?,
                content_id: r.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(Some(MessageBody {
        html,
        text,
        attachments,
    }))
}

/// Unread messages and the newest message start expanded.
fn default_expanded(messages: &[MessageMeta]) -> HashSet<String> {
    let mut expanded: HashSet<String> = messages
        .iter()
        .filter(|m| m.is_unread())
        .map(|m| m.id.clone())
        .collect();
    if let Some(last) = messages.last() {
        expanded.insert(last.id.clone());
    }
    expanded
}

fn empty_state(mailbox: &Mailbox) -> (&'static str, &'static str) {
    let label = match mailbox {
        Mailbox::Unified(label) => *label,
        Mailbox::Label { label_id, .. } => label_id.as_str(),
        Mailbox::Search { .. } => return ("No Results", "system-search-symbolic"),
    };
    match label {
        "INBOX" => ("Inbox Zero", "mailrs-inbox-symbolic"),
        "STARRED" => ("No Starred Mail", "starred-symbolic"),
        "SENT" => ("No Sent Mail", "mail-send-symbolic"),
        "DRAFT" => ("No Drafts", "document-edit-symbolic"),
        _ => ("No Mail", "mailrs-tag-symbolic"),
    }
}

/// `dir/name`, or `dir/name (2).ext` and so on when that exists.
fn unique_path(dir: &std::path::Path, name: &str) -> PathBuf {
    let clean: String = name
        .chars()
        .map(|c| if c == '/' || c == '\0' { '_' } else { c })
        .collect();
    let clean = if clean.trim().is_empty() || clean == "." || clean == ".." {
        "attachment".to_string()
    } else {
        clean
    };
    let candidate = dir.join(&clean);
    if !candidate.exists() {
        return candidate;
    }
    let (stem, ext) = match clean.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem.to_string(), format!(".{ext}")),
        _ => (clean.clone(), String::new()),
    };
    (2..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|p| !p.exists())
        .expect("some name is free")
}
