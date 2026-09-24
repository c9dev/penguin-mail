//! The main window: sidebar, thread list, and conversation, plus the
//! first-run pages. It reacts to engine events and turns user actions into
//! calls on the sync core.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk::{gio, glib};
use mailrs_domain::translate::{fill, fill_plural, gettext, with_reason};
use mailrs_domain::{
    Account, AccountId, AccountState, ChangeEvent, Label, MessageBody, Role, Target, ThreadSummary,
};
use mailrs_sync::{
    History, Listing, Loaded, MailAction, Offers, Permitted, Scope, TriageAction, View,
};

use super::confirm::{Tone, confirm};
use super::contact_card;
use super::conversation::{Action, ConversationView};
use super::list_feed::{Coalesce, Refresh, Splice, Ticket};
use super::permission;
use super::sidebar::Sidebar;
use super::thread_list::{Picked, ThreadList};
use super::{Mailbox, welcome};
use crate::app::{App, Signature};
use crate::assistant::ToolRequest;
use crate::compose::{self, ReplyKind};
use crate::core::Core;
use crate::offered::Filing;
use crate::open_thread::OpenThread;
use crate::permission::{Occasion, Permission};
use crate::settings::{Change, Effect, Settings};
use aftermath::Cause;
use futures::FutureExt;
use futures::future::{LocalBoxFuture, Shared};
use on_screen::{OnScreen, Redraw};
use press::{Press, PressEffects, Pressed, Question};
use reach::Reach;

mod aftermath;
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
mod message_menu;
mod notice;
mod on_screen;
mod organize;
mod outbox;
mod pgp;
mod pictures;
mod press;
mod previews;
mod reach;
mod reminders;
mod reveal;
mod scheduled;
mod senders;
mod shortcuts;
mod thread;
mod translation;
mod triage;

pub use notice::Notice;

/// Bodies fetched at once when a thread opens. Each one is a 5-unit Gmail
/// call and an account may spend 250 units a second.
pub(super) const BODY_FETCHES: usize = 10;

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
    /// The mailbox on screen, the one a search goes back to, the inbox
    /// category, and what the thread list loads next.
    screen: RefCell<OnScreen>,
    /// Requests for the sidebar counts, which share one count.
    counts: RefCell<on_screen::Counts>,
    /// The accounts read in flight, which a redraw of the row colours
    /// waits for, since the colours come with it.
    accounts_read: RefCell<Option<Shared<LocalBoxFuture<'static, ()>>>>,
    authorizing: Cell<bool>,
    assistant: Rc<super::assistant::AssistantPane>,
    assistant_split: adw::OverlaySplitView,
    categories: categories::CategoryBar,
    follow_up: followup::FollowUpBanner,
    /// The inline images and the attachment rows' pictures already
    /// fetched.
    pictures: Rc<pictures::Pictures>,
    /// Senders whose remote images may load. Read from the store once and
    /// kept here, since every thread that opens asks about it.
    image_senders: RefCell<Vec<mailrs_store::image_senders::ImageSender>>,
    /// The conversations in windows of their own, each with the mailbox it
    /// was opened from, so a flag colour or an undo reaches them too. An
    /// entry that no longer upgrades is a window somebody closed.
    detached: RefCell<Vec<(Weak<ConversationView>, Mailbox)>>,
    /// The scratch copies of attachments this window opened.
    previews: previews::Previews,
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

/// What a row of the label popover says. The tick beside the name is the
/// only sign that a label is already on the mail, so the name carries it.
fn label_row_name(label: &str, applied: bool) -> String {
    let shown = shown_name(label);
    match applied {
        true => fill(&gettext("{label}, on this mail"), &[("label", &shown)]),
        false => shown,
    }
}

/// A nested label's name as the picker shows it, `Work › Tax` for
/// `Work/Tax`.
fn shown_name(label: &str) -> String {
    label.replace('/', " › ")
}

