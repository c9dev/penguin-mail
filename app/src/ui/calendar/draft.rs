//! What the event editor and quick create hold while a person changes an
//! event, and the event it comes to. No widgets here: the dialog shows a
//! `Draft` and calls its setters, so the rules (the end moves with the
//! start, Save needs a title, the repeat's day follows the start) live
//! under tests.

use std::collections::HashSet;

use chrono::{NaiveDate, NaiveTime, TimeZone};
use chrono::Duration;
use chrono_tz::Tz;
use mailrs_domain::calendar::repeat::Repeat;
use mailrs_domain::calendar::{
    Access, Attachment, Calendar, Decline, Declines, Event, Guest, Kind, Occurrence, Reminder,
};
use mailrs_domain::translate::gettext;
use mailrs_domain::{AccountId, EpochMillis};
use mailrs_sync::Offers;

const DAY: EpochMillis = 86_400_000;
const HOUR: EpochMillis = 3_600_000;

/// The editor's Type choice for a new event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TypeChoice {
    Event,
    OutOfOffice,
    Focus,
}

/// The order every list of types shows them in: the editor's Type row,
/// the New Event menu and quick add's switch.
pub const TYPE_ORDER: [TypeChoice; 3] = [TypeChoice::Event, TypeChoice::Focus, TypeChoice::OutOfOffice];

/// The types the account's primary calendar takes.
pub fn account_types(offers: &Offers) -> Vec<TypeChoice> {
    TYPE_ORDER
        .into_iter()
        .filter(|t| match t {
            TypeChoice::Event => true,
            TypeChoice::Focus => offers.focus_time,
            TypeChoice::OutOfOffice => offers.out_of_office,
        })
        .collect()
}

/// The Type choices a new entry on `calendar` may take, by what its
/// account's provider keeps (`Offers`). Event always; focus time and out
/// of office only on a primary calendar, the one calendar Google and
/// Outlook keep them on, and only where the provider stores them.
pub fn creatable_types(offers: &Offers, calendar: &Calendar) -> Vec<TypeChoice> {
    if calendar.primary && calendar.access.can_write() {
        account_types(offers)
    } else {
        vec![TypeChoice::Event]
    }
}

/// The Type choices quick add's switch shows for `calendar`, or `None`
/// when it takes events alone and the switch would offer nothing.
pub fn type_switch(offers: &Offers, calendar: &Calendar) -> Option<Vec<TypeChoice>> {
    Some(creatable_types(offers, calendar)).filter(|types| types.len() > 1)
}

/// `choice`, when `types` still holds it, else an ordinary event: quick
/// add keeps the type across a change of calendar only where the new
/// calendar takes it.
pub fn kept_type(choice: TypeChoice, types: &[TypeChoice]) -> TypeChoice {
    if types.contains(&choice) { choice } else { TypeChoice::Event }
}

/// What the New Event menu offers: each type at least one of the
/// `offered` calendars takes, in the order the menu lists them.
pub fn menu_types(offered: &[(AccountId, String, Calendar)], offers: impl Fn(AccountId) -> Offers) -> Vec<TypeChoice> {
    let taken: HashSet<TypeChoice> =
        offered.iter().flat_map(|(account, _, calendar)| creatable_types(&offers(*account), calendar)).collect();
    TYPE_ORDER.into_iter().filter(|t| *t == TypeChoice::Event || taken.contains(t)).collect()
}

/// The calendar a new entry of type `choice` goes on: `default` when it
/// takes the type, else the first of `offered` that does.
pub fn calendar_for(
    choice: TypeChoice,
    offered: &[(AccountId, String, Calendar)],
    offers: impl Fn(AccountId) -> Offers,
    default: &(AccountId, Calendar),
) -> Option<(AccountId, Calendar)> {
    let takes = |account: AccountId, calendar: &Calendar| creatable_types(&offers(account), calendar).contains(&choice);
    if takes(default.0, &default.1) {
        return Some(default.clone());
    }
    offered
        .iter()
        .find(|(account, _, calendar)| takes(*account, calendar))
        .map(|(account, _, calendar)| (*account, calendar.clone()))
}

