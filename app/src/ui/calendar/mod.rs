//! The calendar page. Split so every decision that does not need a
//! widget lives in a plain module with its own tests, and a widget only
//! places what that module worked out.
//!
//! [`CalendarView`] is the page itself: a header with the range's title,
//! Today, the arrows and the view switch; a card holding the grids; and
//! the sidebar content the window swaps in for the mailbox list. The
//! grids read the local copy through [`Core::read`] and hold only the
//! ranges they show: the one on screen and one either side, which an
//! `adw::Carousel` slides between, so a swipe follows the fingers and
//! settles on a neighbour.

pub mod agenda;
pub mod attachments;
pub mod block;
pub mod draft;
pub mod drag;
pub mod editor;
pub mod header;
pub mod headings;
pub mod holding;
pub mod holidays;
pub mod kinds;
pub mod layout;
mod manage;
pub mod month;
pub mod next;
pub(crate) mod pager;
pub mod popover;
pub mod quick;
pub mod range;
pub mod scope;
pub mod shown;
pub mod sidebar;
pub mod time_grid;
pub mod tint;
pub mod words;

use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::rc::{Rc, Weak};
use std::sync::Arc;

use adw::prelude::*;
use chrono::{DateTime, Datelike, Days, NaiveDate, Utc};
use gtk::{gdk, gio, glib};
use mailrs_domain::calendar::series::{self, RepeatScope};
use mailrs_domain::calendar::{Access, Calendar, Guest, Occurrence};
use mailrs_domain::invitation::Answer;
use mailrs_domain::translate::{date_locale, fill, gettext, with_reason};
use mailrs_domain::{Account, AccountId, EpochMillis};
use mailrs_store::calendar::{self as store, CalendarScope};
use mailrs_sync::Permitted;
use mailrs_sync::calendar_copy::Held;
use mailrs_sync::{Offers, Withheld};

use crate::core::Core;
use crate::settings::{Change, Settings};
use crate::ui::autocomplete::Contacts;
use agenda::Agenda;
use block::{EventKey, key_of};
use draft::Draft;
use header::{Extra, HeaderRoom};
use holding::Holding;
use month::MonthGrid;
use popover::EventPopover;
use quick::Quick;
use range::{Range, ViewKind};
use shown::{Refocus, Showing};
use sidebar::{CalendarSidebar, ListChange};
use time_grid::{AllDayStrip, GUTTER, TimeGrid};

/// The most results a search lists.
const SEARCH_LIMIT: usize = 50;

/// What `occurrences` returns at most, so a read that comes back this
/// full is known to have been cut (`store::calendar`'s `MOST_EVENTS`).
const MOST_EVENTS: usize = 500;

/// The window's side of the view: what the view cannot do on its own.
pub struct Hooks {
    /// Says something in a toast.
    pub toast: Box<dyn Fn(&str)>,
    /// Saves a settings change.
    pub change: Box<dyn Fn(Change)>,
    /// Asks the account for the permissions it left out.
    pub grant: Box<dyn Fn(AccountId)>,
    /// Explains that an answer stopped for want of the calendar
    /// permission and offers to ask for it, as every other refusal of
    /// that kind does before a browser opens.
    pub needs_permission: Box<dyn Fn(AccountId)>,
    /// Shows a toast with a button, such as Undo.
    pub add_toast: Box<dyn Fn(adw::Toast)>,
    /// The suggestions the guests field completes from.
    pub contacts: Box<dyn Fn() -> Contacts>,
    /// Sends the account's queued calendar changes now, then reloads.
    pub push: Box<dyn Fn(AccountId)>,
    /// Explains that making, renaming or deleting a calendar, or
    /// changing the calendar list, needs a permission the account
    /// withheld, and offers to ask for it.
    pub needs_manage_permission: Box<dyn Fn(AccountId)>,
    /// Explains that attaching a file from this computer needs Drive,
    /// which the account withheld, and offers to ask for it.
    pub needs_drive_permission: Box<dyn Fn(AccountId)>,
    /// Opens the mail that carries an invitation, switching away from the
    /// calendar to it: the "Waiting for your answer" card's "Open mail"
    /// door and the event popover's "Open the invitation in Mail" link
    /// both call this.
    pub open_mail: Box<dyn Fn(AccountId, String)>,
    /// Starts a calendar sync for every account now, the same pass the
    /// timer runs every 15 seconds. The Refresh action's own trigger.
    pub refresh: Box<dyn Fn()>,
    /// Asks the organizer of an event the account is a guest of for
    /// another time, through the invitation card's own Propose New Time.
    pub propose: Box<dyn Fn(AccountId, Occurrence)>,
    /// Redraws the next-event card at the foot of the mail sidebar, after
    /// a calendar's colour or whether it shows changed.
    pub next_event: Box<dyn Fn()>,
    /// Hears how many invitations "Waiting for your answer" lists, each
    /// time it is read again, for the Calendar toggle's badge.
    pub waiting: Box<dyn Fn(usize)>,
}


/// An account as the calendar reads it: who it is, what its provider
/// offers, and what its consent withheld.
pub type CalendarAccount = (Account, Offers, Withheld);

type Calendars = HashMap<(AccountId, String), Calendar>;

/// One range's grid, as a page of the carousel.
enum PageView {
    Grid(GridPage),
    Month(Rc<MonthGrid>),
}

/// A day or week: the day headings and the all-day row above a scrolled
/// 24-hour grid.
struct GridPage {
    root: gtk::Box,
    headings: headings::DayHeadings,
    strip: AllDayStrip,
    scroller: gtk::ScrolledWindow,
    /// Counts the scrolls asked of `scroller`. A scroll waiting for the
    /// grid's first layout, asked while the page was hidden, must not
    /// land after a later one, such as Show in Calendar's.
    scroll_asked: Rc<Cell<u64>>,
    grid: TimeGrid,
}

/// One page of the carousel and the range it shows.
struct Page {
    holder: adw::Bin,
    range: Cell<Range>,
    view: RefCell<PageView>,
    /// The read this page waits for; an answer from an older one is
    /// dropped, since the page has moved on to another range.
    generation: Cell<u64>,
    /// Whether the grid has been scrolled to the range's first hour, so
    /// a reload of the same range leaves the scroll where the person put
    /// it.
    scrolled: Cell<bool>,
}

pub struct CalendarView {
    /// The page the window puts beside the sidebar.
    pub page: adw::ToolbarView,
    /// What the sidebar shows while the calendar is on screen.
    pub sidebar: gtk::Box,
    /// Shows the sidebar when the window is too narrow to keep it open.
    pub sidebar_button: gtk::ToggleButton,
    /// Opens or closes the assistant beside the mail (R12). The window
    /// binds it to the assistant panel's own toggle path.
    pub assistant_toggle: gtk::ToggleButton,
    core: Rc<Core>,
    settings: Box<dyn Fn() -> Settings>,
    hooks: Hooks,
    calendar_sidebar: Rc<CalendarSidebar>,
    title_bold: gtk::Label,
    title_dim: gtk::Label,
    title_week: gtk::Label,
    today_button: gtk::Button,
    arrows: gtk::Box,
    previous: gtk::Button,
    next: gtk::Button,
    switch: adw::ToggleGroup,
    /// The switch folded into one button, for a header without room for
    /// its toggles: it names the view on screen and lists the others.
    view_menu: gtk::MenuButton,
    /// The view the drop-down marks, as the name of its toggle.
    view_action: gio::SimpleAction,
    /// Holds the switch and the drop-down in the header, one at a time.
    switch_slot: gtk::Box,
    /// Holds the header bar and decides which of its extras fit.
    header_room: HeaderRoom,
    /// The bar under a narrow window's view: Today, the arrows and the
    /// switch.
    bottom_row: gtk::Box,
    header: adw::HeaderBar,
    /// The orange "+" button: the editor on a new event at the slot.
    /// Hidden while no calendar takes new events.
    new_event: gtk::Button,
    search_bar: gtk::SearchBar,
    search_entry: gtk::SearchEntry,
    views: gtk::Stack,
    /// The quiet pill over the bottom of the grid or list that says older
    /// events are loading, or cannot load while offline.
    older_note: gtk::Box,
    older_spinner: gtk::Spinner,
    older_label: gtk::Label,
    /// Counts the asks for older events, so an answer that comes back
    /// after the person moved on leaves the note to the newer ask.
    older_read: Cell<u64>,
    carousel: adw::Carousel,
    pages: RefCell<Vec<Rc<Page>>>,
    list: Rc<Agenda>,
    results: Rc<Agenda>,
    popover: Rc<EventPopover>,
    /// When the editor last opened. The release of a double click's
    /// second press still fires the block's own click, which would open
    /// the popover again beside the editor; a click within the double
    /// click time of this is that release.
    editor_opened: Cell<Option<std::time::Instant>>,
    more: gtk::Popover,
    more_list: Rc<Agenda>,
    /// The popover N and a press on empty time open: the time, a title
    /// field, the calendar, and More Details for the editor.
    quick: Rc<Quick>,
    /// What the "N more" popover was opened from, for the event popover
    /// that replaces it.
    more_anchor: RefCell<Option<gtk::Widget>>,
    /// Watches GNOME's `clock-format` for as long as the page lives;
    /// `None` where its schema is not installed.
    clock_watch: RefCell<Option<gtk::gio::Settings>>,
    /// The day the view is on; every range is the one around it.
    day: Cell<NaiveDate>,
    kind: Cell<ViewKind>,
    /// The week or month the person had before Day, which List stands
    /// for in a narrow window.
    before_day: Cell<ViewKind>,
    narrow: Cell<bool>,
    accounts: RefCell<Vec<CalendarAccount>>,
    calendars: RefCell<Calendars>,
    /// The calendars the person took off the sidebar's list, by account
    /// and id, which the pickers for a new event leave out.
    hidden: RefCell<HashSet<(AccountId, String)>>,
    /// Counts every read, so each can tell whether a newer one replaced
    /// it: an answer can come back after the person moved on.
    reads: Cell<u64>,
    sidebar_read: Cell<u64>,
    /// The latest "Waiting for your answer" read, counted apart from the
    /// sidebar's so neither drops the other's answer.
    waiting_read: Cell<u64>,
    list_read: Cell<u64>,
    /// The earliest day the narrow list already holds. `load_earlier`
    /// reads back from here and moves it once the read comes back.
    list_first: Cell<NaiveDate>,
    /// Set once `load_earlier` has read down to `range::earliest_agenda_day`,
    /// so a further scroll to the top asks nothing more.
    list_exhausted: Cell<bool>,
    /// Set while an earlier-days read is in flight, so a second scroll
    /// to the top before it answers does not start another one.
    list_loading: Cell<bool>,
    /// The latest day the list already holds, `None` until its first
    /// read answers. `load_later` reads on from here.
    list_last: Cell<Option<NaiveDate>>,
    /// Set while a later-days read is in flight.
    list_later_loading: Cell<bool>,
    search_read: Cell<u64>,
    /// Set by a sidebar read that should fill the pages once it answers.
    /// A newer read drops the older one's answer, so the flag carries
    /// the fill over to whichever read answers last.
    fill_owed: Cell<bool>,
    /// Counts `open`'s reads, so a newer one, or a move to another range,
    /// drops an older answer.
    open_read: Cell<u64>,
    /// An occurrence to open once the page that holds it has been read:
    /// the event and the occurrence's own start, since every occurrence
    /// of an unsplit series shares one row and one id.
    pending_open: RefCell<Option<(EventKey, EpochMillis)>>,
    /// Set while the view changes its own switch, so the switch's signal
    /// does not echo the change back.
    switching: Cell<bool>,
    /// Set while the view adds, removes or reorders carousel pages
    /// itself: the carousel reports a page change for those too, and
    /// only a swipe or an arrow's slide moves the day.
    arranging: Cell<bool>,
    /// Set when the view took away the page that held the keyboard
    /// focus, so the page that replaces it takes the focus once its
    /// events arrive.
    refocus_owed: Cell<bool>,
    /// The one change waiting on its Undo toast, if any.
    holding: RefCell<Holding<Held>>,
    /// The toast that change's Undo is on, so a new one can dismiss it
    /// and its own watcher can tell it apart from a later toast.
    toast_up: RefCell<Option<adw::Toast>>,
    /// Set while a Refresh press has a calendar sync started, so a
    /// second press before it answers does not start another one.
    refreshing: Cell<bool>,
    /// Whether the last calendar sync attempt came back with an error,
    /// for the offline line under the mini month.
    sync_failed: Cell<bool>,
    /// The clock time of the last calendar sync that succeeded, for the
    /// offline line's own "last updated" time. `None` before the first
    /// one this run.
    last_synced: Cell<Option<chrono::NaiveTime>>,
}

/// What an ask for older events came to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Older {
    /// The copy already reaches back far enough, or the ask failed in a
    /// way the person cannot act on; the view has what there is.
    Held,
    /// A fetch ran, so the view may lack events it has not drawn yet.
    Loaded,
    /// A fetch was due and the computer has no network.
    Offline,
}

/// Whether pressing Refresh should start a calendar sync now: never
/// while one it started is still running, so two presses in a row, or a
/// press while the timer's own pass is in flight, do not queue a second
/// one.
fn should_refresh(already_refreshing: bool) -> bool {
    !already_refreshing
}

