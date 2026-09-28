//! `CalendarSidebar`: the mini month a person jumps around with, and the
//! calendar list they show or hide calendars from. What each account's
//! row says is worked out in pure functions ([`sidebar_accounts`]) so
//! the account's offers and consent, not a widget, decide it.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use adw::prelude::*;
use chrono::{Datelike, Days, NaiveDate};
use gtk::{gdk, gio, glib};
use mailrs_domain::calendar::Calendar;
use mailrs_domain::translate::{date_locale, fill, fill_plural, gettext};
use mailrs_domain::{Account, AccountId, EpochMillis};
use mailrs_sync::{Missing, Offers, Waiting, Withheld};

use super::range::{Range, ViewKind};
use super::tint;
use super::words;

/// How far an account's calendars reach into the sidebar, worked out
/// from what its provider offers and what its own consent withheld.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CalendarReach {
    /// Every calendar the account offers.
    Calendars(Vec<Calendar>),
    /// The primary calendar only: the person granted `calendar.events`
    /// but not the list scope.
    PrimaryOnly(Vec<Calendar>),
    /// The person left the calendar scope unticked, or has never been
    /// asked: no calendars, a line saying so, and Grant Access.
    Withheld,
    /// The provider keeps no calendar Penguin Mail can reach, with the
    /// reason from `offered::reason`.
    NotOffered(String),
}

/// One account's row in the calendar list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidebarAccount {
    pub id: AccountId,
    pub address: String,
    pub reach: CalendarReach,
    /// The calendars the person took off the list, which the "Hidden
    /// Calendars" menu at its foot offers to put back.
    pub hidden: Vec<Calendar>,
}

/// The sidebar's rows from each account's provider offers, its own
/// consent, and the calendars the local copy actually holds for it. An
/// account not yet started passes `Offers::EVERYTHING`/`Withheld::NONE`
/// (`offered::offers_for`/`withheld_for`), so it reads as `Calendars`
/// until a read says otherwise, as every other feature treats an
/// account before it starts.
pub fn sidebar_accounts(
    accounts: &[(Account, Offers, Withheld, Vec<Calendar>)],
) -> Vec<SidebarAccount> {
    accounts
        .iter()
        .map(|(account, offers, withheld, calendars)| {
            let reach = if !offers.calendar {
                CalendarReach::NotOffered(crate::offered::reason(account, Missing::Calendar))
            } else if withheld.calendar {
                CalendarReach::Withheld
            } else if withheld.calendar_list {
                CalendarReach::PrimaryOnly(calendars.clone())
            } else {
                CalendarReach::Calendars(calendars.clone())
            };
            SidebarAccount {
                id: account.id,
                address: account.email.clone(),
                reach,
                hidden: Vec::new(),
            }
        })
        .collect()
}

/// Moves each calendar in `unlisted`, by account, from the list to the
/// account's hidden calendars. The list stays in its own order.
pub fn take_off_the_list(
    mut rows: Vec<SidebarAccount>,
    unlisted: &HashMap<AccountId, HashSet<String>>,
) -> Vec<SidebarAccount> {
    for row in &mut rows {
        let Some(ids) = unlisted.get(&row.id) else {
            continue;
        };
        if let CalendarReach::Calendars(list) | CalendarReach::PrimaryOnly(list) = &mut row.reach {
            let (hidden, kept) = std::mem::take(list)
                .into_iter()
                .partition(|calendar| ids.contains(&calendar.id));
            *list = kept;
            row.hidden = hidden;
        }
    }
    rows
}

/// How many calendars every account together took off the list.
pub fn hidden_count(rows: &[SidebarAccount]) -> usize {
    rows.iter().map(|row| row.hidden.len()).sum()
}

/// The first and last day the grid shows, for the mini month's band: a
/// week's seven days, or a month's own days without the ones before and
/// after that fill its grid. A single day gets no band, since the
/// selected day already marks it.
pub fn in_view(kind: ViewKind, day: NaiveDate) -> Option<(NaiveDate, NaiveDate)> {
    match kind {
        ViewKind::Day => None,
        ViewKind::Week => {
            let range = Range::around(kind, day);
            Some((range.first, range.first + Days::new(u64::from(range.days) - 1)))
        }
        ViewKind::Month => {
            let first = day.with_day(1).unwrap_or(day);
            Some((first, adjacent_month(first, 1) - Days::new(1)))
        }
    }
}

/// How one mini month day looks. `today` fills it with the accent,
/// `selected` tints it, and a day `in_view` sits on the faint band, which
/// rounds off at `band_start` and `band_end`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DayLook {
    pub today: bool,
    pub selected: bool,
    pub outside: bool,
    pub in_view: bool,
    pub band_start: bool,
    pub band_end: bool,
}

/// How `date`, in `column` of the mini month (0 to 6), looks while the
/// month starting on `month` shows, the view sits on `selected`, and the
/// grid shows `band` ([`in_view`]). The band rounds off where the range
/// starts or ends and at each row's edges.
pub fn day_look(
    date: NaiveDate,
    column: usize,
    today: NaiveDate,
    selected: NaiveDate,
    month: NaiveDate,
    band: Option<(NaiveDate, NaiveDate)>,
) -> DayLook {
    let in_view = band.is_some_and(|(first, last)| (first..=last).contains(&date));
    DayLook {
        today: date == today,
        selected: date == selected,
        outside: date.month() != month.month() || date.year() != month.year(),
        in_view,
        band_start: in_view && (column == 0 || band.is_some_and(|(first, _)| first == date)),
        band_end: in_view && (column == 6 || band.is_some_and(|(_, last)| last == date)),
    }
}

/// The day button's own classes for `look`.
pub fn day_classes(look: DayLook) -> Vec<&'static str> {
    let mut classes = vec!["flat"];
    if look.today {
        classes.push("today");
    }
    if look.selected {
        classes.push("selected");
    }
    if look.outside {
        classes.push("outside");
    }
    classes
}

/// The classes of the cell behind a day button, which draws the band.
pub fn band_classes(look: DayLook) -> Vec<&'static str> {
    let mut classes = vec!["mini-day"];
    if look.in_view {
        classes.push("in-view");
    }
    if look.band_start {
        classes.push("band-start");
    }
    if look.band_end {
        classes.push("band-end");
    }
    classes
}

