//! What the event editor and quick create hold while a person changes an
//! event, and the event it comes to. No widgets here: the dialog shows a
//! `Draft` and calls its setters, so the rules (the end moves with the
//! start, Save needs a title, the repeat's day follows the start) live
//! under tests.

use chrono::{NaiveDate, NaiveTime, TimeZone};
use chrono::Duration;
use chrono_tz::Tz;
use mailrs_domain::calendar::repeat::Repeat;
use mailrs_domain::calendar::series::{self, RepeatScope};
use mailrs_domain::calendar::{Access, Calendar, Event, Guest, Occurrence, Reminder};
use mailrs_domain::{AccountId, EpochMillis};

const DAY: EpochMillis = 86_400_000;
const HOUR: EpochMillis = 3_600_000;

/// What the draft opened with, to tell what the person changed.
#[derive(Debug, Clone, PartialEq)]
struct Opened {
    rules: Vec<String>,
    repeat: Repeat,
    day: NaiveDate,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Draft {
    pub account_id: AccountId,
    pub calendar: String,
    pub title: String,
    pub all_day: bool,
    pub start: EpochMillis,
    pub end: EpochMillis,
    /// The zone the event is written in.
    pub zone: String,
    pub repeat: Repeat,
    pub place: String,
    pub guests: Vec<Guest>,
    /// `None` takes the calendar's reminders.
    pub reminders: Option<Vec<Reminder>>,
    pub notes: String,
    pub busy: bool,
    pub private: bool,
    pub color: Option<String>,
    pub add_meet: bool,
    /// The event as it was, when editing one.
    pub base: Option<Event>,
    /// The occurrence the person opened, for the repeat question.
    pub occurrence: Option<Occurrence>,
    opened: Opened,
    view_zone: Tz,
}

impl Draft {
    /// A new event on `calendar`, from `start` to `end`.
    pub fn new(account_id: AccountId, calendar: &Calendar, start: EpochMillis, end: EpochMillis, view_zone: Tz) -> Draft {
        let zone = if calendar.zone.parse::<Tz>().is_ok() { calendar.zone.clone() } else { view_zone.name().to_string() };
        let day = local_day(start, view_zone);
        Draft {
            account_id,
            calendar: calendar.id.clone(),
            title: String::new(),
            all_day: false,
            start,
            end: end.max(start),
            zone,
            repeat: Repeat::Never,
            place: String::new(),
            guests: Vec::new(),
            reminders: None,
            notes: String::new(),
            busy: true,
            private: false,
            color: None,
            add_meet: false,
            base: None,
            occurrence: None,
            opened: Opened { rules: Vec::new(), repeat: Repeat::Never, day },
            view_zone,
        }
    }

    /// The editor on an occurrence. `series_rules` are the series' rules,
    /// which a changed occurrence does not carry itself.
    pub fn open(occurrence: &Occurrence, series_rules: &[String], view_zone: Tz) -> Draft {
        let event = &occurrence.event;
        let zone: Tz = event.zone.parse().unwrap_or(view_zone);
        let day = local_day(occurrence.start, zone);
        let repeat = Repeat::read(series_rules, day, zone);
        Draft {
            account_id: occurrence.account_id,
            calendar: event.calendar.clone(),
            title: event.title.clone(),
            all_day: event.all_day,
            start: occurrence.start,
            end: occurrence.end,
            zone: event.zone.clone(),
            repeat: repeat.clone(),
            place: event.place.clone(),
            guests: event.guests.clone(),
            reminders: event.reminders.clone(),
            notes: event.description.clone(),
            busy: event.busy,
            private: event.private,
            color: event.color.clone(),
            add_meet: false,
            base: Some(Event::clone(event)),
            occurrence: Some(occurrence.clone()),
            opened: Opened { rules: series_rules.to_vec(), repeat, day },
            view_zone,
        }
    }

    pub fn is_new(&self) -> bool {
        self.base.is_none()
    }

    /// Moves the start and takes the end along, keeping the length.
    pub fn set_start(&mut self, at: EpochMillis) {
        let length = self.end - self.start;
        self.start = at;
        self.end = at + length;
    }

    /// Moves the end, never before the start.
    pub fn set_end(&mut self, at: EpochMillis) {
        self.end = at.max(self.start);
    }