/// The title a Type choice fills in: "Out of office", "Focus time", or
/// nothing for an ordinary event.
pub fn type_title(choice: TypeChoice) -> String {
    match choice {
        TypeChoice::Event => String::new(),
        TypeChoice::OutOfOffice => gettext("Out of office"),
        TypeChoice::Focus => gettext("Focus time"),
    }
}

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
    /// The description as lines to edit. Google may hold it as HTML, and
    /// the event keeps that HTML until the person changes these lines.
    pub notes: String,
    pub busy: bool,
    pub private: bool,
    pub color: Option<String>,
    pub add_meet: bool,
    /// An ordinary event, out of office or focus time. A working location
    /// or a birthday never reaches the editor.
    pub kind: Kind,
    /// The files linked to the event, Drive files and files waiting to
    /// upload alike.
    pub attachments: Vec<Attachment>,
    /// Whether the copy has read the event's attachments. An event stored
    /// before it did holds none that the draft can see, and saving a list
    /// then would take Google's files off.
    attachments_known: bool,
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
            kind: Kind::Event,
            attachments: Vec::new(),
            attachments_known: true,
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
            notes: mailrs_mime::notes::text(&event.description),
            busy: event.busy,
            private: event.private,
            color: event.color.clone(),
            add_meet: false,
            kind: event.kind.clone(),
            attachments: event.attachments.clone().unwrap_or_default(),
            attachments_known: event.attachments.is_some(),
            base: Some(Event::clone(event)),
            occurrence: Some(occurrence.clone()),
            opened: Opened { rules: series_rules.to_vec(), repeat, day },
            view_zone,
        }
    }

    /// Whether the person gave the occurrence they opened another time,
    /// which the calendar confirms before writing.
    pub fn moved(&self) -> bool {
        self.occurrence.as_ref().is_some_and(|o| (o.start, o.end) != (self.start, self.end))
    }

    /// The draft as the editor opened it, for telling what changed.
    /// `None` for a new event.
    pub fn before(&self) -> Option<Draft> {
        // `open` reads only the occurrence, the series rules and the zone,
        // all of which the draft keeps, so opening again gives back the
        // draft the editor started from.
        let o = self.occurrence.as_ref()?;
        Some(Draft::open(o, &self.opened.rules, self.view_zone))
    }

    /// This draft with the time it opened with: the other edits stay.
    /// What an editor save writes when the person turns the new time
    /// down in the confirmation.
    pub fn without_move(&self) -> Draft {
        let mut kept = self.clone();
        if let Some(before) = self.before() {
            kept.start = before.start;
            kept.end = before.end;
            kept.all_day = before.all_day;
            kept.zone = before.zone;
        }
        kept
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

    /// Where a drag landed the event: `start` to `end`, all-day or not.
    /// Unlike [`Draft::set_all_day`], the span is the drop's own. An
    /// event that stops being all-day leaves the "UTC" zone all-day
    /// events are written in for the zone the reader sees it in.
    pub fn land(&mut self, start: EpochMillis, end: EpochMillis, all_day: bool) {
        if self.all_day && !all_day && self.zone.parse::<Tz>().is_ok_and(|z| z == Tz::UTC) {
            self.zone = self.view_zone.name().to_string();
        }
        self.all_day = all_day;
        self.set_span(start, end);
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

    /// Adds each address in `text`, as the Guests field holds it, that is
    /// not on the list yet. Answers the parts that are not addresses. The
    /// editor runs this on Enter, on a picked suggestion and again on
    /// Save, since a Save that ignored the field left a typed guest
    /// uninvited.
    pub fn add_guests(&mut self, text: &str) -> Vec<String> {
        let mut refused = Vec::new();
        for address in crate::compose::parse_recipients(text) {
            if !crate::compose::is_address(&address.email) {
                refused.push(address.email);
            } else if !self.guests.iter().any(|g| g.email.eq_ignore_ascii_case(&address.email)) {
                self.guests.push(Guest { email: address.email, name: address.name, ..Guest::default() });
            }
        }
        refused
    }

    /// Adds the guests in `text`, as [`Self::add_guests`] does, and
    /// answers what the Guests field keeps: the parts that are not
    /// addresses, or nothing. Enter, a picked suggestion and Save all come
    /// here, so a pick adds its guest at once.
    pub fn take_guests(&mut self, text: &str) -> String {
        self.add_guests(text).join(", ")
    }

    /// Whether the editor offers the Type choice: only for a new event,
    /// since Google never changes an entry's type, and only on a primary
    /// calendar, the one calendar Google keeps out of office and focus
    /// time on.
    pub fn offers_type(&self, primary: bool) -> bool {
        self.is_new() && primary
    }

    /// The Type choice the draft stands for.
    pub fn type_choice(&self) -> TypeChoice {
        match self.kind {
            Kind::OutOfOffice(_) => TypeChoice::OutOfOffice,
            Kind::Focus(_) => TypeChoice::Focus,
            _ => TypeChoice::Event,
        }
    }

    /// Makes the draft an ordinary event, out of office or focus time.
    ///
    /// Out of office and focus time run for hours, as Google requires,
    /// and always block the time. An empty title, or the one the last
    /// choice filled in, becomes the new choice's name, as Google's own
    /// editor does; a title the person typed stays.
    pub fn set_type(&mut self, choice: TypeChoice) {
        if choice == self.type_choice() {
            return;
        }
        if self.title.trim().is_empty() || self.title == type_title(self.type_choice()) {
            self.title = type_title(choice);
        }
        self.kind = match choice {
            TypeChoice::Event => Kind::Event,
            TypeChoice::OutOfOffice => Kind::OutOfOffice(Decline {
                meetings: Declines::All,
                message: gettext("Declined because I am out of office"),
            }),
            TypeChoice::Focus => Kind::Focus(Decline::default()),
        };
        if choice != TypeChoice::Event {
            self.set_all_day(false);
            self.busy = true;
        }
    }

    /// Makes a new draft `choice` with the type's own defaults, for the
    /// New Event menu and quick add. Focus time runs at least two hours
    /// from the picked time. Out of office covers whole days, from today
    /// through the picked day (or the picked day alone when it is past),
    /// midnight to midnight in the reader's zone, since Google refuses
    /// one all day. Where the provider keeps no decline choice
    /// (`declines` false, as on Outlook), the entry declines nothing.
    pub fn new_as(&mut self, choice: TypeChoice, today: NaiveDate, declines: bool) {
        self.set_type(choice);
        if !declines {
            self.clear_declines();
        }
        match choice {
            TypeChoice::Event => {}
            TypeChoice::Focus => {
                if self.end - self.start <= HOUR {
                    self.end = self.start + 2 * HOUR;
                }
            }
            TypeChoice::OutOfOffice => {
                let picked = local_day(self.start, self.view_zone);
                let first = today.min(picked);
                let midnight = |day: NaiveDate| {
                    let at = day.and_hms_opt(0, 0, 0).expect("midnight exists");
                    // A zone that skips midnight starts the day at the
                    // first instant it has.
                    self.view_zone
                        .from_local_datetime(&at)
                        .earliest()
                        .or_else(|| self.view_zone.from_local_datetime(&(at + Duration::hours(1))).earliest())
                        .map_or(utc_midnight(day), |t| t.timestamp_millis())
                };
                self.start = midnight(first);
                self.end = midnight(picked + Duration::days(1));
            }
        }
    }

    /// Makes an out-of-office or focus-time draft decline nothing, with no
    /// message, for a provider that keeps neither.
    pub fn clear_declines(&mut self) {
        if let Kind::OutOfOffice(decline) | Kind::Focus(decline) = &mut self.kind {
            *decline = Decline::default();
        }
    }

    /// Which meetings an out-of-office or focus-time draft declines.
    pub fn set_declines(&mut self, meetings: Declines) {
        if let Kind::OutOfOffice(decline) | Kind::Focus(decline) = &mut self.kind {
            decline.meetings = meetings;
        }
    }

    /// The words an organizer gets with each meeting it declines.
    pub fn set_decline_message(&mut self, message: &str) {
        if let Kind::OutOfOffice(decline) | Kind::Focus(decline) = &mut self.kind {
            decline.message = message.to_string();
        }
    }

    /// Whether "Attach File…" may add to the list: the copy knows the
    /// event's files, the account organizes it, and it is an ordinary
    /// event, not out of office or focus time.
    pub fn can_attach(&self) -> bool {
        self.attachments_known && !self.base.as_ref().is_some_and(limited) && self.kind == Kind::Event
    }

    /// Adds a file to the event.
    pub fn attach(&mut self, file: Attachment) {
        self.attachments.push(file);
    }

    /// Takes the file at `index` off the event. The file itself stays
    /// where it is, on Drive or on this computer.
    pub fn detach(&mut self, index: usize) {
        if index < self.attachments.len() {
            self.attachments.remove(index);
        }
    }

    /// Whether the guests may open the file at `index`, one the app
    /// uploaded. A file someone else attached stays as it is: the app
    /// cannot share it.
    pub fn set_share(&mut self, index: usize, share: bool) {
        if let Some(file) = self.attachments.get_mut(index)
            && file.share.is_some()
        {
            file.share = Some(share);
        }
    }

    /// Whether anyone but the account itself is invited, the people an
    /// uploaded file is shared with.
    pub fn has_other_guests(&self) -> bool {
        self.guests.iter().any(|guest| !guest.me)
    }

    pub fn can_save(&self) -> bool {
        !self.title.trim().is_empty()
    }

    pub fn rule_changed(&self) -> bool {
        self.repeat != self.opened.repeat
    }

    /// The event the draft stands for. A new one takes `new_id`; a Meet
    /// link is asked for under `meet_request` when the person turned it on
    /// and the event has none yet.
    ///
    /// On someone else's event the account is only a guest of ([`limited`]),
    /// only the guest's reminders, colour and busy change, since Google
    /// keeps a guest's changes to anything else to the organizer. For
    /// an occurrence nobody changed, `base` is the series with its first
    /// occurrence's times; `series::change` takes only those three fields
    /// from it, and the PATCH carries nothing else.
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
        // Notes nobody touched go back as Google holds them, since turning
        // its HTML into lines and back would drop its formatting.
        if self.base.as_ref().is_none_or(|base| mailrs_mime::notes::text(&base.description) != self.notes) {
            event.description = mailrs_mime::notes::html(&self.notes);
        }
        event.busy = self.busy;
        event.private = self.private;
        event.color = self.color.clone();
        event.rules = self.rules();
        event.meet_request = (self.add_meet && event.conference.is_none()).then(|| meet_request.to_string());
        event.kind = self.kind.clone();
        event.attachments = self.attachments_known.then(|| self.attachments.clone());
        if event.kind.decline().is_some() {
            // Google refuses guests, a place and a Meet link on out of
            // office and focus time.
            event.guests.clear();
            event.place.clear();
            event.meet_request = None;
            event.all_day = false;
            event.busy = true;
        }
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

/// Whether going from `before` to `after` changes something the guests
/// see: the title, the time, the place, the notes, the guest list, the
/// Meet link, the repeat, the attachments, or the calendar, whose owner
/// becomes the organizer. Reminders, colour, busy or free and privacy are the
/// account's own, and Google sends nobody mail about them.
pub fn reaches_guests(before: &Draft, after: &Draft) -> bool {
    // The save trims the title and the place, so compare what it writes.
    before.title.trim() != after.title.trim()
        || (before.start, before.end, before.all_day) != (after.start, after.end, after.all_day)
        || before.zone != after.zone
        || before.place.trim() != after.place.trim()
        || before.notes != after.notes
        || before.guests != after.guests
        || (after.add_meet && !before.add_meet)
        || before.repeat != after.repeat
        || before.calendar != after.calendar
        || before.attachments != after.attachments
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
/// guest. The editor then leaves the time, place and guests to them,
/// and the popover asks this account for an answer.
pub fn limited(event: &Event) -> bool {
    event.limited()
}

/// One part of the editor's form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    Title,
    When,
    Repeat,
    Calendar,
    Place,
    Guests,
    Reminders,
    Notes,
    Zone,
    Busy,
    Private,
    Color,
    Meet,
}

/// Whether the editor lets the person change `part` of `event`, `None`
/// for a new one. A guest owns only their reminders, the colour and busy
/// or free on their copy; Google leaves the rest, the calendar included,
/// to the organizer, so the editor shows it read-only.
pub fn may_change(event: Option<&Event>, part: Part) -> bool {
    !event.is_some_and(limited) || matches!(part, Part::Reminders | Part::Busy | Part::Color)
}

/// The calendars the editor's Calendar row offers: any the person can
/// write to for a new event, and for an existing one those of its own
/// account, since a move between accounts is a copy and a delete that
/// Google does not do in one call. A calendar the person took off the
/// sidebar's list (`hidden`, by account and id) is left out, except the
/// draft's own, so the row never shows blank.
pub fn calendar_choices<'a>(
    draft: &Draft,
    writable: &'a [(AccountId, String, Calendar)],
    hidden: &HashSet<(AccountId, String)>,
) -> Vec<&'a (AccountId, String, Calendar)> {
    writable
        .iter()
        .filter(|(account, _, _)| draft.is_new() || *account == draft.account_id)
        .filter(|(account, _, calendar)| {
            (*account == draft.account_id && calendar.id == draft.calendar)
                || !hidden.contains(&(*account, calendar.id.clone()))
        })
        .collect()
}

