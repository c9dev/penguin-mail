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
use super::thread_list::{Picked, ThreadList};
use super::{Folder, Mailbox, summarize_search, welcome};
use crate::app::App;
use crate::assistant::ToolRequest;
use crate::compose::{self, Draft, OutgoingAttachment, ReplyKind};
use crate::core::Core;
use crate::settings::{Choice, MarkRead, RemoteImages, Settings, TextSize};

mod arrange;
mod assistant;
mod detached;
mod flags;
mod organize;
mod reminders;
mod scheduled;
mod senders;

/// Largest inline image embedded into a page.
const INLINE_IMAGE_LIMIT: usize = 5 * 1024 * 1024;

type WindowAction = Box<dyn Fn(&Rc<MainWindow>)>;
/// Work to do once a triage action has reached Gmail.
type AfterApply = Box<dyn FnOnce(&Rc<MainWindow>, &[Target])>;
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
    /// How to reverse the last organizing action.
    undo: RefCell<Option<(Vec<Target>, TriageAction)>>,
    labels: RefCell<HashMap<AccountId, Vec<Label>>>,
    assistant: Rc<super::assistant::AssistantPane>,
    assistant_split: adw::OverlaySplitView,
}

/// One thing an action applies to: a thread, or one message of it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Target {
    account_id: AccountId,
    thread_id: String,
    message_id: Option<String>,
}

impl Target {
    fn from_row(row: &ThreadSummary) -> Target {
        Target {
            account_id: row.account_id,
            thread_id: row.id.clone(),
            message_id: row.message_id.clone(),
        }
    }
}

/// The toast after an action, or `None` when the change speaks for itself.
fn done_message(action: &TriageAction, count: usize, threaded: bool) -> Option<String> {
    let noun = match (threaded, count) {
        (true, 1) => "conversation",
        (true, _) => "conversations",
        (false, 1) => "message",
        (false, _) => "messages",
    };
    let many = count > 1;
    Some(match action {
        TriageAction::Archive if many => format!("Archived {count} {noun}"),
        TriageAction::Archive => "Archived".into(),
        TriageAction::Trash if many => format!("Moved {count} {noun} to Trash"),
        TriageAction::Trash => "Moved to Trash".into(),
        TriageAction::Junk if many => format!("Marked {count} {noun} as junk"),
        TriageAction::Junk => "Marked as junk".into(),
        TriageAction::Untrash | TriageAction::NotJunk if many => {
            format!("Moved {count} {noun} to the Inbox")
        }
        TriageAction::Untrash | TriageAction::NotJunk => "Moved to the Inbox".into(),
        TriageAction::AddLabel(_) | TriageAction::RemoveLabel(_) | TriageAction::Relabel { .. } => {
            "Labels changed".into()
        }
        _ => return None,
    })
}

