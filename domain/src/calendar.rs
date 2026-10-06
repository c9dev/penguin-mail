//! Calendars and events as Penguin Mail keeps them, whatever the
//! provider. The Google adapter maps Google's JSON to these; CalDAV and
//! Microsoft Graph will map theirs. A repeating event is kept as its
//! rule, and [`expand`] turns it into occurrences for the range on
//! screen, in the event's own time zone, so a 09:00 meeting stays at
//! 09:00 when the clocks change.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use rrule::RRuleSet;
use serde::{Deserialize, Serialize};

use crate::invitation;
use crate::{AccountId, EpochMillis};

pub mod clock;
pub mod hours;
pub mod list;
pub mod repeat;
pub mod series;
pub mod week;

/// Most occurrences one expansion returns. A daily series over a month
/// view is 42; this is far above any range the window asks for, and it
/// stops a rule with no end from running on.
const MOST_OCCURRENCES: u16 = 1000;

/// What the account may do with a calendar.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Access {
    Owner,
    Writer,
    #[default]
    Reader,
    /// Sees when the owner is busy, and nothing about what.
    FreeBusy,
}

impl Access {
    pub fn can_write(self) -> bool {
        matches!(self, Access::Owner | Access::Writer)
    }

    /// Google's word for it, which the store keeps too.
    pub fn as_str(self) -> &'static str {
        match self {
            Access::Owner => "owner",
            Access::Writer => "writer",
            Access::Reader => "reader",
            Access::FreeBusy => "freeBusyReader",
        }
    }

    pub fn parse(word: &str) -> Access {
        match word {
            "owner" => Access::Owner,
            "writer" => Access::Writer,
            "freeBusyReader" => Access::FreeBusy,
            _ => Access::Reader,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReminderMethod {
    /// A notification on this computer.
    Notification,
    /// An email the provider sends. Penguin Mail shows it and sends
    /// nothing itself.
    Email,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reminder {
    pub minutes: u32,
    pub method: ReminderMethod,
}

/// One calendar on an account.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Calendar {
    /// The provider's id, such as Google's `primary`-calendar address.
    pub id: String,
    pub name: String,
    /// `#rrggbb`.
    pub color: String,
    pub access: Access,
    /// The IANA zone new events on this calendar are written in. Empty
    /// when the provider has not said, and a new event then names none.
    pub zone: String,
    pub primary: bool,
    /// Whether the view shows it. Kept on this computer only.
    pub shown: bool,
    /// Whether the provider's own list hides it, as a person can on
    /// another device. The store follows a change to it; the view reads
    /// whether the calendar is listed from the store instead.
    #[serde(default)]
    pub hidden: bool,
    /// What an event on this calendar reminds of when it names none.
    pub reminders: Vec<Reminder>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Guest {
    pub email: String,
    pub name: Option<String>,
    /// This guest's answer to the invitation. `None` until they answer.
    pub answer: Option<invitation::Answer>,
    pub organizer: bool,
    /// This guest is the account itself.
    pub me: bool,
}

/// A file linked to an event: a Google Drive file, or a file on this
/// computer that goes to Drive before the event's next write reaches the
/// provider.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    pub title: String,
    /// Where the file opens. Empty while the file waits to upload.
    pub file_url: String,
    pub mime_type: String,
    /// Google's icon for the file, kept so a write can send the list back
    /// as it came. The app draws its own icon by mime type and never
    /// loads this one.
    pub icon_link: String,
    /// Drive's id for the file. Empty while the file waits to upload.
    pub file_id: String,
    /// The path of a file on this computer that waits to upload, for one
    /// attached while the upload could not run. The queue uploads it
    /// before it sends the event. If the file has moved by then, the
    /// event goes out without it and the window says so.
    #[serde(default)]
    pub waiting: Option<String>,
    /// For a file Penguin Mail uploaded, whether the event's guests may
    /// open it: Drive makes each one a reader when the event is saved.
    /// `None` for a file someone else attached, which `drive.file` cannot
    /// share. Kept on this computer only.
    #[serde(default)]
    pub share: Option<bool>,
    /// The guests already made readers of the file, so a save shares it
    /// with the ones added since and asks Drive nothing for the rest.
    #[serde(default)]
    pub shared_with: Vec<String>,
    /// Why a waiting file did not upload when the queue tried. The event
    /// went out without it, and the file stays here until the person
    /// grants access or takes it off.
    #[serde(default)]
    pub problem: Option<UploadProblem>,
}

/// Why a file waiting to upload stayed behind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum UploadProblem {
    /// The account has not granted Drive. The queue tries again once it
    /// does.
    NeedsAccess,
    /// The file is no longer at its path, or cannot be read.
    NotFound,
    /// Drive turned it down, for the reason given.
    Refused(String),
}

impl Attachment {
    /// The link a click opens: `file_url` when it is `https`, and `None`
    /// for anything else, such as a file still waiting to upload.
    pub fn link(&self) -> Option<&str> {
        let url = self.file_url.trim();
        let scheme = url.get(..8)?;
        (scheme.eq_ignore_ascii_case("https://") && url.len() > 8).then_some(url)
    }
}

/// `fresh`, a list the provider just sent, with what this computer knows
/// of the files that the provider does not: whether each file the app
/// uploaded is shared and with whom, found by Drive id, and the files
/// `held` still has waiting to upload, which the provider has never seen.
pub fn keep_local(fresh: &mut Vec<Attachment>, held: &[Attachment]) {
    for file in fresh.iter_mut() {
        if let Some(old) = held.iter().find(|old| !old.file_id.is_empty() && old.file_id == file.file_id) {
            file.share = old.share;
            file.shared_with = old.shared_with.clone();
        }
    }
    for old in held.iter().filter(|old| old.waiting.is_some()) {
        if !fresh.iter().any(|file| file.waiting == old.waiting) {
            fresh.push(old.clone());
        }
    }
}

/// The addresses among `guests` to make readers of `file`: every guest
/// but the account itself that the file is not shared with yet, and none
/// when the file is not one the app uploaded, still waits, or the person
/// turned sharing off.
pub fn to_share<'a>(file: &Attachment, guests: &'a [Guest]) -> Vec<&'a str> {
    if file.share != Some(true) || file.file_id.is_empty() || file.waiting.is_some() {
        return Vec::new();
    }
    guests
        .iter()
        .filter(|guest| !guest.me && !guest.email.is_empty())
        .filter(|guest| !file.shared_with.iter().any(|done| done.eq_ignore_ascii_case(&guest.email)))
        .map(|guest| guest.email.as_str())
        .collect()
}

