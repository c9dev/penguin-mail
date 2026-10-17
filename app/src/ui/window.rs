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
    Account, AccountId, AccountState, Category, ChangeEvent, EpochMillis, Label, MessageBody,
    Provider, RemoveSetting, Role, Target, ThreadSummary,
};
use mailrs_sync::sign_in::{Browser, Wanted};
use mailrs_sync::{
    History, MailAction, MovedFrom, Offers, Permitted, Reopened, Scope, TriageAction, View,
    Withheld,
};

use super::add_account::{Done, Opening};
use super::calendar::{CalendarView, Hooks};
use super::confirm::{Tone, confirm};
use super::contact_card;
use super::conversation::{Action, ConversationView};
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
use crate::unread::Counted;
use aftermath::Cause;
use futures::FutureExt;
use futures::future::{LocalBoxFuture, Shared};
use listing::ListingRun;
use on_screen::{OnScreen, Redraw};
use press::{Press, PressEffects, Pressed, Question};
use reach::Reach;

mod across;
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
mod in_flight;
mod invitation;
mod listing;
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
mod room;
mod save_contacts;
mod scheduled;
mod searching;
mod senders;
mod shortcuts;
mod spaces;
mod storage;
mod thread;
mod translation;
mod triage;
mod undo_send;

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
    /// Mail actions still on their way to the server: the rows they took
    /// out of the list and the toast that speaks for them.
    in_flight: in_flight::InFlight,
    /// Says a release is available, installing, waiting to restart, or failed.
    update_banner: adw::Banner,
    /// Says a snap cannot reach the keyring and gives the command that
    /// connects it. Revealed once, for the rest of the run.
    keyring_banner: adw::Banner,
    /// One bar per account whose own consent leaves something out, at the
    /// top of the mail list; see [`crate::permission::wants_banner`].
    /// Rebuilt whenever the accounts are read again.
    grant_banners: gtk::Box,
    /// The banner `grant_banners` holds for each account that wants one,
    /// so a later rebuild changes only what changed rather than tearing
    /// every banner down and putting it back: a screen reader can be
    /// mid-walk when the accounts are read again, and a banner that
    /// blinks out and back shifts every row below it for a moment.
    grant_banner_widgets: RefCell<HashMap<AccountId, adw::Banner>>,
    /// The storage banner of each nearly full account, beside the Grant
    /// Access banners, and whether the account was full when it went up.
    storage_banner_widgets: RefCell<HashMap<AccountId, storage::StorageBand>>,
    /// The main menu's update entry: Check for Updates, or what to do with
    /// the one that is waiting.
    update_menu: gio::Menu,
    /// The About window while it is open, so an update's progress reaches
    /// its button.
    about: RefCell<Option<Rc<crate::ui::about::About>>>,
    /// Preferences while it is open, so another press brings it back
    /// rather than stacking a second one.
    preferences: RefCell<Option<Rc<super::preferences::dialog::Dialog>>>,
    stack: gtk::Stack,
    split: adw::OverlaySplitView,
    /// The mail's list and conversation, or the calendar, beside the
    /// sidebar: inside `split`, so the sidebar and its Mail / Calendar
    /// switch stay on screen with either.
    spaces: gtk::Stack,
    nav: adw::NavigationSplitView,
    pub calendar: Rc<CalendarView>,
    /// Whether the accounts have been read once, when the window goes
    /// back to the space it showed last.
    space_restored: Cell<bool>,
    startup_account_applied: Cell<bool>,
    sidebar: Rc<Sidebar>,
    list: Rc<ThreadList>,
    conversation: Rc<ConversationView>,
    /// The mailbox on screen and everything that lists it: the feed, the
    /// search field's typing and the server search still out.
    listing: Rc<ListingRun>,
    /// The accounts read in flight, which a redraw of the row colours
    /// waits for, since the colours come with it.
    accounts_read: RefCell<Option<Shared<LocalBoxFuture<'static, ()>>>>,
    authorizing: Cell<bool>,
    assistant: Rc<super::assistant::AssistantPane>,
    assistant_split: adw::OverlaySplitView,
    assistant_window: RefCell<Option<adw::Window>>,
    /// The group must outlive construction to keep the adjoining toolbars aligned.
    toolbar_heights: gtk::SizeGroup,
    assistant_top_bar: Option<gtk::Widget>,
    categories: categories::CategoryBar,
    /// Focused and Other, over one Microsoft account's inbox.
    focus: categories::CategoryBar,
    follow_up: followup::FollowUpBanner,
    /// The inline images and the attachment rows' pictures already
    /// fetched.
    pictures: Rc<pictures::Pictures>,
    /// Senders whose remote images may load. Read from the store once and
    /// kept here, since every thread that opens asks about it.
    image_senders: RefCell<Vec<mailrs_store::image_senders::ImageSender>>,
    /// The conversations in windows of their own, each with the mailbox it
    /// was opened from and its own action group, so a flag colour, an
    /// undo or a gate set again reaches them too. An entry that no longer
    /// upgrades is a window somebody closed.
    detached: RefCell<Vec<detached::Detached>>,
    /// The scratch copies of attachments this window opened.
    previews: previews::Previews,
    /// Undo Send: the sends waiting out their delay, and what calls each
    /// one back.
    undo_sends: RefCell<scheduled::UndoSends>,
    /// The event the next-event card shows, so a click on it knows where
    /// to open the calendar. `None` while the card is hidden.
    next_up: RefCell<Option<crate::ui::calendar::next::NextUp>>,
    /// The next-event card's reads; only the newest writes the card.
    next_card: Rc<crate::ui::calendar::run::NextCard>,
}

/// The class that marks a toplevel window dark. `@media
/// (prefers-color-scheme: dark)` never matches this app's own
/// stylesheet on GTK 4.22 (the gtk4-css-support skill confirms it), so
/// every dark rule in `app/data/style.css` and `calendar::tint` keys off
/// this class instead of that query.
pub(super) const DARK_CLASS: &str = "app-dark";
/// Marks a window that takes the mockup's window, sidebar and view
/// colours in place of libadwaita's (style.css).
const SURFACES_CLASS: &str = "app-surfaces";
/// Marks a window while the desktop accent is orange, so style.css can
/// give filled controls the darker orange that holds white text at AA.
const ACCENT_ORANGE_CLASS: &str = "accent-orange";