impl CalendarView {
    pub fn new(
        core: Rc<Core>,
        settings: impl Fn() -> Settings + 'static,
        hooks: Hooks,
    ) -> Rc<CalendarView> {
        let kind = settings().calendar_view;
        let today = chrono::Local::now().date_naive();

        let title_bold = gtk::Label::builder()
            .css_classes(["bold"])
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        let title_dim = gtk::Label::builder().css_classes(["dim"]).build();
        let title_week = gtk::Label::builder()
            .css_classes(["week"])
            .valign(gtk::Align::Baseline)
            .build();
        let title = gtk::Box::builder()
            .spacing(8)
            .css_classes(["range-title"])
            .valign(gtk::Align::Center)
            .build();
        title.append(&title_bold);
        title.append(&title_dim);
        title.append(&title_week);

        let today_button = gtk::Button::builder()
            .label(gettext("Today"))
            .css_classes(["calendar-today"])
            .valign(gtk::Align::Center)
            .build();
        let previous = gtk::Button::builder()
            .icon_name("go-previous-symbolic")
            .css_classes(["flat"])
            .build();
        let next = gtk::Button::builder()
            .icon_name("go-next-symbolic")
            .css_classes(["flat"])
            .build();
        // One pill holding both arrows with a hairline between them, as
        // the mockup draws them.
        let arrows = gtk::Box::builder()
            .css_classes(["calendar-arrows"])
            .valign(gtk::Align::Center)
            .margin_start(4)
            .build();
        arrows.append(&previous);
        arrows.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        arrows.append(&next);

        let switch = adw::ToggleGroup::builder()
            .css_classes(["round", "view-switch"])
            .valign(gtk::Align::Center)
            .build();
        crate::ui::name(&switch, &gettext("View"));
        // The same choice as the switch, for a header too short for its
        // toggles. The action's state is the name of the toggle on
        // screen, so the menu marks it.
        let view_action = gio::SimpleAction::new_stateful(
            "view",
            Some(glib::VariantTy::STRING),
            &"week".to_variant(),
        );
        let view_actions = gio::SimpleActionGroup::new();
        view_actions.add_action(&view_action);
        let view_items = gio::Menu::new();
        for (name, label) in [
            ("day", gettext("Day")),
            ("week", gettext("Week")),
            ("month", gettext("Month")),
            ("agenda", gettext("Agenda")),
        ] {
            let item = gio::MenuItem::new(Some(&label), None);
            item.set_action_and_target_value(Some("calendar-view.view"), Some(&name.to_variant()));
            view_items.append_item(&item);
        }
        let view_menu = gtk::MenuButton::builder()
            .menu_model(&view_items)
            .always_show_arrow(true)
            .tooltip_text(gettext("View"))
            .css_classes(["view-menu"])
            .valign(gtk::Align::Center)
            .visible(false)
            .build();
        view_menu.insert_action_group("calendar-view", Some(&view_actions));
        let switch_slot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        switch_slot.append(&switch);
        switch_slot.append(&view_menu);

        let new_event = gtk::Button::builder()
            .icon_name("list-add-symbolic")
            .css_classes(["suggested-action", "circular", "new-event"])
            .valign(gtk::Align::Center)
            .build();
        crate::ui::name_with_shortcut(&new_event, &gettext("New Event (N)"));
        let search_button = gtk::ToggleButton::builder()
            .icon_name("system-search-symbolic")
            .tooltip_text(gettext("Search the Calendar (Ctrl+F)"))
            .build();
        crate::ui::name_with_shortcut(&search_button, &gettext("Search the Calendar (Ctrl+F)"));
        let sidebar_button = gtk::ToggleButton::builder()
            .icon_name("sidebar-show-symbolic")
            .tooltip_text(gettext("Show Calendars"))
            .visible(false)
            .build();
        crate::ui::name(&sidebar_button, &gettext("Show Calendars"));
        // The window binds this to the assistant panel's own open state
        // (R12); it starts hidden until the window says the assistant is
        // on.
        let assistant_toggle = gtk::ToggleButton::builder()
            .icon_name("penguin-mail-sparkle-symbolic")
            .tooltip_text(gettext("Assistant (Ctrl+J)"))
            .css_classes(["flat", "assistant-toggle"])
            .visible(false)
            .build();
        crate::ui::name_with_shortcut(&assistant_toggle, &gettext("Assistant (Ctrl+J)"));

        // No centred title: the range's title sits at the start, and
        // without a centre the bar's natural width is its two sides, the
        // width HeaderRoom weighs the view switch against.
        let header = adw::HeaderBar::builder()
            .show_title(false)
            .css_classes(["calendar-header"])
            .build();
        header.pack_start(&sidebar_button);
        header.pack_start(&title);
        header.pack_start(&today_button);
        header.pack_start(&arrows);
        // The assistant toggle sits at the header's outer right edge.
        header.pack_end(&assistant_toggle);
        header.pack_end(&search_button);
        header.pack_end(&new_event);
        header.pack_end(&switch_slot);
        // When the room runs short the header drops its extras, and then
        // folds the view switch into its drop-down, rather than lose New
        // Event, Search or the window's buttons. The title's parts sit
        // 8 px apart; the switch and the drop-down share one slot.
        let header_room = HeaderRoom::new(
            &header,
            [
                (title_week.clone().upcast(), 8, None),
                (title_dim.clone().upcast(), 8, None),
                (switch.clone().upcast(), 0, Some(view_menu.clone().upcast())),
            ],
        );

        let search_entry = gtk::SearchEntry::builder()
            .hexpand(true)
            .placeholder_text(gettext("Search the Calendar"))
            .build();
        crate::ui::name(&search_entry, &gettext("Search the Calendar"));
        let search_bar = gtk::SearchBar::builder()
            .child(
                &adw::Clamp::builder()
                    .maximum_size(480)
                    .child(&search_entry)
                    .build(),
            )
            .build();
        search_bar.connect_entry(&search_entry);
        search_button
            .bind_property("active", &search_bar, "search-mode-enabled")
            .bidirectional()
            .sync_create()
            .build();

        let carousel = adw::Carousel::builder()
            .allow_long_swipes(false)
            .allow_mouse_drag(false)
            .allow_scroll_wheel(false)
            .vexpand(true)
            .hexpand(true)
            .build();
        carousel.set_scroll_params(&adw::SpringParams::new(1.0, 1.0, 400.0));
        let list = Agenda::new();
        let results = Agenda::new();
        // Only the view on screen sets the width, so the list is not held
        // to the seven columns of a week it does not show.
        let views = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .transition_duration(150)
            .hhomogeneous(false)
            .build();
        for agenda in [&list, &results] {
            agenda.widget.set_margin_start(12);
            agenda.widget.set_margin_end(12);
        }
        views.add_named(&carousel, Some("grid"));
        views.add_named(&list.widget, Some("list"));
        views.add_named(&results.widget, Some("search"));

        let card = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .css_classes(["calendar-card"])
            .margin_start(10)
            .margin_end(10)
            // The header bar stands 3 px taller than the mockup's 48, so
            // the card keeps 5 px under it where the mockup keeps 8, and
            // its top edge lands at the mockup's 56.
            .margin_top(5)
            .margin_bottom(10)
            .overflow(gtk::Overflow::Hidden)
            .build();
        let older_spinner = gtk::Spinner::new();
        let older_label = gtk::Label::new(None);
        let older_note = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .css_classes(["older-note"])
            .halign(gtk::Align::Center)
            .valign(gtk::Align::End)
            .margin_bottom(14)
            .can_target(false)
            .visible(false)
            .build();
        older_note.append(&older_spinner);
        older_note.append(&older_label);
        crate::ui::name(&older_spinner, &gettext("Loading older events"));
        let over_views = gtk::Overlay::builder().child(&views).build();
        over_views.add_overlay(&older_note);
        card.append(&over_views);

        let bin = adw::Bin::builder()
            .child(&card)
            .width_request(300)
            .height_request(240)
            .build();

        let bottom_slot = adw::Bin::builder()
            .halign(gtk::Align::Center)
            .margin_top(6)
            .margin_bottom(6)
            .build();

        let page = adw::ToolbarView::new();
        page.add_top_bar(&header_room);
        page.add_top_bar(&search_bar);
        page.set_content(Some(&bin));
        let bottom_row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        bottom_slot.set_child(Some(&bottom_row));
        page.add_bottom_bar(&bottom_slot);
        page.set_reveal_bottom_bars(false);

        let popover = EventPopover::new(&card, &today_button);
        let more_list = Agenda::new();
        more_list.scrolled.set_propagate_natural_height(true);
        // A ScrolledWindow keeps its minimum width unless told to grow
        // with its rows, so a long title fell back to that minimum and
        // ellipsized after about a dozen characters. Growing with the
        // rows, up to a sensible width, lets a title use the room
        // before it ellipsizes.
        more_list.scrolled.set_propagate_natural_width(true);
        more_list.scrolled.set_min_content_width(280);
        more_list.scrolled.set_max_content_width(420);
        more_list.scrolled.set_max_content_height(360);
        let more = gtk::Popover::builder().child(&more_list.widget).build();
        more.set_parent(&card);
        let quick = Quick::new(&card);

        let view = Rc::new_cyclic(|weak: &Weak<CalendarView>| {
            let (on_date, on_change, on_grant, on_open_waiting, on_open_mail) =
                (weak.clone(), weak.clone(), weak.clone(), weak.clone(), weak.clone());
            let calendar_sidebar = CalendarSidebar::new(
                move |day| {
                    if let Some(view) = on_date.upgrade() {
                        view.go_to(day);
                    }
                },
                move |change| {
                    if let Some(view) = on_change.upgrade() {
                        view.list_changed(change);
                    }
                },
                move |account| {
                    if let Some(view) = on_grant.upgrade() {
                        (view.hooks.grant)(account);
                    }
                },
                move |account_id, calendar, id, start| {
                    if let Some(view) = on_open_waiting.upgrade() {
                        view.open(account_id, &calendar, &id, start);
                    }
                },
                move |account_id, thread_id| {
                    if let Some(view) = on_open_mail.upgrade() {
                        (view.hooks.open_mail)(account_id, thread_id);
                    }
                },
            );
            let sidebar = calendar_sidebar.widget.clone();
            CalendarView {
                page,
                sidebar,
                sidebar_button,
                assistant_toggle,
                core,
                settings: Box::new(settings),
                hooks,
                calendar_sidebar,
                title_bold,
                title_dim,
                title_week,
                today_button: today_button.clone(),
                arrows,
                previous,
                next,
                switch,
                view_menu,
                view_action,
                switch_slot,
                header_room: header_room.clone(),
                bottom_row,
                header: header.clone(),
                new_event: new_event.clone(),
                search_bar,
                search_entry,
                views,
                older_note,
                older_spinner,
                older_label,
                older_read: Cell::new(0),
                carousel,
                pages: RefCell::new(Vec::new()),
                list,
                results,
                popover,
                editor_opened: Cell::new(None),
                more,
                more_list,
                quick,
                more_anchor: RefCell::new(None),
                clock_watch: RefCell::new(None),
                day: Cell::new(today),
                kind: Cell::new(kind),
                before_day: Cell::new(match kind {
                    ViewKind::Day | ViewKind::Agenda => ViewKind::Week,
                    other => other,
                }),
                narrow: Cell::new(false),
                accounts: RefCell::new(Vec::new()),
                calendars: RefCell::new(HashMap::new()),
                hidden: RefCell::new(HashSet::new()),
                reads: Cell::new(0),
                sidebar_read: Cell::new(0),
                waiting_read: Cell::new(0),
                list_read: Cell::new(0),
                list_first: Cell::new(today),
                list_exhausted: Cell::new(false),
                list_loading: Cell::new(false),
                list_last: Cell::new(None),
                list_later_loading: Cell::new(false),
                search_read: Cell::new(0),
                fill_owed: Cell::new(false),
                open_read: Cell::new(0),
                pending_open: RefCell::new(None),
                switching: Cell::new(false),
                arranging: Cell::new(false),
                refocus_owed: Cell::new(false),
                holding: RefCell::new(Holding::new()),
                toast_up: RefCell::new(None),
                refreshing: Cell::new(false),
                sync_failed: Cell::new(false),
                last_synced: Cell::new(None),
            }
        });

        let weak = Rc::downgrade(&view);
        new_event.connect_clicked(move |_| {
            if let Some(view) = weak.upgrade() {
                view.new_event();
            }
        });
        let weak = Rc::downgrade(&view);
        view.quick.popover.connect_closed(move |_| {
            if let Some(view) = weak.upgrade() {
                view.clear_ghost();
            }
        });

        let weak = Rc::downgrade(&view);
        today_button.connect_clicked(move |_| {
            if let Some(view) = weak.upgrade() {
                view.today();
            }
        });
        for (button, by) in [(&view.previous, -1), (&view.next, 1)] {
            let weak = Rc::downgrade(&view);
            button.connect_clicked(move |_| {
                if let Some(view) = weak.upgrade() {
                    view.step(by);
                }
            });
        }
        let weak = Rc::downgrade(&view);
        view.switch.connect_active_name_notify(move |switch| {
            let Some(view) = weak.upgrade() else { return };
            if view.switching.get() {
                return;
            }
            let name = switch.active_name();
            if let Some(kind) = name.and_then(|n| shown::kind_for(&n, view.before_day.get())) {
                view.set_kind(kind);
            }
        });
        let weak = Rc::downgrade(&view);
        view.view_action.connect_activate(move |_, name| {
            let Some(view) = weak.upgrade() else { return };
            let name = name.and_then(|n| n.get::<String>());
            if let Some(kind) = name.and_then(|n| shown::kind_for(&n, view.before_day.get())) {
                view.set_kind(kind);
            }
        });
        let weak = Rc::downgrade(&view);
        view.carousel.connect_map(move |_| {
            if let Some(view) = weak.upgrade() {
                view.center();
            }
        });
        let weak = Rc::downgrade(&view);
        view.carousel.connect_page_changed(move |_, index| {
            if let Some(view) = weak.upgrade() {
                view.settled_on(index);
            }
        });
        // The tints are stronger in dark mode (tint.rs), against the
        // `app-dark` class the toplevel window carries
        // (`ui::window::track_dark_class`), which reaches this page
        // whichever window it sits in.
        // Redraws in the new clock as soon as the person flips GNOME's own
        // setting, not only the next time they navigate. Kept in
        // `clock_watch` for as long as the view lives, which is what
        // keeps the watch itself alive.
        let weak = Rc::downgrade(&view);
        *view.clock_watch.borrow_mut() = crate::clock_format::watch(move || {
            if let Some(view) = weak.upgrade() {
                view.show_range();
            }
        });
        let weak = Rc::downgrade(&view);
        view.more.connect_closed(move |_| {
            let Some(view) = weak.upgrade() else { return };
            let anchor = view.more_anchor.borrow().clone();
            let back = match shown::after_popover(anchor.as_ref().is_some_and(|a| a.is_mapped())) {
                Refocus::Anchor => anchor.is_some_and(|a| a.grab_focus()),
                _ => false,
            };
            if !back {
                view.take_focus();
            }
        });
        view.connect_search();
        view.connect_lists();
        let weak = Rc::downgrade(&view);
        view.header_room.connect_change(move || {
            if let Some(view) = weak.upgrade() {
                view.show_extras();
            }
        });
        view.calendar_sidebar
            .set_folded((view.settings)().folded_calendar_accounts.into_iter().collect());

        view.build_switch();
        view.rebuild_pages();
        view.show_range();
        view
    }

    /// Reads the calendars and the ranges on screen again, as after the
    /// copy changed.
    pub fn reload(self: &Rc<Self>) {
        self.read_sidebar(true);
        self.refresh_waiting();
    }

    /// Redraws the Week and Month grids and the mini month after the
    /// "Week Starts On" choice changes, so an open calendar reflects it
    /// at once rather than at the next navigation.
    pub fn week_start_changed(self: &Rc<Self>) {
        self.calendar_sidebar.week_start_changed();
        self.rebuild_pages();
        self.show_range();
        self.fill_all();
        self.read_sidebar(false);
    }

    /// The Refresh action: starts a calendar sync for every account,
    /// unless one it started is still running.
    pub fn refresh_now(self: &Rc<Self>) {
        if !should_refresh(self.refreshing.get()) {
            return;
        }
        self.refreshing.set(true);
        (self.hooks.refresh)();
    }

    /// What `App::refresh_calendars` calls once its pass over every
    /// account ends, whatever it found, so the next press can start
    /// another one.
    pub fn refresh_done(&self) {
        self.refreshing.set(false);
    }

    /// What a calendar sync attempt found, for the offline line: `ok`
    /// clears any earlier failure and remembers when it succeeded; a
    /// failure marks the last attempt as failed without losing the
    /// earlier time.
    pub fn synced(&self, ok: bool) {
        self.sync_failed.set(!ok);
        if ok {
            self.last_synced.set(Some(chrono::Local::now().time()));
        }
        self.show_offline_line();
    }

    /// Redraws the offline line after the computer's own network state
    /// changes, without waiting for the next sync attempt.
    pub fn network_changed(&self) {
        self.show_offline_line();
    }

    /// Shows or hides "Offline, last updated 14:32" under the mini
    /// month: nothing while the account is online and its last sync
    /// succeeded, [`words::offline_line`] decides the rest.
    fn show_offline_line(&self) {
        let last = self.last_synced.get().map(crate::clock_format::time_text);
        let text = words::offline_line(self.core.network(), self.sync_failed.get(), last.as_deref());
        self.calendar_sidebar.set_offline_line(text.as_deref());
    }

