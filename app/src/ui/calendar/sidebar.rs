//! `CalendarSidebar`: the mini month a person jumps around with, and the
//! calendar list they show or hide calendars from. What each account's
//! row says is worked out in pure functions ([`sidebar_accounts`]) so
//! Task 0's Grant Access story, not a widget, decides it.

use std::cell::Cell;
use std::collections::HashSet;
use std::rc::Rc;

use adw::prelude::*;
use chrono::{Datelike, Days, NaiveDate};
use mailrs_domain::calendar::Calendar;
use mailrs_domain::translate::{date_locale, fill, gettext};
use mailrs_domain::{Account, AccountId};
use mailrs_sync::{Missing, Offers, Withheld};

use super::tint;
use super::words;

/// How far an account's calendars reach into the sidebar, worked out
/// from what its provider offers and what its own consent withheld
/// (reconcile.md Task 5 item 11).
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
}

/// The sidebar's rows from each account's provider offers, its own
/// consent, and the calendars the local copy actually holds for it. An
/// account not yet started passes `Offers::EVERYTHING`/`Withheld::NONE`
/// (`offered::offers_for`/`withheld_for`), so it reads as `Calendars`
/// until a read says otherwise, matching Provider neutrality's "before
/// an account starts" rule.
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
            }
        })
        .collect()
}

/// The Monday on or before `day`, the mini month's own copy of
/// `range::monday_of` (private there).
fn monday_of(day: NaiveDate) -> NaiveDate {
    day - Days::new(u64::from(day.weekday().num_days_from_monday()))
}

/// "M", "T", "W", … for the mini month's weekday row, from a known
/// Monday so the locale's own weekday names decide the letter.
fn weekday_initials() -> Vec<String> {
    let monday = NaiveDate::from_ymd_opt(2024, 1, 1).expect("2024-01-01 is a Monday");
    (0..7u64)
        .map(|i| {
            (monday + Days::new(i))
                .format_localized(&gettext("%a"), date_locale())
                .to_string()
                .chars()
                .next()
                .map(|c| c.to_string())
                .unwrap_or_default()
        })
        .collect()
}

type OnDate = dyn Fn(NaiveDate);
type OnShown = dyn Fn(AccountId, String, bool);
type OnGrant = dyn Fn(AccountId);

/// One mini month day button, and the labels `show` fills each redraw.
/// `date` is shared with the button's own click closure through an `Rc`,
/// so `show` moving it forward is what the closure reads at the next
/// click, not a stale copy taken when the button was built.
struct MiniDay {
    button: gtk::Button,
    number: gtk::Label,
    dot: gtk::Widget,
    date: Rc<Cell<NaiveDate>>,
}

pub struct CalendarSidebar {
    pub widget: gtk::Box,
    month_title: gtk::Label,
    days: Vec<MiniDay>,
    calendar_list: gtk::Box,
    on_date: Rc<OnDate>,
    on_shown: Rc<OnShown>,
    on_grant: Rc<OnGrant>,
}