/// The accounts whose invitations "Waiting for your answer" lists: those
/// whose provider offers a calendar the person has not withheld. The
/// copy keeps an account's events after it withdraws the calendar
/// permission, and a card for one of them would lead to an event the
/// person cannot answer here.
pub fn waiting_accounts(accounts: &[(Account, Offers, Withheld)]) -> Vec<AccountId> {
    accounts
        .iter()
        .filter(|(_, offers, withheld)| offers.calendar && !withheld.calendar)
        .map(|(account, _, _)| account.id)
        .collect()
}

/// The first day, on or before `day`, of the week
/// [`crate::locale_time::week_start_weekday`] starts: the mini month's
/// own copy of `range::week_start_of` (private there).
fn week_start_of(day: NaiveDate) -> NaiveDate {
    mailrs_domain::calendar::week::week_start_on_or_before(day, crate::locale_time::week_start_weekday())
}

/// The 1st of the month `step` months from the one `month` falls in, for
/// the mini month's arrows. Counted from the month shown, not from the
/// grid's first cell, which sits in the month before whenever the 1st is
/// not a Monday.
fn adjacent_month(month: NaiveDate, step: i32) -> NaiveDate {
    let first = month.with_day(1).unwrap_or(month);
    let moved = match step >= 0 {
        true => first.checked_add_months(chrono::Months::new(step.unsigned_abs())),
        false => first.checked_sub_months(chrono::Months::new(step.unsigned_abs())),
    };
    moved.unwrap_or(first)
}

/// How many weeks the mini month shows for the month starting on
/// `first`: only those holding one of its days, as the mockup draws
/// September 2026 in five rows.
fn weeks_shown(first: NaiveDate) -> usize {
    let last = adjacent_month(first, 1) - Days::new(1);
    let days = (last - week_start_of(first)).num_days() as usize + 1;
    days.div_ceil(7)
}

/// "M", "T", "W", … for the mini month's weekday row, starting on
/// [`crate::locale_time::week_start_weekday`], from a known Monday so
/// the locale's own weekday names decide the letter.
fn weekday_initials() -> Vec<String> {
    let monday = NaiveDate::from_ymd_opt(2024, 1, 1).expect("2024-01-01 is a Monday");
    mailrs_domain::calendar::week::week_columns(crate::locale_time::week_start_weekday())
        .into_iter()
        .map(|day| {
            (monday + Days::new(u64::from(day.num_days_from_monday())))
                .format_localized(&gettext("%a"), date_locale())
                .to_string()
                .chars()
                .next()
                .map(|c| c.to_string())
                .unwrap_or_default()
        })
        .collect()
}

/// A "Waiting for your answer" card's height and the gap between two,
/// from the mockup.
const WAITING_CARD: i32 = 42;
const WAITING_GAP: i32 = 8;

/// What the person changed in the calendar list. Every choice here stays
/// on this computer: the account may only read Google's calendar list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListChange {
    /// Ticked or unticked a calendar's check.
    Shown { account: AccountId, calendar: String, shown: bool },
    /// Took a calendar off the list, or put it back.
    Listed { account: AccountId, calendar: String, listed: bool },
    /// Gave a calendar a colour of its own, or `None` for the provider's.
    Color { account: AccountId, calendar: String, color: Option<String> },
    /// Folded an account's calendars under its heading, or opened them.
    Folded { address: String, folded: bool },
}

type OnDate = dyn Fn(NaiveDate);
type OnChange = dyn Fn(ListChange);
type OnGrant = dyn Fn(AccountId);
/// Opens the occurrence a "Waiting for your answer" card names: the
/// account, the calendar, the event's id and the occurrence's own start,
/// exactly what `CalendarView::open` takes.
type OnOpenWaiting = dyn Fn(AccountId, String, String, EpochMillis);
/// Opens a "Waiting for your answer" card's "Open mail" door: the
/// account and the thread its invitation arrived in.
type OnOpenMail = dyn Fn(AccountId, String);

/// One mini month day button, and the labels `show` fills each redraw.
/// `date` is shared with the button's own click closure through an `Rc`,
/// so `show` moving it forward is what the closure reads at the next
/// click, not a stale copy taken when the button was built.
struct MiniDay {
    /// The cell behind the button, which draws the band for the week or
    /// month in view edge to edge, where the button is only as wide as
    /// its number.
    cell: gtk::Box,
    button: gtk::Button,
    number: gtk::Label,
    dot: gtk::Widget,
    date: Rc<Cell<NaiveDate>>,
}

pub struct CalendarSidebar {
    /// The whole sidebar: the mini month and the calendar list scroll,
    /// and "Waiting for your answer" stays pinned below them.
    pub widget: gtk::Box,
    month_title: gtk::Label,
    /// The 1st of the month the mini month shows, which its arrows step
    /// from.
    month: Rc<Cell<NaiveDate>>,
    days: Vec<MiniDay>,
    /// The mini month's "M T W T F S S" row, kept so [`Self::week_start_changed`]
    /// can swap its letters for the new order without rebuilding the
    /// whole sidebar.
    weekday_labels: Vec<gtk::Label>,
    calendar_list: gtk::Box,
    /// "Offline, last updated 14:32" under the mini month, hidden while
    /// nothing is wrong. Sits above `calendar_list`, not under it, so it
    /// stays on screen without scrolling whatever that list holds.
    offline_line: gtk::Label,
    /// What the calendar list was last built from. A redraw that would
    /// build the same rows leaves them, and the focus on one of them,
    /// where they are.
    listed: RefCell<Vec<SidebarAccount>>,
    /// The "Waiting for your answer" section pinned to the sidebar's
    /// foot, hidden while nothing is waiting.
    waiting_section: gtk::Box,
    waiting_list: gtk::ListBox,
    /// What the waiting list was last built from, read by its own
    /// `row-activated` handler to say which occurrence a card opens, and
    /// compared before a redraw to leave unchanged rows where they are.
    waiting_shown: Rc<RefCell<Vec<Waiting>>>,
    /// The accounts, by lower-case address, whose calendars sit folded
    /// under their heading.
    folded: Rc<RefCell<HashSet<String>>>,
    on_date: Rc<OnDate>,
    on_change: Rc<OnChange>,
    on_grant: Rc<OnGrant>,
    on_open_mail: Rc<OnOpenMail>,
}