    /// Reads "Waiting for your answer" again, and nothing else: after an
    /// invitation's message is saved, which may give a card its "Open
    /// mail" door, and after an answer. It leaves the pages and the
    /// open popover alone, so it can run while the view opens an event.
    pub fn refresh_waiting(self: &Rc<Self>) {
        let read = self.waiting_read.get() + 1;
        self.waiting_read.set(read);
        let accounts = sidebar::waiting_accounts(&self.accounts.borrow());
        let now = chrono::Local::now().timestamp_millis();
        let weak = Rc::downgrade(self);
        let core = Rc::clone(&self.core);
        let invitations = self.core.invitations();
        glib::spawn_future_local(async move {
            // `Db::read`'s `spawn_blocking` needs the tokio runtime, which
            // `call` gives it and the GTK loop does not.
            let waiting = core
                .call(async move { invitations.waiting_for_answer(&accounts, now).await })
                .await;
            let Some(view) = weak.upgrade() else { return };
            if view.waiting_read.get() != read {
                return;
            }
            match waiting {
                Ok(waiting) => {
                    view.calendar_sidebar.show_waiting(&waiting);
                    (view.hooks.waiting)(waiting.len());
                }
                Err(err) => tracing::warn!(%err, "could not read what is waiting for an answer"),
            }
        });
    }

    /// Moves the view to the range around `day`.
    pub fn go_to(self: &Rc<Self>, day: NaiveDate) {
        self.open_read.set(self.next_read());
        self.day.set(day);
        self.place_ranges();
        self.show_range();
        self.fill_all();
        self.read_sidebar(false);
        self.reach_current();
    }

    /// The start of what the view shows now, which the copy has to reach:
    /// the range's first day, or for the list its first day.
    fn wanted_from(&self) -> EpochMillis {
        let day = match self.showing() {
            Showing::List => range::agenda_window(self.day.get()).0,
            _ => return Range::around(self.effective_kind(), self.day.get()).span(&chrono::Local).0,
        };
        day_span(day, day).0
    }

