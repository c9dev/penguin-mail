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
pub mod block;
pub mod layout;
pub mod month;
pub mod popover;
pub mod range;
pub mod shown;
pub mod sidebar;
pub mod time_grid;
pub mod tint;
pub mod words;

use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap};
use std::rc::{Rc, Weak};

use adw::prelude::*;
use chrono::{DateTime, Datelike, Days, NaiveDate, Utc};
use gtk::{gdk, glib};
use mailrs_domain::calendar::{Calendar, Occurrence};
use mailrs_domain::invitation::Answer;
use mailrs_domain::translate::{date_locale, gettext, with_reason};
use mailrs_domain::{Account, AccountId, EpochMillis};
use mailrs_store::calendar::{self as store, CalendarScope};
use mailrs_sync::Permitted;
use mailrs_sync::{Offers, Withheld};

use crate::core::Core;
use crate::settings::{Change, Settings};
use agenda::Agenda;
use block::{EventKey, key_of};
use month::MonthGrid;
use popover::EventPopover;
use range::{Range, ViewKind};
use shown::{Refocus, Showing};
use sidebar::CalendarSidebar;
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
    headings: gtk::Box,
    strip: AllDayStrip,
    scroller: gtk::ScrolledWindow,
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
    pub sidebar: gtk::ScrolledWindow,
    /// Shows the sidebar when the window is too narrow to keep it open.
    pub sidebar_button: gtk::ToggleButton,
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
    switch_slot: adw::Bin,
    bottom_slot: adw::Bin,
    search_bar: gtk::SearchBar,
    search_entry: gtk::SearchEntry,
    views: gtk::Stack,
    carousel: adw::Carousel,
    pages: RefCell<Vec<Rc<Page>>>,
    list: Rc<Agenda>,
    results: Rc<Agenda>,
    popover: Rc<EventPopover>,
    more: gtk::Popover,
    more_list: Rc<Agenda>,
    /// What the "N more" popover was opened from, for the event popover
    /// that replaces it.
    more_anchor: RefCell<Option<gtk::Widget>>,
    /// The day the view is on; every range is the one around it.
    day: Cell<NaiveDate>,
    kind: Cell<ViewKind>,
    /// The week or month the person had before Day, which List stands
    /// for in a narrow window.
    before_day: Cell<ViewKind>,
    narrow: Cell<bool>,
    /// Below the width where the sidebar folds away, the header keeps
    /// the range's bold part only.
    compact: Cell<bool>,
    month_rows: Cell<usize>,
    accounts: RefCell<Vec<CalendarAccount>>,
    calendars: RefCell<Calendars>,
    /// Counts every read, so each can tell whether a newer one replaced
    /// it: an answer can come back after the person moved on.
    reads: Cell<u64>,
    sidebar_read: Cell<u64>,
    list_read: Cell<u64>,
    /// The earliest day the narrow list already holds. `load_earlier`
    /// reads back from here and moves it once the read comes back.
    list_first: Cell<NaiveDate>,
    /// Set once `load_earlier` has read down to `range::earliest_kept_day`,
    /// so a further scroll to the top asks nothing more.
    list_exhausted: Cell<bool>,
    /// Set while an earlier-days read is in flight, so a second scroll
    /// to the top before it answers does not start another one.
    list_loading: Cell<bool>,
    search_read: Cell<u64>,
    /// Set by a sidebar read that should fill the pages once it answers.
    /// A newer read drops the older one's answer, so the flag carries
    /// the fill over to whichever read answers last.
    fill_owed: Cell<bool>,
    /// Counts `open`'s reads, so a newer one, or a move to another range,
    /// drops an older answer.
    open_read: Cell<u64>,
    /// An event to open once the page that holds it has been read.
    pending_open: RefCell<Option<EventKey>>,
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
        let switch_slot = adw::Bin::builder().child(&switch).build();

        let new_event = gtk::Button::builder()
            .icon_name("list-add-symbolic")
            .css_classes(["suggested-action", "circular"])
            .visible(false)
            .build();
        crate::ui::name(&new_event, &gettext("New Event"));
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

        let header = adw::HeaderBar::builder()
            .title_widget(&gtk::Box::new(gtk::Orientation::Horizontal, 0))
            .css_classes(["calendar-header"])
            .build();
        header.pack_start(&sidebar_button);
        header.pack_start(&title);
        header.pack_start(&today_button);
        header.pack_start(&arrows);
        header.pack_end(&search_button);
        header.pack_end(&new_event);
        header.pack_end(&switch_slot);

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
        let views = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .transition_duration(150)
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
        card.append(&views);

        // The month grid keeps fewer rows of events per day once the card
        // is short, which a breakpoint on the card's own height decides.
        let bin = adw::BreakpointBin::builder()
            .child(&card)
            .width_request(300)
            .height_request(240)
            .build();
        let short = adw::Breakpoint::new(
            adw::BreakpointCondition::parse("max-height: 660sp").expect("valid breakpoint"),
        );
        bin.add_breakpoint(short.clone());

        let bottom_slot = adw::Bin::builder()
            .halign(gtk::Align::Center)
            .margin_top(6)
            .margin_bottom(6)
            .build();

        let page = adw::ToolbarView::new();
        page.add_top_bar(&header);
        page.add_top_bar(&search_bar);
        page.set_content(Some(&bin));
        page.add_bottom_bar(&bottom_slot);
        page.set_reveal_bottom_bars(false);

        let popover = EventPopover::new(&card, &today_button);
        let more_list = Agenda::new();
        more_list.widget.set_propagate_natural_height(true);
        more_list.widget.set_min_content_width(280);
        more_list.widget.set_max_content_height(360);
        let more = gtk::Popover::builder().child(&more_list.widget).build();
        more.set_parent(&card);

        let view = Rc::new_cyclic(|weak: &Weak<CalendarView>| {
            let (on_date, on_shown, on_grant) = (weak.clone(), weak.clone(), weak.clone());
            let calendar_sidebar = CalendarSidebar::new(
                move |day| {
                    if let Some(view) = on_date.upgrade() {
                        view.go_to(day);
                    }
                },
                move |account, calendar, shown| {
                    if let Some(view) = on_shown.upgrade() {
                        view.set_shown(account, calendar, shown);
                    }
                },
                move |account| {
                    if let Some(view) = on_grant.upgrade() {
                        (view.hooks.grant)(account);
                    }
                },
            );
            calendar_sidebar.widget.set_margin_start(12);
            calendar_sidebar.widget.set_margin_end(12);
            calendar_sidebar.widget.set_margin_top(6);
            calendar_sidebar.widget.set_margin_bottom(12);
            let sidebar = gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .vexpand(true)
                .child(&calendar_sidebar.widget)
                .build();
            CalendarView {
                page,
                sidebar,
                sidebar_button,
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
                switch_slot,
                bottom_slot,
                search_bar,
                search_entry,
                views,
                carousel,
                pages: RefCell::new(Vec::new()),
                list,
                results,
                popover,
                more,
                more_list,
                more_anchor: RefCell::new(None),
                day: Cell::new(today),
                kind: Cell::new(kind),
                before_day: Cell::new(match kind {
                    ViewKind::Day => ViewKind::Week,
                    other => other,
                }),
                narrow: Cell::new(false),
                compact: Cell::new(false),
                month_rows: Cell::new(4),
                accounts: RefCell::new(Vec::new()),
                calendars: RefCell::new(HashMap::new()),
                reads: Cell::new(0),
                sidebar_read: Cell::new(0),
                list_read: Cell::new(0),
                list_first: Cell::new(today),
                list_exhausted: Cell::new(false),
                list_loading: Cell::new(false),
                search_read: Cell::new(0),
                fill_owed: Cell::new(false),
                open_read: Cell::new(0),
                pending_open: RefCell::new(None),
                switching: Cell::new(false),
                arranging: Cell::new(false),
                refocus_owed: Cell::new(false),
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
        let (apply, unapply) = (Rc::downgrade(&view), Rc::downgrade(&view));
        short.connect_apply(move |_| {
            if let Some(view) = apply.upgrade() {
                view.set_month_rows(3);
            }
        });
        short.connect_unapply(move |_| {
            if let Some(view) = unapply.upgrade() {
                view.set_month_rows(4);
            }
        });
        // The tints are stronger in dark mode (tint.rs), and the rules key
        // off this class.
        let style = adw::StyleManager::default();
        let page = view.page.downgrade();
        let mark = move |style: &adw::StyleManager| {
            let Some(page) = page.upgrade() else { return };
            match style.is_dark() {
                true => page.add_css_class("calendar-dark"),
                false => page.remove_css_class("calendar-dark"),
            }
        };
        mark(&style);
        // The style manager lives as long as the process, and a window
        // closed to the tray goes; the handler goes with the page.
        let handler = RefCell::new(Some(style.connect_dark_notify(mark)));
        view.page.connect_destroy(move |_| {
            if let Some(handler) = handler.take() {
                adw::StyleManager::default().disconnect(handler);
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

        view.build_switch();
        view.rebuild_pages();
        view.show_range();
        view
    }

    /// Reads the calendars and the ranges on screen again, as after the
    /// copy changed.
    pub fn reload(self: &Rc<Self>) {
        self.read_sidebar(true);
    }

    /// Moves the view to the range around `day`.
    pub fn go_to(self: &Rc<Self>, day: NaiveDate) {
        self.open_read.set(self.next_read());
        self.day.set(day);
        self.place_ranges();
        self.show_range();
        self.fill_all();
        self.read_sidebar(false);
    }

    /// Shows a day, a week or a month, around the day the view is on.
    pub fn set_kind(self: &Rc<Self>, kind: ViewKind) {
        if kind != ViewKind::Day {
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
    }

    pub fn today(self: &Rc<Self>) {
        self.go_to(chrono::Local::now().date_naive());
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
            None => self.go_to(shown::stepped(self.kind.get(), self.day.get(), by)),
        }
    }

    /// Goes to the day of an event the store holds and opens its
    /// popover, as the toast about a change the provider turned down
    /// does. Nothing happens when the store no longer has the event.
    pub fn open(self: &Rc<Self>, account_id: AccountId, calendar: &str, id: &str) {
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
                    let day = date_of(event.start, event.all_day);
                    view.pending_open.replace(Some(key));
                    view.go_to(day);
                }
                Ok(None) => {}
                Err(err) => tracing::warn!(%err, "could not read the event to open"),
            }
        });
    }

    /// Answers the window's narrow breakpoint: a list in place of Week
    /// and Month, and the view switch in a bar at the bottom where the
    /// header has no room for it.
    pub fn set_narrow(self: &Rc<Self>, narrow: bool) {
        if narrow == self.narrow.get() {
            return;
        }
        self.narrow.set(narrow);
        self.switch_slot.set_child(None::<&gtk::Widget>);
        self.bottom_slot.set_child(None::<&gtk::Widget>);
        match narrow {
            true => self.bottom_slot.set_child(Some(&self.switch)),
            false => self.switch_slot.set_child(Some(&self.switch)),
        }
        self.page.set_reveal_bottom_bars(narrow);
        self.build_switch();
        self.show_range();
        if self.showing() == Showing::List {
            self.fill_list();
        }
    }

    /// Puts the focus in the page, on Today, so the calendar's keys
    /// answer after the window switches to it.
    pub fn take_focus(&self) {
        self.today_button.grab_focus();
    }

    /// Answers the window's medium breakpoint, where the sidebar folds
    /// away: the header drops the year and the week number so the rest
    /// still fits.
    pub fn set_compact(&self, compact: bool) {
        self.compact.set(compact);
        match compact {
            true => self.page.add_css_class("calendar-compact"),
            false => self.page.remove_css_class("calendar-compact"),
        }
        self.show_range();
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
                _ => gettext("Month"),
            };
            self.switch.add(adw::Toggle::builder().name(name).label(&label).build());
        }
        self.switch
            .set_active_name(Some(shown::toggle_name(self.showing())));
        self.switching.set(false);
    }

    /// Brings the header and the visible view in line with the range.
    fn show_range(&self) {
        let showing = self.showing();
        self.switching.set(true);
        self.switch.set_active_name(Some(shown::toggle_name(showing)));
        self.switching.set(false);
        let range = match showing {
            Showing::List => Range::around(ViewKind::Month, self.day.get()),
            _ => Range::around(self.kind.get(), self.day.get()),
        };
        // The list names its month the way a month's title does.
        let (bold, dim, week) = range.title();
        self.title_bold.set_label(&bold);
        self.title_dim.set_label(&dim);
        self.title_week.set_label(&week);
        let roomy = !self.compact.get() && !self.narrow.get();
        self.title_dim.set_visible(roomy);
        self.title_week.set_visible(!week.is_empty() && roomy);
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
        let had_focus = self.pages.borrow().iter().any(|p| self.holds_focus(p));
        if had_focus {
            self.refocus_owed.set(true);
        }
        self.arranging.set(true);
        let old: Vec<Rc<Page>> = self.pages.replace(Vec::new());
        for page in &old {
            self.carousel.remove(&page.holder);
        }
        let current = Range::around(self.kind.get(), self.day.get());
        let mut pages = Vec::with_capacity(3);
        for range in [current.previous(), current, current.next()] {
            let page = Rc::new(Page {
                holder: adw::Bin::builder().hexpand(true).vexpand(true).build(),
                range: Cell::new(range),
                view: RefCell::new(self.page_view()),
                generation: Cell::new(0),
                scrolled: Cell::new(false),
            });
            page.holder.set_child(Some(&page.view.borrow().widget()));
            self.carousel.append(&page.holder);
            pages.push(page);
        }
        self.pages.replace(pages);
        self.carousel.scroll_to(&self.pages.borrow()[1].holder, false);
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
        let current = Range::around(self.kind.get(), self.day.get());
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

    /// A page's widgets for the current kind.
    fn page_view(self: &Rc<Self>) -> PageView {
        match self.kind.get() {
            ViewKind::Month => {
                let month = MonthGrid::new();
                month.set_rows(self.month_rows.get());
                let weak = Rc::downgrade(self);
                month.connect_day_activated(move |day| {
                    if let Some(view) = weak.upgrade() {
                        view.open_day(day);
                    }
                });
                let weak = Rc::downgrade(self);
                month.connect_event_activated(move |_, o, anchor| {
                    if let Some(view) = weak.upgrade() {
                        view.show_event(anchor, o);
                    }
                });
                let weak = Rc::downgrade(self);
                month.connect_more_clicked(move |_, day, anchor| {
                    if let Some(view) = weak.upgrade() {
                        view.show_more(anchor, day);
                    }
                });
                PageView::Month(month)
            }
            ViewKind::Day | ViewKind::Week => PageView::Grid(self.grid_page()),
        }
    }

    fn grid_page(self: &Rc<Self>) -> GridPage {
        let headings = gtk::Box::builder()
            .homogeneous(true)
            .margin_start(GUTTER as i32)
            .margin_top(11)
            .margin_bottom(10)
            .build();
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
        strip.connect_more_clicked(move |_, hidden, anchor| {
            if let Some(view) = weak.upgrade() {
                view.show_more(anchor, hidden);
            }
        });
        GridPage {
            root,
            headings,
            strip,
            scroller,
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
        let had_focus = self.holds_focus(&pages[1]);
        let arrived = &pages[if by < 0 { 0 } else { 2 }];
        self.day
            .set(shown::stepped(self.kind.get(), self.day.get(), by));
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
            self.refocus(&middle, None);
        }
        self.fill(&recycled);
        self.show_range();
        self.read_sidebar(false);
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
        let show_declined = (self.settings)().show_declined_events;
        let found: Vec<Occurrence> = found
            .into_iter()
            .filter(|o| shown::keep(o, show_declined))
            .collect();
        ensure_tints(found.iter().filter_map(|o| o.event.color.as_deref()));
        let range = page.range.get();
        // A refill replaces every block; the one with the focus comes
        // back by its event.
        let had_focus = self.holds_focus(page);
        let focused = page.view.borrow().focused_key();
        let calendars = self.calendars.borrow().clone();
        let days: Vec<NaiveDate> = (0..range.days)
            .map(|i| range.first + Days::new(u64::from(i)))
            .collect();
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
                self.fill_headings(&grid.headings, &days);
                grid.strip.show(&days, &found, &calendars);
                let now = chrono::Local::now().timestamp_millis();
                grid.grid.show(&days, &found, &calendars, now, &chrono::Local);
                if !page.scrolled.get() {
                    page.scrolled.set(true);
                    let hour = first_hour(&found, range);
                    let y = grid.grid.scroll_to_hour(hour);
                    scroll_when_ready(&grid.scroller, y);
                }
                self.pending_block(&found, |key| {
                    grid.grid.block_of(key).or_else(|| grid.strip.block_of(key))
                })
            }
            PageView::Month(month) => {
                month.show(range, &found, &calendars);
                self.pending_block(&found, |key| month.block_of(key))
            }
        };
        let is_current = self
            .pages
            .borrow()
            .get(1)
            .is_some_and(|current| Rc::ptr_eq(current, page));
        drop(view);
        if is_current && (had_focus || self.refocus_owed.replace(false)) {
            self.refocus(page, focused);
        }
        if let (true, Some((anchor, o))) = (is_current, block) {
            self.pending_open.replace(None);
            // The block has no size until the grid lays it out, and a
            // popover needs one to point at.
            let weak = Rc::downgrade(self);
            glib::idle_add_local_once(move || {
                if let Some(view) = weak.upgrade() {
                    view.show_event(&anchor, &o);
                }
            });
        }
    }

    /// The block and occurrence of the event waiting to open, when
    /// `found` holds it.
    fn pending_block(
        &self,
        found: &[Occurrence],
        block_of: impl Fn(&EventKey) -> Option<gtk::Widget>,
    ) -> Option<(gtk::Widget, Occurrence)> {
        let key = self.pending_open.borrow().clone()?;
        let o = found.iter().find(|o| key_of(o) == key)?;
        Some((block_of(&key)?, o.clone()))
    }

    /// The day headings over a grid: "MON 21", today's in a pill. Each
    /// opens its day.
    fn fill_headings(self: &Rc<Self>, headings: &gtk::Box, days: &[NaiveDate]) {
        while let Some(child) = headings.first_child() {
            headings.remove(&child);
        }
        let today = chrono::Local::now().date_naive();
        for &day in days {
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
            let inner = gtk::Box::builder().spacing(8).build();
            inner.append(&weekday);
            inner.append(&date);
            let button = gtk::Button::builder()
                .child(&inner)
                .css_classes(["flat", "day-heading"])
                .halign(gtk::Align::Center)
                .valign(gtk::Align::Center)
                .build();
            if day == today {
                button.add_css_class("today");
            }
            crate::ui::name(&button, &words::full_date_words(day));
            let weak = Rc::downgrade(self);
            button.connect_clicked(move |_| {
                if let Some(view) = weak.upgrade() {
                    view.open_day(day);
                }
            });
            headings.append(&button);
        }
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

    fn set_month_rows(&self, rows: usize) {
        self.month_rows.set(rows);
        for page in self.pages.borrow().iter() {
            if let PageView::Month(month) = &*page.view.borrow() {
                month.set_rows(rows);
            }
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
                    let found = keep_agenda_events(found, show_declined, first, last);
                    view.list
                        .show(&found, &view.calendars.borrow(), &chrono::Local);
                }
                Err(err) => tracing::warn!(%err, "could not read the calendar"),
            }
        });
    }

    /// Loads the 30 days before what the narrow list already holds, once
    /// the reader scrolls to its top. Keeps what the list holds bounded
    /// by loading in these steps rather than all at once, and stops at
    /// `range::earliest_kept_day`: the copy's own first read went back no
    /// further than a year, so nothing earlier could ever be there.
    fn load_earlier(self: &Rc<Self>) {
        if self.list_loading.get() || self.list_exhausted.get() {
            return;
        }
        let cutoff = range::earliest_kept_day(chrono::Local::now().date_naive());
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
                    let found = shown::not_yet_listed(found, listed_from);
                    let found = keep_agenda_events(found, show_declined, first, last);
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

    fn connect_lists(self: &Rc<Self>) {
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
            view.pending_open.replace(Some(key_of(o)));
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

    /// Opens the popover for `o`, pointed at `anchor`.
    fn show_event(self: &Rc<Self>, anchor: &gtk::Widget, o: &Occurrence) {
        let calendar = self
            .calendars
            .borrow()
            .get(&(o.account_id, o.event.calendar.clone()))
            .cloned()
            .unwrap_or_default();
        let weak = Rc::downgrade(self);
        let occurrence = o.clone();
        self.popover.show(anchor, o, &calendar, move |answer| {
            if let Some(view) = weak.upgrade() {
                view.answer(occurrence.clone(), answer);
            }
        });
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

    /// Sends a guest's answer for the whole series, as Google keeps one
    /// answer per series, then
    /// reads the copy again so the block shows it.
    fn answer(self: &Rc<Self>, o: Occurrence, answer: Answer) {
        let account_id = o.account_id;
        let invitations = self.core.invitations();
        let weak = Rc::downgrade(self);
        let core = Rc::clone(&self.core);
        glib::spawn_future_local(async move {
            let sent = core
                .call(async move { invitations.answer_event(account_id, &o, answer).await })
                .await;
            let Some(view) = weak.upgrade() else { return };
            match sent {
                Ok(Permitted::Done(())) => view.reload(),
                Ok(Permitted::NeedsPermission) => (view.hooks.needs_permission)(account_id),
                Err(err) => (view.hooks.toast)(&with_reason(
                    &gettext("Could not send your answer: {reason}"),
                    &err,
                    &[],
                )),
            }
        });
    }

    // ---- The sidebar ----------------------------------------------------

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
                Ok(()) => view.reload(),
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
                        calendars.push((id, store::calendars(c, id)?));
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
        calendars: Vec<(AccountId, Vec<Calendar>)>,
        busy: &[Occurrence],
        mini: Range,
    ) {
        let mut by_key: Calendars = HashMap::new();
        for (account, list) in &calendars {
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
                    .find(|(id, _)| *id == account.id)
                    .map(|(_, list)| list.clone())
                    .unwrap_or_default();
                (account.clone(), *offers, *withheld, list)
            })
            .collect();
        let show_declined = (self.settings)().show_declined_events;
        let kept: Vec<Occurrence> = busy
            .iter()
            .filter(|o| shown::keep(o, show_declined))
            .cloned()
            .collect();
        let busy_days = shown::busy_days(&kept, mini.first, mini.days, &chrono::Local);
        let today = chrono::Local::now().date_naive();
        self.calendar_sidebar.show(
            self.day.get(),
            today,
            &busy_days,
            &sidebar::sidebar_accounts(&rows),
        );
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

/// Drops a declined event unless `show_declined` keeps it, warms the
/// tint stylesheet for any colour among what is left, and logs once
/// when `found` came back full: `MOST_EVENTS` may have cut the read to
/// `first`..`last` short.
fn keep_agenda_events(
    found: Vec<Occurrence>,
    show_declined: bool,
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
        .filter(|o| shown::keep(o, show_declined))
        .collect();
    ensure_tints(found.iter().filter_map(|o| o.event.color.as_deref()));
    found
}

/// "MON TUE WED …" over the month grid.
fn weekday_row() -> gtk::Box {
    let row = gtk::Box::builder()
        .homogeneous(true)
        .margin_top(10)
        .margin_bottom(4)
        .css_classes(["day-heading"])
        .build();
    let monday = NaiveDate::from_ymd_opt(2024, 1, 1).expect("2024-01-01 is a Monday");
    for i in 0..7u64 {
        let label = gtk::Label::builder()
            .label(
                (monday + Days::new(i))
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
fn scroll_when_ready(scroller: &gtk::ScrolledWindow, y: f64) {
    let adjustment = scroller.vadjustment();
    if adjustment.page_size() > 0.0 && adjustment.upper() > adjustment.page_size() {
        adjustment.set_value(y.min(adjustment.upper() - adjustment.page_size()));
        return;
    }
    let handler: Rc<RefCell<Option<glib::SignalHandlerId>>> = Rc::new(RefCell::new(None));
    let slot = Rc::clone(&handler);
    let id = adjustment.connect_changed(move |adjustment| {
        if adjustment.page_size() <= 0.0 || adjustment.upper() <= adjustment.page_size() {
            return;
        }
        if let Some(id) = slot.borrow_mut().take() {
            adjustment.disconnect(id);
        }
        // The scrolled window sets its own value while it lays out the
        // first time, after this signal, so the scroll waits for that.
        let adjustment = adjustment.clone();
        glib::idle_add_local_once(move || {
            adjustment.set_value(y.min(adjustment.upper() - adjustment.page_size()));
        });
    });
    handler.replace(Some(id));
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