impl CalendarSidebar {
    /// `on_date` runs for a day cell, and for the mini month's own
    /// Previous/Next Month arrows with the 1st of that month. The view
    /// goes to the date and calls `show` again, so the grid and the mini
    /// month always show the same month. `on_change` hears each choice
    /// made in the calendar list.
    pub fn new(
        on_date: impl Fn(NaiveDate) + 'static,
        on_change: impl Fn(ListChange) + 'static,
        on_grant: impl Fn(AccountId) + 'static,
        on_open_waiting: impl Fn(AccountId, String, String, EpochMillis) + 'static,
        on_open_mail: impl Fn(AccountId, String) + 'static,
    ) -> Rc<CalendarSidebar> {
        let on_date: Rc<OnDate> = Rc::new(on_date);
        let on_change: Rc<OnChange> = Rc::new(on_change);
        let on_grant: Rc<OnGrant> = Rc::new(on_grant);
        let on_open_waiting: Rc<OnOpenWaiting> = Rc::new(on_open_waiting);
        let on_open_mail: Rc<OnOpenMail> = Rc::new(on_open_mail);

        let widget = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .build();
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(10)
            // The redesign's sidebar card (`Sidebar::page`, A2) sits 8 px
            // in from the window, and these 18 px inside it put the mini
            // month's title and the calendar list at x 26 of the window,
            // where calendar_mock.py draws them.
            .margin_start(18)
            .margin_end(18)
            .margin_top(6)
            .margin_bottom(12)
            .build();
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&content)
            .build();
        // The pinned Waiting section below cuts the list at whatever pixel
        // is left, often through a line of text. A short fade in the
        // sidebar's own colour marks that more sits below instead. GTK's
        // CSS has no mask-image, so the fade is a strip laid over the
        // scroller's foot, shown only while the list runs past it.
        let fade = gtk::Box::builder()
            .css_classes(["list-fade"])
            .height_request(24)
            .valign(gtk::Align::End)
            .can_target(false)
            .can_focus(false)
            .accessible_role(gtk::AccessibleRole::Presentation)
            .build();
        let overlay = gtk::Overlay::builder().child(&scroller).vexpand(true).build();
        overlay.add_overlay(&fade);
        let adjustment = scroller.vadjustment();
        let show_fade = {
            let fade = fade.clone();
            move |a: &gtk::Adjustment| {
                fade.set_visible(more_below(a.value(), a.page_size(), a.upper()));
            }
        };
        show_fade(&adjustment);
        adjustment.connect_changed(show_fade.clone());
        adjustment.connect_value_changed(show_fade);
        widget.append(&overlay);

        let month_title = gtk::Label::builder()
            .css_classes(["mini-month-title"])
            .hexpand(true)
            .xalign(0.0)
            .build();
        // The mockup draws the month arrows as the glyphs ‹ and ›, lighter
        // than the header's arrows. Each glyph is a child label, since GTK
        // names a button after its own label, over the names below.
        let previous = gtk::Button::builder()
            .child(&gtk::Label::new(Some("‹")))
            .css_classes(["flat", "dim-label"])
            .build();
        let next = gtk::Button::builder()
            .child(&gtk::Label::new(Some("›")))
            .css_classes(["flat", "dim-label"])
            .build();
        crate::ui::name(&previous, &gettext("Previous Month"));
        crate::ui::name(&next, &gettext("Next Month"));
        let header = gtk::Box::builder().spacing(0).css_classes(["mini-month-header"]).build();
        header.append(&month_title);
        header.append(&previous);
        header.append(&next);
        content.append(&header);

        let weekdays = gtk::Grid::builder()
            .column_homogeneous(true)
            .margin_top(4)
            .build();
        let mut weekday_labels = Vec::with_capacity(7);
        for (column, initial) in weekday_initials().into_iter().enumerate() {
            let label = gtk::Label::builder()
                .label(&initial)
                .css_classes(["mini-month-weekday"])
                .build();
            weekdays.attach(&label, column as i32, 0, 1, 1);
            weekday_labels.push(label);
        }
        content.append(&weekdays);

        let mini = gtk::Grid::builder()
            .row_spacing(4)
            .valign(gtk::Align::Start)
            .column_homogeneous(true)
            .css_classes(["mini-month"])
            .build();
        // A placeholder date for each button before the first `show`;
        // never read, since `connect_clicked` always fires after a real
        // date landed there.
        let placeholder = chrono::Local::now().date_naive();
        let mut days = Vec::with_capacity(42);
        for row in 0..6i32 {
            for column in 0..7i32 {
                let number = gtk::Label::new(None);
                // The dot hangs under the number without taking a line of
                // its own, so each day is the mockup's 26 pixels tall.
                let dot = gtk::Box::builder()
                    .css_classes(["dot"])
                    .halign(gtk::Align::Center)
                    .valign(gtk::Align::End)
                    .margin_bottom(2)
                    .can_target(false)
                    .build();
                let inner = gtk::Overlay::builder().child(&number).build();
                inner.add_overlay(&dot);
                let button = gtk::Button::builder()
                    .child(&inner)
                    .halign(gtk::Align::Center)
                    .hexpand(true)
                    .build();
                let cell = gtk::Box::builder().css_classes(["mini-day"]).build();
                cell.append(&button);
                mini.attach(&cell, column, row, 1, 1);
                let date = Rc::new(Cell::new(placeholder));
                days.push(MiniDay {
                    cell,
                    button,
                    number,
                    dot: dot.upcast(),
                    date,
                });
            }
        }
        content.append(&mini);

        // Offline, or the account's last calendar sync failed: a small
        // row right under the mini month, hidden while all is well. It
        // sits above the calendar list, not under it, so it is on
        // screen without scrolling however many calendars are listed or
        // how short the window is; the sidebar's redesign can move it,
        // and `dim_line`'s own styling keeps it looking at home here in
        // the meantime.
        let offline_line = dim_line("");
        offline_line.set_visible(false);
        content.append(&offline_line);

        let calendar_list = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(0)
            .margin_top(8)
            .build();
        content.append(&calendar_list);