    /// Makes sure the copy holds what the view shows now, fetching an
    /// older range from Google when the person went back further than the
    /// copy reaches, and draws it again once it arrives.
    fn reach_current(self: &Rc<Self>) {
        let from = self.wanted_from();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Some(view) = weak.upgrade() else { return };
            if view.reach_back(from).await == Older::Loaded && view.wanted_from() == from {
                view.fill_all();
            }
        });
    }

    /// Asks the copy for the events back to `from`, with the note over
    /// the view saying so while it waits. `Loaded` means a fetch ran, so
    /// what the view drew before may lack events; `Offline` means one was
    /// due and could not run.
    async fn reach_back(self: &Rc<Self>, from: EpochMillis) -> Older {
        let ask = self.older_read.get() + 1;
        self.older_read.set(ask);
        let accounts = self.account_ids();
        let copy = self.core.calendar_copy();
        let missing = {
            let (copy, accounts) = (Arc::clone(&copy), accounts.clone());
            self.core.call(async move { copy.older_missing(&accounts, from).await }).await
        };
        match missing {
            Ok(true) => {}
            Ok(false) => {
                self.show_older_note(ask, None);
                return Older::Held;
            }
            Err(err) => {
                tracing::warn!(%err, "could not tell whether the copy reaches back far enough");
                self.show_older_note(ask, None);
                return Older::Held;
            }
        }
        if !self.core.network() {
            self.show_older_note(ask, Some(Older::Offline));
            return Older::Offline;
        }
        self.show_older_note(ask, Some(Older::Loaded));
        let read = self.core.call(async move { copy.reach_back(&accounts, from).await }).await;
        match read {
            Ok(_) => {
                self.show_older_note(ask, None);
                Older::Loaded
            }
            Err(err) => {
                let offline = err
                    .downcast_ref::<mailrs_sync::SyncError>()
                    .is_some_and(|e| matches!(e, mailrs_sync::SyncError::Backend(b) if b.is_transient()));
                if offline {
                    self.show_older_note(ask, Some(Older::Offline));
                    return Older::Offline;
                }
                tracing::warn!(%err, "could not fetch older calendar events");
                self.show_older_note(ask, None);
                Older::Held
            }
        }
    }

    /// Shows, changes or hides the note over the view, unless a newer ask
    /// for older events has taken it over. `Loaded` stands for the loading
    /// line and `Offline` for the offline one.
    fn show_older_note(&self, ask: u64, saying: Option<Older>) {
        if self.older_read.get() != ask {
            return;
        }
        let offline = saying == Some(Older::Offline);
        self.older_spinner.set_visible(!offline);
        self.older_spinner.set_spinning(saying == Some(Older::Loaded));
        self.older_label.set_label(&match offline {
            true => gettext("Older events can't load while offline"),
            false => gettext("Loading older events"),
        });
        self.older_note.set_visible(saying.is_some());
    }

    /// Shows a day, a week or a month, around the day the view is on.
    pub fn set_kind(self: &Rc<Self>, kind: ViewKind) {
        if !matches!(kind, ViewKind::Day | ViewKind::Agenda) {
            self.before_day.set(kind);
        }
        if kind == self.kind.get() {
            self.show_range();
            return;
        }
        self.kind.set(kind);
        (self.hooks.change)(Change::CalendarView(kind));
        self.rebuild_pages();
        self.show_range();
        self.fill_all();
        self.reach_current();
        // The mini month's band follows the grid: a week, a month, or
        // none for a single day.
        self.read_sidebar(false);
    }

    pub fn today(self: &Rc<Self>) {
        self.go_to(chrono::Local::now().date_naive());
    }

    /// A small date picker, for G: jumps to the day chosen and closes.
    pub fn go_to_date(self: &Rc<Self>) {
        let picker = gtk::Calendar::new();
        picker.set_date(&editor::day_to_glib(self.day.get()));
        crate::ui::name(&picker, &gettext("Go to date"));
        let popover = gtk::Popover::builder().child(&picker).autohide(true).build();
        popover.set_parent(&self.today_button);
        let weak = Rc::downgrade(self);
        let closing = popover.clone();
        picker.connect_day_selected(move |picker| {
            let Some(this) = weak.upgrade() else { return };
            let picked = picker.date();
            if let Some(day) =
                NaiveDate::from_ymd_opt(picked.year(), picked.month() as u32, picked.day_of_month() as u32)
            {
                this.go_to(day);
            }
            closing.popdown();
        });
        popover.popup();
    }

    /// Moves `by` ranges forward, or back when negative. One step slides
    /// the carousel to its neighbour page along the same spring a swipe
    /// settles with; the page change then brings the pages round.
    pub fn step(self: &Rc<Self>, by: i32) {
        if self.showing() == Showing::List {
            self.go_to(shown::stepped(ViewKind::Month, self.day.get(), by));
            return;
        }
        let target = match by {
            1 => self.pages.borrow().get(2).map(|p| p.holder.clone()),
            -1 => self.pages.borrow().first().map(|p| p.holder.clone()),
            _ => None,
        };
        match target {
            Some(holder) => self.carousel.scroll_to(&holder, true),
            None => self.go_to(shown::stepped(self.effective_kind(), self.day.get(), by)),
        }
    }

    /// Goes to the day `start` falls on and opens that occurrence's
    /// popover once the range has drawn, as the toast about a change the
    /// provider turned down does. `start` is the occurrence's own start,
    /// not the series' first one: an invitation to next Tuesday's design
    /// review names that Tuesday, not the series' beginning. Nothing
    /// happens when the store no longer has the event.
    pub fn open(self: &Rc<Self>, account_id: AccountId, calendar: &str, id: &str, start: EpochMillis) {
        let (calendar, id) = (calendar.to_string(), id.to_string());
        let key: EventKey = (account_id, calendar.clone(), id.clone());
        let read = self.next_read();
        self.open_read.set(read);
        let weak = Rc::downgrade(self);
        let core = Rc::clone(&self.core);
        let asked = read;
        glib::spawn_future_local(async move {
            let read = core
                .read(move |c| store::event(c, account_id, &calendar, &id))
                .await;
            let Some(view) = weak.upgrade() else { return };
            // The person moved on while the store answered: to another
            // range, another event, or away from the calendar.
            if view.open_read.get() != asked || !view.page.is_mapped() {
                return;
            }
            match read {
                Ok(Some(event)) => {
                    let day = date_of(start, event.all_day);
                    view.pending_open.replace(Some((key, start)));
                    view.go_to(day);
                }
                Ok(None) => {}
                Err(err) => tracing::warn!(%err, "could not read the event to open"),
            }
        });
    }

    /// The pending target, in the flat shape [`shown::keep`] matches
    /// against.
    fn pending(&self) -> Option<(AccountId, String, String, EpochMillis)> {
        self.pending_open
            .borrow()
            .clone()
            .map(|((account_id, calendar, id), start)| (account_id, calendar, id, start))
    }

    /// Answers the window's narrow breakpoint: a list in place of Week
    /// and Month, and the view switch in a bar at the bottom where the
    /// header has no room for it.
    pub fn set_narrow(self: &Rc<Self>, narrow: bool) {
        if narrow == self.narrow.get() {
            return;
        }
        self.narrow.set(narrow);
        // A phone's header keeps its title, New Event, Search and the
        // window's buttons; Today and the arrows go down beside the
        // switch.
        if narrow {
            self.switch_slot.remove(&self.switch);
            self.header.remove(&self.today_button);
            self.header.remove(&self.arrows);
            self.bottom_row.append(&self.today_button);
            self.bottom_row.append(&self.arrows);
            self.bottom_row.append(&self.switch);
        } else {
            for widget in [
                self.today_button.upcast_ref::<gtk::Widget>(),
                self.arrows.upcast_ref(),
                self.switch.upcast_ref(),
            ] {
                self.bottom_row.remove(widget);
            }
            self.switch_slot.prepend(&self.switch);
            self.header.pack_start(&self.today_button);
            self.header.pack_start(&self.arrows);
        }
        self.switch_slot.set_visible(!narrow);
        self.header_room.want(Extra::Switch, !narrow);
        self.page.set_reveal_bottom_bars(narrow);
        match narrow {
            true => self.page.add_css_class("calendar-narrow"),
            false => self.page.remove_css_class("calendar-narrow"),
        }
        self.build_switch();
        self.show_range();
        if self.showing() == Showing::List {
            self.fill_list();
        }
        self.reach_current();
    }

    /// Puts the focus in the page, on Today, so the calendar's keys
    /// answer after the window switches to it.
    pub fn take_focus(&self) {
        self.today_button.grab_focus();
    }

    /// The narrowest the calendar can go, the header with its extras
    /// gone or the card with the view it shows, whichever is wider, and
    /// then the window's buttons in the header (`ui::header_least`).
    pub fn least_width(&self) -> (i32, i32) {
        let (header, buttons) = crate::ui::header_least(&self.header_room);
        let card = self
            .page
            .content()
            .map_or(0, |c| c.measure(gtk::Orientation::Horizontal, -1).0);
        (header.max(card), buttons)
    }

    /// Opens the search and puts the cursor in it.
    pub fn focus_search(&self) {
        self.search_bar.set_search_mode(true);
        self.search_entry.grab_focus();
    }

    /// The accounts the calendar reads, read again whenever the window
    /// reads its accounts, which it does when one starts: an account's
    /// reason line, such as a provider with no calendar yet, arrives
    /// then.
    pub fn set_accounts(self: &Rc<Self>, accounts: Vec<CalendarAccount>) {
        self.accounts.replace(accounts);
        self.reload();
    }

    /// What the view shows now.
    fn showing(&self) -> Showing {
        shown::showing(self.kind.get(), self.narrow.get())
    }

    /// The name of the switch entry that marks what is on screen.
    fn active_toggle(&self) -> &'static str {
        shown::active_toggle(self.kind.get(), self.narrow.get())
    }

    /// The grid actually on screen: the kind the person picked, unless
    /// the breakpoint replaced it. List draws Month's grid behind its
    /// own agenda page (unused while List is on screen, but built all
    /// the same). Every page built from the current range, and anything
    /// that steps by or matches against it, follows this rather than the
    /// raw [`kind`](Self::kind), so what such code does lines up with
    /// what the reader sees.
    fn effective_kind(&self) -> ViewKind {
        match self.showing() {
            Showing::Day => ViewKind::Day,
            Showing::Week => ViewKind::Week,
            Showing::Month | Showing::List => ViewKind::Month,
        }
    }

    fn account_ids(&self) -> Vec<AccountId> {
        self.accounts.borrow().iter().map(|(a, _, _)| a.id).collect()
    }

    fn next_read(&self) -> u64 {
        let read = self.reads.get() + 1;
        self.reads.set(read);
        read
    }

    // ---- The header -----------------------------------------------------

    /// Puts the toggles a window of this width offers in the switch, and
    /// marks the one on screen.
    fn build_switch(&self) {
        self.switching.set(true);
        self.switch.remove_all();
        for (name, on) in shown::offered(self.narrow.get()) {
            if !on {
                continue;
            }
            let label = match name {
                "list" => gettext("List"),
                "day" => gettext("Day"),
                "week" => gettext("Week"),
                "agenda" => gettext("Agenda"),
                _ => gettext("Month"),
            };
            self.switch.add(adw::Toggle::builder().name(name).label(&label).build());
        }
        self.switch.set_active_name(Some(self.active_toggle()));
        self.switching.set(false);
    }

    /// Shows the header's extras it has room for and hides the rest.
    fn show_extras(&self) {
        let room = &self.header_room;
        self.title_dim.set_visible(room.keeps(Extra::Year));
        self.title_week
            .set_visible(!self.title_week.label().is_empty() && room.keeps(Extra::Week));
        let whole = room.keeps(Extra::Switch);
        self.switch.set_visible(whole || self.narrow.get());
        self.view_menu.set_visible(!whole);
    }

    /// Brings the header and the visible view in line with the range.
    fn show_range(&self) {
        let showing = self.showing();
        self.switching.set(true);
        self.switch.set_active_name(Some(self.active_toggle()));
        self.switching.set(false);
        let active = self.active_toggle();
        self.view_action.set_state(&active.to_variant());
        self.view_menu.set_label(&match active {
            "day" => gettext("Day"),
            "week" => gettext("Week"),
            "month" => gettext("Month"),
            "list" => gettext("List"),
            _ => gettext("Agenda"),
        });
        // The list names its month the way a month's title does. So does
        // a narrow window's Day, whose heading under the header already
        // names the day.
        let titled = match (showing, self.narrow.get()) {
            (Showing::Day, true) => ViewKind::Month,
            _ => self.effective_kind(),
        };
        let range = Range::around(titled, self.day.get());
        let (bold, dim, week) = range.title();
        self.title_bold.set_label(&bold);
        self.title_dim.set_label(&dim);
        self.title_week.set_label(&week);
        self.header_room.want(Extra::Week, !week.is_empty());
        self.show_extras();
        let (back, forward) = shown::arrow_names(showing);
        crate::ui::name(&self.previous, &back);
        crate::ui::name(&self.next, &forward);
        self.previous.set_tooltip_text(Some(&back));
        self.next.set_tooltip_text(Some(&forward));
        // The list loads its earlier days as it scrolls, so it has no
        // arrows of its own.
        self.arrows.set_visible(showing != Showing::List);
        if !self.search_open() {
            self.views.set_visible_child_name(match showing {
                Showing::List => "list",
                _ => "grid",
            });
        }
    }

    // ---- The pages ------------------------------------------------------

    /// Builds the three pages for the current kind, around the current
    /// day. A page's widgets go when it is replaced, so at most three
    /// ranges' worth of blocks exist at a time.
    fn rebuild_pages(self: &Rc<Self>) {
        self.popover.hide();
        self.more.popdown();
        self.quick.hide();
        self.clear_ghost();
        let had_focus = self.pages.borrow().iter().any(|p| self.holds_focus(p));
        if had_focus {
            self.refocus_owed.set(true);
        }
        self.arranging.set(true);
        let old: Vec<Rc<Page>> = self.pages.replace(Vec::new());
        let current = Range::around(self.effective_kind(), self.day.get());
        let ranges = [current.previous(), current, current.next()];
        let views = ranges.map(|_| self.page_view());
        let holders = pager::show_views(
            &self.carousel,
            old.iter().map(|page| page.holder.clone()).collect(),
            views.each_ref().map(PageView::widget),
        );
        let pages: Vec<Rc<Page>> = holders
            .into_iter()
            .zip(ranges)
            .zip(views)
            .map(|((holder, range), view)| {
                Rc::new(Page {
                    holder,
                    range: Cell::new(range),
                    view: RefCell::new(view),
                    generation: Cell::new(0),
                    scrolled: Cell::new(false),
                })
            })
            .collect();
        self.pages.replace(pages);
        self.arranging.set(false);
        self.mark_reachable();
    }

    /// Lets Tab and a screen reader into the middle page only: the pages
    /// either side are off screen until a swipe brings one in.
    fn mark_reachable(&self) {
        for (position, page) in self.pages.borrow().iter().enumerate() {
            let reachable = shown::reachable(position);
            page.holder.set_can_focus(reachable);
            page.holder
                .upcast_ref::<gtk::Widget>()
                .update_state(&[gtk::accessible::State::Hidden(!reachable)]);
        }
    }

    /// Whether the keyboard focus is somewhere inside `page`.
    fn holds_focus(&self, page: &Page) -> bool {
        self.page
            .root()
            .and_then(|root| root.focus())
            .is_some_and(|focus| focus.is_ancestor(&page.holder))
    }

    /// Clears the ghost span quick create's popover marks, on whichever
    /// page is on screen: the only one it can be showing on.
    fn clear_ghost(&self) {
        if let Some(page) = self.pages.borrow().get(1)
            && let PageView::Grid(grid) = &*page.view.borrow()
        {
            grid.grid.show_ghost(None);
        }
    }

    /// Puts the focus on `key`'s block in `page` when it has one, else
    /// wherever [`shown::refocus`] sends focus the page took away.
    fn refocus(&self, page: &Page, key: Option<EventKey>) {
        let view = page.view.borrow();
        if let Some(block) = key.and_then(|key| view.block_of(&key)) {
            block.grab_focus();
            return;
        }
        let first = view.first_block();
        match (shown::refocus(true, first.is_some()), first) {
            (Some(Refocus::FirstEvent), Some(first)) => {
                first.grab_focus();
            }
            (Some(_), _) => self.take_focus(),
            (None, _) => {}
        }
    }

    /// Puts the carousel on its middle page, the range on screen.
    fn center(&self) {
        let middle = self.pages.borrow().get(1).map(|p| p.holder.clone());
        if let Some(middle) = middle {
            self.arranging.set(true);
            self.carousel.scroll_to(&middle, false);
            self.arranging.set(false);
        }
    }

    /// Gives the three pages the ranges around the current day, without
    /// building their widgets again.
    fn place_ranges(self: &Rc<Self>) {
        self.popover.hide();
        self.more.popdown();
        self.quick.hide();
        self.clear_ghost();
        let current = Range::around(self.effective_kind(), self.day.get());
        let pages = self.pages.borrow().clone();
        for (page, range) in pages.iter().zip([current.previous(), current, current.next()]) {
            if page.range.get() != range {
                page.range.set(range);
                page.scrolled.set(false);
            }
        }
        if let Some(middle) = pages.get(1) {
            self.arranging.set(true);
            self.carousel.scroll_to(&middle.holder, false);
            self.arranging.set(false);
        }
    }

    // ---- Moving events ---------------------------------------------------

    /// A drag, or a run of keyboard nudges once the keyboard rests, moved
    /// or resized `o`, in the time grid, the all-day row or Month, to
    /// `landing`, which may also make it all-day or timed. The move is
    /// confirmed first, in one dialog that also asks which occurrences of
    /// a series it covers and whether the guests get an update. The write
    /// goes through the draft, so a weekly series moved to another day
    /// moves its day in the rule. The card stays where it landed until
    /// the write is held; a Cancel or a failed write runs `spring_back`,
    /// which the view the drag happened in hands over holding that view
    /// weakly, so it acts only while the view is still on screen.
    pub(super) fn moved(self: &Rc<Self>, spring_back: Rc<dyn Fn()>, o: &Occurrence, landing: drag::Landing) {
        let this = Rc::clone(self);
        let o = o.clone();
        let drag::Landing { start, end, all_day } = landing;
        glib::spawn_future_local(async move {
            let rules = this.series_rules(&o).await;
            let mut draft = Draft::open(&o, &rules, draft::local_zone());
            draft.land(start, end, all_day);
            let offered = series::scopes(&o.event, false);
            let when = words::landing_words(start, end, all_day, &draft::local_zone());
            let change = this.guest_change(o.account_id);
            let answer = match scope::question(scope::Action::Move, &offered, &o.event.guests, change) {
                Some(question) => match scope::ask(&this.page, &question, &o.event, Some(&when)).await {
                    Some(answer) => answer,
                    None => {
                        spring_back();
                        return;
                    }
                },
                None => scope::unasked(scope::Action::Move, change),
            };
            let scope = answer.scope;
            let event = draft.to_event(
                &mailrs_sync::calendar_copy::new_event_id(),
                &mailrs_sync::calendar_copy::new_event_id(),
            );
            let copy = this.core.calendar_copy();
            let (account_id, occurrence) = (o.account_id, o.clone());
            let held = this
                .core
                .call(async move {
                    let steps = copy.change_steps(account_id, &occurrence, event, scope).await?;
                    copy.hold_with(account_id, steps, answer.notify).await
                })
                .await;
            match held {
                Ok(Permitted::Done(held)) => {
                    // A drop from the all-day row marked its hour in the
                    // grid, which a refill keeps for quick create.
                    this.clear_ghost();
                    this.reload();
                    this.offer_undo(fill(&gettext("Moved “{title}”"), &[("title", &o.event.title)]), held);
                }
                Ok(Permitted::NeedsPermission) => {
                    spring_back();
                    (this.hooks.needs_permission)(account_id);
                }
                Err(err) => {
                    spring_back();
                    (this.hooks.toast)(&with_reason(
                        &gettext("Could not move the event: {reason}"),
                        &err,
                        &[],
                    ));
                }
            }
        });
    }

    /// The rules of the series `o` belongs to: its own, or for a changed
    /// occurrence the series row's.
    pub(super) async fn series_rules(&self, o: &Occurrence) -> Vec<String> {
        let Some(series_id) = o.event.series.clone() else {
            return o.event.rules.clone();
        };
        let (account, calendar) = (o.account_id, o.event.calendar.clone());
        self.core
            .read(move |c| Ok(store::event(c, account, &calendar, &series_id)?.map(|e| e.rules)))
            .await
            .ok()
            .flatten()
            .unwrap_or_default()
    }

    /// Whether a drag may move `o`: its calendar must be one the
    /// account can write to, the account must offer a calendar and not
    /// have withheld it, and the event itself must allow it (not a
    /// guest's own event, not on its way out).
    /// The guests-choice facts of `account_id`'s calendar: a Microsoft
    /// account mails the guests of every change and cannot send nobody.
    /// An account not listed keeps Google's choice.
    fn guest_change(&self, account_id: AccountId) -> scope::Change {
        let quiet = self
            .accounts
            .borrow()
            .iter()
            .find(|(a, _, _)| a.id == account_id)
            .is_none_or(|(_, offers, _)| offers.quiet_changes);
        scope::Change { always_mails: !quiet, ..scope::Change::default() }
    }

    fn can_move(&self, o: &Occurrence) -> bool {
        let access = self
            .calendars
            .borrow()
            .get(&(o.account_id, o.event.calendar.clone()))
            .map_or(Access::Reader, |c| c.access);
        let (offers, withheld) = self
            .accounts
            .borrow()
            .iter()
            .find(|(a, _, _)| a.id == o.account_id)
            .map_or((false, true), |(_, offers, withheld)| {
                (offers.calendar, withheld.calendar)
            });
        drag::can_move(o, access, offers, withheld)
    }

    /// A page's widgets for the grid actually on screen.
    fn page_view(self: &Rc<Self>) -> PageView {
        match self.effective_kind() {
            ViewKind::Month | ViewKind::Agenda => {
                let month = MonthGrid::new();
                let weak = Rc::downgrade(self);
                month.connect_day_activated(move |day| {
                    if let Some(view) = weak.upgrade() {
                        view.open_day(day);
                    }
                });
                let weak = Rc::downgrade(self);
                month.connect_day_clicked(move |day| {
                    if let Some(view) = weak.upgrade() {
                        view.quick_create_on_day(day);
                    }
                });
                let weak = Rc::downgrade(self);
                month.connect_event_activated(move |_, o, anchor| {
                    if let Some(view) = weak.upgrade() {
                        view.show_event(anchor, o);
                    }
                });
                let weak = Rc::downgrade(self);
                month.connect_event_edited(move |month, o| {
                    if let Some(view) = weak.upgrade() {
                        view.edit_or_show(o, month.block_at(&key_of(o), o.start).as_ref());
                    }
                });
                let weak = Rc::downgrade(self);
                month.connect_more_clicked(move |_, day, anchor| {
                    if let Some(view) = weak.upgrade() {
                        view.show_more(anchor, day);
                    }
                });
                let weak = Rc::downgrade(self);
                let month_weak = Rc::downgrade(&month);
                month.connect_moved(move |_, o, landing| {
                    let Some(view) = weak.upgrade() else { return };
                    let month = month_weak.clone();
                    let spring_back = Rc::new(move || {
                        if let Some(month) = month.upgrade() {
                            month.spring_back();
                        }
                    });
                    view.moved(spring_back, o, landing);
                });
                let weak = Rc::downgrade(self);
                month.set_can_move(move |o| weak.upgrade().is_some_and(|view| view.can_move(o)));
                let carousel = self.carousel.clone();
                month.connect_carousel_interactive(move |on| carousel.set_interactive(on));
                PageView::Month(month)
            }
            ViewKind::Day | ViewKind::Week => PageView::Grid(self.grid_page()),
        }
    }

    fn grid_page(self: &Rc<Self>) -> GridPage {
        let headings = headings::DayHeadings::new();
        headings.set_margin_start(GUTTER as i32);
        headings.set_margin_top(11);
        headings.set_margin_bottom(10);
        let strip = AllDayStrip::new();
        crate::ui::name(&strip, &gettext("All-day events"));
        let all_day = gtk::Label::builder()
            .label(gettext("All-day"))
            .css_classes(["all-day-label"])
            .halign(gtk::Align::Start)
            .valign(gtk::Align::Center)
            .xalign(1.0)
            .width_request(GUTTER as i32 - 10)
            .can_target(false)
            .build();
        let strip_row = gtk::Overlay::builder().child(&strip).build();
        strip_row.add_overlay(&all_day);
        let grid = TimeGrid::new();
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .css_classes(["time-scroller"])
            .child(&grid)
            .build();
        let label_edge = grid.clone();
        scroller
            .vadjustment()
            .connect_value_changed(move |a| label_edge.set_view(a.value(), a.page_size()));
        let label_edge = grid.clone();
        scroller
            .vadjustment()
            .connect_changed(move |a| label_edge.set_view(a.value(), a.page_size()));
        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .build();
        root.append(&headings);
        root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        root.append(&strip_row);
        root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        root.append(&scroller);

        let weak = Rc::downgrade(self);
        grid.connect_event_activated(move |_, o, anchor| {
            if let Some(view) = weak.upgrade() {
                view.show_event(anchor, o);
            }
        });
        let weak = Rc::downgrade(self);
        grid.connect_event_edited(move |grid, o| {
            if let Some(view) = weak.upgrade() {
                view.edit_or_show(o, grid.block_at(&key_of(o), o.start).as_ref());
            }
        });
        let weak = Rc::downgrade(self);
        grid.connect_more_clicked(move |_, hidden, anchor| {
            if let Some(view) = weak.upgrade() {
                view.show_more(anchor, hidden);
            }
        });
        let weak = Rc::downgrade(self);
        strip.connect_event_activated(move |_, o, anchor| {
            if let Some(view) = weak.upgrade() {
                view.show_event(anchor, o);
            }
        });
        let weak = Rc::downgrade(self);
        strip.connect_event_edited(move |strip, o| {
            if let Some(view) = weak.upgrade() {
                view.edit_or_show(o, strip.block_at(&key_of(o), o.start).as_ref());
            }
        });
        let weak = Rc::downgrade(self);
        strip.connect_more_clicked(move |_, hidden, anchor| {
            if let Some(view) = weak.upgrade() {
                view.show_more(anchor, hidden);
            }
        });
        let weak = Rc::downgrade(self);
        grid.connect_moved(move |grid, o, landing| {
            if let Some(view) = weak.upgrade() {
                let grid = grid.downgrade();
                let spring_back = Rc::new(move || {
                    if let Some(grid) = grid.upgrade() {
                        grid.spring_back();
                    }
                });
                view.moved(spring_back, o, landing);
            }
        });
        let weak = Rc::downgrade(self);
        grid.connect_selected(move |start, end| {
            if let Some(view) = weak.upgrade() {
                view.quick_create_at(start, end);
            }
        });
        let weak = Rc::downgrade(self);
        strip.connect_moved(move |strip, o, landing| {
            if let Some(view) = weak.upgrade() {
                let strip = strip.downgrade();
                let spring_back = Rc::new(move || {
                    if let Some(strip) = strip.upgrade() {
                        strip.spring_back();
                    }
                });
                view.moved(spring_back, o, landing);
            }
        });
        grid.set_strip(&strip);
        let weak = Rc::downgrade(self);
        strip.set_can_move(move |o| weak.upgrade().is_some_and(|view| view.can_move(o)));
        let weak = Rc::downgrade(self);
        grid.set_can_move(move |o| weak.upgrade().is_some_and(|view| view.can_move(o)));
        let weak = Rc::downgrade(self);
        grid.set_can_select(move || weak.upgrade().is_some_and(|view| !view.writable().is_empty()));
        let carousel = self.carousel.clone();
        grid.connect_carousel_interactive(move |on| carousel.set_interactive(on));
        let carousel = self.carousel.clone();
        strip.connect_carousel_interactive(move |on| carousel.set_interactive(on));
        GridPage {
            root,
            headings,
            strip,
            scroller,
            scroll_asked: Rc::new(Cell::new(0)),
            grid,
        }
    }

    /// The carousel came to rest on page `index`. The middle page is the
    /// range on screen; landing on either end moves the day, and the page
    /// furthest away goes round to the other end for the next range.
    fn settled_on(self: &Rc<Self>, index: u32) {
        let pages = self.pages.borrow().clone();
        // An unmapped carousel has no width to hold a position in and
        // reports its first page; `connect_map` puts it back on the middle.
        if self.arranging.get() || pages.len() != 3 || !self.carousel.is_mapped() {
            return;
        }
        // Which of the view's pages the carousel landed on, by widget:
        // the index alone says nothing once pages have been reordered.
        let Some(landed) = (index < self.carousel.n_pages()).then(|| self.carousel.nth_page(index)) else {
            return;
        };
        let by = if landed == pages[0].holder.clone().upcast::<gtk::Widget>() {
            -1
        } else if landed == pages[2].holder.clone().upcast::<gtk::Widget>() {
            1
        } else {
            return;
        };
        self.popover.hide();
        self.more.popdown();
        self.quick.hide();
        self.clear_ghost();
        let had_focus = self.holds_focus(&pages[1]);
        let arrived = &pages[if by < 0 { 0 } else { 2 }];
        self.day
            .set(shown::stepped(self.effective_kind(), self.day.get(), by));
        // Keep the day inside the range the carousel landed on, which a
        // month step with a clamped date can otherwise miss.
        let range = arrived.range.get();
        if !(range.first..range.first + Days::new(u64::from(range.days))).contains(&self.day.get()) {
            self.day.set(match range.kind {
                ViewKind::Month => range.month(),
                _ => range.first,
            });
        }
        self.arranging.set(true);
        let order = shown::after_step(by);
        let reordered: Vec<Rc<Page>> = order.iter().map(|&old| Rc::clone(&pages[old])).collect();
        let recycled = Rc::clone(&reordered[if by < 0 { 0 } else { 2 }]);
        match by {
            1 => {
                self.carousel.reorder(&recycled.holder, -1);
                recycled.range.set(range.next());
            }
            _ => {
                self.carousel.reorder(&recycled.holder, 0);
                recycled.range.set(range.previous());
            }
        }
        recycled.scrolled.set(false);
        self.pages.replace(reordered);
        self.carousel
            .scroll_to(&self.pages.borrow()[1].holder, false);
        self.arranging.set(false);
        self.mark_reachable();
        if had_focus {
            let middle = Rc::clone(&self.pages.borrow()[1]);
            let has_event = middle.view.borrow().first_block().is_some();
            if shown::owed_after_step(had_focus, has_event) {
                // The middle page's read has not answered yet: leave the
                // focus where it is rather than sending it to Today, and
                // `show_page` moves it once the read comes back.
                self.refocus_owed.set(true);
            } else {
                self.refocus(&middle, None);
            }
        }
        self.fill(&recycled);
        self.show_range();
        self.read_sidebar(false);
        self.reach_current();
    }

    /// Reads every page again, and the list when it shows.
    fn fill_all(self: &Rc<Self>) {
        let pages = self.pages.borrow().clone();
        // The page on screen first, so its read is not queued behind its
        // neighbours'.
        for index in [1, 0, 2] {
            if let Some(page) = pages.get(index) {
                self.fill(page);
            }
        }
        if self.showing() == Showing::List {
            self.fill_list();
        }
    }

    /// Reads the occurrences of a page's range and shows them, unless the
    /// page has moved to another range by the time the read comes back.
    fn fill(self: &Rc<Self>, page: &Rc<Page>) {
        let read = self.next_read();
        page.generation.set(read);
        let range = page.range.get();
        let (from, to) = range.span(&chrono::Local);
        let accounts = self.account_ids();
        let (weak, page) = (Rc::downgrade(self), Rc::downgrade(page));
        let core = Rc::clone(&self.core);
        glib::spawn_future_local(async move {
            let found = core
                .read(move |c| store::occurrences(c, &accounts, from, to, CalendarScope::Shown))
                .await;
            let (Some(view), Some(page)) = (weak.upgrade(), page.upgrade()) else {
                return;
            };
            if page.generation.get() != read {
                return;
            }
            match found {
                Ok(found) => {
                    if found.len() >= MOST_EVENTS {
                        tracing::info!(
                            first = %range.first,
                            days = range.days,
                            "the calendar range holds more events than one read shows"
                        );
                    }
                    view.show_page(&page, found);
                }
                Err(err) => tracing::warn!(%err, "could not read the calendar"),
            }
        });
    }

    fn show_page(self: &Rc<Self>, page: &Rc<Page>, found: Vec<Occurrence>) {
        let settings = (self.settings)();
        let show_declined = settings.show_declined_events;
        let pending = self.pending();
        let range = page.range.get();
        let days: Vec<NaiveDate> = (0..range.days)
            .map(|i| range.first + Days::new(u64::from(i)))
            .collect();
        // Working locations leave `found` below, so their words under each
        // day's heading come from the whole read.
        let places = kinds::workplaces(&found, &days, &chrono::Local);
        let found: Vec<Occurrence> = found
            .into_iter()
            .filter(|o| shown::keep(o, show_declined, pending.as_ref()))
            .collect();
        ensure_tints(found.iter().filter_map(|o| o.event.color.as_deref()));
        // A refill replaces every block; the one with the focus comes
        // back by its event.
        let had_focus = self.holds_focus(page);
        let focused = page.view.borrow().focused_key();
        let calendars = self.calendars.borrow().clone();
        let view = page.view.borrow();
        let block = match &*view {
            PageView::Grid(grid) => {
                // The grid is a group, which a screen reader names by the
                // range it holds.
                let (bold, dim, week) = range.title();
                let title: Vec<&str> = [bold.as_str(), dim.as_str(), week.as_str()]
                    .into_iter()
                    .filter(|part| !part.is_empty())
                    .collect();
                crate::ui::name(&grid.grid, &title.join(" "));
                self.fill_headings(&grid.headings, &days, &places);
                grid.strip.show(&days, &found, &calendars);
                let now = chrono::Local::now().timestamp_millis();
                grid.grid.show(&days, &found, &calendars, now, &chrono::Local, settings.working_hours);
                let block = self.pending_block(&found, |key, start| {
                    grid.grid
                        .block_at(key, start)
                        .or_else(|| grid.strip.block_at(key, start))
                });
                // An event about to open brings its hour into view, however
                // far the page was scrolled before; a fresh page otherwise
                // opens at its first event.
                let opening = block
                    .as_ref()
                    .filter(|(_, o)| !o.event.all_day)
                    .map(|(_, o)| layout::open_hour(o.start, &chrono::Local));
                let hour = match opening {
                    Some(hour) => Some(hour),
                    None if !page.scrolled.get() => Some(first_hour(&found, range)),
                    None => None,
                };
                let scroll = hour.map(|hour| {
                    page.scrolled.set(true);
                    (
                        grid.scroller.clone(),
                        Rc::clone(&grid.scroll_asked),
                        grid.grid.scroll_to_hour(hour),
                    )
                });
                (block, scroll)
            }
            PageView::Month(month) => {
                month.show(range, &found, &places, &calendars, settings.working_hours);
                let block = self.pending_block(&found, |key, start| month.block_at(key, start));
                (block, None)
            }
        };
        let (block, scroll) = block;
        let is_current = self
            .pages
            .borrow()
            .get(1)
            .is_some_and(|current| Rc::ptr_eq(current, page));
        let hours = match &*view {
            PageView::Grid(grid) => Some(grid.scroller.clone()),
            PageView::Month(_) => None,
        };
        drop(view);
        if is_current && (had_focus || self.refocus_owed.replace(false)) {
            // A refill asks no scroll of its own, so the hours stay where
            // the person left them; one that does ask scrolls below.
            match hours {
                Some(hours) => pager::refocus_in_place(&hours, || self.refocus(page, focused)),
                None => self.refocus(page, focused),
            }
        }
        let open = match (is_current, block) {
            (true, Some((anchor, o))) => {
                self.pending_open.replace(None);
                let weak = Rc::downgrade(self);
                Some(move || {
                    if let Some(view) = weak.upgrade() {
                        view.show_event(&anchor, &o);
                    }
                })
            }
            _ => None,
        };
        match (scroll, open) {
            // The popover measures the block when it opens, so it waits
            // until the scroll has landed and the grid has laid the block
            // out at its new place.
            (Some((scroller, asked, y)), Some(open)) => {
                let after = scroller.clone();
                scroll_when_ready(&scroller, &asked, y, move || after_layout(&after, open));
            }
            (Some((scroller, asked, y)), None) => scroll_when_ready(&scroller, &asked, y, || {}),
            // The block has no size until the grid lays it out, and a
            // popover needs one to point at.
            (None, Some(open)) => {
                glib::idle_add_local_once(open);
            }
            (None, None) => {}
        }
    }

    /// The block and occurrence of the event waiting to open, when
    /// `found` holds it. Matches the occurrence's start as well as its
    /// key, since every occurrence of an unsplit series shares the same
    /// key and a page can show several.
    fn pending_block(
        &self,
        found: &[Occurrence],
        block_at: impl Fn(&EventKey, EpochMillis) -> Option<gtk::Widget>,
    ) -> Option<(gtk::Widget, Occurrence)> {
        let (key, start) = self.pending_open.borrow().clone()?;
        let o = found.iter().find(|o| key_of(o) == key && o.start == start)?;
        Some((block_at(&key, start)?, o.clone()))
    }

    /// The day headings over a grid: "MON 21", today's in a pill, and
    /// under it where the person works that day, from `places`, which
    /// runs parallel to `days`. Each opens its day.
    fn fill_headings(
        self: &Rc<Self>,
        headings: &headings::DayHeadings,
        days: &[NaiveDate],
        places: &[Option<String>],
    ) {
        let mut row = Vec::with_capacity(days.len());
        let today = chrono::Local::now().date_naive();
        for (index, &day) in days.iter().enumerate() {
            let place = places.get(index).cloned().flatten();
            let weekday = gtk::Label::builder()
                .label(
                    day.format_localized(&gettext("%a"), date_locale())
                        .to_string(),
                )
                .css_classes(["weekday"])
                .build();
            let date = gtk::Label::builder()
                .label(day.day().to_string())
                .css_classes(["date"])
                .build();
            let inner = gtk::Box::builder().spacing(headings::SPACING).build();
            inner.append(&weekday);
            inner.append(&date);
            let place_shown = place.as_deref().map(place_label);
            if let Some(label) = &place_shown {
                inner.append(label);
            }
            let button = gtk::Button::builder()
                .child(&inner)
                .css_classes(["flat", "day-heading"])
                .halign(gtk::Align::Center)
                .valign(gtk::Align::Center)
                .build();
            if day == today {
                button.add_css_class("today");
            }
            crate::ui::name(&button, &kinds::heading_words(&words::full_date_words(day), place.as_deref()));
            let weak = Rc::downgrade(self);
            button.connect_clicked(move |_| {
                if let Some(view) = weak.upgrade() {
                    view.open_day(day);
                }
            });
            row.push((button, place_shown));
        }
        headings.replace(row);
    }

    /// Shows `day` alone.
    fn open_day(self: &Rc<Self>, day: NaiveDate) {
        self.day.set(day);
        if self.kind.get() == ViewKind::Day {
            self.go_to(day);
        } else {
            self.set_kind(ViewKind::Day);
            self.read_sidebar(false);
        }
    }

    // ---- The list, the search and the popovers ---------------------------

    /// Reads the narrow list's first window, replacing whatever it held.
    fn fill_list(self: &Rc<Self>) {
        let read = self.next_read();
        self.list_read.set(read);
        let (first, last) = range::agenda_window(self.day.get());
        self.list_first.set(first);
        self.list_exhausted.set(false);
        self.list_loading.set(false);
        self.list_last.set(None);
        self.list_later_loading.set(false);
        let (from, to) = day_span(first, last);
        let accounts = self.account_ids();
        let weak = Rc::downgrade(self);
        let core = Rc::clone(&self.core);
        glib::spawn_future_local(async move {
            let found = core
                .read(move |c| store::occurrences(c, &accounts, from, to, CalendarScope::Shown))
                .await;
            let Some(view) = weak.upgrade() else { return };
            if view.list_read.get() != read {
                return;
            }
            match found {
                Ok(found) => {
                    let show_declined = (view.settings)().show_declined_events;
                    let pending = view.pending();
                    let found =
                        keep_agenda_events(found, show_declined, pending.as_ref(), first, last);
                    view.list
                        .show(&found, &view.calendars.borrow(), &chrono::Local);
                    view.list_last.set(Some(last));
                }
                Err(err) => tracing::warn!(%err, "could not read the calendar"),
            }
        });
    }

    /// Loads the 30 days before what the narrow list already holds, once
    /// the reader scrolls to its top. Keeps what the list holds bounded
    /// by loading in these steps rather than all at once, and stops at
    /// `range::earliest_agenda_day`, fetching the months the copy lacks
    /// from the provider as it goes.
    fn load_earlier(self: &Rc<Self>) {
        if self.list_loading.get() || self.list_exhausted.get() {
            return;
        }
        let cutoff = range::earliest_agenda_day(chrono::Local::now().date_naive());
        let last = self.list_first.get() - Days::new(1);
        if last < cutoff {
            self.list_exhausted.set(true);
            self.list.show_no_earlier();
            return;
        }
        let first = range::earlier(self.list_first.get()).max(cutoff);
        self.list_loading.set(true);
        let read = self.list_read.get();
        let (from, to) = day_span(first, last);
        // `to` is where the list's earlier reads began.
        let listed_from = to;
        let accounts = self.account_ids();
        let weak = Rc::downgrade(self);
        let core = Rc::clone(&self.core);
        glib::spawn_future_local(async move {
            // The copy may not reach this far back yet; fetch the months
            // it lacks before reading, or the list would grow by days that
            // look empty. Offline, the list stays where it is, so the
            // next scroll to the top asks again.
            let Some(view) = weak.upgrade() else { return };
            let reached = view.reach_back(from).await;
            if reached == Older::Offline {
                view.list_loading.set(false);
                return;
            }
            drop(view);
            let found = core
                .read(move |c| store::occurrences(c, &accounts, from, to, CalendarScope::Shown))
                .await;
            let Some(view) = weak.upgrade() else { return };
            view.list_loading.set(false);
            if view.list_read.get() != read {
                return;
            }
            match found {
                Ok(found) => {
                    let show_declined = (view.settings)().show_declined_events;
                    let pending = view.pending();
                    let found = shown::not_yet_listed(found, listed_from);
                    let found =
                        keep_agenda_events(found, show_declined, pending.as_ref(), first, last);
                    view.list
                        .prepend(&found, &view.calendars.borrow(), &chrono::Local);
                    view.list_first.set(first);
                    if first <= cutoff {
                        view.list_exhausted.set(true);
                        view.list.show_no_earlier();
                    }
                }
                Err(err) => tracing::warn!(%err, "could not read the calendar"),
            }
        });
    }

    /// Reads the 30 days after what the list holds once the reader
    /// scrolls near its end, up to `range::latest_agenda_day`.
    fn load_later(self: &Rc<Self>) {
        let Some(held_to) = self.list_last.get() else { return };
        if self.list_later_loading.get() {
            return;
        }
        let today = chrono::Local::now().date_naive();
        let Some((first, last)) = range::agenda_later(held_to, today) else { return };
        self.list_later_loading.set(true);
        let read = self.list_read.get();
        let (from, to) = day_span(first, last);
        let accounts = self.account_ids();
        let weak = Rc::downgrade(self);
        let core = Rc::clone(&self.core);
        glib::spawn_future_local(async move {
            let found = core
                .read(move |c| store::occurrences(c, &accounts, from, to, CalendarScope::Shown))
                .await;
            let Some(view) = weak.upgrade() else { return };
            if view.list_read.get() != read {
                return;
            }
            view.list_later_loading.set(false);
            match found {
                Ok(found) => {
                    let show_declined = (view.settings)().show_declined_events;
                    let pending = view.pending();
                    let found =
                        keep_agenda_events(found, show_declined, pending.as_ref(), first, last);
                    view.list
                        .append(&found, first, &view.calendars.borrow(), &chrono::Local);
                    view.list_last.set(Some(last));
                }
                Err(err) => tracing::warn!(%err, "could not read the calendar"),
            }
        });
    }

    fn connect_lists(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.list.connect_near_end(400.0, move || {
            if let Some(view) = weak.upgrade() {
                view.load_later();
            }
        });
        let weak = Rc::downgrade(self);
        self.list.connect_event_activated(move |o| {
            if let Some(view) = weak.upgrade() {
                let anchor = view.list.widget.clone().upcast::<gtk::Widget>();
                view.show_event(&anchor, o);
            }
        });
        let weak = Rc::downgrade(self);
        self.list.connect_scrolled_to_top(move || {
            if let Some(view) = weak.upgrade() {
                view.load_earlier();
            }
        });
        let weak = Rc::downgrade(self);
        self.more_list.connect_event_activated(move |o| {
            let Some(view) = weak.upgrade() else { return };
            view.more.popdown();
            let anchor = view.more_anchor.borrow().clone();
            if let Some(anchor) = anchor {
                view.show_event(&anchor, o);
            }
        });
        let weak = Rc::downgrade(self);
        self.results.connect_event_activated(move |o| {
            let Some(view) = weak.upgrade() else { return };
            view.search_bar.set_search_mode(false);
            view.pending_open.replace(Some((key_of(o), o.start)));
            view.go_to(date_of(o.start, o.event.all_day));
        });
    }

    fn connect_search(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.search_entry.connect_search_changed(move |entry| {
            if let Some(view) = weak.upgrade() {
                view.search(entry.text().trim());
            }
        });
        let weak = Rc::downgrade(self);
        self.search_bar.connect_search_mode_enabled_notify(move |bar| {
            let Some(view) = weak.upgrade() else { return };
            if !bar.is_search_mode() {
                view.search_entry.set_text("");
                view.search_read.set(view.next_read());
                view.show_range();
            }
        });
    }

    fn search_open(&self) -> bool {
        self.search_bar.is_search_mode() && !self.search_entry.text().trim().is_empty()
    }

    /// Lists the events that mention `text`, soonest first.
    fn search(self: &Rc<Self>, text: &str) {
        let read = self.next_read();
        self.search_read.set(read);
        if text.is_empty() {
            self.show_range();
            return;
        }
        let text = text.to_string();
        let accounts = self.account_ids();
        let now = mailrs_sync::now_millis();
        let weak = Rc::downgrade(self);
        let core = Rc::clone(&self.core);
        glib::spawn_future_local(async move {
            let found = core
                .read(move |c| {
                    store::search(c, &accounts, &text, now, CalendarScope::Shown, SEARCH_LIMIT)
                })
                .await;
            let Some(view) = weak.upgrade() else { return };
            if view.search_read.get() != read {
                return;
            }
            match found {
                Ok(found) => {
                    view.results
                        .show(&found, &view.calendars.borrow(), &chrono::Local);
                    view.views.set_visible_child_name("search");
                }
                Err(err) => tracing::warn!(%err, "could not search the calendar"),
            }
        });
    }

    /// Opens the popover for `o`, pointed at `anchor`. Edit and Delete
    /// show for an event the account may change as a whole; a guest gets
    /// Edit, limited to their own parts, and Remove.
    fn show_event(self: &Rc<Self>, anchor: &gtk::Widget, o: &Occurrence) {
        if self.editor_opened.get().is_some_and(|at| within_double_click(at.elapsed())) {
            return;
        }
        let calendar = self
            .calendars
            .borrow()
            .get(&(o.account_id, o.event.calendar.clone()))
            .cloned()
            .unwrap_or_default();
        let weak = Rc::downgrade(self);
        let occurrence = o.clone();
        let buttons = self.editing(o).popover();
        let (edit_o, delete_o) = (o.clone(), o.clone());
        let edit_view = Rc::downgrade(self);
        let delete_view = Rc::downgrade(self);
        let on_edit: Option<Box<dyn Fn()>> = buttons.filter(|b| b.edit).map(|_| {
            Box::new(move || {
                if let Some(view) = edit_view.upgrade() {
                    view.open_editor(&edit_o);
                }
            }) as Box<dyn Fn()>
        });
        let on_delete = buttons.map(|b| {
            let run = Box::new(move || {
                if let Some(view) = delete_view.upgrade() {
                    view.delete(&delete_o);
                }
            }) as Box<dyn Fn()>;
            (b.removal, run)
        });
        // A proposal moves a meeting's time, so a whole-day event and one
        // with nobody to ask get no door to it.
        let proposable = !o.event.all_day
            && (o.event.organizer.is_some() || o.event.guests.iter().any(|g| g.organizer && !g.me));
        let propose_view = Rc::downgrade(self);
        let propose_o = o.clone();
        let on_propose = proposable.then(|| {
            Box::new(move || {
                if let Some(view) = propose_view.upgrade() {
                    (view.hooks.propose)(propose_o.account_id, propose_o.clone());
                }
            }) as Box<dyn Fn()>
        });
        self.popover.show(
            anchor,
            o,
            &calendar,
            move |answer, note| {
                if let Some(view) = weak.upgrade() {
                    view.answer(occurrence.clone(), answer, note);
                }
            },
            on_propose,
            on_edit,
            on_delete,
        );
        self.find_invitation_mail(o);
    }

    /// Looks for the mail that carries `o`'s invitation, and puts "Open
    /// the invitation in Mail" on the popover once found, if it is still
    /// open on this occurrence. An event with no uid, such as one
    /// made straight on the calendar, has no invitation to find.
    fn find_invitation_mail(self: &Rc<Self>, o: &Occurrence) {
        let uid = o.event.uid.clone();
        if uid.is_empty() {
            return;
        }
        let account_id = o.account_id;
        let weak = Rc::downgrade(self);
        let for_popover = uid.clone();
        glib::spawn_future_local(async move {
            let Some(view) = weak.upgrade() else { return };
            let uid_read = uid.clone();
            let saved = view
                .core
                .read(move |c| mailrs_store::invitations::saved(c, account_id, &uid_read))
                .await;
            let Ok(Some(saved)) = saved else { return };
            let thread = view
                .core
                .read(move |c| mailrs_store::messages::thread_id_of(c, account_id, &saved.message_id))
                .await;
            let Ok(Some(thread_id)) = thread else { return };
            let open_view = Rc::downgrade(&view);
            view.popover.set_open_mail(
                account_id,
                &for_popover,
                Some(Box::new(move || {
                    if let Some(view) = open_view.upgrade() {
                        (view.hooks.open_mail)(account_id, thread_id.clone());
                    }
                })),
            );
        });
    }

    /// What the view lets a person do to `o`: see [`draft::editing`].
    /// Gates Edit and Delete in the popover, a double click or Enter
    /// opening the editor, and the Delete key.
    fn editing(&self, o: &Occurrence) -> draft::Editing {
        let access = self
            .calendars
            .borrow()
            .get(&(o.account_id, o.event.calendar.clone()))
            .map_or(Access::Reader, |c| c.access);
        let (offers, withheld) = self
            .accounts
            .borrow()
            .iter()
            .find(|(a, _, _)| a.id == o.account_id)
            .map_or((false, true), |(_, offers, withheld)| {
                (offers.calendar, withheld.calendar)
            });
        draft::editing(&o.event, access, offers, withheld)
    }

    /// The Delete key: takes the focused event off the grid at once and
    /// offers Undo, for an event the account may change as a whole or a
    /// guest's own copy of an invitation, or asks for the calendar
    /// permission the account withheld.
    pub fn delete_focused(self: &Rc<Self>) {
        let Some(o) = self.focused() else { return };
        match self.editing(&o) {
            draft::Editing::Whole | draft::Editing::Guest => self.delete(&o),
            draft::Editing::NeedsPermission => (self.hooks.needs_permission)(o.account_id),
            draft::Editing::None => {}
        }
    }

    /// A double click or Enter on a block. The editor opens over the
    /// popover a single click already opened, limited to reminders,
    /// colour and busy on someone else's event. An account that
    /// withheld the calendar permission is asked for it instead, and
    /// an event nobody here may change opens its popover.
    fn edit_or_show(self: &Rc<Self>, o: &Occurrence, anchor: Option<&gtk::Widget>) {
        match self.editing(o) {
            draft::Editing::Whole | draft::Editing::Guest => self.open_editor(o),
            draft::Editing::NeedsPermission => (self.hooks.needs_permission)(o.account_id),
            draft::Editing::None => {
                if let Some(anchor) = anchor {
                    self.show_event(anchor, o);
                }
            }
        }
    }

    /// Reads the series `o` belongs to, and asks the editor over it: its
    /// own rules, or a changed occurrence's series row's. Closes the
    /// event popover first, a no-op when it is not the popover's own
    /// Edit button asking (that already closed it), so a double click
    /// never leaves it open behind the editor.
    pub fn open_editor(self: &Rc<Self>, o: &Occurrence) {
        self.popover.hide();
        self.editor_opened.set(Some(std::time::Instant::now()));
        let this = Rc::clone(self);
        let o = o.clone();
        glib::spawn_future_local(async move {
            let rules = this.series_rules(&o).await;
            let draft = Draft::open(&o, &rules, draft::local_zone());
            this.edit(draft);
        });
    }

    /// Deletes `o` at once and offers Undo. An occurrence of a series asks
    /// which occurrences the delete covers first, and a meeting whether
    /// the guests get a cancellation, in one dialog; a delete still goes
    /// through Undo either way. A guest's delete removes only their own
    /// copy, never asks about the other guests, and is never offered
    /// "This and following", which would cut a series they do not run.
    pub fn delete(self: &Rc<Self>, o: &Occurrence) {
        let this = Rc::clone(self);
        let o = o.clone();
        glib::spawn_future_local(async move {
            let guest = draft::limited(&o.event);
            let mut offered = series::scopes(&o.event, false);
            if guest {
                offered.retain(|s| *s != RepeatScope::Following);
            }
            let guests: &[Guest] = if guest { &[] } else { &o.event.guests };
            let change = this.guest_change(o.account_id);
            let answer = match scope::question(scope::Action::Delete, &offered, guests, change) {
                Some(question) => match scope::ask(&this.page, &question, &o.event, None).await {
                    Some(answer) => answer,
                    None => return,
                },
                None => scope::unasked(scope::Action::Delete, change),
            };
            let copy = this.core.calendar_copy();
            let (account_id, occurrence) = (o.account_id, o.clone());
            let held = this
                .core
                .call(async move { copy.hold_removal(account_id, &occurrence, answer.scope, answer.notify).await })
                .await;
            match held {
                Ok(Permitted::Done(held)) => {
                    this.focus_past(&key_of(&o));
                    this.reload();
                    let said = match guest {
                        true => gettext("Removed “{title}” from your calendar"),
                        false => gettext("Deleted “{title}”"),
                    };
                    this.offer_undo(fill(&said, &[("title", &o.event.title)]), held);
                }
                Ok(Permitted::NeedsPermission) => (this.hooks.needs_permission)(account_id),
                Err(err) => (this.hooks.toast)(&with_reason(&gettext("Could not delete the event: {reason}"), &err, &[])),
            }
        });
    }

    /// Moves the keyboard focus off `key`'s block, which a delete is about
    /// to take away, onto the next event in Tab order, or the one before
    /// it for the last event. The reload that follows keeps the focus on
    /// that event by its key. A block without the focus is left alone.
    fn focus_past(&self, key: &EventKey) {
        let Some(root) = self.page.root() else { return };
        let Some(page) = self.pages.borrow().get(1).cloned() else { return };
        let view = page.view.borrow();
        if view.focused_key().as_ref() != Some(key) {
            return;
        }
        let Some(block) = view.block_of(key) else { return };
        for direction in [gtk::DirectionType::TabForward, gtk::DirectionType::TabBackward] {
            block.grab_focus();
            root.child_focus(direction);
            if view.focused_key().is_some_and(|k| k != *key) {
                return;
            }
        }
        block.grab_focus();
    }

    /// A 10-second toast with Undo for a held change. Undo puts the rows
    /// back; the toast closing any other way queues the change. Only one
    /// toast shows at a time, so two Undo offers never stack: holding
    /// another dismisses this one, whose own `dismissed` handler queues
    /// it.
    fn offer_undo(self: &Rc<Self>, said: String, held: Held) {
        // Take the toast out in a statement of its own: `dismiss` runs the
        // toast's dismissed handler at once, which borrows `toast_up`
        // again, and a borrow held across the call panics inside a GTK
        // signal handler, which aborts the app.
        let before = self.toast_up.borrow_mut().take();
        if let Some(toast) = before {
            toast.dismiss();
        }
        let id = self.holding.borrow_mut().hold(held);
        let toast = adw::Toast::builder()
            .title(crate::ui::window::toast_title(&said))
            .button_label(gettext("Undo"))
            .timeout(10)
            .build();
        let weak = Rc::downgrade(self);
        toast.connect_button_clicked(move |_| {
            if let Some(view) = weak.upgrade() {
                view.undo_held(id);
            }
        });
        let weak = Rc::downgrade(self);
        toast.connect_dismissed(move |_| {
            if let Some(view) = weak.upgrade() {
                view.toast_up.borrow_mut().take();
                view.commit_held(id);
            }
        });
        self.toast_up.replace(Some(toast.clone()));
        (self.hooks.add_toast)(toast);
        self.watch_held(id);
    }

    /// Polls whether `id` is still the copy's own waiting change, closing
    /// the toast at once if an assistant edit, made through `apply`,
    /// committed it first: an Undo that would do nothing is worse than
    /// none. Stops once the toast has gone, however it went.
    fn watch_held(self: &Rc<Self>, id: u64) {
        let weak = Rc::downgrade(self);
        glib::timeout_add_local(std::time::Duration::from_millis(500), move || {
            let Some(view) = weak.upgrade() else { return glib::ControlFlow::Break };
            let holding = view.holding.borrow();
            let Some(held) = holding.peek(id) else { return glib::ControlFlow::Break };
            if view.core.calendar_copy().still_waiting(held) {
                return glib::ControlFlow::Continue;
            }
            drop(holding);
            // As in `offer_undo`: no borrow of `toast_up` may span the
            // dismissed handler that `dismiss` runs.
            let up = view.toast_up.borrow_mut().take();
            if let Some(toast) = up {
                toast.dismiss();
            }
            glib::ControlFlow::Break
        });
    }

    /// Undoes the change the Undo toast still offers, the same way its
    /// own button does: Ctrl+Z while the toast would still be up. Does
    /// nothing once it has gone, however it went.
    pub fn undo_last_held(self: &Rc<Self>) {
        if let Some(id) = self.holding.borrow().last_id() {
            self.undo_held(id);
        }
    }

    fn undo_held(self: &Rc<Self>, id: u64) {
        let Some(held) = self.holding.borrow_mut().take(id) else { return };
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let copy = this.core.calendar_copy();
            if let Err(err) = this.core.call(async move { copy.revert(held).await }).await {
                tracing::warn!(%err, "could not take a calendar change back");
            }
            this.reload();
        });
    }

    fn commit_held(self: &Rc<Self>, id: u64) {
        let Some(held) = self.holding.borrow_mut().take(id) else { return };
        let account_id = held.account_id;
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let copy = this.core.calendar_copy();
            match this.core.call(async move { copy.commit(held).await }).await {
                Ok(()) => (this.hooks.push)(account_id),
                Err(err) => tracing::warn!(%err, "could not queue a calendar change"),
            }
        });
    }

    /// Queues every held change before the window closes or the app
    /// quits, so a delete or a move whose toast was still up is not
    /// lost. Blocks on the store: the main loop may not run again.
    pub fn commit_all_now(&self) {
        let held = self.holding.borrow_mut().drain();
        let copy = self.core.calendar_copy();
        for one in held {
            if let Err(err) = self.core.runtime().block_on(copy.commit(one)) {
                tracing::warn!(%err, "could not queue a calendar change on the way out");
            }
        }
    }

    /// Lists `occurrences` in a popover pointed at `anchor`: the events a
    /// crowded hour or day had no room for.
    fn show_more(self: &Rc<Self>, anchor: &gtk::Widget, occurrences: &[Occurrence]) {
        self.more_list
            .show(occurrences, &self.calendars.borrow(), &chrono::Local);
        self.more_anchor.replace(Some(anchor.clone()));
        if let Some(parent) = self.more.parent()
            && let Some(bounds) = anchor.compute_bounds(&parent)
        {
            self.more.set_pointing_to(Some(&gdk::Rectangle::new(
                bounds.x().round() as i32,
                bounds.y().round() as i32,
                bounds.width().round().max(1.0) as i32,
                bounds.height().round().max(1.0) as i32,
            )));
        }
        self.more.popup();
    }

    /// Sends a guest's answer, with `note` for the organizer. An
    /// occurrence of a series first asks whether the answer covers this
    /// event or all of them, as Google Calendar does. The answer goes into
    /// the calendar's queue, so the block shows it at once and it goes out
    /// now or once the network is back.
    fn answer(self: &Rc<Self>, o: Occurrence, answer: Answer, note: Option<String>) {
        let account_id = o.account_id;
        let invitations = self.core.invitations();
        let weak = Rc::downgrade(self);
        let core = Rc::clone(&self.core);
        let page = self.page.clone();
        glib::spawn_future_local(async move {
            let offered = series::answer_scopes(&o.event);
            let scope = match scope::question(scope::Action::Answer, &offered, &[], scope::Change::default()) {
                Some(question) => match scope::ask(&page, &question, &o.event, None).await {
                    Some(picked) => picked.scope.unwrap_or(RepeatScope::All),
                    None => return,
                },
                None => RepeatScope::All,
            };
            let sent = core
                .call(async move { invitations.answer_event(account_id, &o, answer, scope, note).await })
                .await;
            let Some(view) = weak.upgrade() else { return };
            match sent {
                Ok(Permitted::Done(())) => {
                    view.reload();
                    (view.hooks.push)(account_id);
                }
                Ok(Permitted::NeedsPermission) => (view.hooks.needs_permission)(account_id),
                Err(err) => (view.hooks.toast)(&with_reason(
                    &gettext("Could not send your answer: {reason}"),
                    &err,
                    &[],
                )),
            }
        });
    }

    /// Sends the account's queued calendar changes now, for an answer the
    /// invitation card queued.
    pub fn push(&self, account_id: AccountId) {
        (self.hooks.push)(account_id);
    }

    // ---- Making events ---------------------------------------------------

    /// The calendars a new event may go on, each with its account's
    /// address, as the sidebar lists them: only for an account whose
    /// provider offers a calendar and has not withheld it, so an IMAP
    /// account or one waiting on the calendar permission offers neither.
    fn writable(&self) -> Vec<(AccountId, String, Calendar)> {
        let accounts = self.accounts.borrow();
        let mut writable = self
            .calendars
            .borrow()
            .iter()
            .filter(|(_, c)| c.access.can_write())
            .filter_map(|((account_id, _), c)| {
                accounts
                    .iter()
                    .find(|(a, offers, withheld)| a.id == *account_id && offers.calendar && !withheld.calendar)
                    .map(|(a, _, _)| (*account_id, a.email.clone(), c.clone()))
            })
            .collect::<Vec<_>>();
        let order: Vec<AccountId> = accounts.iter().map(|(a, _, _)| a.id).collect();
        draft::sort_writable(&mut writable, &order);
        writable
    }

    /// [`Self::writable`] without the calendars the person took off the
    /// list, for quick create and the calendar a new event starts on.
    fn offered(&self) -> Vec<(AccountId, String, Calendar)> {
        draft::without_hidden(self.writable(), &self.hidden.borrow())
    }

    /// Insensitive, with why, while no calendar takes new events: an
    /// IMAP-only setup, or every account still starting or waiting on
    /// the calendar permission.
    fn update_new_event(&self) {
        let can = !self.offered().is_empty();
        self.new_event.set_sensitive(can);
        if can {
            crate::ui::name_with_shortcut(&self.new_event, &gettext("New Event (N)"));
        } else {
            crate::ui::name(&self.new_event, &gettext("None of your calendars take new events"));
        }
    }

    /// The draft for a new event from `start` to `end` on the default
    /// calendar, or `None` when no calendar takes new events.
    fn fresh_draft(&self, start: EpochMillis, end: EpochMillis) -> Option<Draft> {
        let last = (self.settings)().last_calendar_account;
        let (account, calendar) = draft::default_calendar(&self.offered(), last.as_deref())?;
        Some(Draft::new(account, &calendar, start, end, draft::local_zone()))
    }

    /// The occurrence whose block has the keyboard focus, wherever it is
    /// shown now: the middle page's grid or month, or the narrow list.
    fn focused(&self) -> Option<Occurrence> {
        if self.showing() == Showing::List {
            return self.list.focused();
        }
        let page = self.pages.borrow().get(1)?.clone();
        page.view.borrow().focused()
    }

    /// The focused day heading or card in the month page now on screen,
    /// for N to start a new event on.
    fn focused_day(&self) -> Option<NaiveDate> {
        let page = self.pages.borrow().get(1)?.clone();
        match &*page.view.borrow() {
            PageView::Month(month) => month.focused_day(),
            PageView::Grid(_) => None,
        }
    }

    /// The span N and the + button start from: after the focused event,
    /// else at the time last clicked, else near now, else the range's
    /// first morning.
    fn slot(&self) -> (EpochMillis, EpochMillis) {
        let range = Range::around(self.effective_kind(), self.day.get());
        let span = range.span(&chrono::Local);
        let morning = layout::instant_at(range.first, 9.0, &chrono::Local);
        let focused = self.focused().map(|o| (o.start, o.end));
        let cursor = self.pages.borrow().get(1).and_then(|page| match &*page.view.borrow() {
            PageView::Grid(grid) => grid.grid.cursor(),
            PageView::Month(_) => None,
        });
        drag::new_slot(focused, cursor, mailrs_sync::now_millis(), span, morning)
    }

    /// The + button: the editor on a new event at the slot.
    pub fn new_event(self: &Rc<Self>) {
        let (start, end) = self.slot();
        if let Some(draft) = self.fresh_draft(start, end) {
            self.edit(draft);
        }
    }

    /// N: the quick-create popover at the slot, in Day and Week, or on
    /// the focused day in Month. The narrow agenda has no grid to point
    /// at, so [`Self::quick_create_at`] opens the editor instead.
    pub fn quick_create(self: &Rc<Self>) {
        match self.effective_kind() {
            ViewKind::Month => {
                let day = self.focused_day().unwrap_or_else(|| chrono::Local::now().date_naive());
                self.quick_create_on_day(day);
            }
            _ => {
                let (start, end) = self.slot();
                self.quick_create_at(start, end);
            }
        }
    }

    /// Quick create on `day` in Month view, at the same 09:00 default N
    /// opens: a click on a cell's own empty background and N on the
    /// focused day both land here, since a month cell has no time of day
    /// of its own to click.
    pub(super) fn quick_create_on_day(self: &Rc<Self>, day: NaiveDate) {
        let nine = layout::instant_at(day, 9.0, &chrono::Local);
        self.quick_create_at(nine, nine + 3_600_000);
    }

    /// The widget to point quick create's popover at, and the slot's
    /// bounds in that widget's own coordinates. `None` in a narrow
    /// window (the list has no grid to point at), for a month day off
    /// screen, or a grid range that does not hold `start`; the caller
    /// opens the editor instead.
    ///
    /// On the time grid, the grid first scrolls so the slot shows: an
    /// evening slot sits below the hours a page opens on. The rect is
    /// then given in the scrolled window's coordinates, which the
    /// scroll does not move, clamped to what shows, and the popover
    /// opens toward the wider side of the grid.
    fn quick_anchor(
        &self,
        start: EpochMillis,
        end: EpochMillis,
    ) -> Option<(gtk::Widget, gdk::Rectangle, gtk::PositionType)> {
        if self.narrow.get() {
            return None;
        }
        let page = self.pages.borrow().get(1)?.clone();
        let view = page.view.borrow();
        match &*view {
            PageView::Grid(grid) => {
                let rect = grid.grid.slot_rect(start, end)?;
                grid.grid.show_ghost(Some((start, end)));
                let adjustment = grid.scroller.vadjustment();
                let (top, height) = (f64::from(rect.y()), f64::from(rect.height()));
                let value = quick::reveal(
                    top,
                    top + height,
                    adjustment.value(),
                    adjustment.page_size(),
                    adjustment.upper(),
                );
                adjustment.set_value(value);
                let (y, height) = quick::clamp_span(top - value, height, adjustment.page_size());
                let shown = gdk::Rectangle::new(rect.x(), y.round() as i32, rect.width(), height.round().max(1.0) as i32);
                let middle = f64::from(rect.x()) + f64::from(rect.width()) / 2.0;
                let side = quick::side(middle, f64::from(grid.grid.width()));
                Some((grid.scroller.clone().upcast(), shown, side))
            }
            PageView::Month(month) => {
                let rect = month.day_rect(date_of(start, false))?;
                let middle = f64::from(rect.x()) + f64::from(rect.width()) / 2.0;
                let side = quick::side(middle, f64::from(month.widget().width()));
                Some((month.widget(), rect, side))
            }
        }
    }

    /// Opens quick create for `start` to `end`, marking the span with a
    /// ghost card on the time grid when it points at one.
    pub(super) fn quick_create_at(self: &Rc<Self>, start: EpochMillis, end: EpochMillis) {
        let Some(draft) = self.fresh_draft(start, end) else {
            self.clear_ghost();
            return;
        };
        let Some((anchor, rect, side)) = self.quick_anchor(start, end) else {
            self.clear_ghost();
            return self.edit(draft);
        };
        let choices = self.offered();
        let current = choices
            .iter()
            .position(|(a, _, c)| *a == draft.account_id && c.id == draft.calendar)
            .unwrap_or(0);
        let when = words::span_words(start, end, false, &chrono::Local);
        let (save_view, more_view) = (Rc::clone(self), Rc::clone(self));
        let picked = Rc::new(choices.clone());
        let (save_picked, more_picked) = (Rc::clone(&picked), picked);
        // A calendar picked in the popover starts the draft again on it,
        // so its zone and reminders follow, as the editor's choice does.
        let on = move |choices: &[(AccountId, String, Calendar)], index: usize, title: String| {
            let mut draft = match choices.get(index) {
                Some((account, _, calendar)) if index != current => {
                    Draft::new(*account, calendar, start, end, draft::local_zone())
                }
                _ => draft.clone(),
            };
            draft.title = title;
            draft
        };
        let on_more = on.clone();
        self.quick.show(
            &anchor,
            &rect,
            side,
            &when,
            choices,
            current,
            move |title, index| save_view.save_draft(on(&save_picked, index, title)),
            move |title, index| more_view.edit(on_more(&more_picked, index, title)),
        );
    }

    /// The editor on `draft`.
    pub(super) fn edit(self: &Rc<Self>, draft: Draft) {
        let contacts = (self.hooks.contacts)();
        let choices = editor::Choices {
            writable: self.writable(),
            hidden: self.hidden.borrow().clone(),
            calendars: self.calendars.borrow().clone(),
            attaching: Rc::new(self.attaching()),
            moving: self
                .accounts
                .borrow()
                .iter()
                .filter(|(_, offers, _)| offers.moves_events)
                .map(|(account, _, _)| account.id)
                .collect(),
        };
        let this = Rc::clone(self);
        editor::open(&self.page, draft, choices, contacts, move |draft| this.save_draft(draft));
    }

    /// What the editor's "Attach File…" works through: the account's
    /// Drive permission, the question that asks for it, and the upload on
    /// the sync runtime.
    fn attaching(self: &Rc<Self>) -> editor::Attaching {
        let (withheld_view, ask_view) = (Rc::downgrade(self), Rc::downgrade(self));
        let core = Rc::clone(&self.core);
        let offered_view = Rc::downgrade(self);
        editor::Attaching {
            offered: Box::new(move |account| {
                offered_view.upgrade().is_some_and(|view| {
                    view.accounts.borrow().iter().any(|(a, offers, _)| a.id == account && offers.event_files)
                })
            }),
            withheld: Box::new(move |account| {
                withheld_view.upgrade().is_some_and(|view| {
                    view.accounts.borrow().iter().any(|(a, _, withheld)| a.id == account && withheld.drive)
                })
            }),
            ask: Box::new(move |account| {
                if let Some(view) = ask_view.upgrade() {
                    (view.hooks.needs_drive_permission)(account);
                }
            }),
            upload: Box::new(move |account, file, sent| {
                let copy = core.calendar_copy();
                core.runtime().spawn(async move { attachments::uploaded(copy.upload(account, file, sent).await) })
            }),
        }
    }

    /// Writes a new or changed event. A new time is confirmed first, and
    /// so is a change the guests of a meeting would see; a change to an
    /// occurrence of a series asks which occurrences it covers. All of it
    /// is one dialog, so the person answers at most once per Save. When
    /// the save moved the event and changed more, the dialog's Cancel
    /// turns only the new time down: the other edits go out at the old
    /// time.
    pub fn save_draft(self: &Rc<Self>, draft: Draft) {
        let weak = Rc::downgrade(self);
        let core = Rc::clone(&self.core);
        glib::spawn_future_local(async move {
            let occurrence = draft.occurrence.clone();
            let offered = draft.scopes();
            let action = if draft.moved() { scope::Action::Move } else { scope::Action::Edit };
            let before = draft.before();
            let always_mails = match weak.upgrade() {
                Some(view) => view.guest_change(draft.account_id).always_mails,
                None => return,
            };
            let change = match &before {
                Some(before) => {
                    let rest = draft.without_move();
                    scope::Change {
                        always_mails,
                        seen: draft::reaches_guests(before, &draft),
                        adds_guests: scope::adds_guests(&before.guests, &draft.guests),
                        more_than_time: action == scope::Action::Move && rest != *before,
                        rest_seen: draft::reaches_guests(before, &rest),
                    }
                }
                // A new event's guests get their invitation.
                None => scope::Change { seen: true, always_mails, ..scope::Change::default() },
            };
            // A guest the edit removed still hears of it.
            let guests = match &before {
                Some(before) if scope::has_other_guests(&before.guests) => before.guests.clone(),
                _ => draft.guests.clone(),
            };
            let asked = match (&draft.base, scope::question(action, &offered, &guests, change)) {
                (Some(base), Some(question)) => {
                    let Some(view) = weak.upgrade() else { return };
                    let when = words::span_words(draft.start, draft.end, draft.all_day, &draft::local_zone());
                    let when = (action == scope::Action::Move).then_some(when);
                    match scope::ask(&view.page, &question, base, when.as_deref()).await {
                        Some(answer) => answer,
                        None => return,
                    }
                }
                _ => scope::unasked(action, change),
            };
            let draft = if asked.keep_time { draft.without_move() } else { draft };
            let (scope, notify) = (asked.scope, asked.notify);
            let event = draft.to_event(
                &mailrs_sync::calendar_copy::new_event_id(),
                &mailrs_sync::calendar_copy::new_event_id(),
            );
            let account_id = draft.account_id;
            let is_new = draft.is_new();
            let copy = core.calendar_copy();
            let written = core
                .call(async move {
                    let steps = match &occurrence {
                        Some(o) => copy.change_steps(account_id, o, event, scope).await?,
                        None => vec![series::Step::Save(event)],
                    };
                    copy.apply_with(account_id, steps, notify).await
                })
                .await;
            let Some(view) = weak.upgrade() else { return };
            match written {
                Ok(Permitted::Done(())) => {
                    if is_new {
                        view.remember_account(account_id);
                    }
                    view.reload();
                    (view.hooks.push)(account_id);
                }
                Ok(Permitted::NeedsPermission) => (view.hooks.needs_permission)(account_id),
                Err(err) => (view.hooks.toast)(&with_reason(
                    &gettext("Could not save the event: {reason}"),
                    &err,
                    &[],
                )),
            }
        });
    }

    /// Remembers the account a new event went on, so the next one
    /// defaults to it.
    fn remember_account(&self, account_id: AccountId) {
        if let Some((account, _, _)) = self.accounts.borrow().iter().find(|(a, _, _)| a.id == account_id) {
            (self.hooks.change)(Change::LastCalendarAccount(account.email.clone()));
        }
    }

    // ---- The sidebar ----------------------------------------------------

    /// Does what the person chose in the calendar list. Ticking a
    /// calendar and folding an account stay on this computer; the rest
    /// go through the copy (`manage`), which sends them to the provider
    /// when the account allows it.
    fn list_changed(self: &Rc<Self>, change: ListChange) {
        match change {
            ListChange::Shown { account, calendar, shown } => self.set_shown(account, calendar, shown),
            ListChange::Folded { address, folded } => {
                (self.hooks.change)(Change::CalendarAccountFolded { email: address, folded })
            }
            other => self.manage(other),
        }
    }

    /// Shows or hides one calendar, then reads the ranges again.
    fn set_shown(self: &Rc<Self>, account_id: AccountId, calendar: String, shown: bool) {
        self.calendar_sidebar.note_shown(account_id, &calendar, shown);
        if let Some(entry) = self
            .calendars
            .borrow_mut()
            .get_mut(&(account_id, calendar.clone()))
        {
            entry.shown = shown;
        }
        let weak = Rc::downgrade(self);
        let core = Rc::clone(&self.core);
        glib::spawn_future_local(async move {
            let saved = core
                .write(move |c| store::set_shown(c, account_id, &calendar, shown))
                .await;
            let Some(view) = weak.upgrade() else { return };
            match saved {
                Ok(()) => {
                    view.reload();
                    (view.hooks.next_event)();
                }
                Err(err) => tracing::warn!(%err, "could not show or hide the calendar"),
            }
        });
    }

    /// Reads every account's calendars and the mini month's busy days,
    /// redraws the sidebar, and with `then_fill` reads the pages again,
    /// since a calendar's colour or shown flag may have changed.
    fn read_sidebar(self: &Rc<Self>, then_fill: bool) {
        if then_fill {
            self.fill_owed.set(true);
        }
        let read = self.next_read();
        self.sidebar_read.set(read);
        let accounts = self.account_ids();
        let mini = Range::around(ViewKind::Month, self.day.get());
        let (from, to) = mini.span(&chrono::Local);
        let weak = Rc::downgrade(self);
        let core = Rc::clone(&self.core);
        glib::spawn_future_local(async move {
            let found = core
                .read(move |c| {
                    let mut calendars = Vec::with_capacity(accounts.len());
                    for &id in &accounts {
                        calendars.push((id, store::calendars(c, id)?, store::unlisted(c, id)?));
                    }
                    let busy = store::occurrences(c, &accounts, from, to, CalendarScope::Shown)?;
                    Ok((calendars, busy))
                })
                .await;
            let Some(view) = weak.upgrade() else { return };
            if view.sidebar_read.get() != read {
                return;
            }
            match found {
                Ok((calendars, busy)) => view.show_sidebar(calendars, &busy, mini),
                Err(err) => tracing::warn!(%err, "could not read the calendars"),
            }
            if view.fill_owed.replace(false) {
                view.fill_all();
            }
        });
    }

    fn show_sidebar(
        &self,
        calendars: Vec<(AccountId, Vec<Calendar>, Vec<String>)>,
        busy: &[Occurrence],
        mini: Range,
    ) {
        let mut by_key: Calendars = HashMap::new();
        for (account, list, _) in &calendars {
            for calendar in list {
                by_key.insert((*account, calendar.id.clone()), calendar.clone());
            }
        }
        ensure_tints(by_key.values().map(|c| c.color.as_str()));
        self.calendars.replace(by_key);
        let rows: Vec<(Account, Offers, Withheld, Vec<Calendar>)> = self
            .accounts
            .borrow()
            .iter()
            .map(|(account, offers, withheld)| {
                let list = calendars
                    .iter()
                    .find(|(id, _, _)| *id == account.id)
                    .map(|(_, list, _)| list.clone())
                    .unwrap_or_default();
                (account.clone(), *offers, *withheld, list)
            })
            .collect();
        let show_declined = (self.settings)().show_declined_events;
        let pending = self.pending();
        let kept: Vec<Occurrence> = busy
            .iter()
            .filter(|o| shown::keep(o, show_declined, pending.as_ref()))
            .cloned()
            .collect();
        let busy_days = shown::busy_days(&kept, mini.first, mini.days, &chrono::Local);
        let today = chrono::Local::now().date_naive();
        let unlisted = calendars
            .into_iter()
            .map(|(account, _, ids)| (account, ids.into_iter().collect()))
            .collect();
        let accounts = sidebar::take_off_the_list(sidebar::sidebar_accounts(&rows), &unlisted);
        self.hidden.replace(
            accounts
                .iter()
                .flat_map(|a| a.hidden.iter().map(|c| (a.id, c.id.clone())))
                .collect(),
        );
        self.update_new_event();
        let day = self.day.get();
        let band = match self.showing() {
            Showing::List => None,
            _ => sidebar::in_view(self.effective_kind(), day),
        };
        self.calendar_sidebar.show(day, band, today, &busy_days, &accounts);
    }
}