/// What choosing a row of the label picker does. On an account that files
/// in folders a message sits in one folder, so a row moves it there and
/// the toast names it as `name`; with labels a row puts its label on, or
/// takes it off when `applied`.
fn filing_choice(filing: Filing, id: &str, name: &str, applied: bool) -> Press {
    match filing {
        Filing::Folders => Press::Move {
            folder: id.to_string(),
            name: name.to_string(),
        },
        Filing::Labels if applied => Press::Label(TriageAction::RemoveLabel(id.to_string())),
        Filing::Labels => Press::Label(TriageAction::AddLabel(id.to_string())),
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
    with_reason(&gettext("Could not load mail: {reason}"), err, &[])
}

/// Where release builds, which carry the Google client, are published.
const RELEASES: &str = "https://github.com/c9dev/penguin-mail/releases";

/// A toast's title as Pango markup. A toast reads its title as markup, so
/// a label called "R&D" would otherwise show nothing at all.
fn toast_title(text: &str) -> glib::GString {
    glib::markup_escape_text(text)
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
            list.set_row_menu(&conversation.thread_menu());
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
                .pin_sidebar(true)
                .build();
            // Unpinned, libadwaita shows the sidebar again whenever the
            // window grows past a breakpoint, so the assistant would open
            // by itself after the window had been narrow. Pinned, it stays
            // as the person left it; narrowing the window still closes it.
            assistant_split.connect_collapsed_notify(|split| {
                if split.is_collapsed() {
                    split.set_show_sidebar(false);
                }
            });
            assistant_split
                .bind_property("show-sidebar", &list.assistant_button, "active")
                .bidirectional()
                .sync_create()
                .build();
            let stack = gtk::Stack::builder()
                .transition_type(gtk::StackTransitionType::Crossfade)
                .build();
            stack.add_named(&assistant_split, Some("mail"));
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
            // The assistant sits beside the mail only while the window has
            // room for both: the mailboxes, the list and the conversation
            // need about 720sp between them, and the assistant 320sp. Below
            // that it slides over the mail instead. Beside the mail in a
            // narrower window, libadwaita cuts off the right edge of the
            // open assistant, and squeezes the closed one below its minimum
            // width.
            let wide = adw::Breakpoint::new(
                adw::BreakpointCondition::parse("max-width: 1100sp").expect("valid breakpoint"),
            );
            wide.add_setter(&assistant_split, "collapsed", Some(&true.to_value()));
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
            window.add_breakpoint(wide);
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
                screen: RefCell::new(OnScreen::new(app.settings_with(|s| s.default_category))),
                counts: RefCell::new(on_screen::Counts::default()),
                accounts_read: RefCell::new(None),
                authorizing: Cell::new(false),
                assistant,
                assistant_split,
                categories: categories::CategoryBar::new(app.settings_with(|s| s.default_category)),
                follow_up: followup::FollowUpBanner::new(),
                pictures: Rc::new(pictures::Pictures::new(Rc::clone(&app.core))),
                image_senders: RefCell::new(Vec::new()),
                detached: RefCell::new(Vec::new()),
                previews: previews::Previews::default(),
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
                    let reach = win.reach(&win.conversation);
                    let accounts = reach.targets.iter().map(|t| t.account_id);
                    win.word_filing(&win.conversation, accounts);
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
                .labels()
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
            if !button.is_active() {
                win.change_screen(OnScreen::search_closed);
            }
        });
        let weak = Rc::downgrade(&window);
        window.list.banner.connect_button_clicked(move |_| {
            let Some(win) = weak.upgrade() else { return };
            let email = win
                .accounts()
                .into_iter()
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
            .set_zoom(app.settings_with(|s| s.text_size.zoom()));
        window.refresh_accounts(Reload::Yes);
        window.reload_image_senders();
        // Copies another program opened in an earlier run have had their
        // chance; nothing else deletes them.
        // The handle is dropped; the sweep runs on to the end regardless.
        drop(gio::spawn_blocking(|| previews::sweep(&previews::folder())));
        window
    }

    pub fn present(&self) {
        self.window.present();
    }

    pub fn is_active(&self) -> bool {
        self.window.is_active() && self.window.is_visible()
    }

    /// Shows where an update stands, or hides the banner when nothing does.
    /// Each state's button runs an app action, so the banner needs no
    /// callbacks of its own.
    fn show_update(&self, state: &crate::update::State) {
        if let Some(about) = self.about.borrow().as_ref() {
            about.show_update(state);
        }
        let shown = crate::update::shown(state);
        self.update_menu.remove_all();
        self.update_menu
            .append(Some(&shown.menu.label), shown.menu.action);
        let banner = &self.update_banner;
        let Some(news) = shown.banner else {
            banner.set_revealed(false);
            return;
        };
        banner.set_title(&news.title);
        match news.button {
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
                .title(toast_title(text))
                .timeout(4)
                .build(),
        );
    }

    /// Toasts a failure. `said` is `gettext` of the sentence, with
    /// `{reason}` where the error goes.
    fn failed(&self, said: &str, err: &impl std::fmt::Display) {
        self.toast(&with_reason(said, err, &[]));
    }

    // ---- Engine events -------------------------------------------------

    fn handle(self: &Rc<Self>, event: &ChangeEvent) {
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
                let coalesce = self.screen.borrow_mut().feed().changed(changed.collect());
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
        let coalesce = self.screen.borrow_mut().feed().everything();
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
            let refresh = win.screen.borrow_mut().feed().fire();
            win.refresh_counts();
            // A conversation the events did not name has nothing new.
            win.refresh_open_threads(|account_id, thread_id| refresh.names(account_id, thread_id));
            match refresh {
                Refresh::Reload => win.reload_list(),
                Refresh::Splice(ticket, changed) => win.splice_changed(ticket, changed),
            }
        });
    }

    /// Re-reads the accounts and their labels, and with [`Reload::Yes`]
    /// lists the mailbox again. A remote mailbox lists through Gmail, so
    /// only a change that can alter its rows is worth that.
    fn refresh_accounts(self: &Rc<Self>, reload: Reload) {
        let this = Rc::clone(self);
        let read = async move { this.read_accounts(reload).await }
            .boxed_local()
            .shared();
        *self.accounts_read.borrow_mut() = Some(read.clone());
        glib::spawn_future_local(read);
    }

    /// Reads the accounts and their labels and redraws what shows them.
    async fn read_accounts(self: &Rc<Self>, reload: Reload) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let data = match app.reload_accounts().await {
            Ok(data) => data,
            Err(err) => {
                return self.failed(&gettext("Could not read accounts: {reason}"), &err);
            }
        };
        let page = if data.is_empty() {
            "first-account"
        } else {
            "mail"
        };
        self.stack.set_visible_child_name(page);
        // A label deleted elsewhere, by the assistant or in the browser,
        // leaves the window on a mailbox that is no longer there, so the
        // inbox takes over as it does for a signed-out account.
        let still_there = still_there(&self.shown(), &data);
        let vanished = self.screen.borrow_mut().accounts_read(still_there);
        let settings = self.settings();
        let (data, extras) = self.arrange(data, &settings);
        self.list.set_vips(settings.vips.keys().cloned().collect());
        let mailbox = self.shown();
        if !matches!(mailbox, Mailbox::Search { .. }) {
            self.sidebar
                .rebuild(&data, &extras, &mailbox, |id| self.offers(id));
        }
        // Each hidden address comes with its own rules, so the window's
        // Hide My Email works only while some account can hold them.
        if let Some(action) = self
            .actions
            .lookup_action("hide-my-email")
            .and_downcast::<gio::SimpleAction>()
        {
            action.set_enabled(data.iter().any(|(a, _)| self.offers(a.id).rules));
        }
        self.list
            .set_show_accounts(mailbox.account().is_none() && data.len() > 1);
        let reauth: Vec<&str> = data
            .iter()
            .filter(|(a, _)| a.state == AccountState::NeedsReauth)
            .map(|(a, _)| a.email.as_str())
            .collect();
        match reauth.first() {
            Some(email) => {
                self.list.banner.set_title(&fill(
                    &gettext("Sign in again to keep {account} syncing"),
                    &[("account", email)],
                ));
                self.list.banner.set_button_label(Some(&gettext("Sign In")));
                self.list.banner.set_revealed(true);
            }
            None => self.list.banner.set_revealed(false),
        }
        match vanished {
            // Showing the inbox lists it, so it is not listed again below.
            Some(redraw) => self.redraw(redraw),
            None => {
                self.follow_categories();
                if reload == Reload::Yes {
                    self.reload_list();
                }
            }
        }
        self.refresh_counts();
    }

    /// What the sidebar and the category switcher show. Two grouped
    /// queries replace the one-per-mailbox counting this used to do, and
    /// every request made before they start shares them.
    fn refresh_counts(self: &Rc<Self>) {
        if self.counts.borrow_mut().ask() == on_screen::Count::Joined {
            return;
        }
        let this = Rc::clone(self);
        glib::idle_add_local_once(move || this.count_now());
    }

    fn count_now(self: &Rc<Self>) {
        self.counts.borrow_mut().start();
        let mailboxes = self.sidebar.mailboxes();
        let shown = self.shown();
        let view = self.view();
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let lists = this.core.lists();
            let counted = this
                .core
                .call(async move { lists.counts(&mailboxes, &shown, &view).await })
                .await;
            if let Ok(counts) = counted {
                this.sidebar.set_counts(&counts.mailboxes);
                let waiting = counts
                    .mailboxes
                    .get(&Mailbox::FollowUp)
                    .copied()
                    .unwrap_or(0);
                this.set_follow_up_count(waiting as usize);
                this.set_category_counts(&counts.categories);
            }
            let again = this.counts.borrow_mut().done();
            if again.is_some() {
                glib::idle_add_local_once(move || this.count_now());
            }
        });
    }

    // ---- Mailboxes and the thread list ---------------------------------

    /// The accounts a listing may read, in sidebar order.
    fn scope(&self) -> Scope {
        Scope::over(self.accounts())
    }

    /// The settings that change what a mailbox lists.
    fn view(&self) -> View {
        let category = self
            .shows_categories(&self.shown())
            .then(|| self.screen.borrow().category());
        self.settings_with(|settings| View {
            threading: settings.threading,
            category,
            follow_ups: settings.suggest_follow_ups,
            now: chrono::Utc::now().timestamp_millis(),
            limit: None,
        })
    }

    /// The mailbox on screen.
    fn shown(&self) -> Mailbox {
        self.screen.borrow().mailbox().clone()
    }

    /// Puts `mailbox` on screen.
    fn show_mailbox(self: &Rc<Self>, mailbox: Mailbox) {
        let search_open = self.list.search_open();
        self.change_screen(|screen| Some(screen.show(mailbox, search_open)));
    }

    /// Makes one change to the mailbox on screen and redraws what it left
    /// stale. The borrow ends before the redraw, which can close the
    /// search bar and so come back here.
    fn change_screen(self: &Rc<Self>, change: impl FnOnce(&mut OnScreen) -> Option<Redraw>) {
        let redraw = change(&mut self.screen.borrow_mut());
        if let Some(redraw) = redraw {
            self.redraw(redraw);
        }
    }

    /// Carries out what a change to the mailbox on screen left stale.
    fn redraw(self: &Rc<Self>, redraw: Redraw) {
        match &redraw.sidebar {
            Some(on_screen::Sidebar::Select(mailbox)) => self.sidebar.select(mailbox),
            Some(on_screen::Sidebar::Clear) => self.sidebar.clear_selection(),
            None => {}
        }
        if redraw.close_search {
            self.list.close_search();
        }
        if let Some((title, subtitle)) = &redraw.title {
            self.list.set_title(title, subtitle);
        }
        if redraw.leave {
            self.list.unselect();
            self.conversation.leave();
            self.nav.set_show_content(false);
            if self.split.is_collapsed() {
                self.split.set_show_sidebar(false);
            }
        }
        if redraw.follow {
            let mailbox = self.shown();
            self.list
                .set_show_accounts(mailbox.account().is_none() && self.accounts().len() > 1);
            let accounts = self.accounts_of(&mailbox);
            self.word_buttons(&self.conversation, &mailbox, accounts);
            self.follow_outbox();
            self.follow_categories();
            self.follow_follow_ups();
        }
        if redraw.category {
            self.categories
                .show_names(self.screen.borrow().category());
        }
        if let Some(ticket) = redraw.list {
            self.list_first_page(ticket);
        }
    }

    /// Fetches a folder or a smart mailbox that lives only in Gmail again.
    fn reload_folder(self: &Rc<Self>) {
        if matches!(self.shown(), Mailbox::Folder { .. } | Mailbox::Smart(_)) {
            self.reload_list();
        }
    }

    /// Loads the first page of the mailbox on screen.
    fn reload_list(self: &Rc<Self>) {
        let ticket = self.screen.borrow_mut().feed().reload();
        self.list_first_page(ticket);
    }

    /// Lists the first page under `ticket`, which the feed dropped all
    /// earlier requests for.
    fn list_first_page(self: &Rc<Self>, ticket: Ticket) {
        let mailbox = self.shown().clone();
        if mailbox.is_remote() {
            self.list.show_loading();
        }
        let (scope, view) = (self.scope(), self.view());
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let lists = this.core.lists();
            let loaded = this
                .core
                .call(async move { lists.list(&mailbox, &scope, &view, Loaded::nothing()).await })
                .await;
            let landed = this.screen.borrow_mut().feed().first_page(ticket, &loaded);
            let Some(landed) = landed else {
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
        let ticket = self.screen.borrow_mut().feed().scrolled_to_end();
        let Some(ticket) = ticket else {
            return;
        };
        let mailbox = self.shown().clone();
        // The page starts after the last row on screen rather than after
        // as many rows as the list holds, so mail that arrived or left since
        // the list loaded neither repeats a row nor skips one.
        let (scope, view, held) = (self.scope(), self.view(), self.list.loaded());
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let lists = this.core.lists();
            let loaded = this
                .core
                .call(async move { lists.list(&mailbox, &scope, &view, held).await })
                .await;
            let current = this.screen.borrow_mut().feed().next_page(ticket, &loaded);
            if !current {
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
        let mailbox = self.shown().clone();
        let view = self.view();
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let (lists, named) = (this.core.lists(), changed.clone());
            let fresh = this
                .core
                .call(async move { lists.changed(&mailbox, &named, &view).await })
                .await
                .unwrap_or(None);
            let remote = this.shown().is_remote();
            let splice = this.screen.borrow_mut().feed().spliced(ticket, fresh, remote);
            match splice {
                Splice::Stale => {}
                Splice::Put(fresh) => {
                    let title = this.shown().title();
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
        self.change_screen(|screen| Some(screen.search(query)));
    }

    // ---- Opening threads -------------------------------------------------

    fn addresses_for(&self, account_id: AccountId) -> Vec<String> {
        self.account(account_id)
            .map(|a| vec![a.email])
            .unwrap_or_default()
    }

    fn open_thread(self: &Rc<Self>, summary: ThreadSummary) {
        self.nav.set_show_content(true);
        if self.conversation.is_showing_row(&summary) {
            return;
        }
        self.follow_categorize_sender(summary.account_id);
        self.load_into(Rc::clone(&self.conversation), summary);
    }

    // ---- Actions on the selection or the open conversation -----------------

    fn picked(self: &Rc<Self>, picked: Picked) {
        // The rows picked are what the Labels button reaches next, before
        // the conversation they open has loaded.
        let accounts: Vec<AccountId> = match &picked {
            Picked::One(row) => vec![row.account_id],
            Picked::Many(rows) => rows.iter().map(|r| r.account_id).collect(),
            Picked::None => Vec::new(),
        };
        let shown = self.shown();
        let reached = match accounts.is_empty() {
            true => self.accounts_of(&shown),
            false => accounts.clone(),
        };
        self.word_buttons(&self.conversation, &shown, reached);
        self.word_filing(&self.conversation, accounts);
        match picked {
            // A queued message has no Gmail thread; the thread run shows
            // it from what the outbox kept.
            Picked::One(row) => self.open_thread(row),
            Picked::Many(rows) => {
                self.conversation.show_many(
                    rows.len(),
                    self.settings_with(|s| s.threading),
                    rows.iter().any(|r| r.unread),
                    rows.iter().all(|r| r.starred),
                    rows.iter().all(|r| r.muted),
                );
            }
            Picked::None => self.conversation.leave(),
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
            | Action::ToggleRead => {
                if let Some(button) = press::Button::of(&action) {
                    self.press(view, Press::Button(button));
                }
            }
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
                if let Some(app) = self.app.upgrade() {
                    app.new_message(self.account_in_view(), &address);
                }
            }
            Action::ShowContact(address) => self.show_contact_from(view, address),
            Action::MessageMenu { message_id, x, y } => {
                self.open_message_menu(view, &message_id, x, y)
            }
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
            let vip = app.settings_with(|s| s.is_vip(&address));
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
                    let added = app.settings_with(|s| s.is_vip(&address));
                    window.toast(&vip_message(added, &display));
                }
                contact_card::Choice::AllMail => {
                    window.search(format!("from:{address}"));
                }
            });
        });
    }

    /// The account the reader is looking at: the open conversation's, else
    /// the mailbox's. A new message comes from it unless Preferences names
    /// another.
    fn account_in_view(&self) -> Option<AccountId> {
        self.conversation
            .read(|o| o.account_id)
            .or_else(|| self.shown().account())
    }

    /// Carries out what `press` comes to on what `view` reaches.
    pub(in crate::ui::window) fn press(self: &Rc<Self>, view: &Rc<ConversationView>, press: Press) {
        let reach = self.reach(view);
        self.press_on(view, reach, press::Scope::Shown, press);
    }

    /// Carries out what `press` comes to on `reach`, made in `view`, and
    /// says whether it does anything. Everything but a question before
    /// erasing happens before this returns, so the next row is open by
    /// the time the press's handler is done.
    pub(in crate::ui::window) fn press_on(
        self: &Rc<Self>,
        view: &Rc<ConversationView>,
        reach: Reach,
        scope: press::Scope,
        press: Press,
    ) -> bool {
        let erases = self.erases(reach.targets.iter().map(|t| t.account_id));
        let plan = press::plan(Pressed {
            press,
            reach,
            scope,
            flag_color: self.settings_with(|s| s.flag_color),
            threaded: self.settings_with(|s| s.threading),
            erases,
        });
        let taken = plan.taken();
        let pressing = Pressing {
            window: Rc::clone(self),
            view: Rc::clone(view),
        };
        let mut run = Box::pin(async move { press::carry_out(plan, &pressing).await });
        if (&mut run).now_or_never().is_none() {
            glib::spawn_future_local(run);
        }
        taken
    }

    /// Adds or removes a label from the label list, or moves into a
    /// folder from it. `follow` is the conversation the list was opened
    /// over; a list opened over one message passes none, and the reader
    /// stays where they are.
    fn press_label(
        self: &Rc<Self>,
        targets: Vec<Target>,
        press: Press,
        follow: Option<&Rc<ConversationView>>,
    ) {
        let view = follow.map_or_else(|| Rc::clone(&self.conversation), Rc::clone);
        let reach = Reach {
            targets,
            marks: Default::default(),
            muted: false,
            mailbox: self.mailbox_of(&view),
        };
        let scope = match follow {
            Some(_) => press::Scope::Shown,
            None => press::Scope::Carried { open: false },
        };
        self.press_on(&view, reach, scope, press);
    }

    /// Mutes the targets, or unmutes them when they are muted already.
    /// Gmail archives the replies to a muted thread with its own filters,
    /// so muting here is the label and one archive.
    fn toggle_mute(self: &Rc<Self>) {
        let view = Rc::clone(&self.conversation);
        self.press(&view, Press::Mute);
    }

    /// Words the trash button of `view` for `mailbox`: the folder's own
    /// words, or what Delete calls off in a mailbox of queued mail. In a
    /// Trash the button shows only while every account in `accounts`, the
    /// ones an action on `view` reaches, can delete mail for good.
    pub(super) fn word_buttons(
        &self,
        view: &ConversationView,
        mailbox: &Mailbox,
        accounts: impl IntoIterator<Item = AccountId>,
    ) {
        let erases = self.erases(accounts);
        view.set_folder(mailbox.folder(), erases);
        if let Some((word, tip)) = press::trash_words(mailbox) {
            view.set_trash_words(&word, &tip);
        }
        if !view.detached()
            && let Some(action) = self
                .actions
                .lookup_action("trash")
                .and_downcast::<gio::SimpleAction>()
        {
            action.set_enabled(triage::deletes(mailbox, erases));
        }
    }

    /// The accounts whose mail `mailbox` lists: its own, or every account
    /// for a mailbox that spans them.
    fn accounts_of(&self, mailbox: &Mailbox) -> Vec<AccountId> {
        match mailbox.account() {
            Some(id) => vec![id],
            None => self.accounts().iter().map(|a| a.id).collect(),
        }
    }

    /// Whether the server of every account in `accounts` can delete mail
    /// for good.
    fn erases(&self, accounts: impl IntoIterator<Item = AccountId>) -> bool {
        accounts
            .into_iter()
            .all(|id| self.offers(id).delete_forever)
    }

    /// Erases the targets. Nothing reverses this, so the toast offers no
    /// Undo, and a missing permission leaves every row where it is.
    fn delete_forever(self: &Rc<Self>, view: &Rc<ConversationView>, targets: Vec<Target>) {
        let account_id = targets[0].account_id;
        let (this, view) = (Rc::clone(self), Rc::clone(view));
        glib::spawn_future_local(async move {
            let actions = this.core.actions();
            let erased = this
                .core
                .call(async move { actions.erase(&targets).await })
                .await;
            let outcome = match erased {
                Ok(Permitted::Done(outcome)) => outcome,
                Ok(Permitted::NeedsPermission) => {
                    return this.ask_permission(account_id, Permission::Delete, Occasion::Needed);
                }
                Err(err) => {
                    return this.failed(&gettext("Could not delete the mail: {reason}"), &err);
                }
            };
            this.after_mail(Cause::Erased, &outcome, Some(&*view));
            if let Some(error) = outcome.first_error() {
                return this.toast(error);
            }
            this.toast(&deleted_forever_message(
                outcome.done.len(),
                this.settings_with(|s| s.threading),
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

    /// Explains what `permission` adds for the account and offers to ask
    /// Google for it. Every `Permitted::NeedsPermission` answer the window
    /// or the assistant gets comes here; `occasion` decides whether the
    /// question comes each time or once a run.
    pub fn ask_permission(
        self: &Rc<Self>,
        account_id: AccountId,
        permission: Permission,
        occasion: Occasion,
    ) {
        let Some(account) = self.account(account_id) else {
            return;
        };
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let email = &account.email;
            if permission::ask(&this.window, account_id, email, permission, occasion).await {
                this.grant(account.email, permission);
            }
        });
    }

    /// Sends `email` through consent for `permission`, once the person has
    /// chosen Grant Access.
    fn grant(self: &Rc<Self>, email: String, permission: Permission) {
        self.authorize_with(Some(email), permission.scopes());
    }

    /// Says an API is switched off in the Google Cloud project Penguin Mail
    /// signs in with. No permission fixes that, so this offers the page in
    /// Google Cloud that turns it on.
    pub(super) fn explain_api_off(self: &Rc<Self>, service: &str, enable_url: &str) {
        let question = confirm(
            &fill(&gettext("Turn On the {service}"), &[("service", service)]),
            &fill(
                &gettext(
                    "The Google Cloud project Penguin Mail signs in with has the {service} \
                     switched off, so Google refuses before it can ask for your permission. \
                     Turn it on, wait a minute, and try again.",
                ),
                &[("service", service)],
            ),
            &gettext("Open Google Cloud"),
            Tone::Suggested,
        )
        .not_now();
        let this = Rc::clone(self);
        let url = enable_url.to_string();
        glib::spawn_future_local(async move {
            if question.ask(&this.window).await {
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
    fn contacts_loaded(self: &Rc<Self>) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        self.list.set_photos(&app.photos());
        self.reopen_for_photos();
    }

    /// Drops rows that no longer belong in the Gmail folder on screen. The
    /// local store cannot list these folders, so rows go one by one.
    fn prune_folder(self: &Rc<Self>, targets: &[Target]) {
        let Some(folder) = self.shown().folder() else {
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
            this.after_mail(Cause::Did(&action, history), &outcome, None);
            if let Some(error) = outcome.first_error() {
                return this.toast(error);
            }
            if history == History::Skip {
                return;
            }
            let count = outcome.done.len();
            if let Some(done) = message
                .or_else(|| done_message(&action, count, this.settings_with(|s| s.threading)))
            {
                let toast = adw::Toast::builder()
                    .title(toast_title(&done))
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

    /// Reverses the organizing action on top of the undo stack, whether
    /// the window or the assistant took it. The one before it is left for
    /// the next press.
    fn undo(self: &Rc<Self>) {
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let Some(undone) = this.core.undo().await else {
                return this.toast(&gettext("Nothing to undo"));
            };
            this.after_mail(Cause::Undid, &undone.outcome, None);
            match undone.outcome.first_error() {
                Some(error) => this.toast(error),
                None => this.toast(&fill(
                    &gettext("{action} undone"),
                    &[("action", &undone.action.describe())],
                )),
            }
        });
    }

    /// The label list for what the Labels button reaches: the selection,
    /// or the open conversation.
    fn label_popover(self: &Rc<Self>) -> gtk::Popover {
        let view = Rc::clone(&self.conversation);
        let targets = self.reach(&view).targets;
        let applied: HashSet<String> = match targets.len() {
            1 => view
                .read(|o| {
                    o.messages
                        .iter()
                        .flat_map(|m| m.held.mailboxes.clone())
                        .collect()
                })
                .unwrap_or_default(),
            _ => HashSet::new(),
        };
        self.label_popover_for(targets, applied, Some(view))
    }

    /// Labels of the targets' account, with `applied` already ticked. Mail
    /// from several accounts gets every label name those accounts hold.
    /// `follow` is the conversation to move on from when a label change
    /// takes its mail out of the mailbox on screen.
    pub(super) fn label_popover_for(
        self: &Rc<Self>,
        targets: Vec<Target>,
        applied: HashSet<String>,
        follow: Option<Rc<ConversationView>>,
    ) -> gtk::Popover {
        let popover = gtk::Popover::new();
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
        if targets.is_empty() {
            popover.set_child(Some(&message(&gettext("Open or select mail to label it."))));
            return popover;
        }
        let Some(&account_id) = accounts.iter().next().filter(|_| accounts.len() == 1) else {
            // Mail from several accounts takes labels by name, since each
            // account has its own label behind a name.
            match self.labels_by_name(&accounts, &popover) {
                Some(list) => popover.set_child(Some(&list)),
                None => popover.set_child(Some(&message(&gettext(
                    "Select mail from one account to label it.",
                )))),
            }
            return popover;
        };
        let mut labels: Vec<Label> = self
            .labels_of(account_id)
            .into_iter()
            .filter(|l| l.kind == mailrs_domain::LabelKind::User)
            .collect();
        labels.sort_by_key(|l| l.name.to_lowercase());
        let filing = Filing::of([self.offers(account_id)]);
        // A message sits in one folder, so no folder shows as ticked.
        let applied = match filing {
            Filing::Labels => applied,
            Filing::Folders => HashSet::new(),
        };
        let create = gtk::Button::builder()
            .child(
                &adw::ButtonContent::builder()
                    .icon_name("list-add-symbolic")
                    .label(filing.new_item())
                    .build(),
            )
            .css_classes(["flat"])
            .margin_top(4)
            .build();
        let (weak, pop) = (Rc::downgrade(self), popover.clone());
        {
            let (targets, follow) = (targets.clone(), follow.clone());
            create.connect_clicked(move |_| {
                pop.popdown();
                if let Some(win) = weak.upgrade() {
                    let (targets, follow) = (targets.clone(), follow.clone());
                    win.new_label(
                        account_id,
                        Some(Box::new(move |win, label| {
                            win.press_label(
                                targets.clone(),
                                filing_choice(filing, &label.id, &shown_name(&label.name), false),
                                follow.as_ref(),
                            )
                        })),
                    );
                }
            });
        }
        if labels.is_empty() {
            let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
            content.append(&message(&filing.none_yet()));
            content.append(&create);
            popover.set_child(Some(&content));
            return popover;
        }
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
                    .label(shown_name(&label.name))
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
            win.press_label(
                targets.clone(),
                filing_choice(
                    filing,
                    &label.id,
                    &shown_name(&label.name),
                    applied.contains(&label.id),
                ),
                follow.as_ref(),
            );
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
        self.reply_to(view, kind, None)
    }

    /// Replies to or forwards one message of `view`, or its newest one
    /// when `only` names none.
    pub(super) fn reply_to(
        self: &Rc<Self>,
        view: &ConversationView,
        kind: ReplyKind,
        only: Option<&str>,
    ) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let Some(answering) = view.find(|open| open.answering(only, kind == ReplyKind::Forward))
        else {
            return;
        };
        let crate::open_thread::Answering {
            account_id,
            target,
            text,
            html,
            thread,
            attachments,
            secret,
        } = answering;
        let forwarded_html = html.clone();
        // Every address the account sends as, so the reply comes from the
        // one the message was written to.
        let mine = app.my_addresses(account_id);
        let mut draft = compose::respond(
            kind,
            account_id,
            &mine,
            &target,
            &text,
            html.as_deref(),
            &thread,
        );
        // A message that arrived encrypted is answered encrypted.
        draft.encrypt |= secret;
        if attachments.is_empty() {
            app.open_composer(draft, Signature::Add);
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
                    Ok(data) => draft.attachments.push(compose::forwarded_file(
                        attachment,
                        data,
                        forwarded_html.as_deref(),
                    )),
                    Err(err) => this.toast(&with_reason(
                        &gettext("Could not include {file}: {reason}"),
                        &err,
                        &[("file", &attachment.filename)],
                    )),
                }
            }
            app.open_composer(draft, Signature::Add);
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
                .find(|m| m.in_role(Role::Drafts))?
                .clone();
            Some((
                open.account_id,
                open.thread_id.clone(),
                open.messages.len() > 1,
                draft,
            ))
        });
        let Some((account_id, thread_id, in_thread, message)) = found else {
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
            let mut draft = app.blank_draft(account_id);
            // Only the message as Gmail holds it carries the Bcc, the reply
            // headers and the bytes of the files, readable or encrypted.
            // Opening without them would save a draft that had lost them.
            let (s, m) = (sync.clone(), message.id.clone());
            let raw = match this.core.call(async move { s.raw_message(&m).await }).await {
                Ok(raw) => raw,
                Err(err) => {
                    return this.toast(&with_reason(
                        &gettext("Could not open the draft: {reason}"),
                        &err,
                        &[],
                    ));
                }
            };
            if let Err(problem) =
                crate::protection::draft::reopened(&this.core, raw, &mut draft).await
            {
                return this.toast(&problem);
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
            // The composer that saved it signed it then.
            app.open_composer(draft, Signature::AsWritten);
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
                        .title(toast_title(&saved_to_downloads))
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
                Err(err) => this.toast(&with_reason(
                    &gettext("Could not save {file}: {reason}"),
                    &err,
                    &[("file", &attachment.filename)],
                )),
            }
        });
    }

    // ---- Accounts ----------------------------------------------------------

    fn authorize(self: &Rc<Self>, expected: Option<String>) {
        self.authorize_with(expected, &[]);
    }

    /// Runs the consent flow, asking Google for `extra` permissions on top
    /// of the ones sign-in always requests.
    fn authorize_with(self: &Rc<Self>, expected: Option<String>, extra: &'static [&'static str]) {
        // Without the build's Google client the browser would open for
        // nothing, so say why at once. The demo goes on to its own message.
        if !self.core.demo && !self.core.built_with_google_sign_in() {
            return self.no_google_sign_in();
        }
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
                    if let Some(app) = this.app.upgrade() {
                        app.signed_in(&account);
                    }
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

    /// Says that this copy cannot sign in to Google, with a button to the
    /// releases, whose builds can.
    fn no_google_sign_in(&self) {
        let toast = adw::Toast::builder()
            .title(toast_title(&gettext(
                "This copy of Penguin Mail was built without Google sign-in.",
            )))
            .button_label(gettext("Get a Release"))
            .timeout(10)
            .build();
        let window = self.window.clone();
        toast.connect_button_clicked(move |_| {
            gtk::UriLauncher::new(RELEASES).launch(Some(&window), gio::Cancellable::NONE, |_| {});
        });
        self.toasts.add_toast(toast);
    }

    /// The accounts, as the app holds them.
    fn accounts(&self) -> Vec<Account> {
        self.app
            .upgrade()
            .map(|app| app.accounts())
            .unwrap_or_default()
    }

    fn account(&self, account_id: AccountId) -> Option<Account> {
        self.app.upgrade()?.account(account_id)
    }

    /// What `account_id` offers, or everything while it has not started.
    pub(super) fn offers(&self, account_id: AccountId) -> Offers {
        let running = self.core.account(account_id);
        crate::offered::offers_for(running.as_ref().map(|sync| sync.services()))
    }

    fn labels(&self) -> HashMap<AccountId, Vec<Label>> {
        self.app
            .upgrade()
            .map(|app| app.labels())
            .unwrap_or_default()
    }

    fn labels_of(&self, account_id: AccountId) -> Vec<Label> {
        self.app
            .upgrade()
            .map(|app| app.labels_of(account_id))
            .unwrap_or_default()
    }

    fn confirm_remove(self: &Rc<Self>, account: Account) {
        let question = confirm(
            &fill(
                &gettext("Remove {account}?"),
                &[("account", &account.email)],
            ),
            &gettext(
                "Its downloaded mail and saved sign-in are deleted from this computer. \
                 Nothing changes in Gmail.",
            ),
            &gettext("Remove"),
            Tone::Destructive,
        );
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            if !question.ask(&this.window).await {
                return;
            }
            if this.conversation.read(|o| o.account_id) == Some(account.id) {
                this.conversation.leave();
            }
            let email = account.email.clone();
            match this.core.remove_account(account).await {
                Ok(()) => this.toast(&fill(&gettext("Removed {account}"), &[("account", &email)])),
                Err(err) => this.toast(&with_reason(
                    &gettext("Could not remove {account}: {reason}"),
                    &err,
                    &[("account", &email)],
                )),
            }
            this.refresh_accounts(Reload::Yes);
        });
    }

    // ---- Actions, menu, and keys -------------------------------------------

    fn install_actions(self: &Rc<Self>) {
        let view = Rc::clone(&self.conversation);
        self.install_outbox_actions(&self.actions, &view);
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
                win.previews.forget_decrypted();
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
        crate::ui::name_menu_items_of(&button);
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
        self.list.selected_count() > 1 || self.list.search_open()
    }

    /// Escape: drops a selection of several rows, or else closes the search.
    fn clear_selection(&self) {
        if self.list.selected_count() > 1 {
            self.list.unselect();
            self.conversation.leave();
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
        let wrote = self
            .app
            .upgrade()
            .is_some_and(|app| app.new_message(self.account_in_view(), ""));
        if !wrote {
            self.toast(&gettext("Add an account first"));
        }
    }

    /// Opens a thread from outside the window, such as a notification, and
    /// answers it when `then` asks for that.
    pub fn reveal(self: &Rc<Self>, account_id: AccountId, thread_id: String, then: Reveal) {
        // Selecting a row before the list's rows land finds nothing, so
        // the feed holds the thread until the first page is on screen.
        let (redraw, now) = self
            .screen
            .borrow_mut()
            .reveal((account_id, thread_id, then));
        if let Some(redraw) = redraw {
            self.redraw(redraw);
        }
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
        let waiting = Replying(Rc::clone(self));
        reveal::reply_when_open(&waiting, account_id, thread_id, reveal::REVEAL_WAIT).await;
    }

    /// Screenshot hooks, honoured only in demo mode: `MAILRS_DEMO_OPEN`
    /// opens a thread by id, `MAILRS_DEMO_SEARCH` runs a search,
    /// `MAILRS_DEMO_COMPOSE=reply` opens a reply to the open thread,
    /// `MAILRS_DEMO_MESSAGE_MENU` right-clicks the message at that
    /// position in the open thread, and `MAILRS_DEMO_ACTION` activates a
    /// window action such as `shortcuts`, or one with a target such as
    /// `account-rules(int64 1)`. With a thread to open, the action waits
    /// for it, so `toggle-vip` has a sender to add.
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
                let finder = Rc::clone(&this);
                glib::spawn_future_local(async move {
                    let key = thread_id.clone();
                    let found = finder
                        .core
                        .read(move |c| mailrs_store::threads::account_of(c, &key))
                        .await;
                    if let Ok(Some(account_id)) = found {
                        finder.list.select(account_id, &thread_id, None);
                        let replier = Rc::clone(&finder);
                        glib::timeout_add_local_once(
                            std::time::Duration::from_millis(900),
                            move || {
                                if std::env::var("MAILRS_DEMO_COMPOSE").as_deref() == Ok("reply") {
                                    replier.reply(&replier.conversation, ReplyKind::Reply);
                                }
                                if let Ok(position) = std::env::var("MAILRS_DEMO_MESSAGE_MENU")
                                    && let Ok(position) = position.parse()
                                {
                                    replier.conversation.ask_message_menu(position);
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
        self.settings_with(Settings::clone)
    }

    /// What `read` makes of the preferences. Most callers want one field,
    /// and a list load asks several times, so this spares a copy of them
    /// all.
    fn settings_with<R>(&self, read: impl FnOnce(&Settings) -> R) -> R {
        match self.app.upgrade() {
            Some(app) => app.settings_with(read),
            None => read(&Settings::default()),
        }
    }

    fn show_preferences(self: &Rc<Self>) {
        self.show_preferences_for(None);
    }

    /// Opens Preferences, on the signature of `signature_of` when given.
    fn show_preferences_for(self: &Rc<Self>, signature_of: Option<String>) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let accounts = app.accounts();
        super::preferences::present(
            &app,
            &accounts,
            |id| self.offers(id),
            &self.window,
            signature_of.as_deref(),
        );
    }

    fn show_rules(self: &Rc<Self>, account: Account) {
        let labels = self.labels_of(account.id);
        let (grant, email) = (Rc::downgrade(self), account.email.clone());
        super::rules::present(&self.core, &account, labels, &self.window, move || {
            if let Some(win) = grant.upgrade() {
                win.grant(email.clone(), Permission::Settings);
            }
        });
    }

    /// Opens Preferences on one page, such as "assistant".
    fn show_preferences_page(self: &Rc<Self>, page: &str) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let accounts = app.accounts();
        super::preferences::present_page(
            &app,
            &accounts,
            |id| self.offers(id),
            &self.window,
            page,
        );
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
                    win.grant(email.clone(), Permission::Settings);
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
        let added = app.settings_with(|s| s.is_vip(&sender.email));
        self.toast(&vip_message(added, sender.display()));
    }

    /// Brings what is on screen back in line after a settings change.
    /// `Effects` comes in the order the window wants: the accounts first,
    /// because the rows and the smart mailbox on screen read what it sets.
    fn apply_effect(self: &Rc<Self>, effect: Effect) {
        let settings = self.settings();
        match effect {
            Effect::ListShape => self.change_screen(|screen| Some(screen.list_shape())),
            Effect::Accounts => self.refresh_accounts(Reload::Yes),
            Effect::RowColors => {
                // Rows carry account colours, which come with the accounts
                // read that Effect::Accounts started just before.
                let (list, read) = (Rc::clone(&self.list), self.accounts_read.borrow().clone());
                glib::spawn_future_local(async move {
                    if let Some(read) = read {
                        read.await;
                    }
                    list.rebind();
                });
            }
            Effect::SmartMailboxes => {
                self.change_screen(|screen| screen.smart_saved(&settings.smart_mailboxes));
            }
            Effect::Vips => {
                for view in self.views() {
                    view.set_sender_vip(sender_is_vip(&view, &settings));
                }
            }
            Effect::FollowUps => {
                if !settings.suggest_follow_ups {
                    self.change_screen(OnScreen::follow_ups_off);
                }
                self.refresh_counts();
            }
            Effect::Categories => {
                self.change_screen(|screen| Some(screen.categories_changed()));
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
}

/// The main window and one conversation view, as a press changes them.
struct Pressing {
    window: Rc<MainWindow>,
    view: Rc<ConversationView>,
}

impl PressEffects for Pressing {
    fn move_on(&self) {
        self.window.move_on(&self.view);
    }

    fn confirm(&self, question: Question) -> crate::wanted::Answer<'_, bool> {
        let asked = confirm(
            &question.heading,
            &question.body,
            &question.verb,
            Tone::Destructive,
        );
        // The question belongs over the window it was asked in, which for
        // a detached conversation is not the main one.
        let parent = self
            .view
            .window()
            .unwrap_or_else(|| self.window.window.clone().upcast());
        Box::pin(async move { asked.ask(&parent).await })
    }

    fn act(&self, targets: Vec<Target>, action: MailAction, history: History, words: Option<String>) {
        // A colour picked here becomes the one the flag button uses next.
        if let MailAction::Flag(Some(color)) = action
            && let Some(app) = self.window.app.upgrade()
        {
            app.change_settings(Change::FlagColor(color));
        }
        self.window.perform(targets, action, history, words);
    }

    fn erase(&self, targets: Vec<Target>) {
        self.window.delete_forever(&self.view, targets);
    }

    fn cancel(&self, cancel: triage::Cancel, targets: Vec<Target>) {
        match cancel {
            triage::Cancel::Scheduled => self.window.cancel_scheduled(&self.view, targets),
            triage::Cancel::Queued => self.window.drop_queued(&self.view),
            // The plan runs these two as mail actions.
            triage::Cancel::Reminder | triage::Cancel::FollowUp => {}
        }
    }

    fn toast(&self, text: String) {
        self.window.toast(&text);
    }
}

/// The main window as a reply from a notification waits on it.
struct Replying(Rc<MainWindow>);

impl crate::wanted::Screen for Replying {
    fn is_showing(&self, target: &Target) -> bool {
        self.0.conversation.is_showing(target)
    }
}

impl reveal::Waiting for Replying {
    fn target(&self) -> Option<Target> {
        self.0.conversation.read(OpenThread::target)
    }

    fn quotable(&self) -> bool {
        self.0.conversation.read(OpenThread::quotable).unwrap_or(false)
    }

    fn sleep(&self, step: std::time::Duration) -> crate::wanted::Answer<'_, ()> {
        Box::pin(glib::timeout_future(step))
    }

    fn reply(&self) {
        self.0.reply(&self.0.conversation, ReplyKind::Reply);
    }

    fn toast(&self, text: String) {
        self.0.toast(&text);
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

/// Whether `mailbox` is still there once the accounts read as `data`. A
/// mailbox of one account goes with that account, and a label goes when
/// its account no longer lists it.
fn still_there(mailbox: &Mailbox, data: &[(Account, Vec<Label>)]) -> bool {
    match mailbox {
        Mailbox::Label {
            account_id,
            label_id,
            ..
        } => data.iter().any(|(a, labels)| {
            a.id == *account_id && labels.iter().any(|l| &l.id == label_id)
        }),
        _ => mailbox
            .account()
            .is_none_or(|id| data.iter().any(|(a, _)| a.id == id)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mailbox_of_a_removed_account_is_gone() {
        let account = |id| Account {
            id,
            email: format!("{id}@example.com"),
            state: AccountState::Ok,
            provider: mailrs_domain::Provider::Gmail,
        };
        let label = |account_id| Label {
            account_id,
            id: "Label_1".into(),
            name: "Work".into(),
            kind: mailrs_domain::LabelKind::User,
            color: None,
        };
        let both = vec![(account(1), vec![label(1)]), (account(2), vec![])];
        let only_two = vec![(account(2), vec![])];
        let inbox = Mailbox::Standard {
            account_id: 1,
            which: super::super::Standard::Inbox,
        };
        let unread = Mailbox::Set {
            account_id: 1,
            set: mailrs_domain::MailSet::Unseen,
            name: "UNREAD".into(),
        };
        let work = Mailbox::Label {
            account_id: 1,
            label_id: "Label_1".into(),
            name: "Work".into(),
        };
        for mailbox in [&inbox, &unread, &work] {
            assert!(still_there(mailbox, &both), "{mailbox:?}");
            assert!(!still_there(mailbox, &only_two), "{mailbox:?}");
        }
        let unlisted = vec![(account(1), vec![]), (account(2), vec![])];
        assert!(!still_there(&work, &unlisted), "a label the account dropped");
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
    fn a_toast_shows_an_ampersand_in_a_label_name_as_written() {
        assert_eq!(toast_title("Moved to R&D"), "Moved to R&amp;D");
        assert_eq!(toast_title("Archived"), "Archived");
    }

    #[test]
    fn a_label_row_says_whether_the_mail_already_carries_it() {
        assert_eq!(label_row_name("Receipts", false), "Receipts");
        assert_eq!(label_row_name("Receipts", true), "Receipts, on this mail");
        assert_eq!(label_row_name("Work/Tax", false), "Work › Tax");
    }

    #[test]
    fn a_folder_row_moves_the_mail_and_a_label_row_toggles_the_label() {
        assert_eq!(
            filing_choice(Filing::Folders, "Label_5", "Receipts", false),
            Press::Move {
                folder: "Label_5".into(),
                name: "Receipts".into(),
            }
        );
        assert_eq!(
            filing_choice(Filing::Labels, "Label_5", "Receipts", false),
            Press::Label(TriageAction::AddLabel("Label_5".into()))
        );
        assert_eq!(
            filing_choice(Filing::Labels, "Label_5", "Receipts", true),
            Press::Label(TriageAction::RemoveLabel("Label_5".into()))
        );
    }
}