        // calendar_mock.py sets the heading's text at x 26 of the window,
        // like the mini month, and the cards from 18 to 238; the section
        // sits 10 px into the card, so the heading takes 8 more.
        let waiting_heading = gtk::Label::builder()
            .label(gettext("Waiting for your answer"))
            .css_classes(["waiting-heading"])
            .xalign(0.0)
            .margin_start(8)
            .build();
        let waiting_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["waiting-list"])
            .build();
        // The mockup pins the section to the sidebar's foot with room for
        // two whole cards, 42 px each with 8 px between. More than two
        // scroll inside it, so none is ever cut off by what sits below
        // the sidebar.
        let waiting_scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(2 * WAITING_CARD + WAITING_GAP)
            .child(&waiting_list)
            .build();
        let waiting_section = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(10)
            .margin_start(10)
            .margin_end(10)
            .margin_top(12)
            // The mockup leaves 36 px under the last card.
            .margin_bottom(36)
            .visible(false)
            .build();
        waiting_section.append(&waiting_heading);
        waiting_section.append(&waiting_scroller);
        widget.append(&waiting_section);
        let waiting_shown: Rc<RefCell<Vec<Waiting>>> = Rc::new(RefCell::new(Vec::new()));
        waiting_list.connect_row_activated({
            let (shown, open_waiting) = (Rc::clone(&waiting_shown), Rc::clone(&on_open_waiting));
            move |_, row| {
                let Some(w) = shown.borrow().get(row.index() as usize).cloned() else {
                    return;
                };
                open_waiting(w.account_id, w.calendar, w.id, w.start);
            }
        });

        let sidebar = Rc::new(CalendarSidebar {
            widget,
            month_title,
            month: Rc::new(Cell::new(placeholder)),
            days,
            weekday_labels,
            calendar_list,
            offline_line,
            listed: RefCell::new(Vec::new()),
            waiting_section,
            waiting_list,
            waiting_shown,
            folded: Rc::new(RefCell::new(HashSet::new())),
            on_date,
            on_change,
            on_grant,
            on_open_mail,
        });
        sidebar
            .widget
            .insert_action_group("calendars", Some(&list_actions(&sidebar.on_change)));

        for day in &sidebar.days {
            let on_date = Rc::clone(&sidebar.on_date);
            let date = Rc::clone(&day.date);
            day.button.connect_clicked(move |_| on_date(date.get()));
        }
        for (button, step) in [(&previous, -1), (&next, 1)] {
            let on_date = Rc::clone(&sidebar.on_date);
            let month = Rc::clone(&sidebar.month);
            button.connect_clicked(move |_| on_date(adjacent_month(month.get(), step)));
        }

        sidebar
    }

    /// Redraws the mini month around `selected`, the day the view sits
    /// on, with `band` ([`in_view`]) marking what the grid shows, and
    /// rebuilds the calendar list from `accounts` ([`sidebar_accounts`]
    /// and [`take_off_the_list`]) when it differs from the list on
    /// screen.
    pub fn show(
        &self,
        selected: NaiveDate,
        band: Option<(NaiveDate, NaiveDate)>,
        today: NaiveDate,
        busy_days: &HashSet<NaiveDate>,
        accounts: &[SidebarAccount],
    ) {
        self.show_month(selected, band, today, busy_days);
        if *self.listed.borrow() != accounts {
            self.rebuild_calendar_list(accounts);
            self.listed.replace(accounts.to_vec());
        }
    }

    /// Redraws the "Waiting for your answer" section from `waiting`
    /// ([`mailrs_sync::Invitations::waiting_for_answer`]'s own order),
    /// hiding the section while nothing is waiting. Rebuilt only when
    /// the rows differ from what is already shown, so a screen reader
    /// mid-walk keeps its place, as [`Self::show`] does for the calendar
    /// list.
    pub fn show_waiting(&self, waiting: &[Waiting]) {
        if self.waiting_shown.borrow().as_slice() == waiting {
            return;
        }
        while let Some(child) = self.waiting_list.first_child() {
            self.waiting_list.remove(&child);
        }
        for w in waiting {
            self.waiting_list.append(&self.waiting_row(w));
        }
        self.waiting_section.set_visible(!waiting.is_empty());
        self.waiting_shown.replace(waiting.to_vec());
    }

    /// One "Waiting for your answer" card: the mail icon, the title, and
    /// the day and time with an "Open mail" door beside it. The door
    /// shows only while the store holds the message the invitation came
    /// in; without it the card stays, since it still opens the event.
    fn waiting_row(&self, w: &Waiting) -> gtk::ListBoxRow {
        let icon = gtk::Image::from_icon_name("mail-unread-symbolic");
        icon.add_css_class("waiting-icon");
        icon.set_pixel_size(16);
        let title = gtk::Label::builder()
            .label(&w.title)
            .css_classes(["waiting-title"])
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .single_line_mode(true)
            .build();
        // The mockup sets the line as one run of text, "Wed 15:00 · Open
        // mail", so the pieces sit with no spacing of their own and the
        // dot carries its single spaces.
        let when_row = gtk::Box::builder().spacing(0).build();
        when_row.append(
            &gtk::Label::builder()
                .label(words::waiting_when_words(w.start, w.all_day, &chrono::Local))
                .css_classes(["waiting-when", "dim-label"])
                .xalign(0.0)
                .build(),
        );
        if let Some(thread_id) = &w.thread_id {
            when_row.append(&gtk::Label::builder().label(" · ").css_classes(["waiting-when", "dim-label"]).build());
            // A child label, since GTK names a button after its own label
            // over the name set below, and each card's door must say
            // which event's mail it opens.
            let mail_button = gtk::Button::builder()
                .child(
                    &gtk::Label::builder()
                        .label(gettext("Open mail"))
                        .css_classes(["waiting-when", "dim-label"])
                        .build(),
                )
                .css_classes(["flat", "waiting-mail-link"])
                .valign(gtk::Align::Center)
                .build();
            crate::ui::name(&mail_button, &words::waiting_mail_name(&w.title));
            let (on_open_mail, account_id, thread_id) =
                (Rc::clone(&self.on_open_mail), w.account_id, thread_id.clone());
            mail_button.connect_clicked(move |_| on_open_mail(account_id, thread_id.clone()));
            when_row.append(&mail_button);
        }
        let text = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .valign(gtk::Align::Center)
            .hexpand(true)
            .spacing(0)
            .build();
        text.append(&title);
        text.append(&when_row);
        let content = gtk::Box::builder()
            .spacing(8)
            .margin_start(10)
            .margin_end(10)
            .valign(gtk::Align::Center)
            .build();
        content.append(&icon);
        content.append(&text);
        let row = gtk::ListBoxRow::builder()
            .child(&content)
            .css_classes(["waiting-card"])
            .activatable(true)
            .build();
        crate::ui::describe(&row, &w.title, &words::waiting_card_detail(w.start, w.all_day, &chrono::Local));
        row
    }

    /// Records that the person showed or hid a calendar with its own
    /// check, which already says so, so the next `show` finds nothing
    /// new to rebuild.
    pub fn note_shown(&self, account_id: AccountId, calendar_id: &str, shown: bool) {
        for account in self.listed.borrow_mut().iter_mut() {
            if account.id != account_id {
                continue;
            }
            if let CalendarReach::Calendars(calendars) | CalendarReach::PrimaryOnly(calendars) =
                &mut account.reach
            {
                for calendar in calendars.iter_mut().filter(|c| c.id == calendar_id) {
                    calendar.shown = shown;
                }
            }
        }
    }

    /// Swaps the mini month's "M T W T F S S" row for the new order
    /// after the "Week Starts On" choice changes. The day numbers move
    /// with it the next time [`Self::show_month`] runs.
    pub fn week_start_changed(&self) {
        for (label, initial) in self.weekday_labels.iter().zip(weekday_initials()) {
            label.set_label(&initial);
        }
    }

    /// Shows or hides the "Offline, last updated 14:32" line under the
    /// calendar list; `None` hides it. [`super::words::offline_line`]
    /// decides the words.
    pub fn set_offline_line(&self, text: Option<&str>) {
        match text {
            Some(text) => {
                self.offline_line.set_label(text);
                self.offline_line.set_visible(true);
            }
            None => self.offline_line.set_visible(false),
        }
    }

    /// Sets the accounts whose calendars start folded, by lower-case
    /// address, before the first [`Self::show`] builds the list.
    pub fn set_folded(&self, folded: HashSet<String>) {
        self.folded.replace(folded);
    }

    /// Redraws the mini month around `selected` alone.
    fn show_month(
        &self,
        selected: NaiveDate,
        band: Option<(NaiveDate, NaiveDate)>,
        today: NaiveDate,
        busy_days: &HashSet<NaiveDate>,
    ) {
        let first_of_month = selected.with_day(1).unwrap_or(selected);
        self.month.set(first_of_month);
        self.month_title.set_label(
            &first_of_month
                .format_localized(&gettext("%B %Y"), date_locale())
                .to_string(),
        );
        let first_shown = week_start_of(first_of_month);
        let weeks = weeks_shown(first_of_month);
        for (index, day) in self.days.iter().enumerate() {
            day.cell.set_visible(index / 7 < weeks);
            let date = first_shown + Days::new(index as u64);
            day.date.set(date);
            day.number.set_label(&date.day().to_string());
            let has_events = busy_days.contains(&date);
            day.dot.set_visible(has_events);
            let look = day_look(date, index % 7, today, selected, first_of_month, band);
            day.button.set_css_classes(&day_classes(look));
            day.cell.set_css_classes(&band_classes(look));
            day.button
                .update_state(&[gtk::accessible::State::Selected(Some(look.selected))]);
            crate::ui::name(&day.button, &words::mini_day_words(date, has_events));
        }
    }

    fn rebuild_calendar_list(&self, accounts: &[SidebarAccount]) {
        while let Some(child) = self.calendar_list.first_child() {
            self.calendar_list.remove(&child);
        }
        for account in accounts {
            // The gap under the heading sits inside the part that folds,
            // so a folded account's heading runs straight on to the next.
            let body = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .margin_top(12)
                .build();
            match &account.reach {
                CalendarReach::Calendars(calendars) => {
                    for calendar in calendars {
                        body.append(&self.calendar_row(account.id, &account.address, calendar));
                    }
                }
                CalendarReach::PrimaryOnly(calendars) => {
                    for calendar in calendars {
                        body.append(&self.calendar_row(account.id, &account.address, calendar));
                    }
                    body.append(&self.grant_row(
                        account.id,
                        &gettext("Grant Access to show shared calendars"),
                        None,
                    ));
                }
                CalendarReach::Withheld => {
                    body.append(&dim_line(&gettext(
                        "Penguin Mail cannot see this account's calendars",
                    )));
                    body.append(&self.grant_row(
                        account.id,
                        &gettext("Grant Access"),
                        Some(&account.address),
                    ));
                }
                CalendarReach::NotOffered(reason) => {
                    body.append(&dim_line(reason));
                }
            }
            let folded = self.folded.borrow().contains(&account.address.to_lowercase());
            let revealer = gtk::Revealer::builder()
                .child(&body)
                .reveal_child(!folded)
                .transition_type(gtk::RevealerTransitionType::SlideDown)
                .transition_duration(150)
                .build();
            self.calendar_list.append(&self.heading(&account.address, folded, &revealer));
            self.calendar_list.append(&revealer);
        }
        if hidden_count(accounts) > 0 {
            self.calendar_list.append(&hidden_menu(accounts));
        }
    }

    /// An account's heading: its address, which folds the calendars under
    /// it away and opens them again, as a mail account's heading does.
    /// The chevron shows while the account is folded, or under the
    /// pointer or the focus, so an open list looks as the mockup draws it.
    fn heading(&self, address: &str, folded: bool, revealer: &gtk::Revealer) -> gtk::Button {
        let label = gtk::Label::builder()
            .label(address)
            // A2's heading class (`app/data/style.css`) sets the size,
            // weight and faint colour every sidebar heading shares; an
            // account's address stays sentence case, so it takes none of
            // that rule's capitals.
            .css_classes(["sidebar-section"])
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .build();
        let chevron = gtk::Image::builder()
            .icon_name(chevron_icon(folded))
            .pixel_size(12)
            .css_classes(["calendar-chevron"])
            .build();
        let content = gtk::Box::builder().spacing(4).build();
        content.append(&label);
        content.append(&chevron);
        let button = gtk::Button::builder()
            .child(&content)
            .css_classes(["flat", "calendar-heading"])
            .margin_top(14)
            .build();
        if folded {
            button.add_css_class("folded");
        }
        crate::ui::describe(&button, address, &gettext("Show or hide this account's calendars"));
        button.update_state(&[gtk::accessible::State::Expanded(Some(!folded))]);
        let (folded_set, on_change, revealer) =
            (Rc::clone(&self.folded), Rc::clone(&self.on_change), revealer.clone());
        let address = address.to_string();
        button.connect_clicked(move |button| {
            let key = address.to_lowercase();
            // The borrow ends here, before the widgets and the change
            // below run any handler of their own.
            let now_folded = {
                let mut set = folded_set.borrow_mut();
                if set.remove(&key) {
                    false
                } else {
                    set.insert(key);
                    true
                }
            };
            revealer.set_reveal_child(!now_folded);
            chevron.set_icon_name(Some(chevron_icon(now_folded)));
            match now_folded {
                true => button.add_css_class("folded"),
                false => button.remove_css_class("folded"),
            }
            button.update_state(&[gtk::accessible::State::Expanded(Some(!now_folded))]);
            on_change(ListChange::Folded {
                address: address.clone(),
                folded: now_folded,
            });
        });
        button
    }

    fn calendar_row(&self, account_id: AccountId, address: &str, calendar: &Calendar) -> gtk::Box {
        let row = gtk::Box::builder()
            .spacing(8)
            .css_classes(["calendar-row"])
            .build();
        let check = gtk::CheckButton::builder()
            .active(calendar.shown)
            .css_classes(["calendar-check", &tint::css_class(&calendar.color)])
            .build();
        // Two accounts can each have a calendar called Personal; the
        // account address tells them apart.
        let detail = match calendar.access.can_write() {
            true => address.to_string(),
            false => format!("{address}. {}", gettext("You can only read this calendar")),
        };
        crate::ui::describe(&check, &calendar.name, &detail);
        let label = gtk::Label::builder()
            .label(&calendar.name)
            .css_classes(["calendar-name"])
            .hexpand(true)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .single_line_mode(true)
            .build();
        row.append(&check);
        row.append(&label);
        if !calendar.access.can_write() {
            let lock = gtk::Image::from_icon_name("penguin-mail-lock-symbolic");
            lock.add_css_class("calendar-lock");
            lock.set_pixel_size(12);
            row.append(&lock);
        }
        let options = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .menu_model(&calendar_menu(account_id, &calendar.id))
            .css_classes(["flat", "circular", "calendar-options"])
            .valign(gtk::Align::Center)
            .tooltip_text(gettext("Calendar options"))
            .build();
        crate::ui::name(
            &options,
            &fill(&gettext("Options for {calendar}"), &[("calendar", &calendar.name)]),
        );
        crate::ui::name_menu_items_of(&options);
        row.append(&options);
        // A right click or a long press anywhere on the row opens the
        // same menu, from the button, which shows while the pointer is
        // on the row.
        let click = gtk::GestureClick::builder().button(gdk::BUTTON_SECONDARY).build();
        let menu = options.clone();
        click.connect_pressed(move |_, _, _, _| menu.popup());
        row.add_controller(click);
        let press = gtk::GestureLongPress::new();
        let menu = options.clone();
        press.connect_pressed(move |_, _, _| menu.popup());
        row.add_controller(press);
        let on_change = Rc::clone(&self.on_change);
        let calendar_id = calendar.id.clone();
        check.connect_toggled(move |check| {
            on_change(ListChange::Shown {
                account: account_id,
                calendar: calendar_id.clone(),
                shown: check.is_active(),
            })
        });
        row
    }

    /// A Grant Access button, named "Grant Access for {account}" when
    /// `account` is given, so several such rows read apart, as
    /// `app/src/ui/contacts_prefs.rs`'s `grant_access_row` does.
    fn grant_row(&self, account_id: AccountId, label: &str, account: Option<&str>) -> gtk::Button {
        let button = gtk::Button::builder()
            .label(label)
            .css_classes(["flat"])
            .halign(gtk::Align::Start)
            .build();
        if let Some(account) = account {
            crate::ui::name(
                &button,
                &fill(
                    &gettext("Grant Access for {account}"),
                    &[("account", account)],
                ),
            );
        }
        let on_grant = Rc::clone(&self.on_grant);
        button.connect_clicked(move |_| on_grant(account_id));
        button
    }
}