/// Who hears about a write to an event: its guests, by mail from the
/// provider, or nobody. A new event and a change that adds guests tell
/// them, since that mail is their invitation; a move or a delete tells
/// them unless the person said not to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Notify {
    #[default]
    Guests,
    Nobody,
}

impl Notify {
    /// The choice for two writes of one event folded into one queued
    /// change: telling the guests wins, since the earlier write may have
    /// been the one that invited them.
    pub fn and(self, other: Notify) -> Notify {
        if self == Notify::Guests || other == Notify::Guests { Notify::Guests } else { Notify::Nobody }
    }

    /// How the store keeps it: `None` for [`Notify::Guests`], which every
    /// row queued before the choice existed means.
    pub fn stored(self) -> Option<&'static str> {
        match self {
            Notify::Guests => None,
            Notify::Nobody => Some("nobody"),
        }
    }

    pub fn from_stored(value: Option<&str>) -> Notify {
        match value {
            Some("nobody") => Notify::Nobody,
            _ => Notify::Guests,
        }
    }
}

/// Whether the provider mails anyone about taking `event` off the
/// calendar, given what the person `asked`. A guest's removal deletes
/// only their own copy, and Google marks them as having declined, so it
/// goes out quiet: a cancellation from a guest would reach every other
/// guest of a meeting they do not run.
pub fn removal_notify(event: &Event, asked: Notify) -> Notify {
    if event.limited() { Notify::Nobody } else { asked }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status {
    #[default]
    Confirmed,
    Tentative,
    Cancelled,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Confirmed => "confirmed",
            Status::Tentative => "tentative",
            Status::Cancelled => "cancelled",
        }
    }

    pub fn parse(word: &str) -> Status {
        match word {
            "tentative" => Status::Tentative,
            "cancelled" => Status::Cancelled,
            _ => Status::Confirmed,
        }
    }
}

/// Which meetings an out-of-office or focus-time entry turns down while it
/// runs. Google's `autoDeclineMode`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Declines {
    /// Google's `declineNone`.
    #[default]
    Nothing,
    /// Only invitations that arrive after the entry was made.
    /// `declineOnlyNewConflictingInvitations`.
    New,
    /// New invitations and the meetings already on the calendar.
    /// `declineAllConflictingInvitations`.
    All,
}

impl Declines {
    pub fn as_google(self) -> &'static str {
        match self {
            Declines::Nothing => "declineNone",
            Declines::New => "declineOnlyNewConflictingInvitations",
            Declines::All => "declineAllConflictingInvitations",
        }
    }

    pub fn from_google(word: &str) -> Declines {
        match word {
            "declineOnlyNewConflictingInvitations" => Declines::New,
            "declineAllConflictingInvitations" => Declines::All,
            _ => Declines::Nothing,
        }
    }
}

/// What an out-of-office or focus-time entry does with the meetings that
/// fall in it, and the words the organizer gets with each refusal.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decline {
    pub meetings: Declines,
    pub message: String,
}

/// Where the account's owner works on a day, from a working-location
/// entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Workplace {
    Home,
    /// An office, by the building's name when Google gives one.
    Office(String),
    /// Somewhere the person named themselves.
    Elsewhere(String),
}

/// What sort of entry an event is: an ordinary event, or one of the
/// entries Google keeps on the primary calendar to say where the person
/// is. Google's `eventType`, which it never lets a write change.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    #[default]
    Event,
    OutOfOffice(Decline),
    Focus(Decline),
    WorkingLocation(Workplace),
    Birthday,
}

impl Kind {
    /// Whether only Google's own apps make and change it: birthdays come
    /// from contacts and working locations from Google's settings, so
    /// Penguin Mail shows them and writes nothing to them.
    pub fn made_elsewhere(&self) -> bool {
        matches!(self, Kind::WorkingLocation(_) | Kind::Birthday)
    }

    /// Google's `eventType` for it.
    pub fn as_google(&self) -> &'static str {
        match self {
            Kind::Event => "default",
            Kind::OutOfOffice(_) => "outOfOffice",
            Kind::Focus(_) => "focusTime",
            Kind::WorkingLocation(_) => "workingLocation",
            Kind::Birthday => "birthday",
        }
    }

    /// The meetings it turns down, for an out-of-office or focus-time
    /// entry.
    pub fn decline(&self) -> Option<&Decline> {
        match self {
            Kind::OutOfOffice(decline) | Kind::Focus(decline) => Some(decline),
            _ => None,
        }
    }
}

/// One event, or one changed occurrence of a series.
///
/// An all-day event runs from midnight UTC of its first day to midnight
/// UTC of the day after its last, the way iCalendar ends one, and its
/// zone is `UTC`: a date means the same date wherever the reader is.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub calendar: String,
    pub id: String,
    /// The iCalendar UID, shared with any invitation for the event.
    pub uid: String,
    /// The provider's version tag. A write sends it back so the provider
    /// can refuse a change made against an older version.
    pub etag: String,
    pub start: EpochMillis,
    pub end: EpochMillis,
    pub zone: String,
    pub all_day: bool,
    pub title: String,
    pub place: String,
    /// The description as the provider holds it. Google keeps HTML once
    /// someone has edited it in Google Calendar, and plain text otherwise;
    /// `mailrs_mime::notes` turns either into lines to edit and back.
    pub description: String,
    /// `#rrggbb` when the event has its own colour; `None` takes the
    /// calendar's.
    pub color: Option<String>,
    /// The event blocks time: Google's `transparency` is not
    /// `transparent`. Whether it clashes also depends on the status, the
    /// account's answer and `all_day`.
    pub busy: bool,
    pub status: Status,
    pub private: bool,
    pub organizer: Option<String>,
    pub guests: Vec<Guest>,
    /// The account's own answer, when it is a guest. `None` until it
    /// answers.
    pub my_answer: Option<invitation::Answer>,
    /// `None` takes the calendar's reminders.
    pub reminders: Option<Vec<Reminder>>,
    /// A video call link, such as Google Meet's.
    pub conference: Option<String>,
    /// The series' `RRULE`, `EXDATE` and `RDATE` lines, whole. Empty for
    /// an event that does not repeat.
    pub rules: Vec<String>,
    /// For a changed occurrence: the id of the series it belongs to.
    pub series: Option<String>,
    /// For a changed occurrence: the start of the occurrence it replaces.
    pub original_start: Option<EpochMillis>,
    /// A change made here waits in the queue for the provider.
    pub pending: bool,
    /// The organizer's version of the event, iCalendar's `SEQUENCE`,
    /// which Google counts up with each change it sends the guests. A
    /// guest's proposal names it, or the organizer may set it aside as
    /// stale. Only read from the provider; a write never sends it.
    #[serde(default)]
    pub sequence: i64,
    /// A Google Meet link to ask for with the next write, named by a
    /// request id so a retry does not make two. Only the queue body
    /// carries it; the store does not keep it.
    #[serde(default)]
    pub meet_request: Option<String>,
    /// An ordinary event, or out of office, focus time, a working
    /// location or a birthday.
    #[serde(default)]
    pub kind: Kind,
    /// Files linked to the event. `None` when the copy has not read
    /// them, as for a row stored before Penguin Mail read attachments or
    /// a change queued then: a write leaves the provider's list alone,
    /// since sending an empty one would take every file off the event.
    #[serde(default)]
    pub attachments: Option<Vec<Attachment>>,
}