/// `writable` without the calendars the person took off the list, for
/// quick create's menu and the calendar a new event starts on.
pub fn without_hidden(
    writable: Vec<(AccountId, String, Calendar)>,
    hidden: &HashSet<(AccountId, String)>,
) -> Vec<(AccountId, String, Calendar)> {
    writable
        .into_iter()
        .filter(|(account, _, calendar)| !hidden.contains(&(*account, calendar.id.clone())))
        .collect()
}

/// What taking an event off the calendar does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Removal {
    /// Delete it for everyone, which the popover calls Delete.
    Event,
    /// Take it off this account's calendar alone, which the popover calls
    /// Remove. Google marks the guest as having declined.
    OwnCopy,
}

/// The buttons the event popover shows beside the title.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Popover {
    pub edit: bool,
    pub removal: Removal,
}

/// What a person may do to an event from the calendar view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Editing {
    /// Change or delete the whole event: the popover shows Edit and
    /// Delete, and a double click opens the editor.
    Whole,
    /// Someone else organizes it: Edit and a double click open the
    /// editor limited to reminders, colour and busy, and the popover's
    /// Remove takes it off this account's calendar alone.
    Guest,
    /// The account withheld the calendar permission: a double click, Enter
    /// or Delete asks for it, and the popover shows neither.
    NeedsPermission,
    /// A read-only calendar, or an account with no calendar at all.
    None,
}