impl PageView {
    fn block_of(&self, key: &EventKey) -> Option<gtk::Widget> {
        match self {
            PageView::Grid(grid) => grid.grid.block_of(key).or_else(|| grid.strip.block_of(key)),
            PageView::Month(month) => month.block_of(key),
        }
    }

    /// The first event Tab reaches: the all-day row comes before the
    /// hours.
    fn first_block(&self) -> Option<gtk::Widget> {
        match self {
            PageView::Grid(grid) => grid.strip.first_block().or_else(|| grid.grid.first_block()),
            PageView::Month(month) => month.first_block(),
        }
    }

    fn focused_key(&self) -> Option<EventKey> {
        match self {
            PageView::Grid(grid) => grid.grid.focused_key().or_else(|| grid.strip.focused_key()),
            PageView::Month(month) => month.focused_key(),
        }
    }

    /// The occurrence of the block that has the keyboard focus, for the
    /// Delete key.
    fn focused(&self) -> Option<Occurrence> {
        match self {
            PageView::Grid(grid) => grid.grid.focused().or_else(|| grid.strip.focused()),
            PageView::Month(month) => month.focused(),
        }
    }

    fn widget(&self) -> gtk::Widget {
        match self {
            PageView::Grid(grid) => grid.root.clone().upcast(),
            PageView::Month(month) => {
                let root = gtk::Box::builder()
                    .orientation(gtk::Orientation::Vertical)
                    .build();
                root.append(&weekday_row());
                root.append(&month.widget);
                month.widget.set_vexpand(true);
                root.upcast()
            }
        }
    }
}