    /// Both ends at once, as a drag sets them.
    pub fn set_span(&mut self, start: EpochMillis, end: EpochMillis) {
        self.start = start;
        self.end = end.max(start);
    }

    /// An all-day event runs from midnight UTC of its first day to
    /// midnight UTC after its last, whatever zone the reader is in. Leaving
    /// all day starts the first day at 09:00 for an hour.
    pub fn set_all_day(&mut self, on: bool) {
        if on == self.all_day {
            return;
        }
        let first = local_day(self.start, self.view_zone);
        if on {
            let last = local_day((self.end - 1).max(self.start), self.view_zone);
            self.start = utc_midnight(first);
            self.end = utc_midnight(last) + DAY;
        } else {
            // The dates of an all-day event are UTC dates.
            let first = chrono::DateTime::from_timestamp_millis(self.start).unwrap_or_default().date_naive();
            let nine = first.and_time(NaiveTime::from_hms_opt(9, 0, 0).expect("09:00 exists"));
            self.start = self.view_zone.from_local_datetime(&nine).earliest().map_or(self.start, |t| t.timestamp_millis());
            self.end = self.start + HOUR;
        }
        self.all_day = on;
    }

    pub fn can_save(&self) -> bool {
        !self.title.trim().is_empty()
    }

    pub fn rule_changed(&self) -> bool {
        self.repeat != self.opened.repeat
    }

    /// The answers the repeat question offers for this draft. A guest
    /// changes only their own copy of someone else's series, so "This and
    /// following", which would start a new series they organize, is not
    /// among them.
    pub fn scopes(&self) -> Vec<RepeatScope> {
        let Some(o) = &self.occurrence else {
            return Vec::new();
        };
        let offered = series::scopes(&o.event, self.rule_changed());
        if self.base.as_ref().is_some_and(limited) {
            offered.into_iter().filter(|s| *s != RepeatScope::Following).collect()
        } else {
            offered
        }
    }

    /// The event the draft stands for. A new one takes `new_id`; a Meet
    /// link is asked for under `meet_request` when the person turned it on
    /// and the event has none yet.
    ///
    /// On someone else's event the account is only a guest of ([`limited`]),
    /// so Google takes only the guest's own fields and refuses or ignores
    /// the rest (ruling R9): the rest of `base` goes out unchanged.
    pub fn to_event(&self, new_id: &str, meet_request: &str) -> Event {
        let mut event = self.base.clone().unwrap_or_else(|| Event { id: new_id.to_string(), ..Event::default() });
        if let Some(base) = &self.base
            && limited(base)
        {
            event.reminders = self.reminders.clone();
            event.color = self.color.clone();
            event.busy = self.busy;
            return event;
        }
        event.calendar = self.calendar.clone();
        event.title = self.title.trim().to_string();
        event.all_day = self.all_day;
        event.start = self.start;
        event.end = self.end;
        event.zone = if self.all_day { "UTC".to_string() } else { self.zone.clone() };
        event.place = self.place.trim().to_string();
        event.guests = self.guests.clone();
        event.reminders = self.reminders.clone();
        event.description = self.notes.clone();
        event.busy = self.busy;
        event.private = self.private;
        event.color = self.color.clone();
        event.rules = self.rules();
        event.meet_request = (self.add_meet && event.conference.is_none()).then(|| meet_request.to_string());
        event.pending = false;
        event
    }