fn chevron_icon(folded: bool) -> &'static str {
    match folded {
        true => "pan-end-symbolic",
        false => "pan-down-symbolic",
    }
}

/// The `calendars` actions the row menus and the Hidden Calendars menu
/// name, each taking the account and the calendar's id.
fn list_actions(on_change: &Rc<OnChange>) -> gio::SimpleActionGroup {
    let actions = gio::SimpleActionGroup::new();
    for (name, listed) in [("hide", false), ("unhide", true)] {
        let action = gio::SimpleAction::new(name, Some(glib::VariantTy::new("(xs)").expect("a valid type")));
        let on_change = Rc::clone(on_change);
        action.connect_activate(move |_, target| {
            if let Some((account, calendar)) = target.and_then(|t| t.get::<(AccountId, String)>()) {
                on_change(ListChange::Listed { account, calendar, listed });
            }
        });
        actions.add_action(&action);
    }
    let color = gio::SimpleAction::new("color", Some(glib::VariantTy::new("(xss)").expect("a valid type")));
    let on_change = Rc::clone(on_change);
    color.connect_activate(move |_, target| {
        if let Some((account, calendar, color)) = target.and_then(|t| t.get::<(AccountId, String, String)>()) {
            let color = (!color.is_empty()).then_some(color);
            on_change(ListChange::Color { account, calendar, color });
        }
    });
    actions.add_action(&color);
    actions
}