/// Puts [`DARK_CLASS`] on `window` while libadwaita is dark, and keeps it
/// current for as long as the window lives. Every toplevel `adw::Window`
/// this app opens calls this once, right after building it, so the
/// mockup's window, sidebar and view colours reach it: the main window
/// and each detached conversation (`window/detached.rs`), the composer
/// (`composer/mod.rs`), the attachment preview
/// (`window/attachments.rs`), the message-source viewer
/// (`window/detached.rs`), and the startup-failure window (`main.rs`).
/// An `adw::Dialog` such as Preferences or an alert needs no separate
/// call: it is presented inside its parent window's own tree, so it
/// inherits these colours through the CSS custom properties already set
/// there.
pub(crate) fn track_dark_class(window: &adw::Window) {
    // The macOS menu follows the application's active window, including
    // composers and separate conversations, rather than a floating window.
    #[cfg(target_os = "macos")]
    window.set_application(
        gio::Application::default()
            .and_downcast::<gtk::Application>()
            .as_ref(),
    );
    window.add_css_class(SURFACES_CLASS);
    let style = adw::StyleManager::default();
    let target = window.downgrade();
    let mark = move |style: &adw::StyleManager| {
        let Some(window) = target.upgrade() else {
            return;
        };
        match style.is_dark() {
            true => window.add_css_class(DARK_CLASS),
            false => window.remove_css_class(DARK_CLASS),
        }
    };
    mark(&style);
    let target = window.downgrade();
    let mark_accent = move |style: &adw::StyleManager| {
        let Some(window) = target.upgrade() else {
            return;
        };
        match style.accent_color() == adw::AccentColor::Orange {
            true => window.add_css_class(ACCENT_ORANGE_CLASS),
            false => window.remove_css_class(ACCENT_ORANGE_CLASS),
        }
    };
    mark_accent(&style);
    // The style manager lives as long as the process; the handlers go
    // with the window they mark.
    let handlers = RefCell::new(vec![
        style.connect_dark_notify(mark),
        style.connect_accent_color_notify(mark_accent),
    ]);
    window.connect_destroy(move |_| {
        for handler in handlers.take() {
            adw::StyleManager::default().disconnect(handler);
        }
    });
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

/// What the window says once `browser`'s sign-in comes back. Google's
/// consent adds the account or finds the one already here, and says
/// "Added" either way; Microsoft's runs from here only for an account
/// already added.
fn signed_in_toast(browser: Browser, account: &Account) -> String {
    match browser {
        Browser::Google => fill(
            &gettext("Added {account}. Downloading mail…"),
            &[("account", &account.email)],
        ),
        Browser::Microsoft => fill(
            &gettext("{account} is signed in again."),
            &[("account", &account.email)],
        ),
    }
}

/// Where release builds, which carry the Google client, are published.
const RELEASES: &str = "https://github.com/c9dev/penguin-mail/releases";

/// A toast's title as Pango markup. A toast reads its title as markup, so
/// a label called "R&D" would otherwise show nothing at all.
pub(crate) fn toast_title(text: &str) -> glib::GString {
    glib::markup_escape_text(text)
}

/// The toast line when archiving made the account's Archive folder,
/// because its server had none.
fn archive_made_line(provider: &str, name: &str) -> String {
    fill(
        &gettext("Made a folder called “{name}” on {provider} for archived mail"),
        &[("name", name), ("provider", provider)],
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
            let l = weak.clone();
            let sidebar = Sidebar::new(
                move |mailbox| {
                    if let Some(win) = w.upgrade() {
                        win.show_mailbox(mailbox);
                    }
                },
                move |mailbox| d.upgrade().is_some_and(|win| win.drop_on(mailbox)),
                move |drop| {
                    if let Some(win) = l.upgrade() {
                        win.drop_label(drop);
                    }
                },
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
                        win.search_entered(query);
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
            // `.mail-columns` lets style.css put the list column on the
            // window colour, as the mockup draws it, rather than the
            // sidebar shade an `AdwNavigationSplitView` pane takes by
            // default.
            let nav = adw::NavigationSplitView::builder()
                .sidebar(&list.page)
                .content(&conversation.page)
                .css_classes(["mail-columns"])
                .min_sidebar_width(300.0)
                .max_sidebar_width(420.0)
                // The mockup's list is 392 px of the 1,184 left beside
                // the sidebar in a 1,440 px window.
                .sidebar_width_fraction(0.331)
                .build();
            let (t, g, n, m) = (weak.clone(), weak.clone(), weak.clone(), weak.clone());
            let (read_settings, change_settings) = (Rc::downgrade(app), Rc::downgrade(app));
            let (contacts_app, push_app) = (Rc::downgrade(app), Rc::downgrade(app));
            let refresh_app = Rc::downgrade(app);
            let p = weak.clone();
            let calendar = CalendarView::new(
                Rc::clone(&app.core),
                move || {
                    read_settings
                        .upgrade()
                        .map(|a| a.settings())
                        .unwrap_or_default()
                },
                Hooks {
                    toast: Box::new(move |toast| {
                        if let Some(win) = t.upgrade() {
                            win.toasts.add_toast(toast);
                        }
                    }),
                    change: Box::new(move |change| {
                        if let Some(app) = change_settings.upgrade() {
                            app.change_settings(change);
                        }
                    }),
                    grant: Box::new(move |account_id| {
                        if let Some(win) = g.upgrade() {
                            win.grant_access(account_id);
                        }
                    }),
                    ask_permission: Box::new(move |account_id, permission| {
                        if let Some(win) = n.upgrade() {
                            win.ask_permission(account_id, permission, Occasion::Needed);
                        }
                    }),
                    contacts: Box::new(move || {
                        contacts_app
                            .upgrade()
                            .map(|app| app.contacts())
                            .unwrap_or_default()
                    }),
                    push: Box::new(move |account_id| {
                        if let Some(app) = push_app.upgrade() {
                            app.push_calendar(account_id);
                        }
                    }),
                    open_mail: Box::new(move |account_id, thread_id| {
                        if let Some(win) = m.upgrade() {
                            win.open_invitation_mail(account_id, thread_id);
                        }
                    }),
                    refresh: Box::new(move || {
                        if let Some(app) = refresh_app.upgrade() {
                            app.refresh_calendars();
                        }
                    }),
                    propose: Box::new(move |account_id, occurrence| {
                        if let Some(win) = p.upgrade() {
                            win.propose_for_event(account_id, occurrence);
                        }
                    }),
                    next_event: Box::new({
                        let win = weak.clone();
                        move || {
                            if let Some(win) = win.upgrade() {
                                win.refresh_next_event();
                            }
                        }
                    }),
                    waiting: Box::new({
                        let sidebar = Rc::downgrade(&sidebar);
                        move |count| {
                            if let Some(sidebar) = sidebar.upgrade() {
                                sidebar.set_waiting(count as i64);
                            }
                        }
                    }),
                },
            );
            // Each space asks for its own width only, so the calendar's
            // header never widens the mail's minimum, or the other way.
            let spaces = gtk::Stack::builder()
                .transition_type(gtk::StackTransitionType::Crossfade)
                .transition_duration(150)
                .hhomogeneous(false)
                .vhomogeneous(false)
                .build();
            spaces.add_named(&nav, Some("mail"));
            spaces.add_named(&calendar.page, Some("calendar"));
            let split = adw::OverlaySplitView::builder()
                .sidebar(&sidebar.page)
                .content(&spaces)
                .css_classes(["inset-sidebar"])
                .min_sidebar_width(256.0)
                .max_sidebar_width(256.0)
                .sidebar_width_fraction(0.178)
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
            split
                .bind_property("collapsed", &calendar.sidebar_button, "visible")
                .sync_create()
                .build();
            split
                .bind_property("show-sidebar", &calendar.sidebar_button, "active")
                .bidirectional()
                .sync_create()
                .build();

            let w = weak.clone();
            let microsoft = super::add_account::signs_in_to_microsoft(&app.core);
            let (by_hand, calendar_alone) = (weak.clone(), weak.clone());
            let first_page = welcome::first_account_page(
                microsoft,
                move |tile| {
                    if let Some(win) = w.upgrade() {
                        win.present_add_account(match tile.browser() {
                            Some(browser) => Opening::Browser(browser),
                            None => Opening::Tile(tile),
                        });
                    }
                },
                move || {
                    if let Some(win) = by_hand.upgrade() {
                        win.present_add_account(Opening::ByHand);
                    }
                },
                move || {
                    if let Some(win) = calendar_alone.upgrade() {
                        win.present_add_account(Opening::Calendar);
                    }
                },
            );
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
                {
                    let app = Rc::downgrade(app);
                    move |change| {
                        if let Some(app) = app.upgrade() {
                            app.change_settings(change);
                        }
                    }
                },
            );
            let toolbar_heights = gtk::SizeGroup::new(gtk::SizeGroupMode::Vertical);
            let assistant_top_bar = super::top_bar(&assistant.header);
            if let Some(bar) = &assistant_top_bar {
                toolbar_heights.add_widget(bar);
            }
            conversation.group_toolbar_height(&toolbar_heights);
            calendar.group_toolbar_height(&toolbar_heights);
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
            // Only the page on screen sets the width. Measured whole, the
            // hidden first-run page's row of provider tiles held the mail
            // at about 600 px, and a narrower window clipped it.
            let stack = gtk::Stack::builder()
                .transition_type(gtk::StackTransitionType::Crossfade)
                .hhomogeneous(false)
                .build();
            let room = {
                let least =
                    |widget: &gtk::Widget| widget.measure(gtk::Orientation::Horizontal, -1).0;
                let mailboxes = sidebar.page.clone().upcast::<gtk::Widget>().downgrade();
                let list_page = list.page.clone().upcast::<gtk::Widget>().downgrade();
                let panel = assistant.page.clone().upcast::<gtk::Widget>().downgrade();
                let sidebar_least = split.min_sidebar_width() as i32;
                let (columns, reading, days) = (
                    nav.downgrade(),
                    Rc::downgrade(&conversation),
                    Rc::downgrade(&calendar),
                );
                // The window's buttons show in one header at a time, so
                // the widest seen stands for them wherever they are.
                let buttons = Cell::new(0);
                // The space needs the wider of the mail and the calendar,
                // so switching between them leaves the panes where they
                // are. The mail needs its list beside the conversation's
                // bar of buttons, whether or not they stack now: the room
                // decides from that width when they stack.
                arranged_room(&split, &assistant_split, &nav, move || {
                    let list = list_page.upgrade().map_or(0, |w| least(&w));
                    let list = columns
                        .upgrade()
                        .map_or(list, |nav| list.max(nav.min_sidebar_width() as i32));
                    let (bar, bar_buttons) =
                        reading.upgrade().map_or((0, 0), |view| view.least_width());
                    let mail = list + bar;
                    let (calendar, calendar_buttons) =
                        days.upgrade().map_or((0, 0), |view| view.least_width());
                    buttons.set(buttons.get().max(bar_buttons).max(calendar_buttons));
                    room::Needs {
                        mailboxes: mailboxes
                            .upgrade()
                            .map_or(0, |w| least(&w))
                            .max(sidebar_least),
                        space: mail.max(calendar),
                        columns: mail,
                        controls: buttons.get(),
                        panel: panel
                            .upgrade()
                            .map_or(0, |w| least(&w))
                            .max(room::PANEL_LEAST),
                    }
                })
            };
            stack.add_named(&room, Some("mail"));
            stack.add_named(&first_page, Some("first-account"));
            // An update's banner spans the whole window, above the panes,
            // since it is about the app and not the mail on screen.
            let update_banner = adw::Banner::builder()
                .use_markup(false)
                .revealed(false)
                .build();
            // The keyring notice is about the app too, and the command it
            // gives needs the width a pane would cut short.
            let keyring_banner = adw::Banner::builder().revealed(false).build();
            let grant_banners = list.grant_bars.clone();
            let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
            content.append(&update_banner);
            content.append(&keyring_banner);
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
            // Whether the mailboxes fold, the assistant slides over the
            // space and the list stacks over the conversation follow from
            // the widths the panes need (room.rs), not from fixed
            // breakpoints: those drifted from what the panes needed, and
            // libadwaita clips a pane that does not fit.
            // The calendar's header folds its own view switch when short
            // of room (calendar/header.rs).
            let narrow = adw::Breakpoint::new(
                adw::BreakpointCondition::parse("max-width: 620sp").expect("valid breakpoint"),
            );
            let (on, off) = (Rc::clone(&conversation), Rc::clone(&conversation));
            let (calendar_on, calendar_off) = (Rc::clone(&calendar), Rc::clone(&calendar));
            let (list_on, list_off) = (Rc::clone(&list), Rc::clone(&list));
            narrow.connect_apply(move |_| {
                on.set_compact(true);
                calendar_on.set_narrow(true);
                list_on.set_narrow(true);
            });
            narrow.connect_unapply(move |_| {
                off.set_compact(false);
                calendar_off.set_narrow(false);
                list_off.set_narrow(false);
            });
            window.add_breakpoint(narrow);
            // `MAILRS_DEMO_SIZE=WxH` sizes the demo's window for a
            // screenshot, where no window manager would.
            if app.core.demo
                && let Ok(size) = std::env::var("MAILRS_DEMO_SIZE")
                && let Some((width, height)) = size.split_once('x')
                && let (Ok(width), Ok(height)) = (width.parse(), height.parse())
            {
                window.set_default_size(width, height);
            }

            let actions = gio::SimpleActionGroup::new();
            window.insert_action_group("win", Some(&actions));
            // A plain adw::Window, unlike GtkApplicationWindow, does not
            // reach the application's own actions. The update banner, menu
            // entry and About button all run app.* actions.
            window.insert_action_group("app", Some(&app.gio));
            track_dark_class(&window);
            MainWindow {
                window,
                actions,
                app: Rc::downgrade(app),
                core: Rc::clone(&app.core),
                toasts,
                in_flight: in_flight::InFlight::default(),
                update_banner,
                keyring_banner,
                grant_banners,
                grant_banner_widgets: RefCell::new(HashMap::new()),
                storage_banner_widgets: RefCell::new(HashMap::new()),
                update_menu: gio::Menu::new(),
                about: RefCell::new(None),
                preferences: RefCell::new(None),
                stack,
                split,
                spaces,
                nav,
                calendar,
                space_restored: Cell::new(false),
                startup_account_applied: Cell::new(false),
                sidebar,
                list,
                conversation,
                listing: Rc::new(ListingRun::new(
                    app.settings_with(|s| s.default_category),
                    Rc::new(searching::ListPane(weak.clone())),
                    Rc::new(searching::CoreLists(Rc::clone(&app.core))),
                )),
                accounts_read: RefCell::new(None),
                authorizing: Cell::new(false),
                assistant,
                assistant_split,
                assistant_window: RefCell::new(None),
                toolbar_heights,
                assistant_top_bar,
                categories: categories::CategoryBar::new(
                    app.settings_with(|s| s.default_category),
                    &Category::ALL,
                ),
                focus: categories::CategoryBar::new(Category::Focused, &Category::FOCUS),
                follow_up: followup::FollowUpBanner::new(),
                pictures: Rc::new(pictures::Pictures::new(Rc::clone(&app.core))),
                image_senders: RefCell::new(Vec::new()),
                detached: RefCell::new(Vec::new()),
                previews: previews::Previews::default(),
                undo_sends: RefCell::new(scheduled::UndoSends::default()),
                next_up: RefCell::new(None),
                next_card: across::next_card(weak.clone()),
            }
        });
        if window.core.demo {
            window.sidebar.start_expanded.set(Some(true));
        }
        let weak = Rc::downgrade(&window);
        if let Some(arrow) = window.conversation.label_arrow() {
            arrow.set_create_popup_func(move |button| {
                if let Some(win) = weak.upgrade() {
                    let reach = win.reach(&win.conversation);
                    let accounts = reach.targets.iter().map(|t| t.account_id);
                    win.word_filing(&win.conversation, accounts);
                    button.set_popover(Some(&win.label_popover()));
                }
            });
        }
        let weak = Rc::downgrade(&window);
        window
            .conversation
            .tag_button
            .set_create_popup_func(move |button| {
                if let Some(win) = weak.upgrade() {
                    button.set_popover(Some(&win.tag_popover()));
                }
            });
        // Both header buttons run the same toggle_assistant path as
        // Ctrl+J and the menu (R12); the panel's own show-sidebar keeps
        // them in the pressed state it puts on screen, whatever opened
        // or closed it.
        let weak = Rc::downgrade(&window);
        window
            .conversation
            .assistant_toggle
            .connect_clicked(move |_| {
                if let Some(win) = weak.upgrade() {
                    win.toggle_assistant();
                }
            });
        let weak = Rc::downgrade(&window);
        window.calendar.assistant_toggle.connect_clicked(move |_| {
            if let Some(win) = weak.upgrade() {
                win.toggle_assistant();
            }
        });
        let weak = Rc::downgrade(&window);
        window
            .assistant_split
            .connect_show_sidebar_notify(move |split| {
                if let Some(win) = weak.upgrade() {
                    win.sync_assistant_toggle();
                }
                if let Some(room) = split.parent().and_downcast::<room::Room>() {
                    room.rearrange();
                }
            });
        let weak = Rc::downgrade(&window);
        window.assistant.detach.connect_clicked(move |_| {
            if let Some(win) = weak.upgrade() {
                if win.assistant_window.borrow().is_some() {
                    win.dock_assistant(true);
                } else {
                    win.detach_assistant();
                }
            }
        });
        let weak = Rc::downgrade(&window);
        window.assistant.close.connect_clicked(move |_| {
            if let Some(win) = weak.upgrade() {
                win.assistant_split.set_show_sidebar(false);
            }
        });
        let weak = Rc::downgrade(&window);
        window
            .assistant_split
            .connect_collapsed_notify(move |split| {
                if let Some(win) = weak.upgrade() {
                    win.assistant.close.set_visible(
                        split.is_collapsed() && win.assistant_window.borrow().is_none(),
                    );
                }
            });
        // Rows ask for photos as they bind and redraw themselves; an open
        // conversation is drawn again once its senders' photos arrive.
        let photos = app.photos();
        window.list.set_photos(Rc::clone(&photos));
        let weak = Rc::downgrade(&window);
        photos.on_uri(move |_| match weak.upgrade() {
            Some(win) => {
                win.reopen_for_photos();
                true
            }
            None => false,
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
                win.listing.scrolled_to_end();
            }
        });
        window.install_actions();
        window.install_arrange_actions();
        // The assistant's tool calls, one at a time, on this thread.
        let weak = Rc::downgrade(&window);
        glib::spawn_future_local(async move {
            while let Ok(request) = tool_calls.recv().await {
                let Some(win) = weak.upgrade() else { break };
                let outcome = win
                    .run_tool(&request.name, request.input, request.picked_files)
                    .await;
                let _ = request.reply.send(outcome).await;
            }
        });
        window.install_follow_ups();
        window.install_categories();
        window.install_search_typing();
        window.offer_summary();
        window.sync_assistant_toggle();
        window.install_undo_send();
        window.install_next_event();
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
        window.install_spaces();
        let weak = Rc::downgrade(&window);
        window.list.banner.connect_button_clicked(move |_| {
            let Some(win) = weak.upgrade() else { return };
            let account = win
                .accounts()
                .into_iter()
                .find(|a| a.state == AccountState::NeedsReauth);
            match account {
                Some(account) => win.sign_in_again(account),
                None => win.authorize(None),
            }
        });
        if let Some(filter) = app.filter() {
            window.conversation.set_filter(filter);
        }
        window
            .conversation
            .set_zoom(app.settings_with(|s| s.text_size.zoom()));
        app.unread()
            .show_in(Box::new(ShownCounts(Rc::downgrade(&window))));
        window.refresh_accounts(Reload::Yes);
        window.reload_image_senders();
        window.check_keyring_plug();
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

    /// The conversation the person is looking at: the one on screen while
    /// the window has focus. `None` while the window is unfocused or
    /// hidden, shows no conversation, or keeps one open behind the
    /// Calendar space or a narrow window's list, where its page is not
    /// mapped.
    pub fn viewed(&self) -> Option<Target> {
        if !self.is_active() || !self.conversation.page.is_mapped() {
            return None;
        }
        self.conversation.read(OpenThread::target)
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
            ChangeEvent::AccountStateChanged { account_id, state } => {
                self.refresh_accounts(Reload::No);
                if *state == AccountState::Ok {
                    self.account_ready_for_threads(*account_id);
                }
            }
            ChangeEvent::LabelsChanged { .. } => self.refresh_accounts(Reload::Yes),
            ChangeEvent::ThreadsChanged {
                account_id,
                thread_ids,
            } => {
                let changed = thread_ids.iter().map(|id| (*account_id, id.clone()));
                self.listing.changed(changed.collect());
            }
            ChangeEvent::NewMail { .. } => self.listing.everything(),
            ChangeEvent::WriteFailed { message, .. } => self.toast(message),
            // Gmail can hold a bulk change up for the best part of a
            // minute. Say so, or the window looks stuck and the reader
            // presses Delete again.
            ChangeEvent::WaitingOnGmail { message, .. } => self.toast(message),
            // The first archive on a server without an Archive folder made
            // one. Say so once; the LabelsChanged sent with it redraws the
            // sidebar.
            ChangeEvent::ArchiveMade { account_id, name } => {
                if let Some(account) = self.account(*account_id) {
                    let provider = mailrs_discover::resolved_provider_name(account.provider_name());
                    self.toast(&archive_made_line(&provider, name));
                }
            }
        }
    }

    /// The network came back: an IMAP account looked at offline gets its
    /// look for a JMAP session now. The sidebar is built again only when
    /// one turned up, since a rebuild closes any menu open in it.
    pub(crate) fn network_back(self: &Rc<Self>) {
        let (looker, accounts) = (Rc::clone(self), self.accounts());
        glib::spawn_future_local(async move {
            if looker.core.look_for_jmap(accounts).await {
                looker.refresh_accounts(Reload::No);
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
        // The demo shows the first-run page on request, for screenshots.
        let first_run = self.core.demo && std::env::var_os("MAILRS_DEMO_FIRST_RUN").is_some();
        let page = if data.is_empty() || first_run {
            "first-account"
        } else {
            "mail"
        };
        self.stack.set_visible_child_name(page);
        let live: HashSet<AccountId> = data.iter().map(|(a, _)| a.id).collect();
        // Any account can need to sign in again, a calendar account too.
        let reauth = data
            .iter()
            .map(|(a, _)| a)
            .find(|a| a.state == AccountState::NeedsReauth)
            .cloned();
        // A calendar account has no mailboxes, so the mail sidebar and
        // everything worded by what the accounts offer for mail is built
        // from the accounts that have mail. The calendar lists every
        // account, so the others are kept aside for it.
        let (data, calendar_accounts): (Vec<_>, Vec<_>) = data
            .into_iter()
            .partition(|(a, _)| a.provider.serves_mail());
        self.listing.accounts_live(&live);
        // A label deleted elsewhere, by the assistant or in the browser,
        // leaves the window on a mailbox that is no longer there, so the
        // inbox takes over as it does for a signed-out account.
        let still_there = still_there(&self.shown(), &data);
        let vanished = self
            .listing
            .transition(|screen| screen.accounts_read(still_there));
        let settings = self.settings();
        let not_downloading = self
            .core
            .read(mailrs_store::pop3::accounts_failing)
            .await
            .map(|ids| ids.into_iter().collect())
            .unwrap_or_default();
        let (data, extras) = self.arrange(data, &settings, not_downloading, self.core.switchable());
        // Each IMAP account is looked at once a run; finding a JMAP
        // session for one rebuilds its row with Switch to JMAP.
        let imap: Vec<Account> = data.iter().map(|(a, _)| a.clone()).collect();
        let looker = Rc::clone(self);
        glib::spawn_future_local(async move {
            if looker.core.look_for_jmap(imap).await {
                looker.refresh_accounts(Reload::No);
            }
        });
        self.list.set_vips(settings.vips.keys().cloned().collect());
        let mailbox = self.shown();
        if !matches!(mailbox, Mailbox::Search { .. }) {
            self.sidebar
                .rebuild(&data, &extras, &mailbox, |id| self.offers(id));
        }
        // A search on screen skips the rebuild above, but an account's own
        // heading row still needs its Rules, Hide My Email and Automatic
        // Reply gated again when the account starts mid-search.
        self.sidebar.regate(|id| self.offers(id));
        if !data.is_empty()
            && !self.startup_account_applied.replace(true)
            && matches!(self.shown(), Mailbox::Unified(crate::ui::Standard::Inbox))
            && let Some(email) = settings.preferred_account(crate::settings::AccountUse::Startup)
            && let Some((account, _)) = data
                .iter()
                .find(|(account, _)| account.email.eq_ignore_ascii_case(email))
        {
            self.show_inbox_of(account.id);
        }
        // Each hidden address comes with its own rules, so Hide My Email
        // works only while some account can hold them, and each account
        // action only while some account has what it opens.
        let offers: Vec<Offers> = data.iter().map(|(a, _)| self.offers(a.id)).collect();
        for (name, enabled) in crate::offered::account_actions(&offers) {
            if let Some(action) = self
                .actions
                .lookup_action(name)
                .and_downcast::<gio::SimpleAction>()
            {
                action.set_enabled(enabled);
            }
        }
        // This runs when an account starts, so what the open
        // conversations offer is read again here.
        self.follow_gates();
        self.list
            .set_show_accounts(mailbox.account().is_none() && data.len() > 1);
        match &reauth {
            Some(account) => {
                let through_browser = self
                    .app
                    .upgrade()
                    .is_some_and(|app| app.signs_in_through_browser(account.id));
                self.list
                    .banner
                    .set_title(&crate::add_account::sign_in_again_banner(
                        &account.email,
                        through_browser,
                    ));
                self.list.banner.set_button_label(Some(&gettext("Sign In")));
                self.list.banner.set_revealed(true);
            }
            None => self.list.banner.set_revealed(false),
        }
        match vanished {
            // Showing the inbox lists it, so it is not listed again below.
            Some(redraw) => self.listing.carry(redraw),
            None => {
                self.follow_categories();
                if reload == Reload::Yes {
                    self.listing.reload();
                }
            }
        }
        self.refresh_counts();
        // app.reload_accounts already read every account's consent in the
        // same pass as the accounts themselves, so this costs no round
        // trip of its own and settles with everything else above.
        let mut accounts: Vec<Account> = data.iter().map(|(a, _)| a.clone()).collect();
        self.rebuild_grant_banners(&accounts, &app.consent());
        self.rebuild_storage_banners();
        // The mail accounts in the sidebar's order, then the calendar
        // accounts, which the mail sidebar has no place for.
        accounts.extend(calendar_accounts.into_iter().map(|(account, _)| account));
        self.accounts_for_calendar(&accounts);
    }

    /// Asks snapd whether the snap can reach the keyring, and puts a
    /// notice across the top of the window while it cannot. The app asks
    /// again each time the window is presented, and takes the notice down
    /// once the plug is connected. Outside a snap this asks nothing.
    /// `MAILRS_DEMO_KEYRING_UNPLUGGED` shows the notice in the demo.
    pub fn check_keyring_plug(self: &Rc<Self>) {
        let unplugged_demo =
            self.core.demo && std::env::var_os("MAILRS_DEMO_KEYRING_UNPLUGGED").is_some();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let plug = crate::keyring_plug::check(unplugged_demo).await;
            let Some(win) = weak.upgrade() else { return };
            if plug.wants_notice() {
                win.show_keyring_notice();
            } else {
                win.keyring_banner.set_revealed(false);
            }
        });
    }

    fn show_keyring_notice(self: &Rc<Self>) {
        let banner = &self.keyring_banner;
        // A later check can show the notice again after hiding it; the
        // button keeps the one handler it got the first time.
        if banner.button_label().is_none() {
            banner.set_use_markup(true);
            banner.set_title(&crate::keyring_plug::notice_markup());
            banner.set_button_label(Some(&gettext("Copy Command")));
            let weak = Rc::downgrade(self);
            banner.connect_button_clicked(move |banner| {
                banner.clipboard().set_text(crate::keyring_plug::COMMAND);
                if let Some(win) = weak.upgrade() {
                    win.toast(&gettext("Command copied"));
                }
            });
        }
        banner.set_revealed(true);
    }

    /// Brings the Grant Access banners in line with `accounts` and
    /// `consent`: one per account whose own scopes leave something out
    /// and that has never been asked for everything. Only accounts whose
    /// answer changed gain or lose a banner; one that already has the
    /// right banner keeps the same widget, so a later call from an
    /// unrelated account starting or stopping never shifts what is
    /// already on screen. The banner opens the browser only when its
    /// button is pressed; nothing here does at start or from the tray.
    fn rebuild_grant_banners(
        self: &Rc<Self>,
        accounts: &[Account],
        consent: &HashMap<AccountId, mailrs_store::accounts::Consent>,
    ) {
        let wanted: Vec<&Account> = accounts
            .iter()
            .filter(|account| {
                let withheld = self.withheld(account.id);
                let asked = consent.get(&account.id).and_then(|c| c.asked.as_deref());
                crate::permission::wants_banner(withheld, asked)
            })
            .collect();
        let wanted_ids: HashSet<AccountId> = wanted.iter().map(|a| a.id).collect();
        let mut widgets = self.grant_banner_widgets.borrow_mut();
        widgets.retain(|id, banner| {
            let keep = wanted_ids.contains(id);
            if !keep {
                self.grant_banners.remove(banner);
            }
            keep
        });
        for account in wanted {
            let missing = crate::permission::withheld_permissions(self.withheld(account.id));
            if let Some(banner) = widgets.get(&account.id) {
                // The same widget stays; only its words follow what is
                // still missing.
                banner.set_title(&crate::permission::grant_bar_title(
                    &account.email,
                    &missing,
                    account.provider,
                ));
                continue;
            }
            tracing::info!(
                account = account.id,
                ?missing,
                "showing the Grant Access banner"
            );
            // The title holds the address, which may carry an "&".
            let banner = adw::Banner::builder()
                .use_markup(false)
                .title(crate::permission::grant_bar_title(
                    &account.email,
                    &missing,
                    account.provider,
                ))
                .button_label(gettext("Grant Access"))
                .revealed(true)
                .build();
            // A weak reference: the banner lives in the window, and a
            // strong one would keep a closed window and its WebView alive.
            let (weak, account_id) = (Rc::downgrade(self), account.id);
            banner.connect_button_clicked(move |_| {
                if let Some(win) = weak.upgrade() {
                    win.grant_access(account_id);
                }
            });
            self.grant_banners.append(&banner);
            widgets.insert(account.id, banner);
        }
    }

    /// Asks the app's unread counts again; they come back through
    /// `show_counts` with the tray's and the Dock's.
    fn refresh_counts(self: &Rc<Self>) {
        if let Some(app) = self.app.upgrade() {
            app.unread().changed();
        }
    }

    /// What the sidebar, the space badge and the category switcher show.
    fn show_counts(&self, counted: &Counted) {
        self.sidebar.set_counts(&counted.counts.mailboxes);
        self.sidebar.set_unread(counted.all_inboxes());
        self.set_follow_up_count(counted.of(&Mailbox::FollowUp) as usize);
        self.set_category_counts(&counted.counts.categories);
    }

    // ---- Mailboxes and the thread list ---------------------------------

    /// The accounts a listing may read, in sidebar order.
    fn scope(&self) -> Scope {
        Scope::of_mail_accounts(&self.accounts())
    }

    /// The settings that change what the mailbox on screen lists.
    fn view(&self) -> View {
        self.view_of(&self.shown(), self.listing.category())
    }

    /// The settings that change what `mailbox` lists, with `category`
    /// chosen in the inbox.
    fn view_of(&self, mailbox: &Mailbox, category: Category) -> View {
        let category = self.shows_categories(mailbox).then_some(category);
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
        self.listing.mailbox()
    }

    /// Puts one account's inbox on screen and selects its sidebar row.
    pub fn show_inbox_of(self: &Rc<Self>, account_id: AccountId) {
        let inbox = Mailbox::Standard {
            account_id,
            which: crate::ui::Standard::Inbox,
        };
        self.sidebar.select(&inbox);
        self.show_mailbox(inbox);
    }

    /// Puts `mailbox` on screen.
    fn show_mailbox(self: &Rc<Self>, mailbox: Mailbox) {
        let search_open = self.list.search_open();
        self.listing
            .change(|screen| Some(screen.show(mailbox, search_open)));
    }

    /// Carries out what a change to the mailbox on screen left stale,
    /// apart from listing it, which the listing run does.
    fn redraw(self: &Rc<Self>, redraw: &Redraw) {
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
                .set_show_accounts(mailbox.account().is_none() && self.mail_accounts().len() > 1);
            let accounts = self.accounts_of(&mailbox);
            self.word_buttons(&self.conversation, &mailbox, accounts);
            self.follow_outbox();
            self.follow_categories();
            self.follow_follow_ups();
            self.place_storage_banners();
        }
        if redraw.category {
            let category = self.listing.category();
            self.categories.show_names(category);
            self.focus.show_names(category);
        }
    }

    fn search(self: &Rc<Self>, query: String) {
        self.listing.search(query);
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
        self.follow_sender_actions(summary.account_id);
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
            Picked::One(row) => self.open_thread(Rc::unwrap_or_clone(row)),
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
            Action::Summarize => self.summarize(view),
            Action::Receipt { message_id, send } => self.answer_receipt(view, message_id, send),
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
            Action::ViewSource { message_id } => self.view_source_of(view, Some(message_id)),
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
            Action::NeverTranslate(language) => self.never_translate(language),
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
        let accounts = press::reached(&reach.targets, |id| self.account(id), |id| self.offers(id));
        let plan = press::plan(Pressed {
            press,
            reach,
            scope,
            flag_color: self.settings_with(|s| s.flag_color),
            threaded: self.settings_with(|s| s.threading),
            accounts,
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
    /// Trash the button shows while any account in `accounts`, the ones an
    /// action on `view` reaches, can delete mail for good.
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
            None => self.mail_accounts().iter().map(|a| a.id).collect(),
        }
    }

    /// Whether the server of any account in `accounts` can delete mail for
    /// good. Delete Forever then erases the mail of the accounts that can,
    /// and its question names the others.
    fn erases(&self, accounts: impl IntoIterator<Item = AccountId>) -> bool {
        accounts
            .into_iter()
            .any(|id| self.offers(id).delete_forever)
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
    /// the provider for it. Every `Permitted::NeedsPermission` answer the window
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
            if permission::ask(
                &this.window,
                account_id,
                email,
                account.provider,
                permission,
                occasion,
            )
            .await
            {
                this.grant(account.email);
            }
        });
    }

    /// Sends `email` through its provider's consent again, once the person
    /// has chosen Grant Access. Each consent asks for every scope Penguin
    /// Mail uses, so this is the one path a permission or the banner
    /// needs. An IMAP or POP3 account has no consent to run.
    fn grant(self: &Rc<Self>, email: String) {
        let account = self
            .accounts()
            .into_iter()
            .find(|account| account.email.eq_ignore_ascii_case(&email));
        match account {
            Some(account) => {
                if let Some(browser) = Browser::of(account.provider) {
                    self.sign_in_in_browser(browser, Some(email));
                }
            }
            // An address no account has yet goes to Google, as before.
            None => self.sign_in_in_browser(Browser::Google, Some(email)),
        }
    }

    /// [`MainWindow::grant`] for an account known only by its id: the one
    /// path the Grant Access banner, the calendar sidebar row and the
    /// Preferences rows all end at.
    pub fn grant_access(self: &Rc<Self>, account_id: AccountId) {
        if let Some(account) = self.account(account_id) {
            self.grant(account.email);
        }
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

    /// Draws the rows and open conversations again once the contact photos
    /// on disk are known, so they show faces instead of initials.
    fn contacts_loaded(self: &Rc<Self>) {
        self.list.photos_changed();
        self.reopen_for_photos();
    }

    /// Runs `action` on the targets, as mail picked from no one place. See
    /// [`Self::perform_from`].
    fn perform(
        self: &Rc<Self>,
        targets: Vec<Target>,
        action: MailAction,
        history: History,
        message: Option<String>,
    ) {
        self.perform_from(targets, action, history, message, MovedFrom::nowhere());
    }

    /// Runs `action` on the targets, picked from `from`. With
    /// `History::Record`, the toast says what changed, `message` in place
    /// of the usual text, and offers Undo.
    fn perform_from(
        self: &Rc<Self>,
        targets: Vec<Target>,
        action: MailAction,
        history: History,
        message: Option<String>,
        from: MovedFrom,
    ) {
        if targets.is_empty() {
            return;
        }
        // The rows leave and the toast shows now; the server may take a
        // while, behind a sync pass or a slow connection.
        let asked = targets.len();
        let leaving = aftermath::leaves(&action, &self.shown()).then(|| {
            self.list
                .retain(|row| !targets.iter().any(|target| aftermath::covers(target, row)));
            self.follow_selection();
            self.in_flight.leave(&targets)
        });
        let run = match history {
            History::Record => Some(self.announce(&action, asked, message)),
            History::Skip => None,
        };
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let outcome = this.core.act(targets, action.clone(), history, from).await;
            if let Some(leaving) = leaving {
                this.in_flight.arrived(leaving);
            }
            this.after_mail(Cause::Did(&action, history), &outcome, None);
            if let Some(run) = &run {
                this.ran(run, &outcome);
            }
            // What the action did not change lists again where it was.
            if leaving.is_some() && outcome.done.len() < asked {
                this.listing.everything();
            }
            if let Some(error) = outcome.first_error() {
                this.say_now(error);
            }
        });
    }

    /// Reverses the organizing action on top of the undo stack, whether
    /// the window or the assistant took it. The one before it is left for
    /// the next press.
    fn undo(self: &Rc<Self>) {
        // An action still on its way is not on the stack yet, and pressing
        // Undo right after it means that one.
        if self.undo_newest() {
            return;
        }
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let undone = this.core.undo().await;
            if let Some(undone) = &undone {
                this.after_mail(Cause::Undid, &undone.outcome, None);
            }
            this.say_undone(undone);
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
            let shown = self.accounts_of(&self.shown());
            let filing = Filing::of(shown.into_iter().map(|id| self.offers(id)));
            popover.set_child(Some(&message(&filing.nothing_picked())));
            return popover;
        }
        let Some(&account_id) = accounts.iter().next().filter(|_| accounts.len() == 1) else {
            // Mail from several accounts takes labels by name, since each
            // account has its own label behind a name.
            let filing = Filing::across(accounts.iter().map(|id| self.offers(*id)));
            let list = match filing {
                Filing::Labels => self.labels_by_name(&accounts, &popover),
                Filing::Folders => None,
            };
            match list {
                Some(list) => popover.set_child(Some(&list)),
                None => popover.set_child(Some(&message(&filing.one_account_only()))),
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

    /// The tag list for what the Tags button reaches: the selection, or
    /// the open conversation.
    fn tag_popover(self: &Rc<Self>) -> gtk::Popover {
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
        self.tag_popover_for(targets, applied, Some(view))
    }

    /// The tags of the targets' account, with `applied` already ticked. A
    /// tag is added or taken off, never moved to, and none is made here:
    /// Outlook makes them. `follow` is the conversation to move on from
    /// when a change takes its mail out of the mailbox on screen.
    pub(super) fn tag_popover_for(
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
        let Some(&account_id) = accounts.iter().next().filter(|_| accounts.len() == 1) else {
            popover.set_child(Some(&message(&gettext(
                "Select mail from one account to tag it.",
            ))));
            return popover;
        };
        let mut tags: Vec<Label> = self
            .labels_of(account_id)
            .into_iter()
            .filter(|l| l.kind == mailrs_domain::LabelKind::Tag)
            .collect();
        tags.sort_by_key(|l| l.name.to_lowercase());
        if tags.is_empty() {
            popover.set_child(Some(&message(&gettext(
                "This account has no tags yet. Make one in Outlook.",
            ))));
            return popover;
        }
        let list = gtk::ListBox::builder()
            .css_classes(["navigation-sidebar"])
            .selection_mode(gtk::SelectionMode::None)
            .build();
        for tag in &tags {
            let row = gtk::Box::builder().spacing(10).build();
            // The tick keeps its place at zero opacity when the tag is off,
            // so the dot and the name start at the same x on every row.
            let check = gtk::Image::from_icon_name("object-select-symbolic");
            check.set_opacity(if applied.contains(&tag.id) { 1.0 } else { 0.0 });
            row.append(&check);
            row.append(&super::sidebar::tag_dot(tag.color.as_deref()));
            row.append(&gtk::Label::builder().label(&tag.name).xalign(0.0).build());
            let row = gtk::ListBoxRow::builder()
                .child(&row)
                .activatable(true)
                .build();
            // The tick is drawn at zero opacity when the tag is off, which
            // says nothing out loud.
            crate::ui::name(&row, &label_row_name(&tag.name, applied.contains(&tag.id)));
            list.append(&row);
        }
        let (weak, pop) = (Rc::downgrade(self), popover.clone());
        list.connect_row_activated(move |_, row| {
            let (Some(win), Some(tag)) = (weak.upgrade(), tags.get(row.index() as usize)) else {
                return;
            };
            pop.popdown();
            win.press_label(
                targets.clone(),
                filing_choice(
                    Filing::Labels,
                    &tag.id,
                    &tag.name,
                    applied.contains(&tag.id),
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
        popover.set_child(Some(&scroller));
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
        let mine = app.my_addresses(account_id, &target);
        let mut draft = compose::respond(
            kind,
            account_id,
            &mine,
            &target,
            &text,
            html.as_deref(),
            &thread,
        );
        let task = if kind == ReplyKind::Forward {
            crate::settings::AccountUse::Forwards
        } else {
            crate::settings::AccountUse::Replies
        };
        if let Some(outgoing) = app.account_for(task, Some(account_id))
            && outgoing != account_id
        {
            draft.use_account(outgoing, app.blank_draft(outgoing).from);
            if kind != ReplyKind::Forward {
                let mine = app.my_addresses(outgoing, &target);
                draft.to.retain(|to| {
                    !mine
                        .iter()
                        .any(|own| own.email.eq_ignore_ascii_case(&to.email))
                });
                draft.cc.retain(|cc| {
                    !mine
                        .iter()
                        .any(|own| own.email.eq_ignore_ascii_case(&cc.email))
                });
            }
        }
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
            let mut draft_id = this
                .core
                .call(async move { s.draft_id_for(&m).await })
                .await
                .ok()
                .flatten();
            let waiting = match draft_id.clone() {
                Some(id) => {
                    let outbox = this.core.outbox();
                    this.core
                        .call(async move { outbox.find_draft(account_id, &id).await })
                        .await
                        .ok()
                        .flatten()
                }
                None => None,
            };
            // A copy the server holds for Send Later is called back before
            // the composer may touch it, or the server would send it beside
            // whatever the person does here.
            let mut source = message.id.clone();
            let mut kept_bytes = None;
            let mut locked = None;
            // The row of a held message called back for this composer.
            let mut reopened = None;
            let send_at = waiting.as_ref().map(|row| row.send_at);
            if let Some(row) = waiting.filter(|row| row.held.is_some()) {
                let (outbox, id) = (this.core.outbox(), row.id);
                match this
                    .core
                    .call(async move { outbox.reopen_held(id).await })
                    .await
                {
                    Ok(Reopened::Free) => {}
                    Ok(Reopened::Draft(now)) => {
                        reopened = Some(now.id);
                        draft_id = now.draft_id.clone();
                        source = now.message_id.clone().unwrap_or(source);
                        kept_bytes = now.raw;
                        this.scheduled_changed();
                    }
                    Ok(Reopened::AlreadySent) => {
                        this.scheduled_changed();
                        return this.toast(&gettext("The server had already sent this message."));
                    }
                    Ok(Reopened::Gone) => {
                        this.scheduled_changed();
                        return this.toast(&gettext(
                            "This message was canceled in another app and is no longer in Drafts.",
                        ));
                    }
                    Ok(Reopened::Locked { notice, can_cancel }) => {
                        let target = row.thread_id.clone().map(|thread| Target {
                            account_id,
                            thread_id: thread,
                            message_id: row.message_id.clone(),
                        });
                        locked = Some((notice, target.filter(|_| can_cancel)));
                        // The server's copy has no Bcc; the row's bytes do.
                        kept_bytes = row.raw.clone();
                        draft_id = None;
                    }
                    Err(err) => {
                        return this.toast(&with_reason(
                            &gettext("Could not open the draft: {reason}"),
                            &err,
                            &[],
                        ));
                    }
                }
            }
            let mut draft = app.blank_draft(account_id);
            // Only the message as Gmail holds it carries the Bcc, the reply
            // headers and the bytes of the files, readable or encrypted.
            // Opening without them would save a draft that had lost them.
            let raw = match kept_bytes {
                Some(raw) => raw,
                None => {
                    let s = sync.clone();
                    match this
                        .core
                        .call(async move { s.raw_message(&source).await })
                        .await
                    {
                        Ok(raw) => raw.as_ref().clone(),
                        Err(err) => {
                            return this.toast(&with_reason(
                                &gettext("Could not open the draft: {reason}"),
                                &err,
                                &[],
                            ));
                        }
                    }
                }
            };
            if let Err(problem) =
                crate::protection::draft::reopened(&this.core, raw, &mut draft).await
            {
                return this.toast(&problem);
            }
            draft.thread_id = in_thread.then_some(thread_id);
            draft.send_at = send_at;
            draft.draft_id = draft_id;
            // The composer that saved it signed it then.
            let Some(composer) = app.open_composer(draft, Signature::AsWritten) else {
                return;
            };
            // Closed as it opened, it goes back to the server for its hour,
            // so it still goes with the laptop shut.
            if let Some(id) = reopened {
                let weak = Rc::downgrade(&this);
                composer.on_closed_unchanged(Box::new(move || {
                    if let Some(win) = weak.upgrade() {
                        win.hold_unchanged(id);
                    }
                }));
            }
            if let Some((notice, target)) = locked {
                let cancel = target.map(|target| {
                    let weak = Rc::downgrade(&this);
                    Box::new(move || {
                        if let Some(win) = weak.upgrade() {
                            let view = Rc::clone(&win.conversation);
                            win.cancel_scheduled(&view, vec![target.clone()]);
                        }
                    }) as Box<dyn Fn()>
                });
                composer.lock(&notice, cancel);
            }
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

    /// Adds a Google account, or signs in again the one at `expected`.
    fn authorize(self: &Rc<Self>, expected: Option<String>) {
        self.sign_in_in_browser(Browser::Google, expected);
    }

    /// Asks which kind of account to add, then adds it.
    pub(super) fn add_account(self: &Rc<Self>) {
        self.present_add_account(Opening::Pick);
    }

    fn present_add_account(self: &Rc<Self>, opening: Opening) {
        let weak = Rc::downgrade(self);
        super::add_account::present(&self.core, &self.window, opening, move |done| {
            let Some(win) = weak.upgrade() else { return };
            match done {
                Done::Added { account, name } => win.imap_added(&account, name),
                Done::BrowserAdded { account, name } => {
                    win.keep_send_as_name(&account, name);
                    if let Some(app) = win.app.upgrade() {
                        app.signed_in(&account);
                    }
                    win.refresh_accounts(Reload::Yes);
                }
                Done::OpenInbox(account_id) => {
                    let inbox = Mailbox::Standard {
                        account_id,
                        which: crate::ui::Standard::Inbox,
                    };
                    // The calendar shows when only calendar accounts were
                    // here before this one.
                    win.show_space(crate::settings::Space::Mail);
                    win.sidebar.select(&inbox);
                    win.show_mailbox(inbox);
                }
                Done::Grant(email) => win.grant(email),
                Done::CalendarAdded => win.calendar_added(),
                Done::SignedInAgain(account) => {
                    win.toast(&fill(
                        &gettext("{account} is signed in again."),
                        &[("account", &account.email)],
                    ));
                    // A calendar account has no mail for the list to read
                    // again.
                    win.refresh_accounts(if account.provider.serves_mail() {
                        Reload::Yes
                    } else {
                        Reload::No
                    });
                    // A JMAP account may have signed in another way, through
                    // the browser or with an app password for its calendar,
                    // which changes the servers it can reach beside mail.
                    if account.provider == Provider::Jmap {
                        win.look_for_servers(&account);
                    }
                }
            }
        });
    }

    /// A calendar account signed in. The accounts are read again, and
    /// once they are the calendar shows and reads the new account's
    /// calendars, rather than leaving them to the next tick.
    fn calendar_added(self: &Rc<Self>) {
        self.refresh_accounts(Reload::No);
        let read = self.accounts_read.borrow().clone();
        let win = Rc::clone(self);
        glib::spawn_future_local(async move {
            if let Some(read) = read {
                read.await;
            }
            win.show_space(crate::settings::Space::Calendar);
            if let Some(app) = win.app.upgrade() {
                app.refresh_calendars();
            }
        });
    }

    /// Signs `account` in again the way it signed in first: its
    /// provider's page in the browser, step 2 of Another Provider with
    /// its servers, or the calendar page for a calendar account.
    fn sign_in_again(self: &Rc<Self>, account: Account) {
        match Browser::of(account.provider) {
            Some(browser) => self.sign_in_in_browser(browser, Some(account.email)),
            None => self.present_add_account(Opening::Again(account)),
        }
    }

    /// An IMAP account signed in. The name the person typed is what their
    /// mail goes out under, kept as the account's one send-as address,
    /// since the server keeps no name.
    fn imap_added(self: &Rc<Self>, account: &Account, name: Option<String>) {
        self.keep_send_as_name(account, name);
        // The dialog stays open on the account's first sync, which says
        // what a toast would.
        self.refresh_accounts(Reload::Yes);
        self.look_for_servers(account);
    }

    /// Keeps `name` on the account's own send-as address, for a provider
    /// that does not tell the client what its person sends as. Microsoft
    /// gives a name at sign-in, and `offered::reads_send_as` asks only
    /// Gmail.
    fn keep_send_as_name(&self, account: &Account, name: Option<String>) {
        if let (Some(app), Some(name)) = (self.app.upgrade(), name) {
            let mut addresses = app.settings().senders(&account.email);
            if let Some(own) = addresses
                .iter_mut()
                .find(|a| a.email.eq_ignore_ascii_case(&account.email))
            {
                own.name = Some(name);
            }
            app.change_settings(Change::SendAsAddresses {
                account: account.email.clone(),
                addresses,
                at: mailrs_sync::now_millis(),
            });
        }
    }

    /// Searches for the new account's calendar, contacts and rules
    /// servers once it is running, so adding it waits on no DNS. Contacts
    /// switch on when a CardDAV server is found.
    fn look_for_servers(self: &Rc<Self>, account: &Account) {
        let (win, account) = (Rc::clone(self), account.clone());
        glib::spawn_future_local(async move {
            match win.core.find_services(account.clone()).await {
                Ok(found) if found.contacts => {
                    if let Some(app) = win.app.upgrade() {
                        app.change_settings(Change::AccountContacts {
                            email: account.email.clone(),
                            on: true,
                        });
                    }
                    win.refresh_accounts(Reload::Yes);
                }
                Ok(found) if found.calendar => win.refresh_accounts(Reload::Yes),
                Ok(_) => {}
                Err(err) => {
                    tracing::info!(account = account.id, %err, "found no calendar or contacts server");
                }
            }
        });
    }

    /// Runs `browser`'s sign-in for Sign In Again, Grant Access and the
    /// banner. With `address`, a sign-in that comes back as another
    /// address is refused. Add Account runs its own sign-in inside the
    /// dialog.
    fn sign_in_in_browser(self: &Rc<Self>, browser: Browser, address: Option<String>) {
        // Without the build's client the browser would open for nothing,
        // so say why at once. The demo goes on to its own message.
        if !self.core.demo && !self.core.built_with_sign_in(browser) {
            return match browser {
                Browser::Google => self.no_google_sign_in(),
                Browser::Microsoft => self.no_sign_in(&gettext(
                    "This copy of Penguin Mail was built without Microsoft sign-in.",
                )),
            };
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
        self.toast(&gettext("Continue in your browser"));
        // The wait runs until the browser comes back or gives up; nothing
        // here cancels it, so the sender lives as long as the run.
        let (keep, cancel) = async_channel::bounded::<()>(1);
        let wanted = address.map_or(Wanted::Anyone, Wanted::Only);
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let signed = this
                .core
                .sign_in_in_browser(browser, urls, wanted, cancel)
                .await;
            drop(keep);
            match signed {
                Ok((account, _)) => {
                    this.toast(&signed_in_toast(browser, &account));
                    if let Some(app) = this.app.upgrade() {
                        app.signed_in(&account);
                    }
                    this.refresh_accounts(Reload::Yes);
                }
                Err(err) => this.toast(&err.to_string()),
            }
            this.authorizing.set(false);
        });
    }

    /// Says that this copy cannot sign in to Google, with a button to the
    /// releases, whose builds can.
    fn no_google_sign_in(&self) {
        self.no_sign_in(&gettext(
            "This copy of Penguin Mail was built without Google sign-in.",
        ));
    }

    /// Says `reason`, with a button to the releases, whose builds can sign in.
    fn no_sign_in(&self, reason: &str) {
        let toast = adw::Toast::builder()
            .title(toast_title(reason))
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

    /// The accounts with mail: what a mailbox lists and what the mail
    /// buttons are worded for. A calendar account is not among them.
    fn mail_accounts(&self) -> Vec<Account> {
        self.app
            .upgrade()
            .map(|app| app.mail_accounts())
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

    /// What `account_id`'s own consent left withheld, or nothing while it
    /// has not started.
    pub(super) fn withheld(&self, account_id: AccountId) -> Withheld {
        let running = self.core.account(account_id);
        crate::offered::withheld_for(running.as_ref().map(|sync| sync.services()))
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

    fn label_order(&self, account_id: AccountId) -> HashMap<String, i64> {
        self.app
            .upgrade()
            .map(|app| app.label_order(account_id))
            .unwrap_or_default()
    }

    fn confirm_remove(self: &Rc<Self>, account: Account) {
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            // What a POP3 account's removal loses depends on whether its
            // setting has already taken mail off the server.
            let remove = if account.provider == Provider::Pop3 {
                let account_id = account.id;
                this.core
                    .read(move |c| mailrs_store::accounts::pop3_remove(c, account_id))
                    .await
                    .unwrap_or(RemoveSetting::Downloaded)
            } else {
                RemoveSetting::Never
            };
            // The server sends what it holds whether or not the account is
            // here, so those messages are called back first.
            let (outbox, account_id) = (this.core.outbox(), account.id);
            // Not knowing is not zero: removing the account then would
            // leave the server to send what it holds.
            let held = match this
                .core
                .call(async move { outbox.held_count(account_id).await })
                .await
            {
                Ok(held) => held,
                Err(err) => {
                    return this.toast(&with_reason(
                        &gettext("Could not check for messages the server holds to send later, so {account} stays: {reason}"),
                        &err,
                        &[("account", &account.email)],
                    ));
                }
            };
            let mut body = crate::offered::remove_account_body(&account, remove);
            if let Some(line) = crate::offered::held_back_line(held) {
                body = format!("{body}\n\n{line}");
            }
            let title = fill(
                &gettext("Remove {account}?"),
                &[("account", &account.email)],
            );
            let question = confirm(&title, &body, &gettext("Remove"), Tone::Destructive);
            if !question.ask(&this.window).await {
                return;
            }
            if held > 0 {
                let outbox = this.core.outbox();
                let left = this
                    .core
                    .call(async move { outbox.call_back_account(account_id).await })
                    .await
                    .unwrap_or(held);
                this.scheduled_changed();
                if left > 0 {
                    let question = confirm(
                        &title,
                        &crate::offered::still_held_line(left),
                        &gettext("Remove Anyway"),
                        Tone::Destructive,
                    );
                    if !question.ask(&this.window).await {
                        return;
                    }
                }
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
                && win.runs_here("remind-at")
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
            if !win.runs_here("flag-color") {
                return;
            }
            win.flag(name.parse().ok());
        });
        self.actions.add_action(&flag_color);
        let go = gio::SimpleAction::new("go-mailbox", Some(glib::VariantTy::INT32));
        let weak = Rc::downgrade(self);
        go.connect_activate(move |_, parameter| {
            if let (Some(win), Some(position)) =
                (weak.upgrade(), parameter.and_then(|p| p.get::<i32>()))
            {
                // A mailbox opens in the mail, whichever space showed.
                win.show_space(crate::settings::Space::Mail);
                win.go_to_mailbox(position as usize);
            }
        });
        self.actions.add_action(&go);

        let with_account = |name: &str, run: AccountAction| {
            let action = gio::SimpleAction::new(name, Some(glib::VariantTy::INT64));
            let weak = Rc::downgrade(self);
            let gate = name.to_string();
            action.connect_activate(move |_, parameter| {
                let (Some(win), Some(id)) =
                    (weak.upgrade(), parameter.and_then(|p| p.get::<i64>()))
                else {
                    return;
                };
                // One action serves every account's menu, so it turns away
                // an account that lacks what the action opens.
                if !crate::offered::account_action_on(&gate, win.offers(id)) {
                    return;
                }
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
            Box::new(|win, account| win.sign_in_again(account)),
        );
        with_account(
            "account-rules",
            Box::new(|win, account| win.show_rules(account)),
        );
        with_account(
            "account-switch-jmap",
            Box::new(|win, account| win.switch_to_jmap(account)),
        );
        with_account(
            "account-not-downloading",
            Box::new(|win, account| win.show_not_downloading(account)),
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
            ("label-new-inside", |win, account, label| {
                win.new_label_inside(account, label)
            }),
            ("label-up", |win, account, label| {
                win.move_label(account, label, -1)
            }),
            ("label-down", |win, account, label| {
                win.move_label(account, label, 1)
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
                win.dock_assistant(false);
                win.conversation.stop_rendering();
                win.previews.forget_decrypted();
                // A held calendar change whose Undo toast is still up
                // must not vanish with the window: nothing else would
                // queue it (AGENTS.md "Late answers", carried from Task 4).
                win.calendar.commit_all_now();
                app.forget_window(&win);
            }
            glib::Propagation::Proceed
        });
    }

    fn install_menu(&self) {
        let menu = gio::Menu::new();
        // Shown only while the calendar is: the action is off in Mail.
        let calendar = gio::Menu::new();
        let refresh = gio::MenuItem::new(Some(&gettext("Refresh")), Some("win.refresh-calendar"));
        refresh.set_attribute_value("hidden-when", Some(&"action-disabled".to_variant()));
        calendar.append_item(&refresh);
        let declined = gio::MenuItem::new(
            Some(&gettext("Show Declined Events")),
            Some("win.show-declined-events"),
        );
        declined.set_attribute_value("hidden-when", Some(&"action-disabled".to_variant()));
        calendar.append_item(&declined);
        menu.append_section(None, &calendar);
        let first = gio::Menu::new();
        // The header in both spaces carries a button for this now (R12);
        // the menu item stays too, so its key shows here as well.
        let assistant = gio::MenuItem::new(Some(&gettext("Assistant")), Some("win.assistant"));
        assistant.set_attribute_value(
            "accel",
            Some(&crate::ui::primary_accelerator("<Primary>j").to_variant()),
        );
        first.append_item(&assistant);
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
        second.append(Some(&gettext("Donate")), Some("win.donate"));
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

    /// Moves the same pane, keeping its chat and any approval in place.
    fn detach_assistant(self: &Rc<Self>) {
        self.assistant_split.set_show_sidebar(false);
        self.assistant_split.set_sidebar(None::<&gtk::Widget>);
        if let Some(bar) = &self.assistant_top_bar {
            self.toolbar_heights.remove_widget(bar);
        }
        self.assistant.page.remove_css_class("assistant-docked");
        self.assistant
            .page
            .set_top_bar_style(adw::ToolbarStyle::Flat);
        let window = adw::Window::builder()
            .title(gettext("Assistant"))
            .default_width(440)
            .default_height(700)
            .content(&self.assistant.page)
            .build();
        window.set_application(self.window.application().as_ref());
        track_dark_class(&window);
        self.assistant.header.set_show_end_title_buttons(true);
        self.assistant.close.set_visible(false);
        self.assistant.detach.set_icon_name("view-restore-symbolic");
        self.assistant
            .detach
            .set_tooltip_text(Some(&gettext("Dock")));
        super::name(&self.assistant.detach, &gettext("Dock"));
        let weak = Rc::downgrade(self);
        window.connect_close_request(move |_| {
            if let Some(win) = weak.upgrade() {
                win.dock_assistant(false);
            }
            glib::Propagation::Stop
        });
        *self.assistant_window.borrow_mut() = Some(window.clone());
        self.sync_assistant_toggle();
        window.present();
        self.assistant.focus();
    }

    fn dock_assistant(&self, show: bool) {
        let Some(window) = self.assistant_window.borrow_mut().take() else {
            return;
        };
        window.set_content(None::<&gtk::Widget>);
        self.assistant.header.set_show_end_title_buttons(true);
        self.assistant_split.set_sidebar(Some(&self.assistant.page));
        if let Some(bar) = &self.assistant_top_bar {
            self.toolbar_heights.add_widget(bar);
        }
        self.assistant.page.add_css_class("assistant-docked");
        self.assistant
            .page
            .set_top_bar_style(adw::ToolbarStyle::Flat);
        self.assistant
            .close
            .set_visible(self.assistant_split.is_collapsed());
        self.assistant.detach.set_icon_name("window-new-symbolic");
        self.assistant
            .detach
            .set_tooltip_text(Some(&gettext("Detach")));
        super::name(&self.assistant.detach, &gettext("Detach"));
        self.assistant_split.set_show_sidebar(show);
        window.destroy();
        self.sync_assistant_toggle();
        if show {
            self.assistant.focus();
        }
    }

    /// Shows the assistant beside the mail and puts the cursor in it, or
    /// hides it again.
    fn toggle_assistant(&self) {
        let detached = self.assistant_window.borrow().clone();
        if let Some(window) = detached {
            window.present();
            self.assistant.focus();
            return;
        }
        let show = !self.assistant_split.shows_sidebar();
        self.assistant_split.set_show_sidebar(show);
        if show {
            self.assistant.focus();
        }
    }

    /// Opens the assistant beside the mail if it is closed, and asks it to
    /// summarize the conversation on screen, as the suggestion in an empty
    /// chat does.
    fn summarize(self: &Rc<Self>, view: &Rc<ConversationView>) {
        if view.detached() {
            return;
        }
        let detached = self.assistant_window.borrow().clone();
        if let Some(window) = detached {
            window.present();
        } else {
            self.assistant_split.set_show_sidebar(true);
        }
        self.assistant.ask(gettext("Summarize this conversation"));
    }

    /// Offers Summarize above the conversation while the assistant has a
    /// model to answer with.
    fn offer_summary(&self) {
        let offered = self.settings_with(|s| crate::assistant::offers_summary(&s.ai));
        self.conversation.offer_summary(offered);
    }

    /// Shows or hides the assistant toggle in the reading pane's and the
    /// calendar's headers, and keeps it pressed with the panel (R12).
    fn sync_assistant_toggle(&self) {
        let panel_open =
            self.assistant_split.shows_sidebar() || self.assistant_window.borrow().is_some();
        let docked =
            self.assistant_split.shows_sidebar() && self.assistant_window.borrow().is_none();
        self.conversation.set_assistant_docked(docked);
        self.calendar.set_assistant_docked(docked);
        let state = self.settings_with(|s| crate::assistant::assistant_toggle(&s.ai, panel_open));
        for button in [
            &self.conversation.assistant_toggle,
            &self.calendar.assistant_toggle,
        ] {
            button.set_visible(state.visible);
            button.set_active(state.pressed);
        }
    }

    /// True when the focus is in the message itself, where Ctrl+A selects text.
    fn reading_text(&self) -> bool {
        GtkWindowExt::focus(&self.window).is_some_and(|focus| crate::web::holds_focus(&focus))
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
            // Calendar accounts alone have no mail to write from.
            let lacking = match self.accounts().is_empty() {
                true => gettext("Add an account first"),
                false => gettext("Add a mail account first"),
            };
            self.toast(&lacking);
        }
    }

    /// Opens a thread from outside the window, such as a notification, and
    /// answers it when `then` asks for that.
    pub fn reveal(self: &Rc<Self>, account_id: AccountId, thread_id: String, then: Reveal) {
        // Selecting a row before the list's rows land finds nothing, so
        // the feed holds the thread until the first page is on screen.
        self.listing.reveal((account_id, thread_id, then));
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
    /// position in the open thread, `MAILRS_DEMO_OFFLINE` shows the
    /// calendar's offline line as if a sync just failed,
    /// `MAILRS_DEMO_ADD_ACCOUNT` opens Add Account on one of its stages
    /// (see `ui::add_account::preview`), `MAILRS_DEMO_FIRST_RUN` shows the
    /// first-run page over the sample accounts, and
    /// `MAILRS_DEMO_ACTION` activates a window action such as
    /// `shortcuts`, or one with a target such as `account-rules(int64
    /// 1)`. With a thread to open, the action waits for it, so
    /// `toggle-vip` has a sender to add.
    pub fn run_demo_script(self: &Rc<Self>) {
        if !self.core.demo {
            return;
        }
        let this = Rc::clone(self);
        glib::timeout_add_local_once(std::time::Duration::from_millis(900), move || {
            if std::env::var_os("MAILRS_DEMO_OPEN").is_none() {
                this.run_demo_action();
            }
            if let Ok(stage) = std::env::var("MAILRS_DEMO_ADD_ACCOUNT") {
                this.present_add_account(Opening::Preview(stage));
            }
            if std::env::var_os("MAILRS_DEMO_OFFLINE").is_some() {
                this.calendar.synced(true);
                this.calendar.synced(false);
            }
            if let Ok(query) = std::env::var("MAILRS_DEMO_SEARCH") {
                this.list.open_search();
                this.list.search_entry.set_text(&query);
                this.search_entered(query);
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

    /// Switches to Mail and opens thread `thread_id` wherever it is filed,
    /// for the calendar's doors into an invitation's mail. An invitation
    /// the person archived, or one older than the list's first page, has
    /// no row in the list, so this opens the conversation the way the
    /// assistant does and leaves the list on the mailbox it showed. The
    /// row is selected only when the list holds it.
    fn open_invitation_mail(self: &Rc<Self>, account_id: AccountId, thread_id: String) {
        self.show_space(crate::settings::Space::Mail);
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let key = thread_id.clone();
            let found = this
                .core
                .read(move |c| mailrs_store::threads::get_thread(c, account_id, &key))
                .await;
            let summary = match found {
                Ok(Some(summary)) => summary,
                Ok(None) => ThreadSummary {
                    account_id,
                    id: thread_id.clone(),
                    message_count: 1,
                    ..ThreadSummary::default()
                },
                Err(err) => {
                    tracing::warn!(%err, "could not read the invitation's thread");
                    return;
                }
            };
            this.list.select(account_id, &thread_id, None);
            let listed = this
                .list
                .selected_rows()
                .iter()
                .any(|row| row.account_id == account_id && row.id == thread_id);
            if !listed {
                this.list.unselect();
            }
            this.open_thread(summary);
        });
    }

    /// Switches to the calendar and opens one occurrence's popover. A
    /// click on a reminder lands here, and so will Show in Calendar on
    /// an invitation. `start` is that occurrence's own start, not
    /// necessarily the series' first.
    pub(crate) fn show_event(
        &self,
        account_id: AccountId,
        calendar: &str,
        id: &str,
        start: EpochMillis,
    ) {
        let _ = WidgetExt::activate_action(&self.window, "win.show-calendar", None);
        self.calendar.open(account_id, calendar, id, start);
    }

    /// Opens Preferences, on the signature of `signature_of` when given.
    fn show_preferences_for(self: &Rc<Self>, signature_of: Option<String>) {
        self.show_preferences_focus(
            signature_of
                .as_deref()
                .map(super::preferences::Focus::Signature),
        );
    }

    fn show_preferences_focus(self: &Rc<Self>, focus: Option<super::preferences::Focus<'_>>) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        // One Preferences at a time. Asked for an account's signature
        // while it is open, it turns to the accounts rather than starting
        // again, which would drop what the person was in the middle of.
        if let Some(open) = self.open_preferences() {
            if focus.is_some() {
                open.set_visible_page_name("accounts");
            }
            return;
        }
        // Preferences puts each account on the pages that apply to it.
        let accounts = app.accounts();
        let weak = Rc::downgrade(self);
        let dialog = super::preferences::present(
            &app,
            &accounts,
            |id| self.offers(id),
            |id| self.withheld(id),
            move |id| {
                if let Some(win) = weak.upgrade() {
                    win.grant_access(id);
                }
            },
            &self.window,
            focus,
        );
        self.keep_preferences(dialog);
    }

    /// Preferences, when it is open.
    fn open_preferences(&self) -> Option<Rc<super::preferences::dialog::Dialog>> {
        self.preferences.borrow().clone()
    }

    /// Holds `dialog` as the open Preferences until it closes.
    fn keep_preferences(self: &Rc<Self>, dialog: Rc<super::preferences::dialog::Dialog>) {
        let weak = Rc::downgrade(self);
        dialog.window.connect_closed(move |closed| {
            let Some(win) = weak.upgrade() else { return };
            let ours = win
                .preferences
                .borrow()
                .as_ref()
                .is_some_and(|open| &open.window == closed);
            if ours {
                win.preferences.replace(None);
            }
        });
        self.preferences.replace(Some(dialog));
    }

    fn show_rules(self: &Rc<Self>, account: Account) {
        let labels = self.labels_of(account.id);
        let (grant, email) = (Rc::downgrade(self), account.email.clone());
        let filing = crate::offered::Filing::of([self.offers(account.id)]);
        super::rules::present(
            &self.core,
            &account,
            labels,
            filing,
            &self.window,
            move || {
                if let Some(win) = grant.upgrade() {
                    win.grant(email.clone());
                }
            },
        );
    }

    /// Opens Preferences on one page, such as "assistant".
    fn show_preferences_page(self: &Rc<Self>, page: &str) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        if let Some(open) = self.open_preferences() {
            open.set_visible_page_name(page);
            return;
        }
        let accounts = app.accounts();
        let weak = Rc::downgrade(self);
        let dialog = super::preferences::present_page(
            &app,
            &accounts,
            |id| self.offers(id),
            |id| self.withheld(id),
            move |id| {
                if let Some(win) = weak.upgrade() {
                    win.grant_access(id);
                }
            },
            &self.window,
            page,
        );
        self.keep_preferences(dialog);
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
                    win.grant(email.clone());
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
            Effect::ListShape => self.listing.change(|screen| Some(screen.list_shape())),
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
                self.listing
                    .change(|screen| screen.smart_saved(&settings.smart_mailboxes));
            }
            Effect::Vips => {
                for view in self.views() {
                    view.set_sender_vip(sender_is_vip(&view, &settings));
                }
            }
            Effect::FollowUps => {
                if !settings.suggest_follow_ups {
                    self.listing.change(OnScreen::follow_ups_off);
                }
                self.refresh_counts();
            }
            Effect::Categories => {
                self.listing
                    .change(|screen| Some(screen.categories_changed()));
            }
            Effect::Assistant => {
                self.assistant.refresh();
                self.offer_summary();
                self.sync_assistant_toggle();
            }
            Effect::TextSize => {
                for view in self.views() {
                    view.set_zoom(settings.text_size.zoom());
                }
            }
            Effect::Contacts => {
                self.list.photos_changed();
                self.reopen_for_photos();
            }
            // The app follows the light or dark choice; no window to redraw.
            Effect::Theme => {}
            Effect::Language => self.offer_restart(),
            Effect::Calendar => self.calendar.week_start_changed(),
            Effect::Translation => {
                for view in self.views() {
                    self.thread_run(&view).offer_translation_again();
                }
            }
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

    /// Shows the Keyboard Shortcuts window. Its label line says Move to
    /// folder when every account files mail in folders.
    fn show_shortcuts(&self) {
        let offers: Vec<Offers> = self
            .mail_accounts()
            .iter()
            .map(|a| self.offers(a.id))
            .collect();
        shortcuts::dialog(Filing::of(offers)).present(Some(&self.window));
    }

    /// Opens Switch to JMAP for `account`, when a look found its session.
    fn switch_to_jmap(self: &Rc<Self>, account: Account) {
        let Some(found) = self.core.offer_of(&account) else {
            return;
        };
        let win = Rc::clone(self);
        let present = async move {
            // The calendar line shows when the account's calendars sign in
            // with the password the token replaces for mail.
            let id = account.id;
            let calendars = win
                .core
                .read(move |c| mailrs_store::services::load(c, id))
                .await
                .is_ok_and(|found| {
                    found.iter().any(|f| {
                        f.confirmed && f.kind == mailrs_store::services::ServiceKind::CalDav
                    })
                });
            let done = Rc::clone(&win);
            crate::ui::switch_jmap::present(
                &win.window,
                Rc::clone(&win.core),
                account,
                found,
                calendars,
                move |switched| {
                    done.switched_to_jmap(switched);
                },
            );
        };
        glib::spawn_future_local(present);
    }

    /// Says the switch went through, with a way to the Rules when it
    /// turned some off, and reads the accounts again.
    fn switched_to_jmap(self: &Rc<Self>, switched: mailrs_sync::switch::Switched) {
        let line = crate::offered::switched_line(&switched.account.email, switched.rules_off);
        let toast = crate::ui::toast(&line);
        toast.set_timeout(if switched.rules_off > 0 { 10 } else { 4 });
        if switched.rules_off > 0 {
            toast.set_button_label(Some(&gettext("Show Rules")));
            toast.set_action_name(Some("win.account-rules"));
            toast.set_action_target_value(Some(&switched.account.id.to_variant()));
        }
        self.toasts.add_toast(toast);
        self.refresh_accounts(Reload::Yes);
    }

    /// The messages a POP3 account's server would not hand over after
    /// three tries, each with the server's words.
    fn show_not_downloading(self: &Rc<Self>, account: Account) {
        let win = Rc::clone(self);
        glib::spawn_future_local(async move {
            let id = account.id;
            let failing = match win
                .core
                .read(move |c| mailrs_store::pop3::failing(c, id))
                .await
            {
                Ok(failing) => failing,
                Err(err) => {
                    return tracing::warn!(account = id, %err, "could not read the messages that will not download");
                }
            };
            let lines = crate::offered::failing_lines(&failing).join("\n");
            let dialog = adw::AlertDialog::builder()
                .heading(gettext("Messages That Will Not Download"))
                .body(fill(
                    &gettext("Penguin Mail could not download these from {address}'s server. It tries again at each check, except for a message too large to download."),
                    &[("address", &account.email)],
                ))
                .build();
            // The body does not scroll, so a long list goes in its own
            // scrolled area and the dialog stops growing at its height.
            let list = gtk::Label::builder()
                .label(&lines)
                .wrap(true)
                .wrap_mode(gtk::pango::WrapMode::WordChar)
                .xalign(0.0)
                .halign(gtk::Align::Center)
                .build();
            let scroller = gtk::ScrolledWindow::builder()
                .child(&list)
                .hscrollbar_policy(gtk::PolicyType::Never)
                .propagate_natural_height(true)
                .max_content_height(220)
                .build();
            // The stylesheet evens out the gaps libadwaita leaves above
            // and below the list.
            scroller.add_css_class("failing-list");
            dialog.set_extra_child(Some(&scroller));
            dialog.add_responses(&[("close", &gettext("Close"))]);
            dialog.set_close_response("close");
            dialog.present(Some(&win.window));
        });
    }

    fn show_about(self: &Rc<Self>) {
        // One About at a time: a second press finds it open already.
        if self.about.borrow().is_some() {
            return;
        }
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let kept_here = crate::offered::kept_here_lines(&self.accounts());
        let about =
            crate::ui::about::About::new(app.can_update(), &self.core.store_path, &kept_here);
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

    fn act(
        &self,
        targets: Vec<Target>,
        action: MailAction,
        history: History,
        words: Option<String>,
        from: MovedFrom,
    ) {
        // A colour picked here becomes the one the flag button uses next.
        if let MailAction::Flag(Some(color)) = action
            && let Some(app) = self.window.app.upgrade()
        {
            app.change_settings(Change::FlagColor(color));
        }
        self.window
            .perform_from(targets, action, history, words, from);
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
        self.0
            .conversation
            .read(OpenThread::quotable)
            .unwrap_or(false)
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

/// The main window as the app's unread counts see it: the mailboxes its
/// sidebar shows, and where each count goes.
struct ShownCounts(Weak<MainWindow>);

impl crate::unread::Shows for ShownCounts {
    fn wanted(&self) -> Option<crate::unread::Wanted> {
        let window = self.0.upgrade()?;
        Some(crate::unread::Wanted {
            mailboxes: window.sidebar.mailboxes(),
            shown: window.shown(),
            view: window.view(),
        })
    }

    fn show(&self, counted: &Counted) {
        if let Some(window) = self.0.upgrade() {
            window.show_counts(counted);
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

/// The account and mailbox id to keep in step once `mailbox` lands on
/// screen: a folder on an account whose mail sits in one mailbox at a
/// time, since its window otherwise fills only as far as a listing has
/// asked. A label account keeps every label in step already, and no
/// other kind of mailbox names a server mailbox to follow, so both give
/// `None`.
fn follows(mailbox: &Mailbox, offers: Offers) -> Option<(AccountId, String)> {
    match mailbox {
        Mailbox::Label {
            account_id,
            label_id,
            ..
        } if !offers.labels => Some((*account_id, label_id.clone())),
        _ => None,
    }
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
        } => data
            .iter()
            .any(|(a, labels)| a.id == *account_id && labels.iter().any(|l| &l.id == label_id)),
        _ => mailbox
            .account()
            .is_none_or(|id| data.iter().any(|(a, _)| a.id == id)),
    }
}

/// Wraps the assistant's split view in a [`room::Room`] that folds the
/// mailboxes and lays the assistant over the space as the window's width
/// requires, from what `needs` measures.
fn arranged_room(
    split: &adw::OverlaySplitView,
    assistant_split: &adw::OverlaySplitView,
    columns: &adw::NavigationSplitView,
    needs: impl Fn() -> room::Needs + 'static,
) -> room::Room {
    let open = assistant_split.downgrade();
    let decide = move |width: i32| {
        let panel_open = open.upgrade().is_some_and(|s| s.shows_sidebar());
        room::arrange(width, panel_open, needs())
    };
    let (split_weak, assistant_weak) = (split.downgrade(), assistant_split.downgrade());
    let columns = columns.downgrade();
    let apply = move |arranged: room::Arrangement| {
        if let Some(columns) = columns.upgrade() {
            columns.set_collapsed(arranged.columns_fold);
        }
        if let Some(split) = split_weak.upgrade()
            && split.is_collapsed() != arranged.mailboxes_fold
        {
            split.set_collapsed(arranged.mailboxes_fold);
            split.set_show_sidebar(!arranged.mailboxes_fold);
        }
        if let Some(assistant) = assistant_weak.upgrade() {
            let width = f64::from(arranged.panel_width);
            assistant.set_max_sidebar_width(width.max(assistant.min_sidebar_width()));
            assistant.set_min_sidebar_width(width);
            assistant.set_max_sidebar_width(width);
            assistant.set_collapsed(arranged.panel_overlays);
        }
    };
    room::Room::new(assistant_split, decide, apply)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_on_a_folder_account_is_followed_when_opened() {
        let work = Mailbox::Label {
            account_id: 7,
            label_id: "Work".into(),
            name: "Work".into(),
        };
        assert_eq!(
            follows(
                &work,
                Offers {
                    labels: false,
                    ..Offers::EVERYTHING
                }
            ),
            Some((7, "Work".to_string()))
        );
    }

    #[test]
    fn a_label_on_a_gmail_account_is_not_followed() {
        let work = Mailbox::Label {
            account_id: 7,
            label_id: "Label_1".into(),
            name: "Work".into(),
        };
        assert_eq!(follows(&work, Offers::EVERYTHING), None);
    }

    #[test]
    fn a_mailbox_that_names_no_server_mailbox_is_never_followed() {
        let offers = Offers {
            labels: false,
            ..Offers::EVERYTHING
        };
        let standard = Mailbox::Standard {
            account_id: 1,
            which: super::super::Standard::Inbox,
        };
        let unified = Mailbox::Unified(super::super::Standard::Inbox);
        let smart = Mailbox::Smart(mailrs_domain::SmartMailbox {
            id: "smart-1".into(),
            name: "Big mail".into(),
            account: None,
            match_all: true,
            conditions: Vec::new(),
        });
        for mailbox in [&standard, &unified, &smart] {
            assert_eq!(follows(mailbox, offers), None, "{mailbox:?}");
        }
    }

    #[test]
    fn a_mailbox_of_a_removed_account_is_gone() {
        let account = |id| Account {
            id,
            email: format!("{id}@example.com"),
            state: AccountState::Ok,
            provider: mailrs_domain::Provider::Gmail,
            provider_name: None,
        };
        let label = |account_id| Label {
            role: None,
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
        assert!(
            !still_there(&work, &unlisted),
            "a label the account dropped"
        );
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
    fn the_archive_line_names_the_folder_and_the_provider() {
        assert_eq!(
            archive_made_line("Fastmail", "Archive"),
            "Made a folder called “Archive” on Fastmail for archived mail"
        );
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