impl Editing {
    /// The popover's Edit and Delete or Remove, `None` when it shows
    /// neither.
    pub fn popover(self) -> Option<Popover> {
        match self {
            Editing::Whole => Some(Popover { edit: true, removal: Removal::Event }),
            Editing::Guest => Some(Popover { edit: true, removal: Removal::OwnCopy }),
            Editing::NeedsPermission | Editing::None => None,
        }
    }
}

/// What the view lets a person do to `event`, from its calendar's
/// access and what the account's provider offers and its consent
/// withheld.
pub fn editing(event: &Event, access: Access, offers_calendar: bool, withheld_calendar: bool) -> Editing {
    if !offers_calendar || !access.can_write() || event.kind.made_elsewhere() {
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
    use std::collections::HashSet;
    use std::sync::Arc;

    use super::*;
    use chrono::{NaiveDate, TimeZone};
    use chrono_tz::Europe::Lisbon;
    use mailrs_domain::calendar::{Reminder, ReminderMethod};
    use mailrs_domain::invitation::Answer;
    use mailrs_sync::Offers;

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
            hidden: false,
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
    fn an_address_picked_from_the_suggestions_becomes_a_guest() {
        let mut draft = fresh();
        let refused = draft.add_guests("Ann Lee <ann@example.com>, ");
        assert!(refused.is_empty());
        assert_eq!(draft.guests.len(), 1);
        assert_eq!(draft.guests[0].email, "ann@example.com");
        assert_eq!(draft.guests[0].name.as_deref(), Some("Ann Lee"));
    }

    #[test]
    fn a_guest_already_on_the_list_is_not_added_twice() {
        let mut draft = fresh();
        draft.add_guests("ann@example.com");
        draft.add_guests("ANN@example.com, bo@example.com");
        let emails: Vec<&str> = draft.guests.iter().map(|g| g.email.as_str()).collect();
        assert_eq!(emails, ["ann@example.com", "bo@example.com"]);
    }

    #[test]
    fn text_that_is_not_an_address_comes_back_and_adds_nothing() {
        let mut draft = fresh();
        let refused = draft.add_guests("ann@example.com, bob");
        assert_eq!(refused, ["bob"]);
        assert_eq!(draft.guests.len(), 1, "the valid address still joins");
    }

    fn opened() -> Draft {
        Draft::open(&weekly(), &weekly().event.rules, Lisbon)
    }

    fn agenda() -> mailrs_domain::calendar::Attachment {
        mailrs_domain::calendar::Attachment {
            title: "Agenda.pdf".into(),
            file_url: "https://drive.google.com/file/d/1abc/view".into(),
            mime_type: "application/pdf".into(),
            file_id: "1abc".into(),
            ..Default::default()
        }
    }

    fn with_attachments(files: Option<Vec<mailrs_domain::calendar::Attachment>>) -> Draft {
        let mut occurrence = weekly();
        Arc::make_mut(&mut occurrence.event).attachments = files;
        Draft::open(&occurrence, &occurrence.event.rules, Lisbon)
    }

    #[test]
    fn an_event_opens_with_its_attachments_and_saves_them_back() {
        let draft = with_attachments(Some(vec![agenda()]));
        assert_eq!(draft.attachments, vec![agenda()]);
        assert_eq!(draft.to_event("pmnew", "pmmeet").attachments, Some(vec![agenda()]));
    }

    #[test]
    fn a_new_event_starts_with_no_attachments_it_can_add_to() {
        let draft = fresh();
        assert!(draft.can_attach());
        assert_eq!(draft.to_event("pmnew", "pmmeet").attachments, Some(Vec::new()));
    }

    #[test]
    fn an_event_whose_attachments_are_unknown_saves_them_unknown() {
        let draft = with_attachments(None);
        assert!(!draft.can_attach(), "adding to an unread list would drop the files Google holds");
        assert_eq!(draft.to_event("pmnew", "pmmeet").attachments, None);
    }

    #[test]
    fn a_picked_file_joins_the_list_and_a_removed_one_leaves_it() {
        let mut draft = with_attachments(Some(vec![agenda()]));
        let waiting = mailrs_domain::calendar::Attachment {
            title: "Notes.txt".into(),
            waiting: Some("/home/me/Notes.txt".into()),
            ..Default::default()
        };
        draft.attach(waiting.clone());
        assert_eq!(draft.attachments, vec![agenda(), waiting.clone()]);
        draft.detach(0);
        assert_eq!(draft.to_event("pmnew", "pmmeet").attachments, Some(vec![waiting]));
    }

    #[test]
    fn a_guest_cannot_attach_files_to_someone_elses_event() {
        let mut occurrence = weekly();
        let event = Arc::make_mut(&mut occurrence.event);
        event.attachments = Some(vec![agenda()]);
        event.guests = vec![Guest { email: "me@example.com".into(), me: true, ..Guest::default() }];
        let draft = Draft::open(&occurrence, &occurrence.event.rules, Lisbon);
        assert!(!draft.can_attach());
        assert_eq!(draft.to_event("pmnew", "pmmeet").attachments, Some(vec![agenda()]));
    }

    #[test]
    fn unticking_sharing_keeps_the_file_to_the_organizer() {
        let ours = mailrs_domain::calendar::Attachment { share: Some(true), ..agenda() };
        let mut draft = with_attachments(Some(vec![ours]));
        draft.set_share(0, false);
        assert_eq!(draft.to_event("pmnew", "pmmeet").attachments.unwrap()[0].share, Some(false));
    }

    #[test]
    fn sharing_stays_off_a_file_someone_else_attached() {
        let mut draft = with_attachments(Some(vec![agenda()]));
        draft.set_share(0, true);
        assert_eq!(draft.attachments[0].share, None);
    }

    #[test]
    fn only_guests_other_than_the_account_count_for_sharing() {
        let mut draft = fresh();
        draft.guests = vec![Guest { email: "me@example.com".into(), me: true, ..Guest::default() }];
        assert!(!draft.has_other_guests());
        draft.add_guests("ana@example.com");
        assert!(draft.has_other_guests());
    }

    #[test]
    fn out_of_office_takes_no_attachments() {
        let mut draft = fresh();
        draft.set_type(TypeChoice::OutOfOffice);
        assert!(!draft.can_attach());
    }

    #[test]
    fn the_guests_see_a_file_attached() {
        let before = with_attachments(Some(Vec::new()));
        let mut after = before.clone();
        after.attach(agenda());
        assert!(reaches_guests(&before, &after));
    }

    /// Notes as Google's own editor writes them.
    const GOOGLE_NOTES: &str = r#"Bring the numbers<br><a href="https://example.com/q3">Q3 sheet</a>"#;

    fn with_google_notes() -> Draft {
        let mut occurrence = weekly();
        Arc::make_mut(&mut occurrence.event).description = GOOGLE_NOTES.into();
        Draft::open(&occurrence, &occurrence.event.rules, Lisbon)
    }

    #[test]
    fn google_s_html_notes_open_as_lines() {
        assert_eq!(with_google_notes().notes, "Bring the numbers\nQ3 sheet (https://example.com/q3)");
    }

    #[test]
    fn notes_left_alone_go_back_exactly_as_google_holds_them() {
        let mut draft = with_google_notes();
        draft.title = "Quarterly review".into();
        assert_eq!(draft.to_event("", "").description, GOOGLE_NOTES);
    }

    #[test]
    fn edited_notes_go_out_as_html_with_the_link_working() {
        let mut draft = with_google_notes();
        draft.notes.push_str("\nRoom 5");
        assert_eq!(
            draft.to_event("", "").description,
            "Bring the numbers<br>Q3 sheet (<a href=\"https://example.com/q3\">https://example.com/q3</a>)<br>Room 5"
        );
    }

    fn reaches(change: impl FnOnce(&mut Draft)) -> bool {
        let before = opened();
        let mut after = before.clone();
        change(&mut after);
        reaches_guests(&before, &after)
    }

    #[test]
    fn the_guests_see_a_new_title() {
        assert!(reaches(|d| d.title = "Planning".into()));
    }

    #[test]
    fn the_guests_do_not_see_spaces_the_save_trims() {
        assert!(!reaches(|d| d.title = "Review ".into()));
    }

    #[test]
    fn the_guests_see_a_new_start() {
        assert!(reaches(|d| d.set_start(at(23, 16, 0))));
    }

    #[test]
    fn the_guests_see_a_new_end() {
        assert!(reaches(|d| d.set_end(at(23, 17, 0))));
    }

    #[test]
    fn the_guests_see_the_event_become_all_day() {
        assert!(reaches(|d| d.set_all_day(true)));
    }

    #[test]
    fn the_guests_see_a_new_time_zone() {
        assert!(reaches(|d| d.zone = "Europe/Madrid".into()));
    }

    #[test]
    fn the_guests_see_a_new_place() {
        assert!(reaches(|d| d.place = "Room 4".into()));
    }

    #[test]
    fn the_guests_see_new_notes() {
        assert!(reaches(|d| d.notes = "Bring the numbers".into()));
    }

    #[test]
    fn the_guests_see_a_guest_added() {
        assert!(reaches(|d| {
            d.add_guests("ann@example.com");
        }));
    }

    #[test]
    fn the_guests_see_a_guest_removed() {
        let mut before = opened();
        before.add_guests("ann@example.com, bo@example.com");
        let mut after = before.clone();
        after.guests.pop();
        assert!(reaches_guests(&before, &after));
    }

    #[test]
    fn the_guests_see_a_meet_link_asked_for() {
        assert!(reaches(|d| d.add_meet = true));
    }

    #[test]
    fn the_guests_see_a_new_repeat() {
        assert!(reaches(|d| d.repeat = Repeat::EveryDay));
    }

    #[test]
    fn the_guests_do_not_see_the_reminders() {
        assert!(!reaches(|d| d.reminders = Some(Vec::new())));
    }

    #[test]
    fn the_guests_do_not_see_the_colour() {
        assert!(!reaches(|d| d.color = Some("#e8660c".into())));
    }

    #[test]
    fn the_guests_do_not_see_busy_or_free() {
        assert!(!reaches(|d| d.busy = false));
    }

    #[test]
    fn the_guests_do_not_see_privacy() {
        assert!(!reaches(|d| d.private = true));
    }

    #[test]
    fn a_save_with_no_change_reaches_nobody() {
        assert!(!reaches(|_| {}));
    }

    #[test]
    fn the_draft_remembers_how_it_opened() {
        let mut draft = opened();
        draft.title = "Planning".into();
        assert_eq!(draft.before(), Some(opened()));
        assert_eq!(fresh().before(), None);
    }

    #[test]
    fn turning_down_the_new_time_keeps_the_other_edits() {
        let mut draft = opened();
        draft.title = "Planning".into();
        draft.place = "Room 4".into();
        draft.set_all_day(true);
        let kept = draft.without_move();
        assert!(!kept.moved());
        assert!(!kept.all_day);
        assert_eq!((kept.start, kept.end), (at(23, 15, 0), at(23, 16, 0)));
        assert_eq!(kept.title, "Planning");
        assert_eq!(kept.place, "Room 4");
    }

    #[test]
    fn a_new_time_on_an_opened_occurrence_is_a_move() {
        let mut draft = Draft::open(&weekly(), &weekly().event.rules, Lisbon);
        assert!(!draft.moved());
        draft.title = "Renamed".into();
        assert!(!draft.moved(), "a new title is no move");
        draft.set_start(at(23, 16, 0));
        assert!(draft.moved());
    }

    #[test]
    fn a_new_event_is_never_a_move() {
        let mut draft = fresh();
        draft.set_start(at(24, 9, 0));
        assert!(!draft.moved());
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
    fn a_timed_event_dropped_on_the_all_day_row_is_written_all_day_in_utc() {
        let mut draft = Draft::open(&weekly(), &weekly().event.rules, Lisbon);
        draft.land(utc_midnight(24), utc_midnight(25), true);
        let event = draft.to_event("new", "meet");
        assert_eq!((event.all_day, event.start, event.end, event.zone.as_str()), (true, utc_midnight(24), utc_midnight(25), "UTC"));
    }

    #[test]
    fn an_all_day_event_dropped_in_the_hours_takes_the_viewer_s_zone() {
        let mut o = weekly();
        let mut event = (*o.event).clone();
        (event.all_day, event.zone, event.rules) = (true, "UTC".into(), Vec::new());
        o.event = Arc::new(event);
        (o.start, o.end) = (utc_midnight(24), utc_midnight(25));
        let mut draft = Draft::open(&o, &[], Lisbon);
        draft.land(at(24, 14, 0), at(24, 15, 0), false);
        let event = draft.to_event("new", "meet");
        assert_eq!((event.all_day, event.start, event.end, event.zone.as_str()), (false, at(24, 14, 0), at(24, 15, 0), "Europe/Lisbon"));
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

    /// Wednesday 23 September's occurrence of a stand-up that runs Monday
    /// to Friday from Monday 21.
    fn weekdays() -> Occurrence {
        let series = Event {
            id: "standup".into(),
            start: at(21, 9, 30),
            end: at(21, 9, 45),
            title: "Stand-up".into(),
            rules: vec!["RRULE:FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR".into()],
            ..Event::clone(&weekly().event)
        };
        Occurrence { account_id: 1, start: at(23, 9, 30), end: at(23, 9, 45), event: Arc::new(series) }
    }

    #[test]
    fn moving_an_occurrence_of_a_weekday_series_keeps_every_weekday_in_the_rule() {
        let o = weekdays();
        for (start, end) in [(at(23, 10, 30), at(23, 10, 45)), (at(24, 9, 30), at(24, 9, 45))] {
            let mut draft = Draft::open(&o, &o.event.rules, Lisbon);
            assert_eq!(draft.repeat, Repeat::EveryWeekday);
            draft.set_span(start, end);
            assert_eq!(draft.to_event("pmnew", "pmmeet").rules, o.event.rules);
            assert!(!draft.rule_changed());
        }
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
    fn a_birthday_or_a_working_location_edits_nothing() {
        use mailrs_domain::calendar::{Kind, Workplace};
        for kind in [Kind::Birthday, Kind::WorkingLocation(Workplace::Home)] {
            let event = Event { kind, ..Event::clone(&weekly().event) };
            assert_eq!(editing(&event, Access::Owner, true, false), Editing::None);
        }
    }

    #[test]
    fn out_of_office_and_focus_time_edit_as_a_whole_event() {
        use mailrs_domain::calendar::{Decline, Kind};
        let event = Event { kind: Kind::OutOfOffice(Decline::default()), ..Event::clone(&weekly().event) };
        assert_eq!(editing(&event, Access::Owner, true, false), Editing::Whole);
    }

    #[test]
    fn only_a_new_event_on_a_primary_calendar_offers_a_type() {
        assert!(fresh().offers_type(true));
        assert!(!fresh().offers_type(false), "Google keeps out of office and focus time on the primary calendar");
        assert!(!Draft::open(&weekly(), &weekly().event.rules, Lisbon).offers_type(true), "Google never changes a type");
    }

    fn workspace() -> Offers {
        Offers { out_of_office: true, focus_time: true, declines: true, ..Offers::EVERYTHING }
    }

    fn outlook() -> Offers {
        Offers { out_of_office: true, ..Offers::EVERYTHING }
    }

    fn team() -> Calendar {
        Calendar { id: "team".into(), name: "Team".into(), primary: false, ..personal() }
    }

    #[test]
    fn a_workspace_primary_calendar_takes_every_type() {
        assert_eq!(
            creatable_types(&workspace(), &personal()),
            vec![TypeChoice::Event, TypeChoice::Focus, TypeChoice::OutOfOffice]
        );
    }

    #[test]
    fn a_calendar_that_is_not_primary_takes_only_events() {
        assert_eq!(creatable_types(&workspace(), &team()), vec![TypeChoice::Event]);
    }

    #[test]
    fn an_outlook_calendar_takes_events_and_out_of_office() {
        assert_eq!(creatable_types(&outlook(), &personal()), vec![TypeChoice::Event, TypeChoice::OutOfOffice]);
    }

    #[test]
    fn a_provider_that_keeps_no_types_takes_only_events() {
        // Offers::EVERYTHING is a personal Gmail account; CalDAV and POP3
        // keep no types either.
        assert_eq!(creatable_types(&Offers::EVERYTHING, &personal()), vec![TypeChoice::Event]);
    }

    fn offered() -> Vec<(AccountId, String, Calendar)> {
        vec![
            (1, "dana@gmail.com".into(), personal()),
            (2, "dana@fernwood.example".into(), Calendar { id: "dana@fernwood.example".into(), ..personal() }),
            (3, "dana@outlook.com".into(), Calendar { id: "outlook".into(), ..personal() }),
        ]
    }

    fn offers_of(account: AccountId) -> Offers {
        match account {
            2 => workspace(),
            3 => outlook(),
            _ => Offers::EVERYTHING,
        }
    }

    #[test]
    fn the_new_event_menu_offers_each_type_some_calendar_takes() {
        assert_eq!(
            menu_types(&offered(), offers_of),
            vec![TypeChoice::Event, TypeChoice::Focus, TypeChoice::OutOfOffice]
        );
    }

    #[test]
    fn the_new_event_menu_holds_only_event_when_no_calendar_takes_a_type() {
        assert_eq!(menu_types(&offered()[..1], offers_of), vec![TypeChoice::Event]);
    }

    #[test]
    fn a_type_goes_on_the_default_calendar_when_it_takes_it() {
        let default = (3, offered()[2].2.clone());
        assert_eq!(calendar_for(TypeChoice::OutOfOffice, &offered(), offers_of, &default), Some(default.clone()));
    }

    #[test]
    fn a_type_the_default_calendar_cannot_take_goes_on_the_first_that_can() {
        let default = (1, personal());
        let (account, calendar) = calendar_for(TypeChoice::Focus, &offered(), offers_of, &default).unwrap();
        assert_eq!((account, calendar.id.as_str()), (2, "dana@fernwood.example"));
        assert_eq!(calendar_for(TypeChoice::Event, &offered(), offers_of, &default), Some(default));
    }

    #[test]
    fn quick_add_shows_the_type_switch_only_where_a_calendar_takes_several_types() {
        assert_eq!(
            type_switch(&outlook(), &personal()),
            Some(vec![TypeChoice::Event, TypeChoice::OutOfOffice])
        );
        assert_eq!(type_switch(&Offers::EVERYTHING, &personal()), None);
        assert_eq!(type_switch(&workspace(), &team()), None);
    }

    #[test]
    fn a_calendar_that_cannot_take_the_picked_type_goes_back_to_event() {
        let types = [TypeChoice::Event, TypeChoice::OutOfOffice];
        assert_eq!(kept_type(TypeChoice::Focus, &types), TypeChoice::Event);
        assert_eq!(kept_type(TypeChoice::OutOfOffice, &types), TypeChoice::OutOfOffice);
    }

    fn day(d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 9, d).unwrap()
    }

    #[test]
    fn focus_time_from_a_click_runs_two_hours_from_the_picked_time() {
        let mut draft = fresh();
        draft.new_as(TypeChoice::Focus, day(23), true);
        assert_eq!((draft.start, draft.end), (at(23, 15, 0), at(23, 17, 0)));
        assert!(draft.busy && !draft.all_day);
        assert_eq!(draft.title, "Focus time");
        assert_eq!(draft.type_choice(), TypeChoice::Focus);
    }

    #[test]
    fn focus_time_keeps_a_longer_span_the_person_dragged() {
        let mut draft = Draft::new(1, &personal(), at(23, 9, 0), at(23, 12, 0), Lisbon);
        draft.new_as(TypeChoice::Focus, day(23), true);
        assert_eq!((draft.start, draft.end), (at(23, 9, 0), at(23, 12, 0)));
    }

    #[test]
    fn out_of_office_runs_whole_days_from_today_through_the_picked_day() {
        let mut draft = fresh();
        draft.new_as(TypeChoice::OutOfOffice, day(21), true);
        // Google refuses an all-day out of office, so it runs midnight to
        // midnight in the reader's zone.
        assert_eq!((draft.start, draft.end), (at(21, 0, 0), at(24, 0, 0)));
        assert!(draft.busy && !draft.all_day);
        assert_eq!(draft.title, "Out of office");
    }

    #[test]
    fn out_of_office_on_a_day_already_past_covers_that_day_alone() {
        let mut draft = fresh();
        draft.new_as(TypeChoice::OutOfOffice, day(25), true);
        assert_eq!((draft.start, draft.end), (at(23, 0, 0), at(24, 0, 0)));
    }

    #[test]
    fn out_of_office_declines_meetings_where_the_provider_keeps_it() {
        use mailrs_domain::calendar::{Decline, Declines};
        let mut draft = fresh();
        draft.new_as(TypeChoice::OutOfOffice, day(23), true);
        assert_eq!(draft.kind.decline().map(|d| d.meetings), Some(Declines::All));
        let mut outlook = fresh();
        outlook.new_as(TypeChoice::OutOfOffice, day(23), false);
        assert_eq!(outlook.kind, Kind::OutOfOffice(Decline::default()), "Outlook keeps no decline choice");
    }

    #[test]
    fn an_event_from_the_menu_keeps_its_slot() {
        let mut draft = fresh();
        draft.new_as(TypeChoice::Event, day(23), true);
        assert_eq!((draft.start, draft.end, draft.title.as_str()), (at(23, 15, 0), at(23, 16, 0), ""));
    }

    #[test]
    fn choosing_out_of_office_names_it_and_leaves_all_day() {
        use mailrs_domain::calendar::{Decline, Declines, Kind};
        let mut draft = fresh();
        draft.set_all_day(true);
        draft.set_type(TypeChoice::OutOfOffice);
        assert!(!draft.all_day, "Google refuses an all-day out of office");
        assert!(draft.busy);
        assert_eq!(draft.title, "Out of office");
        assert_eq!(draft.type_choice(), TypeChoice::OutOfOffice);
        assert_eq!(
            draft.kind,
            Kind::OutOfOffice(Decline { meetings: Declines::All, message: "Declined because I am out of office".into() })
        );
    }

    #[test]
    fn a_typed_title_stays_when_the_type_changes() {
        let mut draft = fresh();
        draft.title = "Lisbon trip".into();
        draft.set_type(TypeChoice::OutOfOffice);
        draft.set_type(TypeChoice::Focus);
        assert_eq!(draft.title, "Lisbon trip");
    }

    #[test]
    fn a_default_title_follows_the_type() {
        let mut draft = fresh();
        draft.set_type(TypeChoice::OutOfOffice);
        draft.set_type(TypeChoice::Focus);
        assert_eq!(draft.title, "Focus time");
        draft.set_type(TypeChoice::Event);
        assert_eq!(draft.title, "");
        assert_eq!(draft.kind, mailrs_domain::calendar::Kind::Event);
    }

    #[test]
    fn out_of_office_goes_out_without_guests_place_or_meet() {
        let mut draft = fresh();
        draft.add_guests("ann@example.com");
        draft.place = "Lisbon".into();
        draft.add_meet = true;
        draft.set_type(TypeChoice::OutOfOffice);
        let event = draft.to_event("pm0new", "req1");
        assert!(matches!(event.kind, mailrs_domain::calendar::Kind::OutOfOffice(_)));
        assert!(event.guests.is_empty());
        assert!(event.place.is_empty());
        assert_eq!(event.meet_request, None);
    }

    #[test]
    fn the_decline_choice_and_message_reach_the_event() {
        use mailrs_domain::calendar::Declines;
        let mut draft = fresh();
        draft.set_type(TypeChoice::OutOfOffice);
        draft.set_declines(Declines::New);
        draft.set_decline_message("Back Monday");
        let event = draft.to_event("pm0new", "req1");
        let decline = event.kind.decline().unwrap();
        assert_eq!(decline.meetings, Declines::New);
        assert_eq!(decline.message, "Back Monday");
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

    const EVERY_PART: [Part; 13] = [
        Part::Title,
        Part::When,
        Part::Repeat,
        Part::Calendar,
        Part::Place,
        Part::Guests,
        Part::Reminders,
        Part::Notes,
        Part::Zone,
        Part::Busy,
        Part::Private,
        Part::Color,
        Part::Meet,
    ];

    #[test]
    fn a_guest_changes_only_their_reminders_colour_and_busy() {
        let invitation = invitation();
        let changeable: Vec<Part> =
            EVERY_PART.into_iter().filter(|p| may_change(Some(&invitation.event), *p)).collect();
        assert_eq!(changeable, [Part::Reminders, Part::Busy, Part::Color]);
    }

    #[test]
    fn the_organizer_changes_every_part() {
        let weekly = weekly();
        assert!(EVERY_PART.into_iter().all(|p| may_change(Some(&weekly.event), p)));
        assert!(EVERY_PART.into_iter().all(|p| may_change(None, p)), "a new event");
    }

    #[test]
    fn a_guest_gets_edit_and_remove_in_the_popover() {
        assert_eq!(Editing::Guest.popover(), Some(Popover { edit: true, removal: Removal::OwnCopy }));
        assert_eq!(Editing::Whole.popover(), Some(Popover { edit: true, removal: Removal::Event }));
        assert_eq!(Editing::NeedsPermission.popover(), None);
        assert_eq!(Editing::None.popover(), None);
    }

    #[test]
    fn a_new_calendar_reaches_the_guests() {
        let weekly = weekly();
        let before = Draft::open(&weekly, &weekly.event.rules, Lisbon);
        let mut after = before.clone();
        after.calendar = "team".into();
        assert!(reaches_guests(&before, &after));
    }

    #[test]
    fn the_calendar_choice_offers_only_the_events_own_account() {
        let team = Calendar { id: "team".into(), primary: false, ..personal() };
        let work = Calendar { id: "me@work.pt".into(), ..personal() };
        let writable = vec![
            (1, "me@example.com".to_string(), personal()),
            (1, "me@example.com".to_string(), team),
            (2, "me@work.pt".to_string(), work),
        ];
        let weekly = weekly();
        let draft = Draft::open(&weekly, &weekly.event.rules, Lisbon);
        let none = HashSet::new();
        let offered: Vec<&str> =
            calendar_choices(&draft, &writable, &none).iter().map(|(_, _, c)| c.id.as_str()).collect();
        assert_eq!(offered, ["me@example.com", "team"]);
        assert_eq!(calendar_choices(&fresh(), &writable, &none).len(), 3, "a new event may go on any account");
    }

    fn two_calendars() -> Vec<(AccountId, String, Calendar)> {
        let team = Calendar { id: "team".into(), primary: false, ..personal() };
        vec![
            (1, "me@example.com".to_string(), personal()),
            (1, "me@example.com".to_string(), team),
        ]
    }

    #[test]
    fn a_hidden_calendar_is_not_offered_for_a_new_event() {
        let hidden = HashSet::from([(1, "team".to_string())]);
        let writable = two_calendars();
        let offered: Vec<&str> =
            calendar_choices(&fresh(), &writable, &hidden).iter().map(|(_, _, c)| c.id.as_str()).collect();
        assert_eq!(offered, ["me@example.com"]);
    }

    #[test]
    fn an_event_on_a_hidden_calendar_still_shows_its_own_calendar() {
        let weekly = weekly();
        let mut draft = Draft::open(&weekly, &weekly.event.rules, Lisbon);
        draft.calendar = "team".into();
        let hidden = HashSet::from([(1, "team".to_string())]);
        let writable = two_calendars();
        let offered: Vec<&str> =
            calendar_choices(&draft, &writable, &hidden).iter().map(|(_, _, c)| c.id.as_str()).collect();
        assert_eq!(offered, ["me@example.com", "team"]);
    }

    #[test]
    fn quick_create_and_the_default_leave_out_hidden_calendars() {
        let hidden = HashSet::from([(1, "me@example.com".to_string())]);
        let listed = without_hidden(two_calendars(), &hidden);
        assert_eq!(listed.iter().map(|(_, _, c)| c.id.as_str()).collect::<Vec<_>>(), ["team"]);
    }

    #[test]
    fn picking_a_suggestion_adds_the_guest_and_empties_the_field() {
        let mut draft = fresh();
        let text = crate::ui::autocomplete::picked_text("Lo", "Love ❤️ <me.vhtavares@gmail.com>");
        assert_eq!(draft.take_guests(&text), "");
        assert_eq!(draft.guests.len(), 1);
        assert_eq!(draft.guests[0].name.as_deref(), Some("Love ❤️"));
    }

    #[test]
    fn a_pick_after_unparsed_text_keeps_only_that_text_in_the_field() {
        let mut draft = fresh();
        let text = crate::ui::autocomplete::picked_text("ann@example.com, typo, Lo", "Love <love@example.com>");
        assert_eq!(draft.take_guests(&text), "typo");
        let emails: Vec<&str> = draft.guests.iter().map(|g| g.email.as_str()).collect();
        assert_eq!(emails, ["ann@example.com", "love@example.com"]);
    }
}