impl MainWindow {
    pub fn new(app: &Rc<App>) -> Rc<MainWindow> {
        let (tool_requests, tool_calls) = async_channel::unbounded::<ToolRequest>();
        let window = Rc::new_cyclic(|weak: &Weak<MainWindow>| {
            let w = weak.clone();
            let d = weak.clone();
            let sidebar = Sidebar::new(
                move |mailbox| {
                    if let Some(win) = w.upgrade() {
                        win.show_mailbox(mailbox);
                    }
                },
                move |mailbox| d.upgrade().is_some_and(|win| win.drop_on(mailbox)),
            );
            let (w, s) = (weak.clone(), weak.clone());
            let list = ThreadList::new(
                move |picked| {
                    if let Some(win) = w.upgrade() {
                        win.picked(picked);
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
            let (s, w) = (Rc::downgrade(app), weak.clone());
            let assistant = super::assistant::AssistantPane::new(
                Rc::clone(&app.core),
                tool_requests.clone(),
                move || s.upgrade().map(|a| a.settings()).unwrap_or_default(),
                move || {
                    if let Some(win) = w.upgrade() {
                        win.show_preferences_page("assistant");
                    }
                },
            );
            let assistant_split = adw::OverlaySplitView::builder()
                .sidebar(&assistant.page)
                .content(&split)
                .sidebar_position(gtk::PackType::End)
                .show_sidebar(false)
                .min_sidebar_width(320.0)
                .max_sidebar_width(460.0)
                .sidebar_width_fraction(0.3)
                .build();
            assistant_split
                .bind_property("show-sidebar", &list.assistant_button, "active")
                .bidirectional()
                .sync_create()
                .build();
            let stack = gtk::Stack::builder()
                .transition_type(gtk::StackTransitionType::Crossfade)
                .build();
            stack.add_named(&assistant_split, Some("mail"));
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
            narrow.add_setter(&assistant_split, "collapsed", Some(&true.to_value()));
            medium.add_setter(&assistant_split, "collapsed", Some(&true.to_value()));
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
                undo: RefCell::new(None),
                labels: RefCell::new(HashMap::new()),
                assistant,
                assistant_split,
            }
        });
        if window.core.demo {
            window.sidebar.start_expanded.set(Some(true));
        }
        let weak = Rc::downgrade(&window);
        window
            .conversation
            .label_button
            .set_create_popup_func(move |button| {
                if let Some(win) = weak.upgrade() {
                    button.set_popover(Some(&win.label_popover()));
                }
            });
        let weak = Rc::downgrade(&window);
        window.list.connect_open(move |row| {
            if let Some(win) = weak.upgrade() {
                win.open_in_window(row);
            }
        });
        window.install_actions();
        window.install_arrange_actions();
        // The assistant's tool calls, one at a time, on this thread.
        let weak = Rc::downgrade(&window);
        glib::spawn_future_local(async move {
            while let Ok(request) = tool_calls.recv().await {
                let Some(win) = weak.upgrade() else { break };
                let outcome = win.run_tool(&request.name, request.input).await;
                let _ = request.reply.send(outcome).await;
            }
        });
        let labels_of = Rc::downgrade(&window);
        super::search_suggest::attach(&window.list.search_entry, app.contacts(), move || {
            let Some(win) = labels_of.upgrade() else {
                return Vec::new();
            };
            let mut names: Vec<String> = win
                .labels
                .borrow()
                .values()
                .flatten()
                .filter(|l| l.kind == mailrs_domain::LabelKind::User)
                .map(|l| l.name.clone())
                .collect();
            names.sort_by_key(|n| n.to_lowercase());
            names.dedup();
            names
        });
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
        window
            .conversation
            .set_zoom(app.settings().text_size.zoom());
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
            *this.labels.borrow_mut() = data.iter().map(|(a, l)| (a.id, l.clone())).collect();
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
                Mailbox::Label { account_id, .. }
                | Mailbox::Folder {
                    account_id: Some(account_id),
                    ..
                } => data.iter().any(|(a, _)| a.id == *account_id),
                _ => true,
            };
            if !still_exists {
                *this.mailbox.borrow_mut() = Mailbox::Unified("INBOX");
            }
            let settings = this.settings();
            let (data, extras) = this.arrange(data, &settings);
            this.list.set_vips(settings.vips.keys().cloned().collect());
            if !matches!(mailbox, Mailbox::Search { .. }) {
                this.sidebar.rebuild(&data, &extras, &this.mailbox.borrow());
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
                    counts.insert(
                        Mailbox::Scheduled,
                        mailrs_store::scheduled::list(c)?.len() as i64,
                    );
                    counts.insert(
                        Mailbox::Reminders,
                        mailrs_store::reminders::list(c)?.len() as i64,
                    );
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
        self.conversation.set_folder(mailbox.folder());
        match mailbox {
            Mailbox::Folder { account_id, folder } => {
                let (title, icon) = empty_state(&mailbox);
                self.fetch_remote(folder.query().into(), account_id, 100, title, icon);
            }
            Mailbox::Smart { id, .. } => self.show_smart(&id),
            _ => self.reload_list(),
        }
    }

    /// Fetches a folder that lives only in Gmail again.
    fn reload_folder(self: &Rc<Self>) {
        let mailbox = self.mailbox.borrow().clone();
        if let Mailbox::Smart { id, .. } = &mailbox {
            return self.show_smart(id);
        }
        if let Mailbox::Folder { account_id, folder } = mailbox {
            let (title, icon) = empty_state(&mailbox);
            self.fetch_remote(folder.query().into(), account_id, 100, title, icon);
        }
    }

    fn reload_list(self: &Rc<Self>) {
        let mailbox = self.mailbox.borrow().clone();
        if matches!(mailbox, Mailbox::Scheduled | Mailbox::Reminders) {
            let generation = self.list_generation.get() + 1;
            self.list_generation.set(generation);
            return match mailbox {
                Mailbox::Reminders => self.load_reminders(generation),
                _ => self.load_scheduled(generation),
            };
        }
        let Some(filter) = mailbox.filter() else {
            return;
        };
        let generation = self.list_generation.get() + 1;
        self.list_generation.set(generation);
        let threaded = self.settings().threading;
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let loaded = this
                .core
                .read(move |c| {
                    Ok(if threaded {
                        (
                            threads::list_threads(c, &filter, 0, 10_000)?,
                            threads::unread_threads(c, &filter)?,
                        )
                    } else {
                        (
                            threads::list_messages(c, &filter, 0, 10_000)?,
                            threads::unread_messages(c, &filter)?,
                        )
                    })
                })
                .await;
            if this.list_generation.get() != generation {
                return;
            }
            match loaded {
                Ok((rows, unread)) => {
                    let (title, icon) = empty_state(&mailbox);
                    this.list.set_rows(rows, title, icon);
                    this.follow_selection();
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
        self.sidebar.clear_selection();
        self.list.set_title("Search", &query);
        self.conversation.clear();
        self.conversation.set_folder(None);
        self.fetch_remote(query, scope, 50, "No Results", "system-search-symbolic");
    }

    /// Lists what a Gmail search finds, in every account or in `scope`.
    fn fetch_remote(
        self: &Rc<Self>,
        query: String,
        scope: Option<AccountId>,
        limit: usize,
        empty_title: &'static str,
        empty_icon: &'static str,
    ) {
        self.list_generation.set(self.list_generation.get() + 1);
        let generation = self.list_generation.get();
        self.list.show_loading();
        let targets: Vec<Account> = self
            .accounts
            .borrow()
            .iter()
            .filter(|a| scope.is_none_or(|id| a.id == id))
            .cloned()
            .collect();
        let threaded = self.settings().threading;
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let searches = targets.iter().map(|account| {
                let (core, query) = (Rc::clone(&this.core), query.clone());
                let sync = core.account(account.id);
                async move {
                    match sync {
                        Some(sync) => {
                            core.call(async move { sync.search(&query, limit).await })
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
                    Err(err) => {
                        this.toast(&format!("Could not load mail for {}: {err}", account.email))
                    }
                }
            }
            this.list
                .set_rows(summarize_search(hits, threaded), empty_title, empty_icon);
            this.follow_selection();
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
        if self.conversation.is_showing_row(&summary) {
            return;
        }
        self.load_into(Rc::clone(&self.conversation), summary);
    }

    /// Shows the thread `summary` names in `view`: the stored copy first,
    /// then the whole thread and its bodies.
    pub(super) fn load_into(self: &Rc<Self>, view: Rc<ConversationView>, summary: ThreadSummary) {
        let (account_id, thread_id) = (summary.account_id, summary.id.clone());
        let only = summary.message_id.clone();
        let images_allowed = self.settings().remote_images == RemoteImages::Always;
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
            let (mut found, cached) = local.unwrap_or_default();
            if let Some(id) = &only {
                found.retain(|m| &m.id == id);
            }
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
                images_allowed,
                only_message: only,
                me,
                inline_images: HashMap::new(),
                unsubscribed: false,
                flag_color: summary.flag_color,
            };
            view.show(thread, true);
            view.set_sender_vip(sender_is_vip(&view, &this.settings()));
            this.complete_thread(view, account_id, thread_id).await;
        });
    }

    /// Fetches the whole thread and any missing bodies, then marks it read.
    async fn complete_thread(
        self: &Rc<Self>,
        view: Rc<ConversationView>,
        account_id: AccountId,
        thread_id: String,
    ) {
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
        if !view.is_showing(account_id, &thread_id) {
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
                let fresh: Vec<MessageMeta> = fresh
                    .iter()
                    .filter(|m| open.only_message.as_ref().is_none_or(|id| &m.id == id))
                    .cloned()
                    .collect();
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
        if !view.is_showing(account_id, &thread_id) {
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
        view.render(false);
        if unread {
            self.mark_read_later(&view, account_id, thread_id);
        }
    }

    /// Marks the open thread or message read, when the setting says so.
    fn mark_read_later(
        self: &Rc<Self>,
        view: &Rc<ConversationView>,
        account_id: AccountId,
        thread_id: String,
    ) {
        let delay = match self.settings().mark_read {
            MarkRead::Immediately => 0,
            MarkRead::AfterDelay => 2,
            MarkRead::Manually => return,
        };
        let only = view.with_open(|o| o.only_message.clone()).flatten();
        let (weak, view) = (Rc::downgrade(self), Rc::downgrade(view));
        glib::timeout_add_seconds_local_once(delay, move || {
            let (Some(win), Some(view)) = (weak.upgrade(), view.upgrade()) else {
                return;
            };
            let still_open = view
                .with_open(|o| {
                    o.account_id == account_id && o.thread_id == thread_id && o.only_message == only
                })
                .unwrap_or(false);
            if still_open {
                let target = Target {
                    account_id,
                    thread_id,
                    message_id: only,
                };
                win.apply(vec![target], TriageAction::MarkRead, false);
            }
        });
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
            let only = this
                .conversation
                .with_open(|o| o.only_message.clone())
                .flatten();
            let fresh: Vec<MessageMeta> = fresh
                .into_iter()
                .filter(|m| only.as_ref().is_none_or(|id| &m.id == id))
                .collect();
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
                this.complete_thread(Rc::clone(&this.conversation), account_id, thread_id)
                    .await;
            } else {
                this.conversation.render_buttons();
            }
        });
    }

    // ---- Actions on the selection or the open conversation -----------------

    fn picked(self: &Rc<Self>, picked: Picked) {
        match picked {
            Picked::One(row) => self.open_thread(row),
            Picked::Many(rows) => {
                let noun = if self.settings().threading {
                    "Conversations"
                } else {
                    "Messages"
                };
                self.conversation.show_many(
                    rows.len(),
                    noun,
                    rows.iter().any(|r| r.unread),
                    rows.iter().all(|r| r.starred),
                );
            }
            Picked::None => self.conversation.clear(),
        }
    }

    /// Updates the bulk page after the list changed under a multiple selection.
    fn follow_selection(self: &Rc<Self>) {
        match self.list.picked() {
            Picked::Many(rows) => self.picked(Picked::Many(rows)),
            picked if self.conversation.showing_many() => self.picked(picked),
            _ => {}
        }
    }

    /// What an action applies to: every selected row when several are
    /// selected, otherwise the open conversation.
    fn targets(&self) -> Vec<Target> {
        let rows = self.list.selected_rows();
        if rows.len() > 1 {
            return rows.iter().map(Target::from_row).collect();
        }
        self.conversation
            .with_open(|o| Target {
                account_id: o.account_id,
                thread_id: o.thread_id.clone(),
                message_id: o.only_message.clone(),
            })
            .map(|t| vec![t])
            .unwrap_or_else(|| rows.iter().map(Target::from_row).collect())
    }

    /// Whether any target is unread, and whether every target is starred.
    fn target_marks(&self) -> (bool, bool) {
        let rows = self.list.selected_rows();
        if rows.len() > 1 {
            return (
                rows.iter().any(|r| r.unread),
                rows.iter().all(|r| r.starred),
            );
        }
        self.conversation
            .with_open(|o| (o.unread(), o.starred()))
            .or_else(|| rows.first().map(|r| (r.unread, r.starred)))
            .unwrap_or((false, false))
    }

    fn act(self: &Rc<Self>, action: Action) {
        match action {
            Action::Reply(kind) => self.reply(kind),
            Action::EditDraft => self.edit_draft(),
            Action::Archive => self.triage(TriageAction::Archive),
            Action::Trash => self.trash(),
            Action::Junk => self.triage(match self.mailbox.borrow().folder() {
                Some(Folder::Junk) => TriageAction::NotJunk,
                _ => TriageAction::Junk,
            }),
            Action::ToggleStar => self.toggle_flag(),
            Action::ToggleRead => {
                let (unread, _) = self.target_marks();
                self.triage(if unread {
                    TriageAction::MarkRead
                } else {
                    TriageAction::MarkUnread
                });
            }
            Action::Unsubscribe => self.unsubscribe(),
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
                    app.compose(app.signed(draft));
                }
            }
        }
    }

    /// The account a new message comes from: the one set in Preferences,
    /// else the account in view, else the first.
    fn default_account(&self) -> Option<AccountId> {
        let preferred = self.settings().default_account;
        preferred
            .and_then(|email| {
                self.accounts
                    .borrow()
                    .iter()
                    .find(|a| a.email.eq_ignore_ascii_case(&email))
                    .map(|a| a.id)
            })
            .or_else(|| self.conversation.with_open(|o| o.account_id))
            .or_else(|| self.mailbox.borrow().account())
            .or_else(|| self.accounts.borrow().first().map(|a| a.id))
    }

    /// Applies `action` to the targets and keeps an undo for it. Actions that
    /// take mail out of the list move on to the next row, as Apple Mail does.
    fn triage(self: &Rc<Self>, action: TriageAction) {
        let targets = self.targets();
        if targets.is_empty() {
            return;
        }
        if self.leaves_list(&action) {
            let next = self.list.neighbour_of_selected();
            self.conversation.clear();
            self.list.unselect();
            match next {
                Some(next) => {
                    self.list
                        .select(next.account_id, &next.id, next.message_id.as_deref())
                }
                None => self.nav.set_show_content(false),
            }
        }
        self.apply(targets, action, true);
    }

    /// In the Trash, the trash button puts mail back in the inbox. Gmail
    /// empties the Trash itself after 30 days.
    /// The Delete key. Gmail's permission for mailrs covers moving mail to
    /// the Trash, not erasing it, so inside the Trash it only explains that.
    fn delete_key(self: &Rc<Self>) {
        if *self.mailbox.borrow() == Mailbox::Scheduled {
            return self.cancel_scheduled(self.targets());
        }
        if *self.mailbox.borrow() == Mailbox::Reminders {
            return self.cancel_reminders(self.targets());
        }
        if self.mailbox.borrow().folder() == Some(Folder::Trash) {
            self.toast("Gmail deletes mail in the Trash for good after 30 days");
        } else {
            self.triage(TriageAction::Trash);
        }
    }

    fn trash(self: &Rc<Self>) {
        if *self.mailbox.borrow() == Mailbox::Scheduled {
            return self.cancel_scheduled(self.targets());
        }
        if *self.mailbox.borrow() == Mailbox::Reminders {
            return self.cancel_reminders(self.targets());
        }
        if self.mailbox.borrow().folder() == Some(Folder::Trash) {
            self.triage(TriageAction::Untrash);
        } else {
            self.triage(TriageAction::Trash);
        }
    }

    /// Whether `action` takes the targets out of the list on screen.
    fn leaves_list(&self, action: &TriageAction) -> bool {
        let folder = self.mailbox.borrow().folder();
        match action {
            TriageAction::Archive => folder != Some(Folder::AllMail),
            TriageAction::Trash => folder != Some(Folder::Trash),
            TriageAction::Junk => folder != Some(Folder::Junk),
            TriageAction::Untrash => folder == Some(Folder::Trash),
            TriageAction::NotJunk => folder == Some(Folder::Junk),
            _ => false,
        }
    }

    /// Drops rows that no longer belong in the Gmail folder on screen. The
    /// local store cannot list these folders, so rows go one by one.
    fn prune_folder(self: &Rc<Self>, targets: &[Target]) {
        let Some(folder) = self.mailbox.borrow().folder() else {
            return;
        };
        let targets = targets.to_vec();
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let gone = this
                .core
                .read(move |c| {
                    let mut gone = Vec::new();
                    for target in targets {
                        let held =
                            messages::thread_messages(c, target.account_id, &target.thread_id)?
                                .iter()
                                .filter(|m| target.message_id.as_ref().is_none_or(|id| &m.id == id))
                                .any(|m| folder.holds(&m.label_ids));
                        if !held {
                            gone.push(target);
                        }
                    }
                    Ok(gone)
                })
                .await
                .unwrap_or_default();
            if gone.is_empty() {
                return;
            }
            this.list
                .retain(|row| !gone.contains(&Target::from_row(row)));
            if this.conversation.with_open(|o| {
                gone.iter()
                    .any(|t| t.account_id == o.account_id && t.thread_id == o.thread_id)
            }) == Some(true)
            {
                this.conversation.clear();
            }
        });
    }

    /// Runs `action` on every target. With `record`, offers an undo.
    fn apply(self: &Rc<Self>, targets: Vec<Target>, action: TriageAction, record: bool) {
        self.apply_with(targets, action, record, None);
    }

    /// `apply`, with `message` in place of the usual toast text.
    fn apply_with(
        self: &Rc<Self>,
        targets: Vec<Target>,
        action: TriageAction,
        record: bool,
        message: Option<String>,
    ) {
        self.apply_then(targets, action, record, message, None);
    }

    /// `apply_with`, running `after` once Gmail has the change.
    fn apply_then(
        self: &Rc<Self>,
        targets: Vec<Target>,
        action: TriageAction,
        record: bool,
        message: Option<String>,
        after: Option<AfterApply>,
    ) {
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let mut failure = None;
            for target in &targets {
                let Some(sync) = this.core.account(target.account_id) else {
                    continue;
                };
                let (thread, message, act) = (
                    target.thread_id.clone(),
                    target.message_id.clone(),
                    action.clone(),
                );
                let result = this
                    .core
                    .call(async move {
                        match message {
                            Some(id) => sync.triage_message(&thread, &id, &act).await,
                            None => sync.triage_thread(&thread, &act).await,
                        }
                    })
                    .await;
                if let Err(err) = result {
                    failure = Some(err);
                }
            }
            if let Some(err) = failure {
                return this.toast(&format!("{} failed: {err}", action.describe()));
            }
            if let Some(after) = after {
                after(&this, &targets);
            }
            if !record {
                // An undo can put rows back into a Gmail folder.
                this.reload_folder();
                return;
            }
            this.prune_folder(&targets);
            let count = targets.len();
            *this.undo.borrow_mut() = Some((targets, action.inverse()));
            if let Some(done) =
                message.or_else(|| done_message(&action, count, this.settings().threading))
            {
                let toast = adw::Toast::builder()
                    .title(done)
                    .button_label("Undo")
                    .timeout(5)
                    .build();
                let weak = Rc::downgrade(&this);
                toast.connect_button_clicked(move |_| {
                    if let Some(win) = weak.upgrade() {
                        win.undo();
                    }
                });
                this.toasts.add_toast(toast);
            }
        });
    }

    /// Reverses the last organizing action, once.
    fn undo(self: &Rc<Self>) {
        let last = self.undo.borrow_mut().take();
        match last {
            Some((targets, inverse)) => {
                self.apply(targets, inverse, false);
                self.toast("Undone");
            }
            None => self.toast("Nothing to undo"),
        }
    }

    /// Labels of the targets' account, checked when the one open
    /// conversation already has them.
    fn label_popover(self: &Rc<Self>) -> gtk::Popover {
        let popover = gtk::Popover::new();
        let targets = self.targets();
        let accounts: HashSet<AccountId> = targets.iter().map(|t| t.account_id).collect();
        let message = |text: &str| {
            gtk::Label::builder()
                .label(text)
                .wrap(true)
                .max_width_chars(28)
                .margin_top(12)
                .margin_bottom(12)
                .margin_start(12)
                .margin_end(12)
                .build()
        };
        let Some(&account_id) = accounts.iter().next().filter(|_| accounts.len() == 1) else {
            popover.set_child(Some(&message(if targets.is_empty() {
                "Open or select mail to label it."
            } else {
                "Select mail from one account to label it."
            })));
            return popover;
        };
        let mut labels: Vec<Label> = self
            .labels
            .borrow()
            .get(&account_id)
            .map(|all| {
                all.iter()
                    .filter(|l| l.kind == mailrs_domain::LabelKind::User)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        labels.sort_by_key(|l| l.name.to_lowercase());
        let create = gtk::Button::builder()
            .child(
                &adw::ButtonContent::builder()
                    .icon_name("list-add-symbolic")
                    .label("New Label…")
                    .build(),
            )
            .css_classes(["flat"])
            .margin_top(4)
            .build();
        let (weak, pop) = (Rc::downgrade(self), popover.clone());
        create.connect_clicked(move |_| {
            pop.popdown();
            if let Some(win) = weak.upgrade() {
                win.new_label(
                    account_id,
                    Some(Box::new(|win, label_id| {
                        win.triage(TriageAction::AddLabel(label_id))
                    })),
                );
            }
        });
        if labels.is_empty() {
            let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
            content.append(&message("This account has no labels yet."));
            content.append(&create);
            popover.set_child(Some(&content));
            return popover;
        }
        let applied: HashSet<String> = if targets.len() == 1 {
            self.conversation
                .with_open(|o| {
                    o.messages
                        .iter()
                        .flat_map(|m| m.label_ids.clone())
                        .collect()
                })
                .unwrap_or_default()
        } else {
            HashSet::new()
        };
        let list = gtk::ListBox::builder()
            .css_classes(["navigation-sidebar"])
            .selection_mode(gtk::SelectionMode::None)
            .build();
        for label in &labels {
            let row = gtk::Box::builder().spacing(10).build();
            let check = gtk::Image::from_icon_name("object-select-symbolic");
            check.set_opacity(if applied.contains(&label.id) {
                1.0
            } else {
                0.0
            });
            row.append(&check);
            row.append(
                &gtk::Label::builder()
                    .label(label.name.replace('/', " › "))
                    .xalign(0.0)
                    .build(),
            );
            list.append(
                &gtk::ListBoxRow::builder()
                    .child(&row)
                    .activatable(true)
                    .build(),
            );
        }
        let (weak, pop) = (Rc::downgrade(self), popover.clone());
        list.connect_row_activated(move |_, row| {
            let (Some(win), Some(label)) = (weak.upgrade(), labels.get(row.index() as usize))
            else {
                return;
            };
            pop.popdown();
            win.triage(if applied.contains(&label.id) {
                TriageAction::RemoveLabel(label.id.clone())
            } else {
                TriageAction::AddLabel(label.id.clone())
            });
        });
        let scroller = gtk::ScrolledWindow::builder()
            .child(&list)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(360)
            .min_content_width(220)
            .build();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&scroller);
        content.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        content.append(&create);
        popover.set_child(Some(&content));
        popover
    }

    fn change_text_size(self: &Rc<Self>, step: i32) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        app.update_settings(|s| {
            s.text_size = if step == 0 {
                TextSize::Normal
            } else {
                let next =
                    (s.text_size.index() as i32 + step).clamp(0, TextSize::ALL.len() as i32 - 1);
                TextSize::from_index(next as u32)
            };
        });
    }

    /// Opens the mailbox at `position` in the sidebar, counting from 1.
    fn go_to_mailbox(self: &Rc<Self>, position: usize) {
        let Some(mailbox) = self
            .sidebar
            .mailboxes()
            .get(position.saturating_sub(1))
            .cloned()
        else {
            return;
        };
        self.sidebar.select(&mailbox);
        self.show_mailbox(mailbox);
    }

    fn reply(self: &Rc<Self>, kind: ReplyKind) {
        self.reply_from(&self.conversation, kind);
    }

    /// Replies to or forwards the newest message in `view`.
    pub(super) fn reply_from(self: &Rc<Self>, view: &ConversationView, kind: ReplyKind) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let prepared = view.with_open(|open| {
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
        let mut draft = app.signed(compose::respond(
            kind, account_id, &me, &target, &text, &thread,
        ));
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
                        content_id: None,
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
        self.edit_draft_from(&self.conversation);
    }

    /// Opens the draft in `view` in the composer.
    pub(super) fn edit_draft_from(self: &Rc<Self>, view: &ConversationView) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let found = view.with_open(|open| {
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
            if let Some(id) = draft_id.clone() {
                draft.send_at = this
                    .core
                    .read(move |c| mailrs_store::scheduled::find(c, account_id, &id))
                    .await
                    .ok()
                    .flatten()
                    .map(|s| s.send_at);
            }
            draft.draft_id = draft_id;
            app.compose(draft);
        });
    }

    fn save_attachment(self: &Rc<Self>, message_id: String, index: usize) {
        self.save_attachment_from(&self.conversation, message_id, index);
    }

    /// Downloads attachment `index` of `message_id` in `view`.
    pub(super) fn save_attachment_from(
        self: &Rc<Self>,
        view: &ConversationView,
        message_id: String,
        index: usize,
    ) {
        let found = view.with_open(|open| {
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
                win.reload_folder();
                win.toast("Checking for mail");
            }),
        );
        add("add-account", Box::new(|win| win.authorize(None)));
        add("shortcuts", Box::new(|win| win.show_shortcuts()));
        add("reply", Box::new(|win| win.reply(ReplyKind::Reply)));
        add("reply-all", Box::new(|win| win.reply(ReplyKind::ReplyAll)));
        add("forward", Box::new(|win| win.reply(ReplyKind::Forward)));
        add("archive", Box::new(|win| win.triage(TriageAction::Archive)));
        add("trash", Box::new(|win| win.trash()));
        add("junk", Box::new(|win| win.act(Action::Junk)));
        add(
            "label",
            Box::new(|win| win.conversation.label_button.popup()),
        );
        add("undo", Box::new(|win| win.undo()));
        add(
            "assistant",
            Box::new(|win| {
                let show = !win.assistant_split.shows_sidebar();
                win.assistant_split.set_show_sidebar(show);
                if show {
                    win.assistant.focus();
                }
            }),
        );
        add("remind-custom", Box::new(|win| win.remind_custom()));
        let remind_at = gio::SimpleAction::new("remind-at", Some(glib::VariantTy::INT64));
        let weak = Rc::downgrade(self);
        remind_at.connect_activate(move |_, parameter| {
            if let (Some(win), Some(at)) = (weak.upgrade(), parameter.and_then(|p| p.get::<i64>()))
            {
                win.remind(at);
            }
        });
        self.actions.add_action(&remind_at);
        let flag_color = gio::SimpleAction::new("flag-color", Some(glib::VariantTy::STRING));
        let weak = Rc::downgrade(self);
        flag_color.connect_activate(move |_, parameter| {
            let (Some(win), Some(name)) =
                (weak.upgrade(), parameter.and_then(|p| p.get::<String>()))
            else {
                return;
            };
            win.flag(name.parse().ok());
        });
        self.actions.add_action(&flag_color);
        add("unsubscribe", Box::new(|win| win.unsubscribe()));
        add("toggle-vip", Box::new(|win| win.toggle_vip()));
        add("print", Box::new(|win| win.conversation.print()));
        add(
            "view-source",
            Box::new(|win| {
                let view = Rc::clone(&win.conversation);
                win.view_source(&view);
            }),
        );
        add("open-window", Box::new(|win| win.open_current_in_window()));
        add("block-sender", Box::new(|win| win.block_sender()));
        add("select-all", Box::new(|win| win.list.select_all()));
        add("zoom-in", Box::new(|win| win.change_text_size(1)));
        add("zoom-out", Box::new(|win| win.change_text_size(-1)));
        add("zoom-reset", Box::new(|win| win.change_text_size(0)));
        let go = gio::SimpleAction::new("go-mailbox", Some(glib::VariantTy::INT32));
        let weak = Rc::downgrade(self);
        go.connect_activate(move |_, parameter| {
            if let (Some(win), Some(position)) =
                (weak.upgrade(), parameter.and_then(|p| p.get::<i32>()))
            {
                win.go_to_mailbox(position as usize);
            }
        });
        self.actions.add_action(&go);
        add("toggle-star", Box::new(|win| win.act(Action::ToggleStar)));
        add("toggle-read", Box::new(|win| win.act(Action::ToggleRead)));
        add("about", Box::new(|win| win.show_about()));
        add("preferences", Box::new(|win| win.show_preferences()));
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
            "account-rules",
            Box::new(|win, account| win.show_rules(account)),
        );
        with_account(
            "account-rename",
            Box::new(|win, account| win.rename_account(account)),
        );
        with_account(
            "account-up",
            Box::new(|win, account| win.move_account(account, -1)),
        );
        with_account(
            "account-down",
            Box::new(|win, account| win.move_account(account, 1)),
        );
        with_account(
            "account-new-label",
            Box::new(|win, account| win.new_label(account.id, None)),
        );
        for (name, run) in [
            (
                "label-rename",
                (|win: &Rc<MainWindow>, account, label| win.rename_label(account, label))
                    as fn(&Rc<MainWindow>, AccountId, String),
            ),
            ("label-delete", |win, account, label| {
                win.delete_label(account, label)
            }),
        ] {
            let action = gio::SimpleAction::new(
                name,
                Some(&glib::VariantType::new("(xs)").expect("valid type")),
            );
            let weak = Rc::downgrade(self);
            action.connect_activate(move |_, parameter| {
                let (Some(win), Some((account, label))) = (
                    weak.upgrade(),
                    parameter.and_then(|p| p.get::<(i64, String)>()),
                ) else {
                    return;
                };
                run(&win, account, label);
            });
            self.actions.add_action(&action);
        }
        with_account(
            "account-vacation",
            Box::new(|win, account| win.show_vacation(account)),
        );
        with_account(
            "account-signature",
            Box::new(|win, account| win.show_preferences_for(Some(account.email))),
        );
        with_account(
            "account-remove",
            Box::new(|win, account| win.confirm_remove(account)),
        );

        let shortcuts = gtk::ShortcutController::new();
        shortcuts.set_scope(gtk::ShortcutScope::Global);
        // Apple Mail's shortcuts, with Command as Control.
        for (trigger, action) in [
            ("<Control>n", "win.compose"),
            ("<Control>f", "win.search"),
            ("<Control><Alt>f", "win.search"),
            ("F5", "win.check"),
            ("<Control><Shift>n", "win.check"),
            ("<Control>r", "win.reply"),
            ("<Control><Shift>r", "win.reply-all"),
            ("<Control><Shift>f", "win.forward"),
            ("<Control><Alt>a", "win.archive"),
            ("<Control><Shift>u", "win.toggle-read"),
            ("<Control><Shift>l", "win.toggle-star"),
            ("<Control><Shift>j", "win.junk"),
            ("<Control><Alt>m", "win.label"),
            ("<Control>plus", "win.zoom-in"),
            ("<Control>equal", "win.zoom-in"),
            ("<Control>minus", "win.zoom-out"),
            ("<Control>0", "win.zoom-reset"),
            ("<Control>question", "win.shortcuts"),
            ("<Control>comma", "win.preferences"),
            ("<Control>q", "win.quit"),
            ("<Control>p", "win.print"),
            ("<Control><Alt>u", "win.view-source"),
            ("<Control>o", "win.open-window"),
            ("<Control>j", "win.assistant"),
            ("<Control>w", "window.close"),
        ] {
            shortcuts.add_shortcut(gtk::Shortcut::new(
                gtk::ShortcutTrigger::parse_string(trigger),
                Some(gtk::NamedAction::new(action)),
            ));
        }
        // Apple Mail's Option-Command-1 to 7 pick a flag colour.
        for (index, color) in mailrs_domain::FlagColor::ALL.iter().enumerate() {
            let shortcut = gtk::Shortcut::new(
                gtk::ShortcutTrigger::parse_string(&format!("<Control><Alt>{}", index + 1)),
                Some(gtk::NamedAction::new("win.flag-color")),
            );
            shortcut.set_arguments(Some(&color.as_str().to_variant()));
            shortcuts.add_shortcut(shortcut);
        }
        for position in 1..=9i32 {
            let shortcut = gtk::Shortcut::new(
                gtk::ShortcutTrigger::parse_string(&format!("<Control>{position}")),
                Some(gtk::NamedAction::new("win.go-mailbox")),
            );
            shortcut.set_arguments(Some(&position.to_variant()));
            shortcuts.add_shortcut(shortcut);
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
        first.append(Some("New Smart Mailbox…"), Some("win.smart-new"));
        menu.append_section(None, &first);
        let second = gio::Menu::new();
        second.append(Some("Preferences"), Some("win.preferences"));
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
            if win.typing() || win.stack.visible_child_name().as_deref() != Some("mail") {
                return glib::Propagation::Proceed;
            }
            let others = gdk::ModifierType::ALT_MASK | gdk::ModifierType::SUPER_MASK;
            if modifiers.contains(gdk::ModifierType::CONTROL_MASK) {
                if modifiers.intersects(others | gdk::ModifierType::SHIFT_MASK)
                    || win.reading_text()
                {
                    return glib::Propagation::Proceed;
                }
                match key {
                    gdk::Key::a => win.list.select_all(),
                    gdk::Key::z => win.undo(),
                    _ => return glib::Propagation::Proceed,
                }
                return glib::Propagation::Stop;
            }
            if modifiers.intersects(others) {
                return glib::Propagation::Proceed;
            }
            match key {
                gdk::Key::Delete | gdk::Key::BackSpace | gdk::Key::KP_Delete => {
                    win.delete_key();
                    return glib::Propagation::Stop;
                }
                gdk::Key::Escape => {
                    if win.list.selected_rows().len() > 1 {
                        win.list.unselect();
                        win.conversation.clear();
                    } else if win.list.search_open() {
                        win.list.close_search();
                    } else {
                        return glib::Propagation::Proceed;
                    }
                    return glib::Propagation::Stop;
                }
                _ => {}
            }
            match key.to_unicode() {
                Some('j') => win.list.step(1),
                Some('k') => win.list.step(-1),
                Some('e') => win.triage(TriageAction::Archive),
                Some('#') => win.delete_key(),
                Some('s') => win.act(Action::ToggleStar),
                Some('u') => win.act(Action::ToggleRead),
                Some('r') => win.reply(ReplyKind::Reply),
                Some('a') => win.reply(ReplyKind::ReplyAll),
                Some('l') => win.conversation.label_button.popup(),
                Some('f') => win.reply(ReplyKind::Forward),
                Some('c') => win.compose_new(),
                Some('/') => win.list.open_search(),
                _ => return glib::Propagation::Proceed,
            }
            glib::Propagation::Stop
        });
        self.window.add_controller(keys);
    }

    /// True when the focus is in the message itself, where Ctrl+A selects text.
    fn reading_text(&self) -> bool {
        GtkWindowExt::focus(&self.window).is_some_and(|focus| {
            focus.is::<webkit::WebView>()
                || focus.ancestor(webkit::WebView::static_type()).is_some()
        })
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
        app.compose(app.signed(Draft::new(account_id, app.identity(account_id))));
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
            this.list.select(account_id, &thread_id, None);
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
                        finder.list.select(account_id, &thread_id, None);
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

    fn settings(&self) -> Settings {
        self.app
            .upgrade()
            .map(|app| app.settings())
            .unwrap_or_default()
    }

    fn show_preferences(self: &Rc<Self>) {
        self.show_preferences_for(None);
    }

    /// Opens Preferences, on the signature of `signature_of` when given.
    fn show_preferences_for(self: &Rc<Self>, signature_of: Option<String>) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let accounts = self.accounts.borrow().clone();
        super::preferences::present(&app, &accounts, &self.window, signature_of.as_deref());
    }

    fn show_rules(self: &Rc<Self>, account: Account) {
        let labels = self
            .labels
            .borrow()
            .get(&account.id)
            .cloned()
            .unwrap_or_default();
        let (grant, email) = (Rc::downgrade(self), account.email.clone());
        super::rules::present(&self.core, &account, labels, &self.window, move || {
            if let Some(win) = grant.upgrade() {
                win.authorize(Some(email.clone()));
            }
        });
    }

    /// Opens Preferences on one page, such as "assistant".
    fn show_preferences_page(self: &Rc<Self>, page: &str) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let accounts = self.accounts.borrow().clone();
        super::preferences::present_page(&app, &accounts, &self.window, page);
    }

    fn show_vacation(self: &Rc<Self>, account: Account) {
        let (grant, saved) = (Rc::downgrade(self), Rc::downgrade(self));
        let email = account.email.clone();
        super::vacation::present(
            &self.core,
            &account,
            &self.window,
            move || {
                if let Some(win) = grant.upgrade() {
                    win.authorize(Some(email.clone()));
                }
            },
            move |text| {
                if let Some(win) = saved.upgrade() {
                    win.toast(text);
                }
            },
        );
    }

    /// Adds the open conversation's sender to the VIPs, or takes them off.
    fn toggle_vip(self: &Rc<Self>) {
        let me = self
            .conversation
            .with_open(|o| o.me.clone())
            .unwrap_or_default();
        let sender = self.conversation.with_open(|o| {
            o.messages
                .iter()
                .rev()
                .filter_map(|m| m.from.clone())
                .find(|a| !me.iter().any(|mine| mine.eq_ignore_ascii_case(&a.email)))
        });
        let (Some(Some(sender)), Some(app)) = (sender, self.app.upgrade()) else {
            return self.toast("Open a message from the person first");
        };
        let name = sender.name.clone().unwrap_or_default();
        let mut added = false;
        app.update_settings(|s| added = s.toggle_vip(&sender.email, &name));
        self.toast(&if added {
            format!("Added {} to VIPs", sender.display())
        } else {
            format!("Removed {} from VIPs", sender.display())
        });
    }

    /// Applies a settings change to what is on screen.
    pub fn settings_changed(self: &Rc<Self>, before: &Settings, after: &Settings) {
        if before.threading != after.threading {
            self.conversation.clear();
            self.list.unselect();
            let mailbox = self.mailbox.borrow().clone();
            match mailbox {
                Mailbox::Search { query, .. } => self.search(query),
                Mailbox::Folder { .. } => self.reload_folder(),
                _ => self.reload_list(),
            }
        }
        if before.smart_mailboxes != after.smart_mailboxes
            || before.account_order != after.account_order
            || before.account_colors != after.account_colors
            || before.account_names != after.account_names
        {
            self.refresh_accounts();
            if before.account_colors != after.account_colors {
                // Rows carry account colours; refresh_accounts sets the new ones first.
                let list = Rc::clone(&self.list);
                glib::timeout_add_local_once(std::time::Duration::from_millis(200), move || {
                    list.rebind();
                });
            }
            if let Mailbox::Smart { id, .. } = self.mailbox.borrow().clone() {
                self.show_smart(&id);
            }
        }
        if before.vips != after.vips {
            self.refresh_accounts();
            let vip = sender_is_vip(&self.conversation, after);
            self.conversation.set_sender_vip(vip);
        }
        if before.ai != after.ai {
            self.assistant.refresh();
        }
        if before.text_size != after.text_size {
            self.conversation.set_zoom(after.text_size.zoom());
        }
    }

    pub fn toast_sent(&self) {
        self.toast("Message sent");
    }

    fn show_shortcuts(&self) {
        let dialog = adw::ShortcutsDialog::new();
        let groups: [(&str, &[(&str, &str)]); 4] = [
            (
                "Reading",
                &[
                    ("Next or previous conversation", "j k"),
                    ("Open mailbox 1 to 9", "<Control>1...<Control>9"),
                    ("Search", "<Control>f slash"),
                    ("Get new mail", "<Control><Shift>n F5"),
                    ("Select all", "<Control>a"),
                    ("Clear the selection", "Escape"),
                    ("Bigger or smaller text", "<Control>plus <Control>minus"),
                    ("Open in a new window", "<Control>o"),
                    ("Print", "<Control>p"),
                    ("View source", "<Control><Alt>u"),
                    ("Normal text size", "<Control>0"),
                ],
            ),
            (
                "Organizing",
                &[
                    ("Archive", "<Control><Alt>a e"),
                    ("Move to trash", "Delete numbersign"),
                    ("Junk", "<Control><Shift>j"),
                    ("Flag or unflag", "<Control><Shift>l s"),
                    ("Flag colors", "<Control><Alt>1...<Control><Alt>7"),
                    ("Mark read or unread", "<Control><Shift>u u"),
                    ("Labels", "<Control><Alt>m l"),
                    ("Undo", "<Control>z"),
                ],
            ),
            (
                "Writing",
                &[
                    ("New message", "<Control>n c"),
                    ("Reply", "<Control>r r"),
                    ("Reply all", "<Control><Shift>r a"),
                    ("Forward", "<Control><Shift>f f"),
                    ("Send", "<Control><Shift>d <Control>Return"),
                    ("Attach files", "<Control><Shift>a"),
                    ("Bold, italic, link", "<Control>b <Control>i <Control>k"),
                    ("Save draft", "<Control>s"),
                ],
            ),
            (
                "General",
                &[
                    ("Preferences", "<Control>comma"),
                    ("Keyboard shortcuts", "<Control>question"),
                    ("Close window", "<Control>w"),
                    ("Quit", "<Control>q"),
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
    // Readers cannot write, so this leaves the access time alone; the body
    // fetch that follows records the access.
    mailrs_store::bodies::peek_body(c, account_id, message_id)
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

/// Whether the newest sender in `view` who is not the user is a VIP.
fn sender_is_vip(view: &ConversationView, settings: &Settings) -> bool {
    view.with_open(|o| {
        o.messages
            .iter()
            .rev()
            .filter_map(|m| m.from.as_ref())
            .find(|a| !o.me.iter().any(|mine| mine.eq_ignore_ascii_case(&a.email)))
            .is_some_and(|a| settings.is_vip(&a.email))
    })
    .unwrap_or(false)
}

fn empty_state(mailbox: &Mailbox) -> (&'static str, &'static str) {
    let label = match mailbox {
        Mailbox::Unified(label) => *label,
        Mailbox::Label { label_id, .. } => label_id.as_str(),
        Mailbox::Search { .. } => return ("No Results", "system-search-symbolic"),
        Mailbox::Scheduled => return ("Nothing Scheduled", "mail-send-symbolic"),
        Mailbox::Reminders => return ("No Reminders", "alarm-symbolic"),
        Mailbox::Flag(_) => return ("No Flagged Mail", "mailrs-flag-symbolic"),
        Mailbox::Vips { .. } => return ("No Mail from VIPs", "starred-symbolic"),
        Mailbox::Smart { .. } => return ("No Matching Mail", "folder-saved-search-symbolic"),
        Mailbox::Folder { folder, .. } => {
            return match folder {
                Folder::Junk => ("No Junk", folder.icon()),
                Folder::Trash => ("Trash Is Empty", folder.icon()),
                Folder::AllMail => ("No Mail", folder.icon()),
            };
        }
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
