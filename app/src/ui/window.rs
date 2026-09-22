//! The main window: sidebar, thread list, and conversation, plus the
//! first-run pages. It reacts to engine events and turns user actions into
//! calls on the sync core.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use base64::Engine;
use gtk::{gio, glib};
use mailrs_domain::translate::{fill, fill_plural, gettext};
use mailrs_domain::{
    Account, AccountId, AccountState, ChangeEvent, Folder, Label, MessageBody, Target,
    ThreadSummary, system_label,
};
use mailrs_gmail::{CONTACTS_SCOPE, DELETE_SCOPE};
use mailrs_store::{accounts, labels};
use mailrs_sync::{
    History, Listing, MailAction, Outcome, Permitted, Scope, TriageAction, View, outbox_id,
};

use super::contact_card;
use super::conversation::{Action, ConversationView};
use super::list_feed::{Coalesce, ListFeed, Refresh, Splice, Ticket};
use super::sidebar::Sidebar;
use super::thread_list::{Picked, ThreadList};
use super::{Mailbox, welcome};
use crate::app::App;
use crate::assistant::ToolRequest;
use crate::compose::{self, Draft, OutgoingAttachment, ReplyKind};
use crate::core::Core;
use crate::open_thread::OpenThread;
use crate::settings::{Change, Effect, Effects, Settings};

mod arrange;
mod assistant;
mod attachments;
mod categories;
mod detached;
mod export;
mod flags;
mod followup;
mod hide_my_email;
mod images;
mod invitation;
mod organize;
mod outbox;
mod pgp;
mod reach;
mod reminders;
mod scheduled;
mod senders;
mod shortcuts;
mod thread;
mod translation;
mod triage;

/// Largest inline image embedded into a page.
const INLINE_IMAGE_LIMIT: usize = 5 * 1024 * 1024;

/// Bodies fetched at once when a thread opens. Each one is a 5-unit Gmail
/// call and an account may spend 250 units a second.
pub(super) const BODY_FETCHES: usize = 10;

/// Inline images kept in memory, so reopening a conversation does not
/// download the same pictures again.
const INLINE_IMAGE_CACHE: usize = 64;

/// How long a reply from a notification waits for the thread and its body
/// to arrive before it quotes the snippet instead.
const REVEAL_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// How often that wait looks at the conversation.
const REVEAL_STEP: std::time::Duration = std::time::Duration::from_millis(100);

/// Whether a refresh should list the mailbox again. Listing a folder or a
/// search means a Gmail search for every account on screen, so the window
/// asks for one only when the rows themselves can have changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reload {
    Yes,
    No,
}

/// What to do with a thread opened from outside the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reveal {
    /// Show it.
    Read,
    /// Show it and answer its newest message.
    Reply,
}

type AccountAction = Box<dyn Fn(&Rc<MainWindow>, Account)>;

pub struct MainWindow {
    pub window: adw::Window,
    actions: gio::SimpleActionGroup,
    app: Weak<App>,
    core: Rc<Core>,
    toasts: adw::ToastOverlay,
    /// Says a release is available, installing, waiting to restart, or failed.
    update_banner: adw::Banner,
    /// The main menu's update entry: Check for Updates, or what to do with
    /// the one that is waiting.
    update_menu: gio::Menu,
    /// The About window while it is open, so an update's progress reaches
    /// its button.
    about: RefCell<Option<Rc<crate::ui::about::About>>>,
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
    /// What the thread list loads next, and which answers still count.
    feed: RefCell<ListFeed<(AccountId, String, Reveal)>>,
    authorizing: Cell<bool>,
    labels: RefCell<HashMap<AccountId, Vec<Label>>>,
    assistant: Rc<super::assistant::AssistantPane>,
    assistant_split: adw::OverlaySplitView,
    categories: categories::CategoryBar,
    follow_up: followup::FollowUpBanner,
    /// Inline images already downloaded, by account, message and
    /// attachment id. Gmail charges 5 units for each one and a
    /// conversation is often reopened.
    inline_cache: RefCell<HashMap<(AccountId, String, String), String>>,
    /// Pictures for the attachment rows, held the same way and for the
    /// same reason.
    thumbnail_cache: RefCell<HashMap<(AccountId, String, String), String>>,
    /// Senders whose remote images may load. Read from the store once and
    /// kept here, since every thread that opens asks about it.
    image_senders: RefCell<Vec<mailrs_store::image_senders::ImageSender>>,
    /// The conversations in windows of their own, each with the mailbox it
    /// was opened from, so a flag colour or an undo reaches them too. An
    /// entry that no longer upgrades is a window somebody closed.
    detached: RefCell<Vec<(Weak<ConversationView>, Mailbox)>>,
}

/// The heading on the Delete Forever dialog, which names how much goes.
/// Every count writes its own sentence: a language decides for itself
/// where the number goes and which form the noun takes beside it.
fn delete_forever_heading(count: usize, threaded: bool) -> String {
    let number = count.to_string();
    let values = [("count", number.as_str())];
    match (threaded, count) {
        (true, 1) => gettext("Delete This Conversation Forever?"),
        (true, _) => fill_plural(
            "Delete {count} Conversation Forever?",
            "Delete {count} Conversations Forever?",
            count,
            &values,
        ),
        (false, 1) => gettext("Delete This Message Forever?"),
        (false, _) => fill_plural(
            "Delete {count} Message Forever?",
            "Delete {count} Messages Forever?",
            count,
            &values,
        ),
    }
}

/// The toast after erasing.
fn deleted_forever_message(count: usize, threaded: bool) -> String {
    let number = count.to_string();
    let values = [("count", number.as_str())];
    match (threaded, count) {
        (_, 1) => gettext("Deleted forever"),
        (true, _) => fill_plural(
            "Deleted {count} conversation forever",
            "Deleted {count} conversations forever",
            count,
            &values,
        ),
        (false, _) => fill_plural(
            "Deleted {count} message forever",
            "Deleted {count} messages forever",
            count,
            &values,
        ),
    }
}

/// Whether `action` takes the targets out of `mailbox`'s list.
fn leaves_list(mailbox: &Mailbox, action: &TriageAction) -> bool {
    let folder = mailbox.folder();
    match action {
        TriageAction::Archive => folder != Some(Folder::AllMail),
        TriageAction::Trash => folder != Some(Folder::Trash),
        TriageAction::Junk => folder != Some(Folder::Junk),
        TriageAction::Untrash => folder == Some(Folder::Trash),
        TriageAction::NotJunk => folder == Some(Folder::Junk),
        TriageAction::Mute => folder != Some(Folder::AllMail) && !lists_muted(mailbox),
        TriageAction::Unmute => lists_muted(mailbox),
        _ => false,
    }
}

/// What a row of the label popover says. The tick beside the name is the
/// only sign that a label is already on the mail, so the name carries it.
fn label_row_name(label: &str, applied: bool) -> String {
    let shown = label.replace('/', " › ");
    match applied {
        true => fill(&gettext("{label}, on this mail"), &[("label", &shown)]),
        false => shown,
    }
}

/// Whether `mailbox` is the Muted list, unified or for one account.
fn lists_muted(mailbox: &Mailbox) -> bool {
    match mailbox {
        Mailbox::Unified(label) => *label == system_label::MUTE,
        Mailbox::Label { label_id, .. } => label_id == system_label::MUTE,
        _ => false,
    }
}

/// The toast after adding or removing a VIP.
fn vip_message(added: bool, who: &str) -> String {
    match added {
        true => fill(&gettext("Added {person} to VIPs"), &[("person", who)]),
        false => fill(&gettext("Removed {person} from VIPs"), &[("person", who)]),
    }
}

/// What a mailbox that would not load says.
fn load_failed(err: &impl std::fmt::Display) -> String {
    fill(
        &gettext("Could not load mail: {reason}"),
        &[("reason", &err.to_string())],
    )
}

/// The colour a flag toast names.
fn flagged_message(color: mailrs_domain::FlagColor) -> String {
    use mailrs_domain::FlagColor;
    match color {
        FlagColor::Red => gettext("Flagged red"),
        FlagColor::Orange => gettext("Flagged orange"),
        FlagColor::Yellow => gettext("Flagged yellow"),
        FlagColor::Green => gettext("Flagged green"),
        FlagColor::Blue => gettext("Flagged blue"),
        FlagColor::Purple => gettext("Flagged purple"),
        FlagColor::Gray => gettext("Flagged gray"),
    }
}