/// Local midnight of `first` to local midnight after `last`, for a read
/// covering whole days.
fn day_span(first: NaiveDate, last: NaiveDate) -> (EpochMillis, EpochMillis) {
    let (from, _) = Range::around(ViewKind::Day, first).span(&chrono::Local);
    let (_, to) = Range::around(ViewKind::Day, last).span(&chrono::Local);
    (from, to)
}

/// The word under a day's heading that says where the person works:
/// "Home", "Office", or the building's name, cut short with an ellipsis
/// in a narrow column. The heading's own name speaks it.
pub(crate) fn place_label(place: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(place)
        .css_classes(["day-place"])
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .max_width_chars(12)
        .tooltip_text(place)
        .build()
}

/// Drops a declined event unless `show_declined` keeps it or `pending`
/// names it, warms the tint stylesheet for any colour among what is
/// left, and logs once when `found` came back full: `MOST_EVENTS` may
/// have cut the read to `first`..`last` short.
fn keep_agenda_events(
    found: Vec<Occurrence>,
    show_declined: bool,
    pending: Option<&(AccountId, String, String, EpochMillis)>,
    first: NaiveDate,
    last: NaiveDate,
) -> Vec<Occurrence> {
    if found.len() >= MOST_EVENTS {
        tracing::info!(
            %first,
            %last,
            "the calendar list holds more events than one read shows"
        );
    }
    let found: Vec<Occurrence> = found
        .into_iter()
        .filter(|o| shown::keep(o, show_declined, pending))
        .collect();
    ensure_tints(found.iter().filter_map(|o| o.event.color.as_deref()));
    found
}