    /// The rules to write. They stay as they came unless the person picked
    /// another repeat, or moved a weekly, monthly or yearly series to
    /// another day, whose rule names the day it falls on. A `Custom` rule
    /// keeps its text here, so its count or end survives; the series change
    /// moves its weekdays. A `Kept` choice (a rule from another app, or
    /// several `RRULE` lines) never rebuilds: it holds what it was opened
    /// with whatever day the series moves to.
    fn rules(&self) -> Vec<String> {
        let zone: Tz = self.zone.parse().unwrap_or(self.view_zone);
        let day = local_day(self.start, zone);
        let follows_day = matches!(self.repeat, Repeat::EveryWeek | Repeat::EveryMonth | Repeat::EveryYear);
        if !self.rule_changed() && !(follows_day && day != self.opened.day) {
            return self.opened.rules.clone();
        }
        let Some(rule) = self.repeat.rule(day, zone, self.all_day) else {
            return Vec::new();
        };
        std::iter::once(rule)
            .chain(self.opened.rules.iter().filter(|l| !mailrs_domain::calendar::is_rule_line(l)).cloned())
            .collect()
    }
}

/// The zone the desktop is set to, for a new draft nothing else names one
/// for. `Tz::UTC` when GLib's own identifier does not parse as one.
pub fn local_zone() -> Tz {
    gtk::glib::TimeZone::local()
        .identifier()
        .as_str()
        .parse()
        .unwrap_or(Tz::UTC)
}

fn local_day(at: EpochMillis, zone: Tz) -> NaiveDate {
    chrono::DateTime::from_timestamp_millis(at).unwrap_or_default().with_timezone(&zone).date_naive()
}

fn utc_midnight(day: NaiveDate) -> EpochMillis {
    day.and_hms_opt(0, 0, 0).expect("midnight exists").and_utc().timestamp_millis()
}

/// Where a new event goes when nobody said: the primary calendar of the
/// account last used, else the first primary the person can write to,
/// else any calendar they can write to.
pub fn default_calendar(writable: &[(AccountId, String, Calendar)], last: Option<&str>) -> Option<(AccountId, Calendar)> {
    let primary = |email: Option<&str>| {
        writable
            .iter()
            .find(|(_, address, c)| c.primary && email.is_none_or(|e| e.eq_ignore_ascii_case(address)))
    };
    last.and_then(|e| primary(Some(e)))
        .or_else(|| primary(None))
        .or_else(|| writable.first())
        .map(|(account, _, calendar)| (*account, calendar.clone()))
}

/// Whether someone else organizes the event and the account is only a
/// guest. The editor then leaves the time, place and guests to them
/// (ruling R9), and the popover asks this account for an answer.
pub fn limited(event: &Event) -> bool {
    event.guests.iter().any(|g| g.me && !g.organizer)
}

/// What a person may do to an event from the calendar view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Editing {
    /// Change or delete the whole event: the popover shows Edit and
    /// Delete, and a double click opens the editor.
    Whole,
    /// Someone else organizes it: a double click opens the editor
    /// limited to reminders, colour and busy (R9), and the popover shows
    /// neither Edit nor Delete (R1).
    Guest,
    /// The account withheld the calendar permission: a double click, Enter
    /// or Delete asks for it, and the popover shows neither (R8).
    NeedsPermission,
    /// A read-only calendar, or an account with no calendar at all.
    None,
}

/// What the view lets a person do to `event`, from its calendar's
/// access and what the account's provider offers and its consent
/// withheld.
pub fn editing(event: &Event, access: Access, offers_calendar: bool, withheld_calendar: bool) -> Editing {
    if !offers_calendar || !access.can_write() {
        Editing::None
    } else if withheld_calendar {
        Editing::NeedsPermission
    } else if limited(event) {
        Editing::Guest
    } else {
        Editing::Whole
    }
}

/// Puts the calendars a new event may go on in the order the sidebar
/// lists accounts, each account's primary calendar first and the rest
/// by name, so the default and the editor's list stay the same from run
/// to run whatever order the map they came from held.
pub fn sort_writable(writable: &mut [(AccountId, String, Calendar)], account_order: &[AccountId]) {
    let rank = |account: &AccountId| account_order.iter().position(|a| a == account).unwrap_or(usize::MAX);
    writable.sort_by(|(a, _, x), (b, _, y)| {
        rank(a)
            .cmp(&rank(b))
            .then(y.primary.cmp(&x.primary))
            .then_with(|| x.name.to_lowercase().cmp(&y.name.to_lowercase()))
            .then_with(|| x.id.cmp(&y.id))
    });
}