impl CalendarSidebar {
    /// `on_date` runs for a day cell and for the mini month's own
    /// Previous/Next Month arrows, which the window answers by asking
    /// for that month's busy days and calling `show` again: the sidebar
    /// keeps no month of its own between calls.
    pub fn new(
        on_date: impl Fn(NaiveDate) + 'static,
        on_shown: impl Fn(AccountId, String, bool) + 'static,
        on_grant: impl Fn(AccountId) + 'static,
    ) -> Rc<CalendarSidebar> {
        let on_date: Rc<OnDate> = Rc::new(on_date);
        let on_shown: Rc<OnShown> = Rc::new(on_shown);
        let on_grant: Rc<OnGrant> = Rc::new(on_grant);

        let widget = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(10)
            .build();

        let month_title = gtk::Label::builder()
            .css_classes(["heading"])
            .hexpand(true)
            .xalign(0.0)
            .build();
        let previous = gtk::Button::builder()
            .icon_name("go-previous-symbolic")
            .css_classes(["flat"])
            .build();
        let next = gtk::Button::builder()
            .icon_name("go-next-symbolic")
            .css_classes(["flat"])
            .build();
        crate::ui::name(&previous, &gettext("Previous Month"));
        crate::ui::name(&next, &gettext("Next Month"));
        let header = gtk::Box::builder().spacing(4).build();
        header.append(&month_title);
        header.append(&previous);
        header.append(&next);
        widget.append(&header);

        let weekdays = gtk::Grid::builder().column_homogeneous(true).build();
        for (column, initial) in weekday_initials().into_iter().enumerate() {
            weekdays.attach(
                &gtk::Label::builder()
                    .label(&initial)
                    .css_classes(["dim-label", "caption"])
                    .build(),
                column as i32,
                0,
                1,
                1,
            );
        }
        widget.append(&weekdays);

        let mini = gtk::Grid::builder()
            .row_homogeneous(true)
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
                let dot = gtk::Box::builder()
                    .css_classes(["dot"])
                    .halign(gtk::Align::Center)
                    .build();
                let inner = gtk::Box::builder()
                    .orientation(gtk::Orientation::Vertical)
                    .spacing(2)
                    .halign(gtk::Align::Center)
                    .build();
                inner.append(&number);
                inner.append(&dot);
                let button = gtk::Button::builder().child(&inner).build();
                mini.attach(&button, column, row, 1, 1);
                let date = Rc::new(Cell::new(placeholder));
                days.push(MiniDay {
                    button,
                    number,
                    dot: dot.upcast(),
                    date,
                });
            }
        }
        widget.append(&mini);

        let calendar_list = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .build();
        widget.append(&calendar_list);

        let sidebar = Rc::new(CalendarSidebar {
            widget,
            month_title,
            days,
            calendar_list,
            on_date,
            on_shown,
            on_grant,
        });

        for day in &sidebar.days {
            let on_date = Rc::clone(&sidebar.on_date);
            let date = Rc::clone(&day.date);
            day.button.connect_clicked(move |_| on_date(date.get()));
        }
        {
            let on_date = Rc::clone(&sidebar.on_date);
            let first_day = Rc::clone(&sidebar.days[0].date);
            previous.connect_clicked(move |_| on_date(first_day.get() - chrono::Months::new(1)));
        }
        {
            let on_date = Rc::clone(&sidebar.on_date);
            let first_day = Rc::clone(&sidebar.days[0].date);
            next.connect_clicked(move |_| on_date(first_day.get() + chrono::Months::new(1)));
        }

        sidebar
    }

    /// Redraws the mini month around `month` and rebuilds the calendar
    /// list from `accounts` ([`sidebar_accounts`]'s own output).
    pub fn show(
        &self,
        month: NaiveDate,
        today: NaiveDate,
        busy_days: &HashSet<NaiveDate>,
        accounts: &[SidebarAccount],
    ) {
        let first_of_month = month.with_day(1).unwrap_or(month);
        self.month_title.set_label(
            &first_of_month
                .format_localized(&gettext("%B %Y"), date_locale())
                .to_string(),
        );
        let first_shown = monday_of(first_of_month);
        for (index, day) in self.days.iter().enumerate() {
            let date = first_shown + Days::new(index as u64);
            day.date.set(date);
            day.number.set_label(&date.day().to_string());
            let has_events = busy_days.contains(&date);
            day.dot.set_visible(has_events);
            day.button
                .set_css_classes(&mini_day_classes(date, today, first_of_month));
            crate::ui::name(&day.button, &words::mini_day_words(date, has_events));
        }

        self.rebuild_calendar_list(accounts);
    }

    fn rebuild_calendar_list(&self, accounts: &[SidebarAccount]) {
        while let Some(child) = self.calendar_list.first_child() {
            self.calendar_list.remove(&child);
        }
        for account in accounts {
            let heading = gtk::Label::builder()
                .label(&account.address)
                .css_classes(["dim-label", "caption"])
                .xalign(0.0)
                .margin_top(8)
                .build();
            self.calendar_list.append(&heading);
            match &account.reach {
                CalendarReach::Calendars(calendars) => {
                    for calendar in calendars {
                        self.calendar_list
                            .append(&self.calendar_row(account.id, calendar));
                    }
                }
                CalendarReach::PrimaryOnly(calendars) => {
                    for calendar in calendars {
                        self.calendar_list
                            .append(&self.calendar_row(account.id, calendar));
                    }
                    self.calendar_list.append(&self.grant_row(
                        account.id,
                        &gettext("Grant Access to show shared calendars"),
                        None,
                    ));
                }
                CalendarReach::Withheld => {
                    self.calendar_list.append(&dim_line(&gettext(
                        "Penguin Mail cannot see this account's calendars",
                    )));
                    self.calendar_list.append(&self.grant_row(
                        account.id,
                        &gettext("Grant Access"),
                        Some(&account.address),
                    ));
                }
                CalendarReach::NotOffered(reason) => {
                    self.calendar_list.append(&dim_line(reason));
                }
            }
        }
    }

    fn calendar_row(&self, account_id: AccountId, calendar: &Calendar) -> gtk::Box {
        let row = gtk::Box::builder().spacing(8).build();
        let check = gtk::CheckButton::builder()
            .active(calendar.shown)
            .css_classes(["calendar-check", &tint::css_class(&calendar.color)])
            .build();
        crate::ui::name(&check, &calendar.name);
        if !calendar.access.can_write() {
            crate::ui::describe(
                &check,
                &calendar.name,
                &gettext("You can only read this calendar"),
            );
        }
        let label = gtk::Label::builder()
            .label(&calendar.name)
            .hexpand(true)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .single_line_mode(true)
            .build();
        row.append(&check);
        row.append(&label);
        if !calendar.access.can_write() {
            let lock = gtk::Image::from_icon_name("changes-prevent-symbolic");
            lock.set_pixel_size(12);
            row.append(&lock);
        }
        let on_shown = Rc::clone(&self.on_shown);
        let calendar_id = calendar.id.clone();
        check.connect_toggled(move |check| {
            on_shown(account_id, calendar_id.clone(), check.is_active())
        });
        row
    }

    /// A Grant Access button, named "Grant Access for {account}" when
    /// `account` is given, so several such rows read apart (reconcile.md
    /// Task 5 item 11, following `app/src/ui/contacts_prefs.rs`'s
    /// `grant_access_row`).
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

fn dim_line(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .css_classes(["dim-label", "caption"])
        .xalign(0.0)
        .wrap(true)
        .build()
}

fn mini_day_classes(date: NaiveDate, today: NaiveDate, month: NaiveDate) -> Vec<&'static str> {
    let mut classes = vec!["flat"];
    if date == today {
        classes.push("today");
    }
    if date.month() != month.month() || date.year() != month.year() {
        classes.push("outside");
    }
    classes
}

#[cfg(test)]
mod tests {
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
}