impl Event {
    /// Whether the event takes the account's time, for free time and the
    /// clash line. `busy` alone is what Google holds and what a change
    /// sends back; a cancelled event, one the account declined and one
    /// lasting all day leave the time open as well.
    pub fn blocks_time(&self) -> bool {
        self.busy
            && !self.all_day
            && self.status != Status::Cancelled
            && self.my_answer != Some(invitation::Answer::No)
    }

    /// Whether someone else organizes the event and the account is only a
    /// guest. The account then changes only its own reminders, colour and
    /// busy, and leaves the time, place, rules and guests to the organizer.
    pub fn limited(&self) -> bool {
        self.guests.iter().any(|g| g.me && !g.organizer)
    }
}

/// Google's eleven event colours, by the id an event carries. An event
/// takes one of these or its calendar's colour; the editor offers them.
/// The palette is built in rather than read from Google (`colors.get`
/// needs a scope this app does not ask for) and matches what Google's
/// own web app shows.
pub const EVENT_COLORS: [(&str, &str); 11] = [
    ("1", "#7986cb"),
    ("2", "#33b679"),
    ("3", "#8e24aa"),
    ("4", "#e67c73"),
    ("5", "#f6bf26"),
    ("6", "#f4511e"),
    ("7", "#039be5"),
    ("8", "#616161"),
    ("9", "#3f51b5"),
    ("10", "#0b8043"),
    ("11", "#d50000"),
];

/// The `#rrggbb` for one of Google's colour ids, or `None` for an id
/// Google has not defined.
pub fn event_color(id: &str) -> Option<&'static str> {
    EVENT_COLORS.iter().find(|(i, _)| *i == id).map(|(_, hex)| *hex)
}

/// The colour id for a hex value in [`EVENT_COLORS`], read without
/// regard to case, or `None` when the palette holds no such colour.
pub fn color_id(hex: &str) -> Option<&'static str> {
    EVENT_COLORS.iter().find(|(_, h)| h.eq_ignore_ascii_case(hex)).map(|(id, _)| *id)
}

/// One showing of an event on the grid. `event` is shared rather than
/// cloned, so a series with many occurrences in view costs one
/// allocation of it however many times it repeats.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Occurrence {
    pub account_id: AccountId,
    pub event: Arc<Event>,
    pub start: EpochMillis,
    pub end: EpochMillis,
}

impl Occurrence {
    /// The id the assistant's tools name this occurrence by. A one-off event,
    /// or an occurrence someone already changed, has a row and an id of
    /// its own. An occurrence a series expands to takes Google's instance
    /// form, the series id, an underscore and the original start in UTC
    /// (`_20261022T090000Z`, or `_20261022` for a whole day), so a change
    /// to it reaches that one occurrence and never the series.
    pub fn id(&self) -> String {
        if self.event.rules.is_empty() {
            return self.event.id.clone();
        }
        occurrence_id(&self.event, self.start)
    }
}

/// Google's id for one occurrence: the series id, an underscore and the
/// original start in UTC, as a date for an all-day series and as a
/// timestamp for a timed one. [`Occurrence::id`] calls this with the
/// start `expand` gave it; a changed occurrence not yet shown, such as
/// one [`series`] is about to write, has none to give and passes the
/// original start it does have instead.
pub fn occurrence_id(series: &Event, original_start: EpochMillis) -> String {
    let Some(start) = DateTime::<Utc>::from_timestamp_millis(original_start) else {
        return series.id.clone();
    };
    let form = if series.all_day { "%Y%m%d" } else { "%Y%m%dT%H%M%SZ" };
    format!("{}_{}", series.id, start.format(form))
}

/// Splits an occurrence id, as [`Occurrence::id`] writes one, into the
/// series id and the occurrence's original start, or `None` for any other
/// id.
pub fn split_occurrence_id(id: &str) -> Option<(&str, EpochMillis)> {
    let (series, stamp) = id.rsplit_once('_')?;
    if series.is_empty() {
        return None;
    }
    let digits = |text: &str| !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
    let at = match stamp.len() {
        8 if digits(stamp) => chrono::NaiveDate::parse_from_str(stamp, "%Y%m%d").ok()?.and_hms_opt(0, 0, 0)?,
        16 if stamp.ends_with('Z') && digits(&stamp[..8]) && digits(&stamp[9..15]) => {
            chrono::NaiveDateTime::parse_from_str(&stamp[..15], "%Y%m%dT%H%M%S").ok()?
        }
        _ => return None,
    };
    Some((series, at.and_utc().timestamp_millis()))
}

/// What the assistant sets on an event: each field it names, with the
/// rest of the event left as it was. A new event starts from a blank
/// [`Event`] and takes the edit the same way.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventEdit {
    pub title: Option<String>,
    /// The new start. A new start with no new end keeps the event's
    /// length, since a person moving a meeting means to move all of it.
    pub start: Option<EpochMillis>,
    /// The new end. A whole-day event ends at midnight UTC after its last
    /// day, as [`Event`] keeps it.
    pub end: Option<EpochMillis>,
    pub all_day: Option<bool>,
    pub place: Option<String>,
    pub description: Option<String>,
    /// The guests by address, each with no answer of their own yet. The
    /// list replaces the event's.
    pub guests: Option<Vec<String>>,
}