/// "MON TUE WED …" over the month grid, starting on
/// [`crate::locale_time::week_start_weekday`].
fn weekday_row() -> gtk::Box {
    let row = gtk::Box::builder()
        .homogeneous(true)
        .margin_top(10)
        .margin_bottom(4)
        .css_classes(["day-heading"])
        .build();
    let monday = NaiveDate::from_ymd_opt(2024, 1, 1).expect("2024-01-01 is a Monday");
    for day in mailrs_domain::calendar::week::week_columns(crate::locale_time::week_start_weekday()) {
        let label = gtk::Label::builder()
            .label(
                (monday + Days::new(u64::from(day.num_days_from_monday())))
                    .format_localized(&gettext("%a"), date_locale())
                    .to_string(),
            )
            .css_classes(["weekday"])
            .build();
        row.append(&label);
    }
    row
}

/// The date an event starts on: its own UTC date when it lasts all day,
/// the local date otherwise.
fn date_of(at: EpochMillis, all_day: bool) -> NaiveDate {
    let utc = DateTime::<Utc>::from_timestamp_millis(at).unwrap_or_default();
    match all_day {
        true => utc.date_naive(),
        false => utc.with_timezone(&chrono::Local).date_naive(),
    }
}

/// The hour the grid opens at for `range`: 08:00, or earlier when a
/// timed event starts earlier on one of its days.
fn first_hour(found: &[Occurrence], range: Range) -> f64 {
    let (from, to) = range.span(&chrono::Local);
    let starts: Vec<f64> = found
        .iter()
        .filter(|o| !o.event.all_day && o.start >= from && o.start < to)
        .filter_map(|o| {
            let local = DateTime::<Utc>::from_timestamp_millis(o.start)?.with_timezone(&chrono::Local);
            let midnight = local.date_naive().and_hms_opt(0, 0, 0)?;
            Some(layout::wall_offset(o.start, midnight, &chrono::Local))
        })
        .collect();
    layout::first_hour(&starts)
}