/// The toast after an action, or `None` when the change speaks for itself.
fn done_message(action: &MailAction, count: usize, threaded: bool) -> Option<String> {
    let number = count.to_string();
    let values = [("count", number.as_str())];
    let action = match action {
        MailAction::Triage(action) => action,
        MailAction::Flag(color) => return color.map(flagged_message),
        MailAction::Label { .. } => return Some(gettext("Labels changed")),
        MailAction::Mute { muted } => {
            return Some(match (*muted, count > 1, threaded) {
                (true, false, _) => gettext("Muted"),
                (false, false, _) => gettext("Unmuted"),
                (true, true, true) => fill_plural(
                    "Muted {count} conversation",
                    "Muted {count} conversations",
                    count,
                    &values,
                ),
                (true, true, false) => fill_plural(
                    "Muted {count} message",
                    "Muted {count} messages",
                    count,
                    &values,
                ),
                (false, true, true) => fill_plural(
                    "Unmuted {count} conversation",
                    "Unmuted {count} conversations",
                    count,
                    &values,
                ),
                (false, true, false) => fill_plural(
                    "Unmuted {count} message",
                    "Unmuted {count} messages",
                    count,
                    &values,
                ),
            });
        }
        MailAction::Remind { .. } | MailAction::CancelReminder => return None,
        MailAction::DismissFollowUp => {
            return Some(fill_plural(
                "Dismissed {count} follow-up",
                "Dismissed {count} follow-ups",
                count,
                &values,
            ));
        }
    };
    let many = count > 1;
    Some(match (action, many, threaded) {
        (TriageAction::Archive, false, _) => gettext("Archived"),
        (TriageAction::Archive, true, true) => fill_plural(
            "Archived {count} conversation",
            "Archived {count} conversations",
            count,
            &values,
        ),
        (TriageAction::Archive, true, false) => fill_plural(
            "Archived {count} message",
            "Archived {count} messages",
            count,
            &values,
        ),
        (TriageAction::Trash, false, _) => gettext("Moved to Trash"),
        (TriageAction::Trash, true, true) => fill_plural(
            "Moved {count} conversation to Trash",
            "Moved {count} conversations to Trash",
            count,
            &values,
        ),
        (TriageAction::Trash, true, false) => fill_plural(
            "Moved {count} message to Trash",
            "Moved {count} messages to Trash",
            count,
            &values,
        ),
        (TriageAction::Junk, false, _) => gettext("Marked as junk"),
        (TriageAction::Junk, true, true) => fill_plural(
            "Marked {count} conversation as junk",
            "Marked {count} conversations as junk",
            count,
            &values,
        ),
        (TriageAction::Junk, true, false) => fill_plural(
            "Marked {count} message as junk",
            "Marked {count} messages as junk",
            count,
            &values,
        ),
        (TriageAction::Untrash | TriageAction::NotJunk, false, _) => gettext("Moved to the Inbox"),
        (TriageAction::Untrash | TriageAction::NotJunk, true, true) => fill_plural(
            "Moved {count} conversation to the Inbox",
            "Moved {count} conversations to the Inbox",
            count,
            &values,
        ),
        (TriageAction::Untrash | TriageAction::NotJunk, true, false) => fill_plural(
            "Moved {count} message to the Inbox",
            "Moved {count} messages to the Inbox",
            count,
            &values,
        ),
        (
            TriageAction::AddLabel(_) | TriageAction::RemoveLabel(_) | TriageAction::Relabel { .. },
            ..,
        ) => gettext("Labels changed"),
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
                    let view = Rc::clone(&win.conversation);
                    win.act(&view, action);
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
                {
                    let app = Rc::downgrade(app);
                    move |key| {
                        if let Some(app) = app.upgrade() {
                            app.change_settings(crate::settings::Change::AllowTool(key));
                        }
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
            // An update's banner spans the whole window, above the panes,
            // since it is about the app and not the mail on screen.
            let update_banner = adw::Banner::builder().revealed(false).build();
            let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
            content.append(&update_banner);
            content.append(&stack);
            stack.set_vexpand(true);
            let toasts = adw::ToastOverlay::new();
            toasts.set_child(Some(&content));
            let window = adw::Window::builder()
                .title(if app.core.demo {
                    gettext("Penguin Mail (Demo)")
                } else {
                    gettext("Penguin Mail")
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
            // A plain adw::Window, unlike GtkApplicationWindow, does not
            // reach the application's own actions. The update banner, menu
            // entry and About button all run app.* actions.
            window.insert_action_group("app", Some(&app.gio));
            MainWindow {
                window,
                actions,
                app: Rc::downgrade(app),
                core: Rc::clone(&app.core),
                toasts,
                update_banner,
                update_menu: gio::Menu::new(),
                about: RefCell::new(None),
                stack,
                split,
                nav,
                sidebar,
                list,
                conversation,
                first_account,
                mailbox: RefCell::new(Mailbox::Unified(system_label::INBOX)),
                before_search: RefCell::new(Mailbox::Unified(system_label::INBOX)),
                accounts: RefCell::new(Vec::new()),
                feed: RefCell::new(ListFeed::default()),
                authorizing: Cell::new(false),
                labels: RefCell::new(HashMap::new()),
                assistant,
                assistant_split,
                categories: categories::CategoryBar::new(app.settings().default_category),
                follow_up: followup::FollowUpBanner::new(),
                inline_cache: RefCell::new(HashMap::new()),
                thumbnail_cache: RefCell::new(HashMap::new()),
                image_senders: RefCell::new(Vec::new()),
                detached: RefCell::new(Vec::new()),
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
        let weak = Rc::downgrade(&window);
        window.list.connect_more(move || {
            if let Some(win) = weak.upgrade() {
                win.load_more();
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
        window.install_follow_ups();
        window.install_categories();
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
        window.refresh_accounts(Reload::Yes);
        window.reload_image_senders();
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

    /// Shows where an update stands, or hides the banner when nothing does.
    /// Each state's button runs an app action, so the banner needs no
    /// callbacks of its own.
    pub fn show_update(&self, state: &crate::update::State) {
        use crate::update::State;
        let banner = &self.update_banner;
        if let Some(about) = self.about.borrow().as_ref() {
            about.show_update(state);
        }
        let (entry, action) = match state {
            State::Available(release) => (
                fill(
                    &gettext("Install Update {version}"),
                    &[("version", &release.version.to_string())],
                ),
                Some("app.install-update"),
            ),
            State::Installing(_) => (gettext("Installing Update…"), None),
            State::Installed(_) => (gettext("Restart to Update"), Some("app.restart-for-update")),
            State::Failed { .. } => (gettext("Show Update Log"), Some("app.update-log")),
            State::Checking => (gettext("Checking for Updates…"), None),
            State::Idle | State::Current | State::Unreachable => {
                (gettext("Check for Updates"), Some("app.check-for-updates"))
            }
        };
        self.update_menu.remove_all();
        // An entry with no action shows greyed out, which is right while a
        // check or an install is running.
        self.update_menu.append(Some(&entry), action);
        let (title, button) = match state {
            // These answer a check; the About window and a toast say so.
            State::Idle | State::Checking | State::Current | State::Unreachable => {
                banner.set_revealed(false);
                return;
            }
            State::Available(release) => (
                fill(
                    &gettext("Penguin Mail {version} is available"),
                    &[("version", &release.version.to_string())],
                ),
                Some((gettext("Install"), "app.install-update")),
            ),
            State::Installing(version) => (
                fill(
                    &gettext("Installing Penguin Mail {version}"),
                    &[("version", &version.to_string())],
                ),
                None,
            ),
            State::Installed(_) => (
                gettext("Restart to finish updating"),
                Some((gettext("Restart"), "app.restart-for-update")),
            ),
            State::Failed { version, .. } => (
                fill(
                    &gettext("The update to {version} failed"),
                    &[("version", &version.to_string())],
                ),
                Some((gettext("Show Log"), "app.update-log")),
            ),
        };
        banner.set_title(&title);
        match button {
            Some((label, action)) => {
                banner.set_button_label(Some(&label));
                banner.set_action_name(Some(action));
            }
            None => {
                banner.set_button_label(None);
                banner.set_action_name(None);
            }
        }
        banner.set_revealed(true);
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
            // An account going offline and back changes the banner and the
            // sidebar, not the rows. Listing again would cost a Gmail
            // search for every folder and search on screen.
            ChangeEvent::AccountStateChanged { .. } => self.refresh_accounts(Reload::No),
            ChangeEvent::LabelsChanged { .. } => self.refresh_accounts(Reload::Yes),
            ChangeEvent::ThreadsChanged {
                account_id,
                thread_ids,
            } => {
                let changed = thread_ids.iter().map(|id| (*account_id, id.clone()));
                let coalesce = self.feed.borrow_mut().changed(changed.collect());
                self.coalesce(coalesce);
            }
            ChangeEvent::NewMail { .. } => self.queue_refresh(),
            ChangeEvent::WriteFailed { message, .. } => self.toast(message),
            // Gmail can hold a bulk change up for the best part of a
            // minute. Say so, or the window looks stuck and the reader
            // presses Delete again.
            ChangeEvent::WaitingOnGmail { message, .. } => self.toast(message),
        }
    }

    /// Refreshes the counts and loads the whole list again.
    fn queue_refresh(self: &Rc<Self>) {
        let coalesce = self.feed.borrow_mut().everything();
        self.coalesce(coalesce);
    }

    /// Waits 150 ms so that a burst of change events costs one refresh.
    /// The feed decides whether that refresh lists the mailbox again or
    /// re-reads the named threads on their own, which keeps a change
    /// event off the 10,000-row query.
    fn coalesce(self: &Rc<Self>, coalesce: Coalesce) {
        if coalesce == Coalesce::Joined {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(std::time::Duration::from_millis(150), move || {
            let Some(win) = weak.upgrade() else { return };
            let refresh = win.feed.borrow_mut().fire();
            win.refresh_counts();
            match refresh {
                Refresh::Reload => win.reload_list(),
                Refresh::Splice(ticket, changed) => win.splice_changed(ticket, changed),
            }
            win.refresh_open_thread();
        });
    }

    /// Re-reads the accounts and their labels, and with [`Reload::Yes`]
    /// lists the mailbox again. A remote mailbox lists through Gmail, so
    /// only a change that can alter its rows is worth that.
    fn refresh_accounts(self: &Rc<Self>, reload: Reload) {
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
                Err(err) => {
                    return this.toast(&fill(
                        &gettext("Could not read accounts: {reason}"),
                        &[("reason", &err.to_string())],
                    ));
                }
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
                *this.mailbox.borrow_mut() = Mailbox::Unified(system_label::INBOX);
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
                    this.list.banner.set_title(&fill(
                        &gettext("Sign in again to keep {account} syncing"),
                        &[("account", email)],
                    ));
                    this.list.banner.set_button_label(Some(&gettext("Sign In")));
                    this.list.banner.set_revealed(true);
                }
                None => this.list.banner.set_revealed(false),
            }
            if let Some(app) = this.app.upgrade() {
                app.remember_accounts(&this.accounts.borrow());
            }
            this.follow_categories();
            this.refresh_counts();
            if reload == Reload::Yes {
                this.reload_list();
            }
        });
    }

    /// What the sidebar and the category switcher show. Two grouped
    /// queries replace the one-per-mailbox counting this used to do.
    fn refresh_counts(self: &Rc<Self>) {
        let mailboxes = self.sidebar.mailboxes();
        let shown = self.mailbox.borrow().clone();
        let view = self.view();
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let lists = this.core.lists();
            let counted = this
                .core
                .call(async move { lists.counts(&mailboxes, &shown, &view).await })
                .await;
            let Ok(counts) = counted else {
                return;
            };
            this.sidebar.set_counts(&counts.mailboxes);
            let waiting = counts
                .mailboxes
                .get(&Mailbox::FollowUp)
                .copied()
                .unwrap_or(0);
            this.set_follow_up_count(waiting as usize);
            this.set_category_counts(&counts.categories);
        });
    }

    // ---- Mailboxes and the thread list ---------------------------------

    /// The accounts a listing may read, in sidebar order.
    fn scope(&self) -> Scope {
        Scope::over(self.accounts.borrow().clone())
    }

    /// The settings that change what a mailbox lists.
    fn view(&self) -> View {
        let settings = self.settings();
        View {
            threading: settings.threading,
            category: settings
                .inbox_categories
                .then(|| self.categories.chosen.get()),
            follow_ups: settings.suggest_follow_ups,
            now: chrono::Utc::now().timestamp_millis(),
            limit: None,
        }
    }

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
        // A thread clicked in the mailbox before would otherwise land in
        // this one once its store read answers.
        self.conversation.stop_loading();
        self.conversation.clear();
        self.nav.set_show_content(false);
        if self.split.is_collapsed() {
            self.split.set_show_sidebar(false);
        }
        self.set_folder(mailbox.folder());
        self.follow_outbox();
        self.follow_categories();
        self.follow_follow_ups();
        let ticket = self.feed.borrow_mut().shown();
        self.list_first_page(ticket);
    }

    /// Fetches a folder or a smart mailbox that lives only in Gmail again.
    fn reload_folder(self: &Rc<Self>) {
        if matches!(
            *self.mailbox.borrow(),
            Mailbox::Folder { .. } | Mailbox::Smart(_)
        ) {
            self.reload_list();
        }
    }

    /// Loads the first page of the mailbox on screen.
    fn reload_list(self: &Rc<Self>) {
        let ticket = self.feed.borrow_mut().reload();
        self.list_first_page(ticket);
    }

    /// Lists the first page under `ticket`, which the feed dropped all
    /// earlier requests for.
    fn list_first_page(self: &Rc<Self>, ticket: Ticket) {
        let mailbox = self.mailbox.borrow().clone();
        if mailbox.is_remote() {
            self.list.show_loading();
        }
        let (scope, view) = (self.scope(), self.view());
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let lists = this.core.lists();
            let loaded = this
                .core
                .call(async move { lists.list(&mailbox, &scope, &view, 0).await })
                .await;
            let Some(landed) = this.feed.borrow_mut().first_page(ticket, &loaded) else {
                return;
            };
            match loaded {
                Ok(listing) => this.show_listing(listing),
                Err(err) => this.toast(&load_failed(&err)),
            }
            if let Some((account_id, thread_id, then)) = landed.reveal {
                this.select_revealed(account_id, thread_id, then);
            }
        });
    }

    /// Puts a freshly loaded page on screen.
    fn show_listing(self: &Rc<Self>, listing: Listing) {
        for notice in &listing.notices {
            self.toast(notice);
        }
        let rows = listing.rows.into_iter().map(Rc::new).collect();
        self.list
            .set_rows(rows, &listing.empty.title, listing.empty.icon);
        self.follow_selection();
        self.list.set_title(&listing.title, &listing.subtitle);
    }

    /// Loads the next page once the user scrolls near the end.
    fn load_more(self: &Rc<Self>) {
        let Some(ticket) = self.feed.borrow_mut().scrolled_to_end() else {
            return;
        };
        let mailbox = self.mailbox.borrow().clone();
        let (scope, view, from) = (self.scope(), self.view(), self.list.loaded());
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let lists = this.core.lists();
            let loaded = this
                .core
                .call(async move { lists.list(&mailbox, &scope, &view, from).await })
                .await;
            if !this.feed.borrow_mut().next_page(ticket, &loaded) {
                return;
            }
            match loaded {
                Ok(listing) => {
                    this.list
                        .append(listing.rows.into_iter().map(Rc::new).collect());
                }
                Err(err) => this.toast(&load_failed(&err)),
            }
        });
    }

    /// Re-reads the threads a change event named and puts them back in the
    /// list in place, instead of listing the whole mailbox again.
    fn splice_changed(self: &Rc<Self>, ticket: Ticket, changed: Vec<(AccountId, String)>) {
        let mailbox = self.mailbox.borrow().clone();
        let view = self.view();
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let (lists, named) = (this.core.lists(), changed.clone());
            let fresh = this
                .core
                .call(async move { lists.changed(&mailbox, &named, &view).await })
                .await
                .unwrap_or(None);
            let remote = this.mailbox.borrow().is_remote();
            let splice = this.feed.borrow_mut().spliced(ticket, fresh, remote);
            match splice {
                Splice::Stale => {}
                Splice::Put(fresh) => {
                    let title = this.mailbox.borrow().title();
                    this.list.replace_threads(&changed, fresh.rows);
                    this.follow_selection();
                    this.list.set_title(&title, &fresh.subtitle);
                }
                // A folder or a search lists through Gmail, and nothing
                // that changed elsewhere changes its rows. Drop the rows
                // that left the folder and leave the rest alone, rather
                // than paying for the whole search again.
                Splice::Prune => {
                    let targets = changed
                        .iter()
                        .map(|(account_id, thread_id)| Target::thread(*account_id, thread_id))
                        .collect::<Vec<_>>();
                    this.prune_folder(&targets);
                }
                Splice::Reload => this.reload_list(),
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
        self.follow_categories();
        self.follow_follow_ups();
        self.list.set_title(&gettext("Search"), &query);
        self.conversation.clear();
        self.set_folder(None);
        self.reload_list();
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

    /// Downloads `cid:` images that HTML bodies reference, as `data:` URIs.
    pub(super) async fn inline_images(
        &self,
        account_id: AccountId,
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
                let key = (account_id, message_id.clone(), attachment_id.clone());
                if let Some(held) = self.inline_cache.borrow().get(&key) {
                    images.insert(cid.clone(), held.clone());
                    continue;
                }
                let (s, m, a) = (sync.clone(), message_id.clone(), attachment_id.clone());
                if let Ok(bytes) = self
                    .core
                    .call(async move { s.attachment(&m, &a).await })
                    .await
                {
                    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
                    let url = format!("data:{};base64,{encoded}", attachment.mime_type);
                    let mut cache = self.inline_cache.borrow_mut();
                    if cache.len() >= INLINE_IMAGE_CACHE {
                        cache.clear();
                    }
                    cache.insert(key, url.clone());
                    images.insert(cid.clone(), url);
                }
            }
            out.insert(message_id.clone(), images);
        }
        out
    }

    // ---- Actions on the selection or the open conversation -----------------

    fn picked(self: &Rc<Self>, picked: Picked) {
        match picked {
            // A message that never reached Gmail has no thread to open,
            // and asking Gmail for one would be a call thrown away. Its
            // row menu is what acts on it.
            Picked::One(row) if outbox_id(&row.id).is_some() => self.conversation.clear(),
            Picked::One(row) => self.open_thread(row),
            Picked::Many(rows) => {
                self.conversation.show_many(
                    rows.len(),
                    self.settings().threading,
                    rows.iter().any(|r| r.unread),
                    rows.iter().all(|r| r.starred),
                    rows.iter().all(|r| r.muted),
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

    /// The one table over [`Action`]: what a conversation's buttons, menus
    /// and keys do, whether the conversation is in the main window or in a
    /// window of its own. `view` says what the action applies to.
    pub(super) fn act(self: &Rc<Self>, view: &Rc<ConversationView>, action: Action) {
        match action {
            Action::Invitation(action) => self.invitation_action(view, action),
            Action::Reply(kind) => self.reply(view, kind),
            Action::EditDraft => self.edit_draft_from(view),
            Action::Archive
            | Action::Trash
            | Action::Junk
            | Action::ToggleStar
            | Action::ToggleRead => self.organize(view, &action),
            Action::Unsubscribe => self.unsubscribe(Rc::clone(view)),
            Action::LoadImages => self.load_images_once(view),
            Action::SaveAttachment { message_id, index } => {
                self.save_attachment_from(view, message_id, index)
            }
            Action::PreviewAttachment { message_id, index } => {
                self.preview_attachment_from(view, message_id, index)
            }
            Action::SaveAllAttachments { message_id } => {
                self.save_all_attachments_from(view, message_id)
            }
            Action::Mailto(address) => {
                let account_id = self.default_account();
                if let (Some(account_id), Some(app)) = (account_id, self.app.upgrade()) {
                    let mut draft = Draft::new(account_id, app.identity(account_id));
                    draft.to = compose::parse_recipients(&address);
                    app.compose(app.signed(draft));
                }
            }
            Action::ShowContact(address) => self.show_contact_from(view, address),
            Action::Translate => self.translate_message(view),
        }
    }

    /// Opens the card for one sender: what the address book knows, or the
    /// message header alone when the address book has never heard of them.
    fn show_contact_from(self: &Rc<Self>, view: &ConversationView, address: String) {
        if address.trim().is_empty() {
            return;
        }
        let name = view.find(|open| {
            open.messages
                .iter()
                .filter_map(|m| m.from.clone())
                .find(|a| a.email.eq_ignore_ascii_case(&address))
                .map(|a| a.display().to_string())
        });
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let book = this.core.contacts();
            let looked_up = {
                let (book, address) = (book, address.clone());
                this.core
                    .call(async move { book.card(&address).await })
                    .await
            };
            let card = looked_up.unwrap_or_else(|err| {
                tracing::warn!(error = %err, "could not read the contact");
                None
            });
            let Some(app) = this.app.upgrade() else {
                return;
            };
            let vip = app.settings().is_vip(&address);
            let person = match card {
                Some(card) => contact_card::Person {
                    name: card.contact.display().to_string(),
                    email: card.contact.email().unwrap_or(address.as_str()).to_string(),
                    addresses: card.contact.emails.clone(),
                    organization: card.contact.organization.clone(),
                    phone: card.contact.phone.clone(),
                    photo: card.photo,
                    vip,
                },
                None => contact_card::Person {
                    name: name.unwrap_or_else(|| address.clone()),
                    email: address.clone(),
                    addresses: vec![address.clone()],
                    organization: None,
                    phone: None,
                    photo: app.photo(&address),
                    vip,
                },
            };
            let display = person.name.clone();
            let window = Rc::clone(&this);
            contact_card::present(&this.window, person, move |choice| match choice {
                contact_card::Choice::Write(to) => {
                    window.act(&window.conversation, Action::Mailto(to))
                }
                contact_card::Choice::ToggleVip => {
                    let Some(app) = window.app.upgrade() else {
                        return;
                    };
                    app.change_settings(Change::ToggleVip {
                        email: address.clone(),
                        name: display.clone(),
                    });
                    let added = app.settings().is_vip(&address);
                    window.toast(&vip_message(added, &display));
                }
                contact_card::Choice::AllMail => {
                    window.search(format!("from:{address}"));
                }
            });
        });
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
            .or_else(|| self.conversation.read(|o| o.account_id))
            .or_else(|| self.mailbox.borrow().account())
            .or_else(|| self.accounts.borrow().first().map(|a| a.id))
    }

    /// Applies `action` to the targets and keeps an undo for it. Actions that
    /// take mail out of the list move on to the next row, as Apple Mail does.
    fn triage(self: &Rc<Self>, action: TriageAction) {
        let view = Rc::clone(&self.conversation);
        let targets = self.reach(&view).targets;
        if targets.is_empty() {
            return;
        }
        self.follow_out(&view, &action);
        self.perform(targets, MailAction::Triage(action), History::Record, None);
    }

    /// Mutes the targets, or unmutes them when they are muted already.
    /// Gmail archives the replies to a muted thread with its own filters,
    /// so muting here is the label and one archive.
    fn toggle_mute(self: &Rc<Self>) {
        let view = Rc::clone(&self.conversation);
        let reach = self.reach(&view);
        if reach.targets.is_empty() {
            return;
        }
        let muted = !reach.muted;
        self.follow_out(
            &view,
            &if muted {
                TriageAction::Mute
            } else {
                TriageAction::Unmute
            },
        );
        self.perform(
            reach.targets,
            MailAction::Mute { muted },
            History::Record,
            None,
        );
    }

    /// Moves on once `action` takes the targets out of the mailbox on
    /// screen: the main window goes to the next row, and a conversation in
    /// a window of its own has nowhere to go, so the window closes.
    fn follow_out(self: &Rc<Self>, view: &ConversationView, action: &TriageAction) {
        if !leaves_list(&self.mailbox_of(view), action) {
            return;
        }
        if view.detached() {
            return view.close_detached();
        }
        let next = self.list.neighbour_of_selected();
        self.conversation.clear();
        self.list.unselect();
        match next {
            Some(next) => self
                .list
                .select(next.account_id, &next.id, next.message_id.as_deref()),
            None => self.nav.set_show_content(false),
        }
    }

    /// Asks before erasing, because Gmail cannot bring the mail back and no
    /// Undo follows.
    fn confirm_delete_forever(self: &Rc<Self>, view: &Rc<ConversationView>, targets: Vec<Target>) {
        let threaded = self.settings().threading;
        let dialog = adw::AlertDialog::new(
            Some(&delete_forever_heading(targets.len(), threaded)),
            Some(&match targets.len() {
                1 => gettext("Gmail deletes it from every device and cannot bring it back."),
                _ => gettext("Gmail deletes them from every device and cannot bring them back."),
            }),
        );
        dialog.add_responses(&[
            ("cancel", &gettext("Cancel")),
            ("delete", &gettext("Delete Forever")),
        ]);
        dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        dialog.set_close_response("cancel");
        // The question belongs over the window it was asked in, which for
        // a detached conversation is not the main one.
        let parent = view
            .window()
            .unwrap_or_else(|| self.window.clone().upcast());
        let (this, view) = (Rc::clone(self), Rc::clone(view));
        glib::spawn_future_local(async move {
            if dialog.choose_future(Some(&parent)).await == "delete" {
                this.delete_forever(&view, targets);
            }
        });
    }

    /// Erases the targets. Nothing reverses this, so the toast offers no
    /// Undo, and a missing permission leaves every row where it is.
    fn delete_forever(self: &Rc<Self>, view: &Rc<ConversationView>, targets: Vec<Target>) {
        let account_id = targets[0].account_id;
        let next = self.list.neighbour_of_selected();
        let (this, view) = (Rc::clone(self), Rc::clone(view));
        glib::spawn_future_local(async move {
            let actions = this.core.actions();
            let erased = this
                .core
                .call(async move { actions.erase(&targets).await })
                .await;
            let outcome = match erased {
                Ok(Permitted::Done(outcome)) => outcome,
                Ok(Permitted::NeedsPermission) => return this.ask_for_delete_access(account_id),
                Err(err) => {
                    return this.toast(&fill(
                        &gettext("Could not delete the mail: {reason}"),
                        &[("reason", &err.to_string())],
                    ));
                }
            };
            if !outcome.done.is_empty() {
                let kept = |row: &ThreadSummary| !outcome.done.contains(&Target::from_row(row));
                if view.detached() {
                    this.list.retain(kept);
                    view.close_detached();
                } else {
                    this.conversation.clear();
                    this.list.unselect();
                    this.list.retain(kept);
                    match next {
                        Some(next) => {
                            this.list
                                .select(next.account_id, &next.id, next.message_id.as_deref())
                        }
                        None => this.nav.set_show_content(false),
                    }
                }
                this.queue_refresh();
            }
            if let Some(error) = outcome.first_error() {
                return this.toast(error);
            }
            this.toast(&deleted_forever_message(
                outcome.done.len(),
                this.settings().threading,
            ));
        });
    }

    /// Reloads the photos every open conversation shows and draws each again.
    fn reopen_for_photos(self: &Rc<Self>) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        for view in self.views() {
            let senders = view.read(OpenThread::senders);
            if let Some(senders) = senders {
                view.set_photos(app.sender_photos(senders.into_iter().filter(|s| !s.is_empty())));
            }
        }
    }

    /// Explains that reading contacts needs one more Google permission,
    /// and offers to ask for it. Preferences reaches this through the app
    /// the first time somebody turns contacts on.
    pub fn ask_for_contacts_access(self: &Rc<Self>, account_id: AccountId) {
        let Some(account) = self.account(account_id) else {
            return;
        };
        let dialog = adw::AlertDialog::new(
            Some(&gettext("Allow Penguin Mail to Read Your Contacts")),
            Some(&fill(
                &gettext(
                    "Reading the contacts of {account} needs one more permission. Google \
                     asks you to confirm in your browser. Names and photos stay on this \
                     computer.",
                ),
                &[("account", &account.email)],
            )),
        );
        dialog.add_responses(&[
            ("cancel", &gettext("Not Now")),
            ("grant", &gettext("Grant Access")),
        ]);
        dialog.set_response_appearance("grant", adw::ResponseAppearance::Suggested);
        dialog.set_close_response("cancel");
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            if dialog.choose_future(Some(&this.window)).await == "grant" {
                this.authorize_with(Some(account.email), &[CONTACTS_SCOPE]);
            }
        });
    }

    /// Says an API is switched off in the Google Cloud project Penguin Mail
    /// signs in with. No permission fixes that, so this offers the page in
    /// Google Cloud that turns it on.
    pub fn explain_api_off(self: &Rc<Self>, service: &str, enable_url: &str) {
        let dialog = adw::AlertDialog::new(
            Some(&fill(
                &gettext("Turn On the {service}"),
                &[("service", service)],
            )),
            Some(&fill(
                &gettext(
                    "The Google Cloud project Penguin Mail signs in with has the {service} \
                     switched off, so Google refuses before it can ask for your permission. \
                     Turn it on, wait a minute, and try again.",
                ),
                &[("service", service)],
            )),
        );
        dialog.add_responses(&[
            ("cancel", &gettext("Not Now")),
            ("open", &gettext("Open Google Cloud")),
        ]);
        dialog.set_response_appearance("open", adw::ResponseAppearance::Suggested);
        dialog.set_close_response("cancel");
        let this = Rc::clone(self);
        let url = enable_url.to_string();
        glib::spawn_future_local(async move {
            if dialog.choose_future(Some(&this.window)).await == "open" {
                gtk::UriLauncher::new(&url).launch(
                    Some(&this.window),
                    gio::Cancellable::NONE,
                    |_| {},
                );
            }
        });
    }

    /// Hands the list the contact photos that are now on disk, so rows
    /// show faces instead of initials.
    pub fn contacts_loaded(self: &Rc<Self>) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        self.list.set_photos(&app.photos());
        self.reopen_for_photos();
    }

    /// Explains that erasing mail needs one more Gmail permission, and
    /// offers to ask Google for it.
    fn ask_for_delete_access(self: &Rc<Self>, account_id: AccountId) {
        let Some(account) = self.account(account_id) else {
            return;
        };
        let dialog = adw::AlertDialog::new(
            Some(&gettext("Allow Penguin Mail to Delete Mail")),
            Some(&fill(
                &gettext(
                    "Deleting mail for good needs one more permission for {account}. \
                     Google asks you to confirm in your browser.",
                ),
                &[("account", &account.email)],
            )),
        );
        dialog.add_responses(&[
            ("cancel", &gettext("Not Now")),
            ("grant", &gettext("Grant Access")),
        ]);
        dialog.set_response_appearance("grant", adw::ResponseAppearance::Suggested);
        dialog.set_close_response("cancel");
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            if dialog.choose_future(Some(&this.window)).await == "grant" {
                this.authorize_with(Some(account.email), &[DELETE_SCOPE]);
            }
        });
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
            let actions = this.core.actions();
            let gone = this
                .core
                .call(async move { actions.gone_from(folder, &targets).await })
                .await
                .unwrap_or_default();
            if gone.is_empty() {
                return;
            }
            this.list
                .retain(|row| !gone.contains(&Target::from_row(row)));
            if this.conversation.read(|o| o.among(&gone)) == Some(true) {
                this.conversation.clear();
            }
        });
    }

    /// Runs `action` on the targets. With `History::Record`, the toast says
    /// what changed, `message` in place of the usual text, and offers Undo.
    fn perform(
        self: &Rc<Self>,
        targets: Vec<Target>,
        action: MailAction,
        history: History,
        message: Option<String>,
    ) {
        if targets.is_empty() {
            return;
        }
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let outcome = this.core.act(targets, action.clone(), history).await;
            this.show_changes(&action, &outcome);
            if let Some(error) = outcome.first_error() {
                return this.toast(error);
            }
            if history == History::Skip {
                // Putting mail back can add rows to a Gmail folder, and
                // only a fresh search shows them.
                this.core.forget_remote();
                this.reload_folder();
                return;
            }
            this.prune_folder(&outcome.done);
            let count = outcome.done.len();
            if let Some(done) =
                message.or_else(|| done_message(&action, count, this.settings().threading))
            {
                let toast = adw::Toast::builder()
                    .title(done)
                    .button_label(gettext("Undo"))
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

    /// Updates what the store's change events do not cover: flag colours
    /// and the Remind Me list.
    fn show_changes(self: &Rc<Self>, action: &MailAction, outcome: &Outcome) {
        if outcome.done.is_empty() {
            return;
        }
        match action {
            MailAction::Flag(color) => {
                for view in self.views() {
                    if view.read(|o| o.among(&outcome.done)) == Some(true) {
                        view.set_flag_color(*color);
                    }
                }
                self.queue_refresh();
            }
            MailAction::Remind { .. } | MailAction::CancelReminder => self.reminders_changed(),
            MailAction::DismissFollowUp => self.follow_ups_changed(),
            MailAction::Triage(_) | MailAction::Label { .. } | MailAction::Mute { .. } => {}
        }
    }

    /// Reverses the organizing action on top of the undo stack, whether
    /// the window or the assistant took it. The one before it is left for
    /// the next press.
    fn undo(self: &Rc<Self>) {
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let Some(undone) = this.core.undo().await else {
                return this.toast(&gettext("Nothing to undo"));
            };
            // Undo can put rows back into a Gmail folder, which only a
            // fresh search shows. The store's own change events cover
            // every other mailbox, so one reload is enough either way.
            if this.mailbox.borrow().is_remote() {
                this.core.forget_remote();
                this.reload_folder();
                this.refresh_counts();
            } else {
                this.queue_refresh();
            }
            this.refresh_flag_color();
            this.reminders_changed();
            match undone.outcome.first_error() {
                Some(error) => this.toast(error),
                None => this.toast(&fill(
                    &gettext("{action} undone"),
                    &[("action", &undone.action.describe())],
                )),
            }
        });
    }

    /// Labels of the targets' account, checked when the one open
    /// conversation already has them.
    fn label_popover(self: &Rc<Self>) -> gtk::Popover {
        let popover = gtk::Popover::new();
        let targets = self.reach(&self.conversation).targets;
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
            popover.set_child(Some(&message(&if targets.is_empty() {
                gettext("Open or select mail to label it.")
            } else {
                gettext("Select mail from one account to label it.")
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
                    .label(gettext("New Label…"))
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
            content.append(&message(&gettext("This account has no labels yet.")));
            content.append(&create);
            popover.set_child(Some(&content));
            return popover;
        }
        let applied: HashSet<String> = if targets.len() == 1 {
            self.conversation
                .read(|o| {
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
            let row = gtk::ListBoxRow::builder()
                .child(&row)
                .activatable(true)
                .build();
            // The tick beside the name is drawn at zero opacity when the
            // label is off, which says nothing out loud.
            crate::ui::name(
                &row,
                &label_row_name(&label.name, applied.contains(&label.id)),
            );
            list.append(&row);
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
        app.change_settings(Change::StepTextSize(step));
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

    /// Replies to or forwards the newest message in `view`.
    pub(super) fn reply(self: &Rc<Self>, view: &ConversationView, kind: ReplyKind) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let prepared = view.find(|open| {
            let target = open.reply_target()?.clone();
            let text = match open.bodies.get(&target.id) {
                Some(Ok(body)) => compose::body_text(body),
                _ => target.snippet.clone(),
            };
            // A forward keeps the original's HTML and its inline images,
            // so what goes out is the message that arrived.
            let html = match open.bodies.get(&target.id) {
                Some(Ok(body)) if kind == ReplyKind::Forward => body.html.clone(),
                _ => None,
            };
            let attachments = match open.bodies.get(&target.id) {
                Some(Ok(body)) if kind == ReplyKind::Forward => body.attachments.clone(),
                _ => Vec::new(),
            };
            Some((
                open.account_id,
                target,
                text,
                html,
                open.messages.clone(),
                attachments,
            ))
        });
        let Some((account_id, target, text, html, thread, attachments)) = prepared else {
            return;
        };
        let forwarded_html = html.clone();
        // Every address the account sends as, so the reply comes from the
        // one the message was written to.
        let mine = app.my_addresses(account_id);
        let mut draft = app.signed(compose::respond(
            kind,
            account_id,
            &mine,
            &target,
            &text,
            html.as_deref(),
            &thread,
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
                        // An image the forwarded HTML shows keeps its id,
                        // so the `cid:` in that HTML still finds it. One
                        // the HTML never names travels as a file, which is
                        // how it arrived.
                        content_id: attachment.content_id.filter(|cid| {
                            forwarded_html
                                .as_deref()
                                .is_some_and(|html| compose::refers_to_cid(html, cid))
                        }),
                        filename: attachment.filename,
                        mime_type: attachment.mime_type,
                        data,
                    }),
                    Err(err) => this.toast(&fill(
                        &gettext("Could not include {file}: {reason}"),
                        &[("file", &attachment.filename), ("reason", &err.to_string())],
                    )),
                }
            }
            app.compose(draft);
        });
    }

    /// Opens the draft in `view` in the composer.
    pub(super) fn edit_draft_from(self: &Rc<Self>, view: &ConversationView) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let found = view.find(|open| {
            let draft = open
                .messages
                .iter()
                .rev()
                .find(|m| m.has_label(system_label::DRAFT))?
                .clone();
            let body = match open.bodies.get(&draft.id) {
                Some(Ok(body)) => Some(body.clone()),
                _ => None,
            };
            Some((
                open.account_id,
                open.thread_id.clone(),
                open.messages.len() > 1,
                draft,
                body,
            ))
        });
        let Some((account_id, thread_id, in_thread, message, body)) = found else {
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
            if let Some(body) = &body {
                draft.take_body(body);
            }
            draft.thread_id = in_thread.then_some(thread_id);
            if let Some(id) = draft_id.clone() {
                let outbox = this.core.outbox();
                draft.send_at = this
                    .core
                    .call(async move { outbox.find_draft(account_id, &id).await })
                    .await
                    .ok()
                    .flatten()
                    .map(|s| s.send_at);
            }
            draft.draft_id = draft_id;
            app.compose(draft);
        });
    }

    /// Downloads attachment `index` of `message_id` in `view`.
    pub(super) fn save_attachment_from(
        self: &Rc<Self>,
        view: &ConversationView,
        message_id: String,
        index: usize,
    ) {
        let found = view.find(|open| {
            let body = open.bodies.get(&message_id)?.as_ref().ok()?;
            Some((open.account_id, body.attachments.get(index)?.clone()))
        });
        let Some((account_id, attachment)) = found else {
            return;
        };
        // A file out of an encrypted message never reached Gmail, so its
        // bytes are here or nowhere.
        if self.save_opened_file_from(view, &message_id, index, &attachment) {
            return;
        }
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
            this.toast(&fill(
                &gettext("Downloading {file}…"),
                &[("file", &attachment.filename)],
            ));
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
                    let saved_to_downloads =
                        fill(&gettext("Saved {file} to Downloads"), &[("file", &name)]);
                    let toast = adw::Toast::builder()
                        .title(glib::markup_escape_text(&saved_to_downloads))
                        .button_label(gettext("Open"))
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
                Err(err) => this.toast(&fill(
                    &gettext("Could not save {file}: {reason}"),
                    &[("file", &attachment.filename), ("reason", &err.to_string())],
                )),
            }
        });
    }

    // ---- Accounts ----------------------------------------------------------

    fn save_config(self: &Rc<Self>, client_id: String, client_secret: String) {
        match self
            .core
            .save_config(mailrs_sync::config::Config::new(client_id, client_secret))
        {
            Ok(()) => self.refresh_accounts(Reload::Yes),
            Err(err) => self.toast(&fill(
                &gettext("Could not save the settings: {reason}"),
                &[("reason", &err.to_string())],
            )),
        }
    }

    fn authorize(self: &Rc<Self>, expected: Option<String>) {
        self.authorize_with(expected, &[]);
    }

    /// Runs the consent flow, asking Google for `extra` permissions on top
    /// of the ones sign-in always requests.
    fn authorize_with(self: &Rc<Self>, expected: Option<String>, extra: &'static [&'static str]) {
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
        self.first_account
            .set_label(&gettext("Waiting for Your Browser…"));
        self.sidebar.add_account.set_sensitive(false);
        self.toast(&gettext("Continue in your browser"));
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            match this.core.authorize_account(urls, expected, extra).await {
                Ok(account) => {
                    this.toast(&fill(
                        &gettext("Added {account}. Downloading mail…"),
                        &[("account", &account.email)],
                    ));
                    this.refresh_accounts(Reload::Yes);
                }
                Err(err) => this.toast(&err.to_string()),
            }
            this.authorizing.set(false);
            this.first_account.set_sensitive(true);
            this.first_account
                .set_label(&gettext("Sign In with Google"));
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
            Some(&fill(
                &gettext("Remove {account}?"),
                &[("account", &account.email)],
            )),
            Some(&gettext(
                "Its downloaded mail and saved sign-in are deleted from this computer. \
                 Nothing changes in Gmail.",
            )),
        );
        dialog.add_responses(&[
            ("cancel", &gettext("Cancel")),
            ("remove", &gettext("Remove")),
        ]);
        dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
        dialog.set_close_response("cancel");
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            if dialog.choose_future(Some(&this.window)).await != "remove" {
                return;
            }
            if this.conversation.read(|o| o.account_id) == Some(account.id) {
                this.conversation.clear();
            }
            let email = account.email.clone();
            match this.core.remove_account(account).await {
                Ok(()) => this.toast(&fill(&gettext("Removed {account}"), &[("account", &email)])),
                Err(err) => this.toast(&fill(
                    &gettext("Could not remove {account}: {reason}"),
                    &[("account", &email), ("reason", &err.to_string())],
                )),
            }
            this.refresh_accounts(Reload::Yes);
        });
    }

    // ---- Actions, menu, and keys -------------------------------------------

    fn install_actions(self: &Rc<Self>) {
        self.install_outbox_actions();
        self.install_main_actions();
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
            "account-hide-my-email",
            Box::new(|win, account| win.show_hide_my_email(Some(account.id))),
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

        self.window.add_controller(shortcuts::main_chords());

        let weak = Rc::downgrade(self);
        self.window.connect_close_request(move |_| {
            if let (Some(win), Some(app)) =
                (weak.upgrade(), weak.upgrade().and_then(|w| w.app.upgrade()))
            {
                win.conversation.stop_rendering();
                app.forget_window(&win);
            }
            glib::Propagation::Proceed
        });
    }

    fn install_menu(&self) {
        let menu = gio::Menu::new();
        let first = gio::Menu::new();
        first.append(Some(&gettext("Check for Mail")), Some("win.check"));
        first.append(Some(&gettext("Add Account…")), Some("win.add-account"));
        first.append(Some(&gettext("New Smart Mailbox…")), Some("win.smart-new"));
        first.append(Some(&gettext("Hide My Email…")), Some("win.hide-my-email"));
        menu.append_section(None, &first);
        if self.app.upgrade().is_some_and(|app| app.can_update()) {
            menu.append_section(None, &self.update_menu);
        }
        let second = gio::Menu::new();
        second.append(Some(&gettext("Preferences")), Some("win.preferences"));
        second.append(Some(&gettext("Keyboard Shortcuts")), Some("win.shortcuts"));
        second.append(Some(&gettext("About Penguin Mail")), Some("win.about"));
        second.append(Some(&gettext("Quit")), Some("win.quit"));
        menu.append_section(None, &second);
        let button = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .menu_model(&menu)
            .primary(true)
            .tooltip_text(gettext("Main Menu"))
            .build();
        crate::ui::name(&button, &gettext("Main Menu"));
        self.sidebar.header.pack_end(&button);
    }

    fn install_keys(self: &Rc<Self>) {
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, key, _, modifiers| match weak.upgrade() {
            Some(win) => win.letter_pressed(key, modifiers),
            None => glib::Propagation::Proceed,
        });
        self.window.add_controller(keys);
    }

    /// Ctrl+F: find inside the message when the reader is in it, and
    /// search the mailbox everywhere else. The two share the key and
    /// never the focus. A separate window has no mailbox to search.
    fn find(self: &Rc<Self>, view: &ConversationView) {
        match view.detached() || view.has_focus() {
            true => view.open_find(),
            false => self.list.open_search(),
        }
    }

    /// Whether Escape has a selection of several rows or a search to close.
    fn has_selection_to_clear(&self) -> bool {
        self.list.selected_rows().len() > 1 || self.list.search_open()
    }

    /// Escape: drops a selection of several rows, or else closes the search.
    fn clear_selection(&self) {
        if self.list.selected_rows().len() > 1 {
            self.list.unselect();
            self.conversation.clear();
        } else if self.list.search_open() {
            self.list.close_search();
        }
    }

    /// Shows the assistant beside the mail and puts the cursor in it, or
    /// hides it again.
    fn toggle_assistant(&self) {
        let show = !self.assistant_split.shows_sidebar();
        self.assistant_split.set_show_sidebar(show);
        if show {
            self.assistant.focus();
        }
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
            return self.toast(&gettext("Add an account first"));
        };
        app.compose(app.signed(Draft::new(account_id, app.identity(account_id))));
    }

    /// Opens a thread from outside the window, such as a notification, and
    /// answers it when `then` asks for that.
    pub fn reveal(self: &Rc<Self>, account_id: AccountId, thread_id: String, then: Reveal) {
        let inbox = Mailbox::Unified(system_label::INBOX);
        if *self.mailbox.borrow() != inbox {
            self.sidebar.select(&inbox);
            self.show_mailbox(inbox);
        }
        // Selecting a row before the list's rows land finds nothing, so
        // the feed holds the thread until the first page is on screen.
        let now = self.feed.borrow_mut().reveal((account_id, thread_id, then));
        if let Some((account_id, thread_id, then)) = now {
            self.select_revealed(account_id, thread_id, then);
        }
    }

    /// Selects a thread [`MainWindow::reveal`] asked for, now that the
    /// list holds its row, and answers it when `then` asks for that.
    fn select_revealed(self: &Rc<Self>, account_id: AccountId, thread_id: String, then: Reveal) {
        self.list.select(account_id, &thread_id, None);
        if then == Reveal::Reply {
            let this = Rc::clone(self);
            glib::spawn_future_local(async move {
                this.reply_when_open(account_id, &thread_id).await;
            });
        }
    }

    /// Answers a thread once the conversation has it, with its body rather
    /// than its snippet where the wait is long enough for Gmail to answer.
    async fn reply_when_open(self: &Rc<Self>, account_id: AccountId, thread_id: &str) {
        let deadline = std::time::Instant::now() + REVEAL_WAIT;
        loop {
            let quotable = self
                .conversation
                .read(|open| {
                    open.account_id == account_id && open.thread_id == thread_id && open.quotable()
                })
                .unwrap_or(false);
            if quotable || std::time::Instant::now() >= deadline {
                break;
            }
            glib::timeout_future(REVEAL_STEP).await;
        }
        self.reply(&self.conversation, ReplyKind::Reply);
    }

    /// Screenshot hooks, honoured only in demo mode: `MAILRS_DEMO_OPEN`
    /// opens a thread by id, `MAILRS_DEMO_SEARCH` runs a search,
    /// `MAILRS_DEMO_COMPOSE=reply` opens a reply to the open thread, and
    /// `MAILRS_DEMO_ACTION` activates a window action such as `shortcuts`,
    /// or one with a target such as `account-rules(int64 1)`. With a
    /// thread to open, the action waits for it, so `toggle-vip` has a
    /// sender to add.
    pub fn run_demo_script(self: &Rc<Self>) {
        if !self.core.demo {
            return;
        }
        let this = Rc::clone(self);
        glib::timeout_add_local_once(std::time::Duration::from_millis(900), move || {
            if std::env::var_os("MAILRS_DEMO_OPEN").is_none() {
                this.run_demo_action();
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
                        let replier = Rc::clone(&finder);
                        glib::timeout_add_local_once(
                            std::time::Duration::from_millis(900),
                            move || {
                                if std::env::var("MAILRS_DEMO_COMPOSE").as_deref() == Ok("reply") {
                                    replier.reply(&replier.conversation, ReplyKind::Reply);
                                }
                                replier.run_demo_action();
                            },
                        );
                    }
                });
            }
        });
    }

    /// Activates the window action `MAILRS_DEMO_ACTION` names, if any.
    fn run_demo_action(&self) {
        let Ok(detailed) = std::env::var("MAILRS_DEMO_ACTION") else {
            return;
        };
        match gio::Action::parse_detailed_name(&detailed) {
            Ok((name, target)) => {
                let _ = WidgetExt::activate_action(
                    &self.window,
                    &format!("win.{name}"),
                    target.as_ref(),
                );
            }
            Err(err) => tracing::warn!(action = %detailed, error = %err, "unreadable demo action"),
        }
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
        let sender = self.conversation.find(|o| o.other_sender().cloned());
        let (Some(sender), Some(app)) = (sender, self.app.upgrade()) else {
            return self.toast(&gettext("Open a message from the person first"));
        };
        let name = sender.name.clone().unwrap_or_default();
        app.change_settings(Change::ToggleVip {
            email: sender.email.clone(),
            name,
        });
        let added = app.settings().is_vip(&sender.email);
        self.toast(&vip_message(added, sender.display()));
    }

    /// Brings what is on screen back in line after a settings change.
    /// `Effects` comes in the order the window wants: the accounts first,
    /// because the rows and the smart mailbox on screen read what it sets.
    pub fn settings_changed(self: &Rc<Self>, effects: &Effects) {
        for effect in effects.iter() {
            self.apply_effect(effect);
        }
    }

    fn apply_effect(self: &Rc<Self>, effect: Effect) {
        let settings = self.settings();
        match effect {
            Effect::ListShape => {
                self.conversation.clear();
                self.list.unselect();
                let mailbox = self.mailbox.borrow().clone();
                match mailbox {
                    Mailbox::Search { query, .. } => self.search(query),
                    Mailbox::Folder { .. } => self.reload_folder(),
                    _ => self.reload_list(),
                }
            }
            Effect::Accounts => self.refresh_accounts(Reload::Yes),
            Effect::RowColors => {
                // Rows carry account colours; Effect::Accounts sets the new
                // ones first.
                let list = Rc::clone(&self.list);
                glib::timeout_add_local_once(std::time::Duration::from_millis(200), move || {
                    list.rebind();
                });
            }
            Effect::SmartMailboxes => {
                // The mailbox on screen carries its own conditions, so an edit
                // has to put the saved ones back before listing it again.
                if let Mailbox::Smart(shown) = self.mailbox.borrow().clone()
                    && let Some(saved) = settings.smart_mailboxes.iter().find(|m| m.id == shown.id)
                {
                    *self.mailbox.borrow_mut() = Mailbox::Smart(saved.clone());
                    self.reload_list();
                }
            }
            Effect::Vips => {
                for view in self.views() {
                    view.set_sender_vip(sender_is_vip(&view, &settings));
                }
            }
            Effect::FollowUps => {
                if !settings.suggest_follow_ups && *self.mailbox.borrow() == Mailbox::FollowUp {
                    let inbox = Mailbox::Unified(system_label::INBOX);
                    self.sidebar.select(&inbox);
                    self.show_mailbox(inbox);
                }
                self.refresh_counts();
            }
            Effect::Categories => {
                self.follow_categories();
                self.reload_list();
            }
            Effect::Assistant => self.assistant.refresh(),
            Effect::TextSize => {
                for view in self.views() {
                    view.set_zoom(settings.text_size.zoom());
                }
            }
            Effect::Contacts => {
                if let Some(app) = self.app.upgrade() {
                    self.list.set_photos(&app.photos());
                }
                self.reopen_for_photos();
            }
            // The app follows the light or dark choice; no window to redraw.
            Effect::Theme => {}
            Effect::Language => self.offer_restart(),
        }
    }

    pub fn toast_sent(&self) {
        self.toast(&gettext("Message sent"));
    }

    /// Says that the new language waits for a restart, and offers one.
    /// Nothing on screen changes until then, so the toast stays up and is
    /// plain about it.
    fn offer_restart(self: &Rc<Self>) {
        let toast = adw::Toast::builder()
            .title(gettext(
                "Penguin Mail shows the new language after a restart",
            ))
            .button_label(gettext("Restart"))
            .timeout(0)
            .build();
        let weak = Rc::downgrade(self);
        toast.connect_button_clicked(move |_| {
            if let Some(app) = weak.upgrade().and_then(|window| window.app.upgrade()) {
                app.restart();
            }
        });
        self.toasts.add_toast(toast);
    }

    fn show_shortcuts(&self) {
        shortcuts::dialog().present(Some(&self.window));
    }

    fn show_about(self: &Rc<Self>) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let about = crate::ui::about::About::new(app.can_update());
        if let Some(state) = app.update_state() {
            about.show_update(&state);
        }
        let weak = Rc::downgrade(self);
        about.dialog.connect_closed(move |_| {
            if let Some(win) = weak.upgrade() {
                win.about.replace(None);
            }
        });
        about.dialog.present(Some(&self.window));
        self.about.replace(Some(about));
    }

    /// The answer to a check the person asked for, when nothing is waiting
    /// to install. The About window shows it under its button, so a toast
    /// would only repeat it.
    pub fn answer_update_check(&self, text: &str) {
        if self.about.borrow().is_none() {
            self.toast(text);
        }
    }
}