impl EventEdit {
    /// Whether the edit names nothing to change.
    pub fn is_empty(&self) -> bool {
        *self == EventEdit::default()
    }

    /// Writes what the edit names onto `event`.
    pub fn apply(&self, event: &mut Event) {
        if let Some(title) = &self.title {
            event.title = title.clone();
        }
        if let Some(start) = self.start {
            let length = event.end - event.start;
            event.start = start;
            event.end = start + length;
        }
        if let Some(end) = self.end {
            event.end = end;
        }
        if let Some(all_day) = self.all_day {
            event.all_day = all_day;
        }
        if let Some(place) = &self.place {
            event.place = place.clone();
        }
        if let Some(description) = &self.description {
            event.description = description.clone();
        }
        if let Some(guests) = &self.guests {
            event.guests = guests.iter().map(|email| Guest { email: email.clone(), ..Guest::default() }).collect();
        }
    }
}

/// How a repeating event runs, for the series line an invitation to one
/// of its occurrences shows: "Every Tuesday, 6 left".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Series {
    /// The rule without its `RRULE:` prefix, such as
    /// `FREQ=WEEKLY;BYDAY=MO;COUNT=10`.
    pub rule: String,
    /// How many occurrences start at or after the instant asked about.
    /// Only a rule that stops after a number of occurrences has one; a
    /// rule with an end date or none counts nothing.
    pub left: Option<u32>,
}

impl Series {
    /// How `event` repeats, counted from `from`. `None` for an event that
    /// does not repeat.
    pub fn of(event: &Event, from: EpochMillis) -> Option<Series> {
        let line = event.rules.iter().find(|line| is_rule_line(line))?;
        let rule = line.split_once(':').map_or(line.as_str(), |(_, rule)| rule).to_string();
        let counted = rule.split(';').any(|part| part.trim().to_ascii_uppercase().starts_with("COUNT="));
        // `series_end` gives up on a series longer than `expand` would
        // list, and then nothing is counted rather than a short count.
        let left = match (counted, series_end(event)) {
            (true, Some(end)) => {
                let left = expand(event, from, end + 1).iter().filter(|(start, _)| *start >= from).count();
                Some(u32::try_from(left).unwrap_or(u32::MAX))
            }
            _ => None,
        };
        Some(Series { rule, left })
    }
}

/// One page of changes to one calendar, provider-neutral so a CalDAV
/// adapter answers the same shape a Google one does.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventPage {
    pub events: Vec<Event>,
    /// Ids of events deleted or cancelled since the token. A cancelled
    /// occurrence of a series is not here: it comes as an event with
    /// `series` set, since it says which occurrence to leave out.
    pub removed: Vec<String>,
    pub next_page: Option<String>,
    /// On the last page, the token for the next read.
    pub next_sync: Option<String>,
    /// Series this page gives whole: the series and every changed
    /// occurrence it still has. The copy drops any other changed
    /// occurrence it holds of them, which is how an occurrence the
    /// organizer took back leaves a CalDAV calendar, since one resource
    /// holds the series. Google names each removal and leaves this empty.
    pub whole_series: Vec<String>,
}

/// The starts and ends of `event`'s occurrences that overlap `from` to
/// `to`, earliest first. A one-off event gives itself or nothing. A rule
/// the `rrule` crate cannot read gives the first occurrence, so the
/// event stays visible, and the log names the rule.
pub fn expand(event: &Event, from: EpochMillis, to: EpochMillis) -> Vec<(EpochMillis, EpochMillis)> {
    let length = event.end - event.start;
    let overlaps = |start: EpochMillis| start < to && start + length > from;
    if event.rules.is_empty() {
        return if overlaps(event.start) || (length == 0 && event.start >= from && event.start < to) {
            vec![(event.start, event.end)]
        } else {
            Vec::new()
        };
    }
    let Some(set) = rule_set(event) else {
        return if overlaps(event.start) { vec![(event.start, event.end)] } else { Vec::new() };
    };
    let tz = zone(event);
    let (Some(after), Some(before)) = (at(from - length, tz), at(to, tz)) else {
        return Vec::new();
    };
    set.after(after)
        .before(before)
        .all(MOST_OCCURRENCES)
        .dates
        .into_iter()
        .map(|start| start.timestamp_millis())
        .filter(|start| overlaps(*start))
        .map(|start| (start, start + length))
        .collect()
}

/// When the last occurrence of a series ends, or `None` for a series with
/// no end. The store keeps it so a range query can skip series that
/// finished long ago. A rule nobody can read ends with its first
/// occurrence, since that is all [`expand`] shows of it.
pub fn series_end(event: &Event) -> Option<EpochMillis> {
    if event.rules.is_empty() {
        return Some(event.end);
    }
    let Some(set) = rule_set(event) else {
        return Some(event.end);
    };
    let rule = event.rules.iter().find(|line| is_rule_line(line))?;
    let upper = rule.to_ascii_uppercase();
    if !upper.contains("COUNT=") && !upper.contains("UNTIL=") {
        return None;
    }
    let length = event.end - event.start;
    let result = set.all(MOST_OCCURRENCES);
    // A series longer than the cap is treated as endless rather than
    // cut short.
    if result.limited {
        return None;
    }
    result.dates.last().map(|start| start.timestamp_millis() + length)
}

/// Whether `line` is an `RRULE` line, as opposed to an `EXDATE` or
/// `RDATE`. [`series`] tests it too, to tell a rule to rewrite from a
/// date list to filter.
pub fn is_rule_line(line: &str) -> bool {
    line.to_ascii_uppercase().starts_with("RRULE")
}