/// Scrolls `scroller` to `y` once its content has a height to scroll in;
/// a page that was just filled has not been laid out yet.
/// Then runs `then`. `asked` counts the scrolls asked of `scroller`: one
/// still waiting when a later one is asked gives way to it.
fn scroll_when_ready(
    scroller: &gtk::ScrolledWindow,
    asked: &Rc<Cell<u64>>,
    y: f64,
    then: impl FnOnce() + 'static,
) {
    let mine = asked.get() + 1;
    asked.set(mine);
    let adjustment = scroller.vadjustment();
    if adjustment.page_size() > 0.0 && adjustment.upper() > adjustment.page_size() {
        adjustment.set_value(y.min(adjustment.upper() - adjustment.page_size()));
        then();
        return;
    }
    let handler: Rc<RefCell<Option<glib::SignalHandlerId>>> = Rc::new(RefCell::new(None));
    let slot = Rc::clone(&handler);
    let then = Cell::new(Some(then));
    let asked = Rc::clone(asked);
    let id = adjustment.connect_changed(move |adjustment| {
        let superseded = asked.get() != mine;
        if !superseded && (adjustment.page_size() <= 0.0 || adjustment.upper() <= adjustment.page_size()) {
            return;
        }
        let id = slot.borrow_mut().take();
        if let Some(id) = id {
            adjustment.disconnect(id);
        }
        if superseded {
            return;
        }
        // The scrolled window sets its own value while it lays out the
        // first time, after this signal, so the scroll waits for that.
        let adjustment = adjustment.clone();
        let then = then.take();
        let asked = Rc::clone(&asked);
        glib::idle_add_local_once(move || {
            if asked.get() != mine {
                return;
            }
            adjustment.set_value(y.min(adjustment.upper() - adjustment.page_size()));
            if let Some(then) = then {
                then();
            }
        });
    });
    handler.replace(Some(id));
}

/// Runs `f` once `widget` has been laid out after the change just made:
/// a frame's tick comes before its layout, so the second tick follows a
/// finished one.
fn after_layout(widget: &impl IsA<gtk::Widget>, f: impl FnOnce() + 'static) {
    let f = Cell::new(Some(f));
    let ticks = Cell::new(0);
    widget.add_tick_callback(move |_, _| {
        ticks.set(ticks.get() + 1);
        if ticks.get() < 2 {
            return glib::ControlFlow::Continue;
        }
        if let Some(f) = f.take() {
            f();
        }
        glib::ControlFlow::Break
    });
}

thread_local! {
    /// The one stylesheet of calendar colours for the run, and the
    /// colours it holds: rebuilt only when a colour it
    /// lacks turns up, and never one provider per reload or per window.
    static TINTS: RefCell<Option<(gtk::CssProvider, BTreeSet<String>)>> =
        const { RefCell::new(None) };
}

/// Makes sure the stylesheet has a rule for each of `colours`.
fn ensure_tints<'a>(colours: impl IntoIterator<Item = &'a str>) {
    TINTS.with(|slot| {
        let mut slot = slot.borrow_mut();
        let (provider, known) = slot.get_or_insert_with(|| {
            let provider = gtk::CssProvider::new();
            if let Some(display) = gdk::Display::default() {
                gtk::style_context_add_provider_for_display(
                    &display,
                    &provider,
                    gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
                );
            }
            (provider, BTreeSet::new())
        });
        let before = known.len();
        known.extend(
            colours
                .into_iter()
                .filter(|c| !c.is_empty())
                .map(str::to_string),
        );
        if known.len() != before {
            let all: Vec<String> = known.iter().cloned().collect();
            provider.load_from_string(&tint::stylesheet(&all));
        }
    });
}

/// Whether `since`, the time since the editor opened, is short enough
/// for a click to be the release of the double click that opened it.
fn within_double_click(since: std::time::Duration) -> bool {
    let millis = gtk::Settings::default().map_or(400, |s| s.gtk_double_click_time());
    since.as_millis() <= millis.max(0) as u128
}

#[cfg(test)]
mod refresh_tests {
    use super::should_refresh;

    #[test]
    fn a_first_press_starts_a_refresh() {
        assert!(should_refresh(false));
    }

    #[test]
    fn a_press_while_one_is_already_running_does_not_start_a_second() {
        assert!(!should_refresh(true));
    }
}