/// A calendar row's menu: Hide from the List, and Color with Gmail's
/// label colours and the calendar's own colour back.
fn calendar_menu(account_id: AccountId, calendar: &str) -> gio::Menu {
    let menu = gio::Menu::new();
    let hide = gio::MenuItem::new(Some(&gettext("Hide from the List")), None);
    hide.set_action_and_target_value(Some("calendars.hide"), Some(&(account_id, calendar).to_variant()));
    menu.append_item(&hide);
    let colors = gio::Menu::new();
    for (index, (hex, _)) in crate::ui::LABEL_COLORS.iter().enumerate() {
        let entry = gio::MenuItem::new(Some(&crate::ui::label_color_name(index)), None);
        entry.set_action_and_target_value(
            Some("calendars.color"),
            Some(&(account_id, calendar, *hex).to_variant()),
        );
        colors.append_item(&entry);
    }
    let original = gio::Menu::new();
    let entry = gio::MenuItem::new(Some(&gettext("Original Color")), None);
    entry.set_action_and_target_value(
        Some("calendars.color"),
        Some(&(account_id, calendar, "").to_variant()),
    );
    original.append_item(&entry);
    colors.append_section(None, &original);
    menu.append_submenu(Some(&gettext("Color")), &colors);
    menu
}

/// "Hidden Calendars" at the foot of the list, while any calendar is off
/// it: a menu that puts each one back, under its account's address when
/// more than one account has hidden some.
fn hidden_menu(accounts: &[SidebarAccount]) -> gtk::MenuButton {
    let menu = gio::Menu::new();
    let hiding: Vec<&SidebarAccount> = accounts.iter().filter(|a| !a.hidden.is_empty()).collect();
    for account in &hiding {
        let section = gio::Menu::new();
        for calendar in &account.hidden {
            let item = gio::MenuItem::new(
                Some(&fill(&gettext("Show {calendar}"), &[("calendar", &calendar.name)])),
                None,
            );
            item.set_action_and_target_value(
                Some("calendars.unhide"),
                Some(&(account.id, calendar.id.as_str()).to_variant()),
            );
            section.append_item(&item);
        }
        let heading = (hiding.len() > 1).then_some(account.address.as_str());
        menu.append_section(heading, &section);
    }
    let count = hidden_count(accounts);
    let content = gtk::Box::builder().spacing(6).build();
    content.append(
        &gtk::Label::builder()
            .label(gettext("Hidden Calendars"))
            .css_classes(["calendar-name"])
            .build(),
    );
    content.append(
        &gtk::Label::builder()
            .label(count.to_string())
            .css_classes(["hidden-count"])
            .build(),
    );
    let button = gtk::MenuButton::builder()
        .child(&content)
        .menu_model(&menu)
        .css_classes(["flat", "hidden-calendars"])
        .halign(gtk::Align::Start)
        .margin_top(12)
        .build();
    crate::ui::name(
        &button,
        &fill_plural(
            "{count} hidden calendar",
            "{count} hidden calendars",
            count,
            &[("count", &count.to_string())],
        ),
    );
    crate::ui::name_menu_items_of(&button);
    button
}