/// `line` with its `TZID` parameter renamed through `iana`. Outlook writes
/// an `EXDATE` in a Windows zone ("W. Europe Standard Time"), which `rrule`
/// cannot read, and one line it cannot read loses the whole series. A name
/// `iana` does not know stays as written.
pub fn rename_zone(line: &str, iana: &dyn Fn(&str) -> Option<String>) -> String {
    // The head ends at the first colon outside a quoted parameter value.
    let mut quoted = false;
    let Some(colon) = line.char_indices().find_map(|(i, ch)| {
        if ch == '"' {
            quoted = !quoted;
        }
        (ch == ':' && !quoted).then_some(i)
    }) else {
        return line.to_string();
    };
    let (head, value) = line.split_at(colon);
    let mut parts = Vec::new();
    let (mut part, mut quoted) = (String::new(), false);
    for ch in head.chars() {
        match ch {
            '"' => {
                quoted = !quoted;
                part.push(ch);
            }
            ';' if !quoted => parts.push(std::mem::take(&mut part)),
            _ => part.push(ch),
        }
    }
    parts.push(part);
    for part in parts.iter_mut().skip(1) {
        let Some((key, tzid)) = part.split_once('=') else {
            continue;
        };
        if !key.eq_ignore_ascii_case("TZID") {
            continue;
        }
        if let Some(name) = iana(tzid.trim_matches('"')) {
            *part = format!("TZID={name}");
        }
    }
    format!("{}{value}", parts.join(";"))
}

/// Whether `line` lists dates to skip or add: `EXDATE` or `RDATE`.
pub(crate) fn is_date_line(line: &str) -> bool {
    let upper = line.to_ascii_uppercase();
    upper.starts_with("EXDATE") || upper.starts_with("RDATE")
}

fn zone(event: &Event) -> rrule::Tz {
    let tz: chrono_tz::Tz = event.zone.parse().unwrap_or(chrono_tz::UTC);
    rrule::Tz::Tz(tz)
}

fn at(instant: EpochMillis, tz: rrule::Tz) -> Option<DateTime<rrule::Tz>> {
    DateTime::<Utc>::from_timestamp_millis(instant).map(|at| at.with_timezone(&tz))
}

/// The event's rules with its start in front, as `rrule` reads a set.
fn rule_set(event: &Event) -> Option<RRuleSet> {
    let tz = zone(event);
    let start = at(event.start, tz)?;
    let name = match tz {
        rrule::Tz::Tz(tz) => tz.name().to_string(),
        rrule::Tz::Local(_) => "UTC".to_string(),
    };
    // A zone `rrule` cannot read, such as Outlook's Windows names in rows
    // stored before the readers renamed them, reads as the series' own.
    let iana = |tzid: &str| Some(crate::invitation::zone::named(tzid).map_or_else(|| name.clone(), |tz| tz.name().to_string()));
    let rules: Vec<String> = event.rules.iter().map(|rule| dates_in_utc(&until_in_utc(&rename_zone(rule, &iana)))).collect();
    let text = format!(
        "DTSTART;TZID={name}:{}\n{}",
        start.format("%Y%m%dT%H%M%S"),
        rules.join("\n")
    );
    match text.parse::<RRuleSet>() {
        Ok(set) => Some(set),
        Err(err) => {
            tracing::warn!(rules = ?event.rules, %err, "could not read a repeat rule");
            None
        }
    }
}

/// Google writes an all-day series' `UNTIL` as a bare date
/// (`UNTIL=20261102`, RFC 5545's `DATE` form). `rrule` reads a value with
/// no `T` and no `Z` in the machine's own time zone rather than in the
/// `DTSTART`'s, so it never matches the `TZID=UTC` this module always
/// writes and the whole rule fails to parse. Widening it to UTC midnight
/// names the same day and keeps the rule readable.
fn until_in_utc(rule: &str) -> String {
    let Some(at) = rule.to_ascii_uppercase().find("UNTIL=") else {
        return rule.to_string();
    };
    let start = at + "UNTIL=".len();
    let end = rule[start..]
        .find(';')
        .map_or(rule.len(), |offset| start + offset);
    let value = &rule[start..end];
    if value.len() == 8 && value.bytes().all(|b| b.is_ascii_digit()) {
        format!("{}{value}T000000Z{}", &rule[..start], &rule[end..])
    } else {
        rule.to_string()
    }
}