/// The quarter hours of a day, with `current` among them when it falls
/// between two.
pub fn time_choices(current: NaiveTime) -> Vec<NaiveTime> {
    let mut times: Vec<NaiveTime> = (0..96)
        .map(|q| NaiveTime::MIN + Duration::minutes(q * 15))
        .collect();
    if !times.contains(&current) {
        times.push(current);
        times.sort();
    }
    times
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use chrono::{NaiveDate, TimeZone};
    use chrono_tz::Europe::Lisbon;
    use mailrs_domain::calendar::{Reminder, ReminderMethod};
    use mailrs_domain::invitation::Answer;

    fn at(d: u32, h: u32, m: u32) -> EpochMillis {
        Lisbon
            .from_local_datetime(&NaiveDate::from_ymd_opt(2026, 9, d).unwrap().and_hms_opt(h, m, 0).unwrap())
            .single()
            .unwrap()
            .timestamp_millis()
    }

    fn utc_midnight(d: u32) -> EpochMillis {
        NaiveDate::from_ymd_opt(2026, 9, d).unwrap().and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp_millis()
    }

    fn personal() -> Calendar {
        Calendar {
            id: "me@example.com".into(),
            name: "Personal".into(),
            color: "#e8660c".into(),
            access: Access::Owner,
            zone: "Europe/Lisbon".into(),
            primary: true,
            shown: true,
            reminders: vec![Reminder { minutes: 10, method: ReminderMethod::Notification }],
        }
    }

    fn fresh() -> Draft {
        Draft::new(1, &personal(), at(23, 15, 0), at(23, 16, 0), Lisbon)
    }

    fn weekly() -> Occurrence {
        let event = Event {
            calendar: "me@example.com".into(),
            id: "review".into(),
            uid: "review@google.com".into(),
            etag: "\"4\"".into(),
            start: at(23, 15, 0),
            end: at(23, 16, 0),
            zone: "Europe/Lisbon".into(),
            title: "Review".into(),
            busy: true,
            rules: vec!["RRULE:FREQ=WEEKLY;BYDAY=WE".into(), "EXDATE;TZID=Europe/Lisbon:20260930T150000".into()],
            ..Event::default()
        };
        Occurrence { account_id: 1, start: event.start, end: event.end, event: Arc::new(event) }
    }

    #[test]
    fn moving_the_start_moves_the_end_with_it() {
        let mut draft = fresh();
        draft.set_start(at(23, 17, 30));
        assert_eq!((draft.start, draft.end), (at(23, 17, 30), at(23, 18, 30)));
    }

    #[test]
    fn the_end_cannot_go_before_the_start() {
        let mut draft = fresh();
        draft.set_end(at(23, 14, 0));
        assert_eq!(draft.end, draft.start);
        draft.set_span(at(23, 12, 0), at(23, 11, 0));
        assert_eq!((draft.start, draft.end), (at(23, 12, 0), at(23, 12, 0)));
    }

    #[test]
    fn saving_needs_a_title() {
        let mut draft = fresh();
        assert!(!draft.can_save());
        draft.title = "   ".into();
        assert!(!draft.can_save());
        draft.title = "Dentist".into();
        assert!(draft.can_save());
    }

    #[test]
    fn all_day_covers_the_days_on_screen() {
        let mut draft = Draft::new(1, &personal(), at(23, 23, 0), at(24, 1, 0), Lisbon);
        draft.set_all_day(true);
        assert_eq!((draft.start, draft.end), (utc_midnight(23), utc_midnight(25)));
        draft.set_all_day(false);
        assert_eq!((draft.start, draft.end), (at(23, 9, 0), at(23, 10, 0)));
    }

    #[test]
    fn a_new_event_takes_the_new_id_the_calendar_zone_and_its_reminders() {
        let mut draft = fresh();
        draft.title = "Dentist".into();
        let event = draft.to_event("pmnew", "pmmeet");
        assert_eq!((event.id.as_str(), event.calendar.as_str(), event.zone.as_str()), ("pmnew", "me@example.com", "Europe/Lisbon"));
        assert_eq!(event.reminders, None, "None takes the calendar's reminders");
        assert!(event.busy);
        assert!(event.rules.is_empty());
        assert_eq!(event.meet_request, None);
    }

    #[test]
    fn an_edit_keeps_the_id_the_version_and_the_rule() {
        let mut draft = Draft::open(&weekly(), &weekly().event.rules, Lisbon);
        assert_eq!(draft.repeat, Repeat::EveryWeek);
        draft.title = "Design review".into();
        let event = draft.to_event("pmnew", "pmmeet");
        assert_eq!((event.id.as_str(), event.etag.as_str(), event.uid.as_str()), ("review", "\"4\"", "review@google.com"));
        assert_eq!(event.rules, weekly().event.rules);
        assert!(!draft.rule_changed());
    }

    #[test]
    fn moving_a_weekly_series_to_another_day_moves_its_day_in_the_rule() {
        let mut draft = Draft::open(&weekly(), &weekly().event.rules, Lisbon);
        draft.set_span(at(24, 15, 0), at(24, 16, 0));
        let event = draft.to_event("pmnew", "pmmeet");
        assert_eq!(event.rules[0], "RRULE:FREQ=WEEKLY;BYDAY=TH");
        assert_eq!(event.rules[1], "EXDATE;TZID=Europe/Lisbon:20260930T150000", "excluded dates stay");
    }

    #[test]
    fn a_new_repeat_replaces_the_rule_and_keeps_excluded_dates() {
        let mut draft = Draft::open(&weekly(), &weekly().event.rules, Lisbon);
        draft.repeat = Repeat::EveryDay;
        assert!(draft.rule_changed());
        assert_eq!(draft.to_event("pmnew", "pmmeet").rules[0], "RRULE:FREQ=DAILY");
        draft.repeat = Repeat::Never;
        assert!(draft.to_event("pmnew", "pmmeet").rules.is_empty());
    }

    #[test]
    fn a_kept_rule_stays_when_the_series_moves() {
        // BYSETPOS is not a rule the menu or the Custom page can say, so
        // `Repeat::read` keeps it whole as `Kept`.
        let mut event = weekly();
        Arc::make_mut(&mut event.event).rules = vec!["RRULE:FREQ=MONTHLY;BYDAY=MO,TU;BYSETPOS=-1".into()];
        let mut draft = Draft::open(&event, &event.event.rules, Lisbon);
        assert_eq!(draft.repeat, Repeat::Kept("RRULE:FREQ=MONTHLY;BYDAY=MO,TU;BYSETPOS=-1".into()));
        draft.set_span(at(24, 15, 0), at(24, 16, 0));
        assert!(!draft.rule_changed());
        assert_eq!(draft.to_event("pmnew", "pmmeet").rules, event.event.rules, "a kept rule ignores the move");
    }

    #[test]
    fn meet_is_asked_for_only_when_the_event_has_no_link() {
        let mut draft = fresh();
        draft.add_meet = true;
        assert_eq!(draft.to_event("pmnew", "pmmeet").meet_request.as_deref(), Some("pmmeet"));
        let mut linked = weekly();
        Arc::make_mut(&mut linked.event).conference = Some("https://meet.google.com/abc-defg-hij".into());
        let mut draft = Draft::open(&linked, &linked.event.rules, Lisbon);
        draft.add_meet = true;
        assert_eq!(draft.to_event("pmnew", "pmmeet").meet_request, None);
    }

    #[test]
    fn the_default_calendar_follows_the_last_account_used() {
        let work = Calendar { id: "me@work.pt".into(), ..personal() };
        let team = Calendar { id: "team".into(), primary: false, ..personal() };
        let writable = vec![
            (1, "me@example.com".to_string(), personal()),
            (2, "me@work.pt".to_string(), team.clone()),
            (2, "me@work.pt".to_string(), work.clone()),
        ];
        assert_eq!(default_calendar(&writable, Some("ME@work.pt")).map(|(a, c)| (a, c.id)), Some((2, "me@work.pt".into())));
        assert_eq!(default_calendar(&writable, None).map(|(a, c)| (a, c.id)), Some((1, "me@example.com".into())));
        assert_eq!(default_calendar(&writable[1..2], None).map(|(_, c)| c.id), Some("team".into()));
        assert_eq!(default_calendar(&[], None), None);
    }

    #[test]
    fn an_invitation_from_someone_else_is_limited() {
        let mut event = Event::clone(&weekly().event);
        assert!(!limited(&event));
        event.guests = vec![
            Guest { email: "ana@example.com".into(), organizer: true, answer: Some(Answer::Yes), ..Guest::default() },
            Guest { email: "me@example.com".into(), me: true, ..Guest::default() },
        ];
        assert!(limited(&event));
    }

    #[test]
    fn a_guest_may_only_change_reminders_colour_and_busy() {
        let mut event = Event::clone(&weekly().event);
        event.guests = vec![
            Guest { email: "ana@example.com".into(), organizer: true, answer: Some(Answer::Yes), ..Guest::default() },
            Guest { email: "me@example.com".into(), me: true, ..Guest::default() },
        ];
        let occurrence = Occurrence { account_id: 1, start: event.start, end: event.end, event: Arc::new(event) };
        let mut draft = Draft::open(&occurrence, &occurrence.event.rules, Lisbon);
        draft.title = "Renamed".into();
        draft.set_span(at(24, 15, 0), at(24, 16, 0));
        draft.reminders = Some(vec![Reminder { minutes: 5, method: ReminderMethod::Notification }]);
        draft.color = Some("#00ff00".into());
        draft.busy = false;
        let saved = draft.to_event("pmnew", "pmmeet");
        assert_eq!(saved.title, "Review", "a guest cannot rename someone else's event");
        assert_eq!((saved.start, saved.end), (occurrence.start, occurrence.end), "a guest cannot move it");
        assert_eq!(saved.reminders, draft.reminders);
        assert_eq!(saved.color, draft.color);
        assert!(!saved.busy);
    }

    fn invitation() -> Occurrence {
        let mut event = Event::clone(&weekly().event);
        event.guests = vec![
            Guest { email: "ana@example.com".into(), organizer: true, answer: Some(Answer::Yes), ..Guest::default() },
            Guest { email: "me@example.com".into(), me: true, ..Guest::default() },
        ];
        Occurrence { account_id: 1, start: event.start, end: event.end, event: Arc::new(event) }
    }

    #[test]
    fn an_owner_on_a_writable_calendar_edits_the_whole_event() {
        assert_eq!(editing(&weekly().event, Access::Owner, true, false), Editing::Whole);
    }

    #[test]
    fn a_guest_on_a_writable_calendar_gets_the_limited_editor() {
        assert_eq!(editing(&invitation().event, Access::Owner, true, false), Editing::Guest);
    }

    #[test]
    fn a_withheld_calendar_permission_asks_for_it_instead_of_editing() {
        assert_eq!(editing(&weekly().event, Access::Owner, true, true), Editing::NeedsPermission);
        assert_eq!(editing(&invitation().event, Access::Owner, true, true), Editing::NeedsPermission);
    }

    #[test]
    fn a_read_only_calendar_or_an_account_with_no_calendar_edits_nothing() {
        assert_eq!(editing(&weekly().event, Access::Reader, true, false), Editing::None);
        assert_eq!(editing(&weekly().event, Access::Owner, false, false), Editing::None);
    }

    #[test]
    fn a_guest_is_never_offered_this_and_following() {
        let draft = Draft::open(&invitation(), &invitation().event.rules, Lisbon);
        assert_eq!(draft.scopes(), vec![RepeatScope::This, RepeatScope::All]);
    }

    #[test]
    fn an_owner_is_offered_every_scope() {
        let draft = Draft::open(&weekly(), &weekly().event.rules, Lisbon);
        assert_eq!(draft.scopes(), vec![RepeatScope::This, RepeatScope::Following, RepeatScope::All]);
        assert!(fresh().scopes().is_empty(), "a new event asks nothing");
    }

    #[test]
    fn writable_calendars_sort_by_account_then_primary_then_name() {
        let named = |id: &str, name: &str, primary: bool| Calendar { id: id.into(), name: name.into(), primary, ..personal() };
        let mut writable = vec![
            (2, "me@work.pt".to_string(), named("team", "Design team", false)),
            (1, "me@example.com".to_string(), named("family", "Family", false)),
            (2, "me@work.pt".to_string(), named("me@work.pt", "Work", true)),
            (1, "me@example.com".to_string(), named("birthdays", "Birthdays", false)),
            (1, "me@example.com".to_string(), named("me@example.com", "Personal", true)),
        ];
        sort_writable(&mut writable, &[1, 2]);
        let order: Vec<&str> = writable.iter().map(|(_, _, c)| c.name.as_str()).collect();
        assert_eq!(order, ["Personal", "Birthdays", "Family", "Work", "Design team"]);
    }

    #[test]
    fn the_time_list_is_quarter_hours_and_the_odd_time_it_holds() {
        let on = time_choices(NaiveTime::from_hms_opt(10, 15, 0).unwrap());
        assert_eq!(on.len(), 96);
        let odd = time_choices(NaiveTime::from_hms_opt(10, 7, 0).unwrap());
        assert_eq!(odd.len(), 97);
        assert!(odd.windows(2).all(|w| w[0] < w[1]));
        assert!(odd.contains(&NaiveTime::from_hms_opt(10, 7, 0).unwrap()));
    }
}