pub(super) fn read_cached_body(
    c: &rusqlite::Connection,
    account_id: AccountId,
    message_id: &str,
) -> mailrs_store::Result<Option<MessageBody>> {
    // Readers cannot write, so this leaves the access time alone; the body
    // fetch that follows records the access.
    let body = mailrs_store::bodies::peek_body(c, account_id, message_id)?;
    // A body cached before this app read provenance has none, and nothing
    // would ever put it there: the cache would answer for that message
    // forever. Treating it as a miss costs one fetch, once, and fills the
    // gap for good. A message with a body that truly says nothing about
    // its origins is rare, and pays that fetch once as well.
    Ok(body.filter(|body| !body.provenance.is_empty()))
}

/// Whether the newest sender in `view` who is not the user is a VIP.
fn sender_is_vip(view: &ConversationView, settings: &Settings) -> bool {
    view.read(|o| o.other_sender().is_some_and(|a| settings.is_vip(&a.email)))
        .unwrap_or(false)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_muted_list_is_the_one_named_by_the_mute_label() {
        assert!(lists_muted(&Mailbox::Unified(system_label::MUTE)));
        assert!(lists_muted(&Mailbox::Label {
            account_id: 1,
            label_id: system_label::MUTE.into(),
            name: "Muted".into(),
        }));
        assert!(!lists_muted(&Mailbox::Unified(system_label::INBOX)));
        assert!(!lists_muted(&Mailbox::Reminders));
    }

    #[test]
    fn mail_leaves_a_list_only_when_the_action_takes_it_out_of_that_mailbox() {
        let inbox = Mailbox::Unified(system_label::INBOX);
        let all_mail = Mailbox::Folder {
            account_id: None,
            folder: Folder::AllMail,
        };
        assert!(leaves_list(&inbox, &TriageAction::Archive));
        assert!(!leaves_list(&all_mail, &TriageAction::Archive));
        assert!(leaves_list(&all_mail, &TriageAction::Trash));
        assert!(!leaves_list(&all_mail, &TriageAction::Mute));
        assert!(leaves_list(
            &Mailbox::Unified(system_label::MUTE),
            &TriageAction::Unmute
        ));
    }

    #[test]
    fn a_mute_toast_counts_the_conversations_it_covers() {
        let toast = |muted, count| done_message(&MailAction::Mute { muted }, count, true);
        assert_eq!(toast(true, 1).as_deref(), Some("Muted"));
        assert_eq!(toast(true, 3).as_deref(), Some("Muted 3 conversations"));
        assert_eq!(toast(false, 1).as_deref(), Some("Unmuted"));
        assert_eq!(toast(false, 2).as_deref(), Some("Unmuted 2 conversations"));
    }

    #[test]
    fn a_label_row_says_whether_the_mail_already_carries_it() {
        assert_eq!(label_row_name("Receipts", false), "Receipts");
        assert_eq!(label_row_name("Receipts", true), "Receipts, on this mail");
        assert_eq!(label_row_name("Work/Tax", false), "Work › Tax");
    }
}