/// Google writes an all-day series' skipped and added days as bare dates
/// (`EXDATE;VALUE=DATE:20260713`). `rrule` ignores `VALUE=DATE` and reads
/// the bare value at midnight in the machine's own zone, so outside UTC it
/// names no occurrence of a series this module starts at UTC midnight.
/// Widening each bare date to UTC midnight, as [`until_in_utc`] does for
/// `UNTIL`, gives the same day in every zone.
fn dates_in_utc(line: &str) -> String {
    if !is_date_line(line) {
        return line.to_string();
    }
    let Some((head, values)) = line.split_once(':') else {
        return line.to_string();
    };
    let bare = |value: &str| value.len() == 8 && value.bytes().all(|b| b.is_ascii_digit());
    if !values.split(',').any(bare) {
        return line.to_string();
    }
    let params: Vec<&str> = head.split(';').filter(|p| !p.eq_ignore_ascii_case("VALUE=DATE")).collect();
    let values: Vec<String> = values
        .split(',')
        .map(|value| if bare(value) { format!("{value}T000000Z") } else { value.to_string() })
        .collect();
    format!("{}:{}", params.join(";"), values.join(","))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, TimeZone};
    use chrono_tz::Europe::Lisbon;

    fn lisbon(y: i32, m: u32, d: u32, h: u32, min: u32) -> EpochMillis {
        Lisbon
            .from_local_datetime(&NaiveDate::from_ymd_opt(y, m, d).unwrap().and_hms_opt(h, min, 0).unwrap())
            .single()
            .unwrap()
            .timestamp_millis()
    }

    fn standup(rules: &[&str]) -> Event {
        Event {
            calendar: "work".into(),
            id: "standup".into(),
            start: lisbon(2026, 10, 19, 9, 0),
            end: lisbon(2026, 10, 19, 9, 15),
            zone: "Europe/Lisbon".into(),
            title: "Stand-up".into(),
            busy: true,
            rules: rules.iter().map(|r| r.to_string()).collect(),
            ..Event::default()
        }
    }

    #[test]
    fn a_skipped_day_in_a_windows_zone_still_leaves_the_series() {
        // Outlook names the zone its own way; the series stays in Lisbon.
        let event = standup(&[
            "RRULE:FREQ=DAILY;COUNT=3",
            "EXDATE;TZID=\"GMT Standard Time\":20261020T090000",
        ]);
        let got = expand(&event, lisbon(2026, 10, 19, 0, 0), lisbon(2026, 10, 23, 0, 0));
        let starts: Vec<EpochMillis> = got.iter().map(|(start, _)| *start).collect();
        assert_eq!(starts, vec![lisbon(2026, 10, 19, 9, 0), lisbon(2026, 10, 21, 9, 0)]);
    }

    #[test]
    fn a_zone_parameter_is_renamed_and_the_rest_kept() {
        let line = "EXDATE;VALUE=DATE-TIME;TZID=\"W. Europe Standard Time\":20261027T140000";
        let renamed = rename_zone(line, &|tzid| (tzid == "W. Europe Standard Time").then(|| "Europe/Berlin".to_string()));
        assert_eq!(renamed, "EXDATE;VALUE=DATE-TIME;TZID=Europe/Berlin:20261027T140000");
        assert_eq!(rename_zone("RRULE:FREQ=DAILY", &|_| Some("Europe/Berlin".into())), "RRULE:FREQ=DAILY");
        assert_eq!(rename_zone(line, &|_| None), line);
    }

    #[test]
    fn a_one_off_event_shows_once_when_it_overlaps_the_range() {
        let event = standup(&[]);
        let day = (lisbon(2026, 10, 19, 0, 0), lisbon(2026, 10, 20, 0, 0));
        assert_eq!(expand(&event, day.0, day.1), vec![(event.start, event.end)]);
        assert!(expand(&event, lisbon(2026, 10, 20, 0, 0), lisbon(2026, 10, 21, 0, 0)).is_empty());
    }

    #[test]
    fn a_series_keeps_its_local_hour_across_the_clock_change() {
        // Portugal leaves summer time on 25 October 2026.
        let event = standup(&["RRULE:FREQ=DAILY;COUNT=10"]);
        let got = expand(&event, lisbon(2026, 10, 24, 0, 0), lisbon(2026, 10, 27, 0, 0));
        assert_eq!(
            got,
            vec![
                (lisbon(2026, 10, 24, 9, 0), lisbon(2026, 10, 24, 9, 15)),
                (lisbon(2026, 10, 25, 9, 0), lisbon(2026, 10, 25, 9, 15)),
                (lisbon(2026, 10, 26, 9, 0), lisbon(2026, 10, 26, 9, 15)),
            ]
        );
    }

    #[test]
    fn a_count_stops_the_series() {
        let event = standup(&["RRULE:FREQ=DAILY;COUNT=3"]);
        let got = expand(&event, lisbon(2026, 10, 19, 0, 0), lisbon(2026, 11, 1, 0, 0));
        assert_eq!(got.len(), 3);
        assert_eq!(series_end(&event), Some(lisbon(2026, 10, 21, 9, 15)));
    }

    #[test]
    fn an_until_stops_the_series() {
        let event = standup(&["RRULE:FREQ=DAILY;UNTIL=20261021T235959Z"]);
        let got = expand(&event, lisbon(2026, 10, 19, 0, 0), lisbon(2026, 11, 1, 0, 0));
        assert_eq!(got.len(), 3);
    }

    #[test]
    fn a_series_with_no_end_has_no_series_end() {
        assert_eq!(series_end(&standup(&["RRULE:FREQ=WEEKLY"])), None);
    }

    #[test]
    fn an_excluded_date_leaves_a_gap() {
        let event = standup(&[
            "RRULE:FREQ=DAILY;COUNT=3",
            "EXDATE;TZID=Europe/Lisbon:20261020T090000",
        ]);
        let got = expand(&event, lisbon(2026, 10, 19, 0, 0), lisbon(2026, 11, 1, 0, 0));
        assert_eq!(
            got.iter().map(|(s, _)| *s).collect::<Vec<_>>(),
            vec![lisbon(2026, 10, 19, 9, 0), lisbon(2026, 10, 21, 9, 0)]
        );
    }

    #[test]
    fn an_all_day_series_repeats_by_date() {
        let midnight = |d| NaiveDate::from_ymd_opt(2026, 10, d).unwrap().and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp_millis();
        let event = Event {
            start: midnight(19),
            end: midnight(20),
            zone: "UTC".into(),
            all_day: true,
            rules: vec!["RRULE:FREQ=WEEKLY;COUNT=2".into()],
            ..standup(&[])
        };
        let got = expand(&event, midnight(1), midnight(31));
        assert_eq!(got, vec![(midnight(19), midnight(20)), (midnight(26), midnight(27))]);
    }

    #[test]
    fn an_all_day_series_stops_on_a_bare_until_date() {
        // Google writes UNTIL for an all-day series as a bare date, with no
        // time and no Z: it never carries a UTC offset of its own.
        let midnight = |d| NaiveDate::from_ymd_opt(2026, 10, d).unwrap().and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp_millis();
        let event = Event {
            start: midnight(19),
            end: midnight(20),
            zone: "UTC".into(),
            all_day: true,
            rules: vec!["RRULE:FREQ=WEEKLY;UNTIL=20261102".into()],
            ..standup(&[])
        };
        let got = expand(&event, midnight(1), midnight(31));
        assert_eq!(got, vec![(midnight(19), midnight(20)), (midnight(26), midnight(27))]);
    }

    /// Google writes an all-day series' skipped days as bare dates. The
    /// week skipped must stay skipped in any time zone the machine runs
    /// in: run this under `TZ=Europe/Lisbon` as well as `TZ=UTC`.
    #[test]
    fn an_all_day_series_skips_a_bare_excluded_date() {
        let midnight = |m, d| NaiveDate::from_ymd_opt(2026, m, d).unwrap().and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp_millis();
        let event = Event {
            start: midnight(7, 6),
            end: midnight(7, 7),
            zone: "UTC".into(),
            all_day: true,
            rules: vec!["RRULE:FREQ=WEEKLY;COUNT=3".into(), "EXDATE;VALUE=DATE:20260713".into()],
            ..standup(&[])
        };
        let got = expand(&event, midnight(7, 1), midnight(8, 1));
        assert_eq!(got, vec![(midnight(7, 6), midnight(7, 7)), (midnight(7, 20), midnight(7, 21))]);
    }

    #[test]
    fn an_all_day_series_adds_a_bare_extra_date_on_its_own_day() {
        let midnight = |m, d| NaiveDate::from_ymd_opt(2026, m, d).unwrap().and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp_millis();
        let event = Event {
            start: midnight(7, 6),
            end: midnight(7, 7),
            zone: "UTC".into(),
            all_day: true,
            rules: vec!["RRULE:FREQ=WEEKLY;COUNT=1".into(), "RDATE;VALUE=DATE:20260709,20260710".into()],
            ..standup(&[])
        };
        let got = expand(&event, midnight(7, 1), midnight(8, 1));
        assert_eq!(
            got,
            vec![(midnight(7, 6), midnight(7, 7)), (midnight(7, 9), midnight(7, 10)), (midnight(7, 10), midnight(7, 11))]
        );
    }

    #[test]
    fn a_rule_nobody_can_read_shows_the_first_occurrence() {
        let event = standup(&["RRULE:FREQ=SOMETIMES"]);
        let got = expand(&event, lisbon(2026, 10, 1, 0, 0), lisbon(2026, 11, 1, 0, 0));
        assert_eq!(got, vec![(event.start, event.end)]);
        assert_eq!(series_end(&event), Some(event.end));
    }

    #[test]
    fn an_occurrence_that_began_before_the_range_still_shows() {
        let mut event = standup(&["RRULE:FREQ=DAILY;COUNT=2"]);
        event.end = event.start + 3 * 60 * 60 * 1000;
        // The range opens an hour into the first occurrence.
        let got = expand(&event, lisbon(2026, 10, 19, 10, 0), lisbon(2026, 10, 19, 11, 0));
        assert_eq!(got.len(), 1);
    }

    #[test]
    fn an_occurrence_of_a_timed_series_is_named_by_its_utc_start() {
        let event = Arc::new(standup(&["RRULE:FREQ=DAILY;COUNT=5"]));
        let thursday = lisbon(2026, 10, 22, 9, 0);
        let one = Occurrence { account_id: 1, event, start: thursday, end: thursday + 15 * 60_000 };
        // 09:00 in Lisbon is 08:00 UTC in October's summer time.
        assert_eq!(one.id(), "standup_20261022T080000Z");
        assert_eq!(split_occurrence_id("standup_20261022T080000Z"), Some(("standup", thursday)));
    }

    #[test]
    fn an_occurrence_of_an_all_day_series_is_named_by_its_date() {
        let midnight = NaiveDate::from_ymd_opt(2026, 10, 26).unwrap().and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp_millis();
        let event = Arc::new(Event { all_day: true, zone: "UTC".into(), ..standup(&["RRULE:FREQ=WEEKLY"]) });
        let one = Occurrence { account_id: 1, event, start: midnight, end: midnight + 24 * 60 * 60_000 };
        assert_eq!(one.id(), "standup_20261026");
        assert_eq!(split_occurrence_id("standup_20261026"), Some(("standup", midnight)));
    }

    #[test]
    fn an_event_that_does_not_repeat_keeps_its_own_id() {
        let event = Arc::new(standup(&[]));
        let one = Occurrence { account_id: 1, start: event.start, end: event.end, event };
        assert_eq!(one.id(), "standup");
        assert_eq!(split_occurrence_id("standup"), None);
        assert_eq!(split_occurrence_id("team_lunch"), None);
    }

    #[test]
    fn only_an_event_the_account_attends_in_hours_blocks_time() {
        let meeting = standup(&[]);
        assert!(meeting.blocks_time());
        assert!(!Event { busy: false, ..meeting.clone() }.blocks_time(), "marked free");
        assert!(!Event { all_day: true, ..meeting.clone() }.blocks_time(), "all day");
        assert!(!Event { status: Status::Cancelled, ..meeting.clone() }.blocks_time(), "cancelled");
        assert!(!Event { my_answer: Some(invitation::Answer::No), ..meeting.clone() }.blocks_time(), "declined");
        assert!(Event { my_answer: Some(invitation::Answer::Maybe), ..meeting }.blocks_time(), "a maybe still counts");
    }

    #[test]
    fn access_says_who_can_write() {
        assert!(Access::Owner.can_write());
        assert!(Access::Writer.can_write());
        assert!(!Access::Reader.can_write());
        assert!(!Access::FreeBusy.can_write());
        assert_eq!(Access::parse("freeBusyReader"), Access::FreeBusy);
    }

    #[test]
    fn a_colour_and_its_google_id_go_both_ways() {
        for (id, hex) in EVENT_COLORS {
            assert_eq!(event_color(id), Some(hex));
            assert_eq!(color_id(hex), Some(id));
        }
        assert_eq!(color_id("#E67C73"), Some("4"));
        assert_eq!(color_id("#123456"), None);
    }

    #[test]
    fn a_queued_body_from_before_meet_requests_still_reads() {
        let old = r#"{"calendar":"work","id":"a","uid":"","etag":"","start":0,"end":0,"zone":"UTC",
            "all_day":false,"title":"","place":"","description":"","color":null,"busy":true,
            "status":"Confirmed","private":false,"organizer":null,"guests":[],"my_answer":null,
            "reminders":null,"conference":null,"rules":[],"series":null,"original_start":null,"pending":true}"#;
        let event: Event = serde_json::from_str(old).unwrap();
        assert_eq!(event.meet_request, None);
    }

    #[test]
    fn a_body_queued_before_attachments_leaves_them_unknown() {
        let old = r#"{"calendar":"work","id":"a","uid":"","etag":"","start":0,"end":0,"zone":"UTC",
            "all_day":false,"title":"","place":"","description":"","color":null,"busy":true,
            "status":"Confirmed","private":false,"organizer":null,"guests":[],"my_answer":null,
            "reminders":null,"conference":null,"rules":[],"series":null,"original_start":null,"pending":true}"#;
        let event: Event = serde_json::from_str(old).unwrap();
        assert_eq!(event.attachments, None);
    }

    fn linked(url: &str) -> Attachment {
        Attachment { file_url: url.into(), ..Attachment::default() }
    }

    #[test]
    fn an_attachment_opens_an_https_link() {
        let file = linked("https://drive.google.com/file/d/1abc/view");
        assert_eq!(file.link(), Some("https://drive.google.com/file/d/1abc/view"));
    }

    #[test]
    fn an_attachment_opens_nothing_but_https() {
        for url in ["http://example.com/a", "file:///etc/passwd", "javascript:alert(1)", "", "https://"] {
            assert_eq!(linked(url).link(), None, "{url}");
        }
    }

    fn ours(id: &str, share: bool, shared_with: &[&str]) -> Attachment {
        Attachment {
            title: id.into(),
            file_url: format!("https://drive.google.com/file/d/{id}/view"),
            file_id: id.into(),
            share: Some(share),
            shared_with: shared_with.iter().map(|s| s.to_string()).collect(),
            ..Attachment::default()
        }
    }

    fn waiting_at(path: &str) -> Attachment {
        Attachment { title: path.into(), waiting: Some(path.into()), ..Attachment::default() }
    }

    #[test]
    fn a_provider_read_keeps_what_this_computer_knows_of_its_files() {
        let held = vec![ours("1abc", false, &["ana@example.com"]), waiting_at("/home/me/Notes.txt")];
        let mut fresh = vec![Attachment { share: None, ..ours("1abc", true, &[]) }, linked("https://x.example/other")];
        keep_local(&mut fresh, &held);
        assert_eq!(fresh[0].share, Some(false));
        assert_eq!(fresh[0].shared_with, ["ana@example.com"]);
        assert_eq!(fresh[1].share, None, "a file nobody here uploaded stays unshared");
        assert_eq!(fresh[2], waiting_at("/home/me/Notes.txt"), "a waiting file outlives the read");
        assert_eq!(fresh.len(), 3);
    }

    #[test]
    fn a_provider_read_that_dropped_a_file_does_not_bring_it_back() {
        let mut fresh = Vec::new();
        keep_local(&mut fresh, &[ours("1abc", true, &[])]);
        assert!(fresh.is_empty());
    }

    fn guest(email: &str, me: bool) -> Guest {
        Guest { email: email.into(), me, ..Guest::default() }
    }

    #[test]
    fn a_shared_file_goes_to_each_guest_not_yet_given_it() {
        let guests = [guest("me@example.com", true), guest("ana@example.com", false), guest("bo@example.com", false)];
        assert_eq!(to_share(&ours("1abc", true, &["ANA@example.com"]), &guests), ["bo@example.com"]);
    }

    #[test]
    fn a_file_is_not_shared_when_the_person_said_no() {
        let guests = [guest("ana@example.com", false)];
        assert!(to_share(&ours("1abc", false, &[]), &guests).is_empty());
    }

    #[test]
    fn a_file_someone_else_attached_is_not_shared() {
        let guests = [guest("ana@example.com", false)];
        let theirs = Attachment { share: None, ..ours("1abc", true, &[]) };
        assert!(to_share(&theirs, &guests).is_empty());
        assert!(to_share(&Attachment { share: Some(true), ..waiting_at("/tmp/a") }, &guests).is_empty());
    }

    #[test]
    fn an_attachment_link_reads_the_scheme_in_any_case() {
        assert_eq!(linked("HTTPS://docs.google.com/d").link(), Some("HTTPS://docs.google.com/d"));
    }

    #[test]
    fn folding_a_quiet_write_onto_one_that_tells_the_guests_still_tells_them() {
        assert_eq!(Notify::Guests.and(Notify::Nobody), Notify::Guests);
        assert_eq!(Notify::Nobody.and(Notify::Guests), Notify::Guests);
        assert_eq!(Notify::Nobody.and(Notify::Nobody), Notify::Nobody);
    }

    #[test]
    fn a_row_stored_before_the_choice_existed_tells_the_guests() {
        assert_eq!(Notify::from_stored(None), Notify::Guests);
        assert_eq!(Notify::from_stored(Notify::Nobody.stored()), Notify::Nobody);
        assert_eq!(Notify::Guests.stored(), None);
    }

    #[test]
    fn a_counted_series_says_its_rule_and_how_many_are_still_to_come() {
        let event = standup(&["RRULE:FREQ=DAILY;COUNT=10"]);
        // A moment after the fourth one starts, six are still to come.
        let series = Series::of(&event, lisbon(2026, 10, 22, 9, 0) + 1).expect("it repeats");
        assert_eq!(series, Series { rule: "FREQ=DAILY;COUNT=10".into(), left: Some(6) });
    }

    #[test]
    fn a_series_with_no_count_counts_nothing() {
        let series = Series::of(&standup(&["RRULE:FREQ=WEEKLY", "EXDATE:20261026T080000Z"]), 0);
        assert_eq!(series, Some(Series { rule: "FREQ=WEEKLY".into(), left: None }));
    }

    #[test]
    fn a_one_off_event_has_no_series() {
        assert_eq!(Series::of(&standup(&[]), 0), None);
    }

    #[test]
    fn an_edit_sets_what_it_names_and_keeps_the_rest() {
        let mut event = standup(&[]);
        let edit = EventEdit { title: Some("Retro".into()), place: Some("Room 2".into()), ..EventEdit::default() };
        edit.apply(&mut event);
        assert_eq!((event.title.as_str(), event.place.as_str()), ("Retro", "Room 2"));
        assert_eq!((event.start, event.end), (lisbon(2026, 10, 19, 9, 0), lisbon(2026, 10, 19, 9, 15)));
        assert!(EventEdit::default().is_empty() && !edit.is_empty());
    }

    #[test]
    fn a_new_start_alone_keeps_the_length() {
        let mut event = standup(&[]);
        EventEdit { start: Some(lisbon(2026, 10, 19, 14, 0)), ..EventEdit::default() }.apply(&mut event);
        assert_eq!((event.start, event.end), (lisbon(2026, 10, 19, 14, 0), lisbon(2026, 10, 19, 14, 15)));
    }

    #[test]
    fn an_edit_to_whole_days_marks_the_event_all_day_and_names_its_guests() {
        let mut event = standup(&[]);
        let day = |d: u32| NaiveDate::from_ymd_opt(2026, 10, d).unwrap().and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp_millis();
        EventEdit {
            start: Some(day(20)),
            end: Some(day(22)),
            all_day: Some(true),
            guests: Some(vec!["ann@example.com".into()]),
            ..EventEdit::default()
        }
        .apply(&mut event);
        assert!(event.all_day);
        assert_eq!((event.start, event.end), (day(20), day(22)));
        assert_eq!(event.guests, vec![Guest { email: "ann@example.com".into(), ..Guest::default() }]);
    }
}