fn dim_line(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .css_classes(["dim-label", "caption"])
        .xalign(0.0)
        .wrap(true)
        .build()
}


#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use mailrs_domain::calendar::Access;
    use mailrs_domain::{AccountState, Provider};

    use super::*;

    fn account(id: AccountId, email: &str) -> Account {
        Account {
            id,
            email: email.into(),
            state: AccountState::Ok,
            provider: Provider::Gmail,
            provider_name: None,
        }
    }

    fn imap(id: AccountId, email: &str) -> Account {
        Account {
            provider: Provider::Imap,
            ..account(id, email)
        }
    }

    fn calendar(id: &str) -> Calendar {
        Calendar {
            id: id.into(),
            name: id.into(),
            color: "#3584e4".into(),
            access: Access::Owner,
            shown: true,
            ..Calendar::default()
        }
    }

    #[test]
    fn the_mini_month_shows_only_the_weeks_that_hold_the_month() {
        crate::locale_time::set_first_weekday_for_test(chrono::Weekday::Mon);
        let d = |y, m, day| NaiveDate::from_ymd_opt(y, m, day).unwrap();
        // September 2026 starts on a Tuesday and ends on a Wednesday.
        assert_eq!(weeks_shown(d(2026, 9, 1)), 5);
        // August 2026 starts on a Saturday: six weeks.
        assert_eq!(weeks_shown(d(2026, 8, 1)), 6);
        // February 2027 starts on a Monday and has 28 days.
        assert_eq!(weeks_shown(d(2027, 2, 1)), 4);
    }

    #[test]
    fn the_weekday_row_starts_on_the_locales_own_first_weekday() {
        crate::locale_time::set_first_weekday_for_test(chrono::Weekday::Sun);
        assert_eq!(weekday_initials(), vec!["S", "M", "T", "W", "T", "F", "S"]);
        crate::locale_time::set_first_weekday_for_test(chrono::Weekday::Mon);
        assert_eq!(weekday_initials(), vec!["M", "T", "W", "T", "F", "S", "S"]);
    }

    #[test]
    fn the_mini_month_arrows_reach_the_first_of_the_next_and_previous_month() {
        let d = |y, m, day| NaiveDate::from_ymd_opt(y, m, day).unwrap();
        // The grid of October 2026 starts on Monday 28 September, which
        // must not pull the arrows a month further back.
        assert_eq!(adjacent_month(d(2026, 10, 14), -1), d(2026, 9, 1));
        assert_eq!(adjacent_month(d(2026, 10, 31), 1), d(2026, 11, 1));
        assert_eq!(adjacent_month(d(2026, 12, 5), 1), d(2027, 1, 1));
    }

    #[test]
    fn a_fully_granted_account_lists_every_calendar() {
        let rows = sidebar_accounts(&[(
            account(1, "dana@example.com"),
            Offers::EVERYTHING,
            Withheld::NONE,
            vec![calendar("primary"), calendar("team")],
        )]);
        assert_eq!(
            rows,
            vec![SidebarAccount {
                id: 1,
                address: "dana@example.com".into(),
                reach: CalendarReach::Calendars(vec![calendar("primary"), calendar("team")]),
                hidden: Vec::new(),
            }]
        );
    }

    #[test]
    fn an_account_that_withheld_the_calendar_shows_no_calendars() {
        let withheld = Withheld {
            calendar: true,
            ..Withheld::NONE
        };
        let rows = sidebar_accounts(&[(
            account(1, "dana@example.com"),
            Offers::EVERYTHING,
            withheld,
            vec![calendar("primary")],
        )]);
        assert_eq!(rows[0].reach, CalendarReach::Withheld);
    }

    #[test]
    fn an_account_that_withheld_only_the_list_keeps_its_primary() {
        let withheld = Withheld {
            calendar_list: true,
            ..Withheld::NONE
        };
        let rows = sidebar_accounts(&[(
            account(1, "dana@example.com"),
            Offers::EVERYTHING,
            withheld,
            vec![calendar("primary")],
        )]);
        assert_eq!(
            rows[0].reach,
            CalendarReach::PrimaryOnly(vec![calendar("primary")])
        );
    }

    #[test]
    fn an_imap_account_names_why_it_has_no_calendars() {
        let offers = Offers {
            calendar: false,
            ..Offers::EVERYTHING
        };
        let rows = sidebar_accounts(&[(
            imap(1, "dana@fastmail.example"),
            offers,
            Withheld::NONE,
            Vec::new(),
        )]);
        assert_eq!(
            rows[0].reach,
            CalendarReach::NotOffered(crate::offered::reason(
                &imap(1, "dana@fastmail.example"),
                Missing::Calendar
            ))
        );
    }

    #[test]
    fn an_account_not_started_yet_reads_as_fully_granted() {
        let rows = sidebar_accounts(&[(
            account(1, "dana@example.com"),
            crate::offered::offers_for(None),
            crate::offered::withheld_for(None),
            Vec::new(),
        )]);
        assert_eq!(rows[0].reach, CalendarReach::Calendars(Vec::new()));
    }

    fn listed_ids(reach: &CalendarReach) -> Vec<String> {
        match reach {
            CalendarReach::Calendars(list) | CalendarReach::PrimaryOnly(list) => {
                list.iter().map(|c| c.id.clone()).collect()
            }
            _ => Vec::new(),
        }
    }

    #[test]
    fn a_calendar_taken_off_the_list_moves_to_hidden_calendars() {
        let rows = sidebar_accounts(&[(
            account(1, "dana@example.com"),
            Offers::EVERYTHING,
            Withheld::NONE,
            vec![calendar("primary"), calendar("holidays")],
        )]);
        let unlisted = HashMap::from([(1, HashSet::from(["holidays".to_string()]))]);
        let rows = take_off_the_list(rows, &unlisted);
        assert_eq!(listed_ids(&rows[0].reach), vec!["primary"]);
        assert_eq!(rows[0].hidden, vec![calendar("holidays")]);
    }

    #[test]
    fn an_account_with_nothing_unlisted_hides_nothing() {
        let rows = sidebar_accounts(&[(
            account(1, "dana@example.com"),
            Offers::EVERYTHING,
            Withheld::NONE,
            vec![calendar("primary")],
        )]);
        let rows = take_off_the_list(rows, &HashMap::new());
        assert_eq!(listed_ids(&rows[0].reach), vec!["primary"]);
        assert!(rows[0].hidden.is_empty());
    }

    #[test]
    fn hidden_calendars_list_every_accounts_own() {
        let rows = sidebar_accounts(&[
            (account(1, "dana@example.com"), Offers::EVERYTHING, Withheld::NONE, vec![calendar("a")]),
            (account(2, "d.reyes@uni.example"), Offers::EVERYTHING, Withheld::NONE, vec![calendar("b")]),
        ]);
        let unlisted = HashMap::from([
            (1, HashSet::from(["a".to_string()])),
            (2, HashSet::from(["b".to_string()])),
        ]);
        let rows = take_off_the_list(rows, &unlisted);
        assert_eq!(hidden_count(&rows), 2);
    }

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn the_week_in_view_runs_from_the_week_start_for_seven_days() {
        crate::locale_time::set_first_weekday_for_test(chrono::Weekday::Mon);
        assert_eq!(in_view(ViewKind::Week, d(2026, 9, 30)), Some((d(2026, 9, 28), d(2026, 10, 4))));
    }

    #[test]
    fn the_month_in_view_is_its_own_days_not_the_grids() {
        assert_eq!(in_view(ViewKind::Month, d(2026, 9, 30)), Some((d(2026, 9, 1), d(2026, 9, 30))));
    }

    #[test]
    fn a_single_day_marks_no_band() {
        assert_eq!(in_view(ViewKind::Day, d(2026, 9, 30)), None);
    }

    #[test]
    fn today_and_the_selected_day_read_apart() {
        let look = day_look(d(2026, 9, 29), 1, d(2026, 9, 23), d(2026, 9, 29), d(2026, 9, 1), None);
        assert!(look.selected && !look.today);
        let look = day_look(d(2026, 9, 23), 2, d(2026, 9, 23), d(2026, 9, 29), d(2026, 9, 1), None);
        assert!(look.today && !look.selected);
    }

    #[test]
    fn a_day_outside_the_month_shown_is_outside() {
        let look = day_look(d(2026, 8, 31), 0, d(2026, 9, 23), d(2026, 9, 23), d(2026, 9, 1), None);
        assert!(look.outside);
    }

    #[test]
    fn the_band_rounds_off_at_the_ends_of_the_week_in_view() {
        let week = Some((d(2026, 9, 28), d(2026, 10, 4)));
        let first = day_look(d(2026, 9, 28), 0, d(2026, 9, 23), d(2026, 9, 30), d(2026, 9, 1), week);
        assert!(first.in_view && first.band_start && !first.band_end);
        let middle = day_look(d(2026, 9, 30), 2, d(2026, 9, 23), d(2026, 9, 30), d(2026, 9, 1), week);
        assert!(middle.in_view && !middle.band_start && !middle.band_end);
        let last = day_look(d(2026, 10, 4), 6, d(2026, 9, 23), d(2026, 9, 30), d(2026, 9, 1), week);
        assert!(last.in_view && last.band_end);
        let after = day_look(d(2026, 10, 5), 0, d(2026, 9, 23), d(2026, 9, 30), d(2026, 9, 1), week);
        assert!(!after.in_view);
    }

    #[test]
    fn a_month_band_rounds_off_at_each_rows_edges() {
        let month = Some((d(2026, 9, 1), d(2026, 9, 30)));
        let sunday = day_look(d(2026, 9, 13), 6, d(2026, 9, 23), d(2026, 9, 23), d(2026, 9, 1), month);
        assert!(sunday.band_end && !sunday.band_start);
        let monday = day_look(d(2026, 9, 14), 0, d(2026, 9, 23), d(2026, 9, 23), d(2026, 9, 1), month);
        assert!(monday.band_start && !monday.band_end);
    }

    #[test]
    fn the_classes_carry_each_state() {
        let look = DayLook { today: true, selected: true, in_view: true, band_start: true, ..DayLook::default() };
        assert_eq!(
            day_classes(look),
            vec!["flat", "today", "selected"]
        );
        assert_eq!(band_classes(look), vec!["mini-day", "in-view", "band-start"]);
    }

    #[test]
    fn only_accounts_that_can_answer_on_their_calendar_list_waiting_invitations() {
        let no_calendar = Offers { calendar: false, ..Offers::EVERYTHING };
        let withheld = Withheld { calendar: true, ..Withheld::NONE };
        let accounts = [
            (account(1, "dana@example.com"), Offers::EVERYTHING, Withheld::NONE),
            (imap(2, "dana@fastmail.example"), no_calendar, Withheld::NONE),
            (account(3, "d.reyes@uni.example"), Offers::EVERYTHING, withheld),
        ];
        assert_eq!(waiting_accounts(&accounts), vec![1]);
    }
}

/// Whether a scrolled list still has content below what shows, with a
/// pixel of slack so a list that just fits gets no fade.
fn more_below(value: f64, page: f64, upper: f64) -> bool {
    value + page < upper - 1.0
}

#[cfg(test)]
mod fade_tests {
    use super::more_below;

    #[test]
    fn the_fade_shows_only_while_the_list_runs_past_its_foot() {
        assert!(more_below(0.0, 400.0, 520.0), "the list is cut");
        assert!(!more_below(120.0, 400.0, 520.0), "scrolled to the end");
        assert!(!more_below(0.0, 400.0, 400.5), "a list that just fits");
        assert!(!more_below(0.0, 400.0, 300.0), "a short list");
    }
}
