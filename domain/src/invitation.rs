//! Reads a `text/calendar` part into an [`Invitation`].
//!
//! The `icalendar` crate does the iCalendar grammar: it unfolds continued
//! lines, splits parameters off a property, takes the quotes off a
//! parameter value, and puts escaped commas and newlines back. This module
//! does the parts that grammar leaves open: which time zone a `TZID` names,
//! what an `RRULE` says in English, and which of the many shapes a real
//! invitation arrives in counts as one event.
//!
//! Nothing here fails loudly. A part that is not an invitation, or that is
//! cut off halfway, gives back `None`.

mod recurrence;
mod reply;
mod zone;

#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::str::FromStr;

use chrono::{Duration, NaiveDate, NaiveDateTime, TimeZone};
use icalendar::{
    Calendar, CalendarComponent, CalendarDateTime, Component, DatePerhapsTime, Property,
};
use serde::{Deserialize, Serialize};

use crate::translate::gettext;
use crate::{Address, EpochMillis, UnknownVariant};

pub use reply::{Scope, counter, reply};
use zone::Zones;

/// What the sender wants done with the event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// An invitation, or a changed one.
    Request,
    /// Somebody else's answer to an invitation.
    Reply,
    /// The event is off.
    Cancel,
}

/// Yes, No or Maybe: what an attendee said, and what the user sends back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Answer {
    Yes,
    No,
    Maybe,
}

impl Answer {
    pub const ALL: [Answer; 3] = [Answer::Yes, Answer::No, Answer::Maybe];

    /// The stored form, and the one the card's buttons carry.
    pub fn as_str(self) -> &'static str {
        match self {
            Answer::Yes => "yes",
            Answer::No => "no",
            Answer::Maybe => "maybe",
        }
    }

    /// The button label.
    pub fn label(self) -> String {
        match self {
            Answer::Yes => gettext("Yes"),
            Answer::No => gettext("No"),
            Answer::Maybe => gettext("Maybe"),
        }
    }

    /// How the card names an answer somebody already gave.
    pub fn said(self) -> String {
        match self {
            Answer::Yes => gettext("Going"),
            Answer::No => gettext("Not going"),
            Answer::Maybe => gettext("Maybe"),
        }
    }

    /// The iCalendar `PARTSTAT` for this answer, which is what an emailed
    /// reply carries.
    pub fn partstat(self) -> &'static str {
        match self {
            Answer::Yes => "ACCEPTED",
            Answer::No => "DECLINED",
            Answer::Maybe => "TENTATIVE",
        }
    }

    /// Google Calendar's `responseStatus` for this answer.
    pub fn response_status(self) -> &'static str {
        match self {
            Answer::Yes => "accepted",
            Answer::No => "declined",
            Answer::Maybe => "tentative",
        }
    }

    /// The answer an iCalendar `PARTSTAT` stands for. `NEEDS-ACTION` and
    /// anything unrecognized give `None`, meaning nobody has answered yet.
    fn from_partstat(partstat: &str) -> Option<Answer> {
        match partstat.trim().to_ascii_uppercase().as_str() {
            "ACCEPTED" => Some(Answer::Yes),
            "DECLINED" => Some(Answer::No),
            "TENTATIVE" => Some(Answer::Maybe),
            _ => None,
        }
    }

    /// The answer Google Calendar's `responseStatus` stands for.
    /// `needsAction` and anything unrecognized give `None`, meaning nobody
    /// has answered yet.
    pub fn from_response_status(status: &str) -> Option<Answer> {
        match status {
            "accepted" => Some(Answer::Yes),
            "declined" => Some(Answer::No),
            "tentative" => Some(Answer::Maybe),
            _ => None,
        }
    }
}

impl FromStr for Answer {
    type Err = UnknownVariant;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Answer::ALL
            .into_iter()
            .find(|a| a.as_str() == s)
            .ok_or_else(|| UnknownVariant(s.to_string()))
    }
}

/// One person on the invitation, with what they said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Guest {
    pub who: Address,
    /// `None` until this guest answers.
    pub answer: Option<Answer>,
    /// The organizer expects no answer from an optional guest either way;
    /// this only says which list the guest is on.
    pub optional: bool,
}

/// When an event runs. An all-day event has no clock time, so it keeps the
/// dates it covers rather than two instants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum When {
    /// Start and end as instants, already resolved out of their time zone.
    /// The end is missing when the invitation gave neither `DTEND` nor
    /// `DURATION`.
    At {
        starts_at: EpochMillis,
        ends_at: Option<EpochMillis>,
    },
    /// First and last day the event covers, both included. iCalendar's
    /// `DTEND` for a date stops before the last day; this does not.
    Days { first: NaiveDate, last: NaiveDate },
}

impl When {
    /// The instant the event starts, for sorting and for telling one start
    /// from another. An all-day event answers with local midnight.
    pub fn starts_at(&self) -> Option<EpochMillis> {
        match self {
            When::At { starts_at, .. } => Some(*starts_at),
            When::Days { first, .. } => zone::local_midnight(*first),
        }
    }

    pub fn all_day(&self) -> bool {
        matches!(self, When::Days { .. })
    }
}

/// The one occurrence of a repeating event an invitation is about. An
/// organizer who moves next Tuesday's stand-up sends that occurrence
/// alone, under the series' UID and with a `RECURRENCE-ID` naming which
/// day it means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Occurrence {
    /// The `RECURRENCE-ID` as the organizer wrote it, from the semicolon
    /// or colon on. An answer copies it back unchanged, because the zone
    /// it names is part of which occurrence it is.
    pub written: String,
    /// The instant it names, read out of its zone the way `DTSTART` is.
    /// `None` when the zone is one this app could not work out.
    pub at: Option<EpochMillis>,
}

/// What one `text/calendar` part says about one event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invitation {
    /// The organizer's id for the event. Every update to it carries the
    /// same one.
    pub uid: String,
    /// Counts up with each change the organizer sends out.
    pub sequence: i64,
    pub method: Method,
    /// The title. Empty when the organizer left it out.
    pub summary: String,
    pub when: Option<When>,
    pub location: Option<String>,
    pub description: Option<String>,
    pub organizer: Option<Address>,
    pub guests: Vec<Guest>,
    /// How the event repeats, in words: "Every Monday until 30 June".
    pub repeats: Option<String>,
    /// Which occurrence of a repeating event this message is about, when
    /// it is about one rather than the series.
    pub occurrence: Option<Occurrence>,
}

impl Invitation {
    /// Whether the organizer called the event off, by `METHOD:CANCEL` or by
    /// `STATUS:CANCELLED`.
    pub fn cancelled(&self) -> bool {
        self.method == Method::Cancel
    }

    /// How the series this occurrence belongs to runs, in words, from the
    /// rule the calendar holds for it and the occurrences it counts still
    /// to come. An invitation to one occurrence carries no rule of its
    /// own, so the card asks the calendar and says this instead. `None`
    /// for a rule this cannot put in words.
    pub fn series_in_words(&self, rule: &str, left: Option<u32>) -> Option<String> {
        recurrence::series_in_words(rule, left, start_year(&self.when))
    }

    /// The guest whose address is one of `me`, if the invitation lists one.
    pub fn me<'a>(&'a self, me: &[String]) -> Option<&'a Guest> {
        self.guests.iter().find(|g| {
            me.iter()
                .any(|mine| mine.eq_ignore_ascii_case(&g.who.email))
        })
    }
}

/// Reads the first event out of an iCalendar part. `None` means the text
/// held no event this app can show, which covers an empty part, a part cut
/// off mid-property, and a `VTODO` or `VFREEBUSY` that is not an event.
pub fn read(ics: &str) -> Option<Invitation> {
    let calendar: Calendar = repair(ics).parse().ok()?;
    let method = match calendar.property_value("METHOD") {
        Some(value) if value.eq_ignore_ascii_case("REPLY") => Method::Reply,
        Some(value) if value.eq_ignore_ascii_case("CANCEL") => Method::Cancel,
        _ => Method::Request,
    };
    let zones = Zones::read(&calendar);
    let event = calendar.components.iter().find_map(event_of)?;
    let cancelled = event
        .value("STATUS")
        .is_some_and(|status| status.eq_ignore_ascii_case("CANCELLED"));
    let when = when_of(&event, &zones);
    let organizer = event.properties.get("ORGANIZER").map(address_of);
    let mut guests: Vec<Guest> = event
        .multi
        .get("ATTENDEE")
        .into_iter()
        .flatten()
        .map(|property| Guest {
            who: address_of(property),
            answer: property
                .params()
                .get("PARTSTAT")
                .and_then(|partstat| Answer::from_partstat(partstat.value())),
            optional: property
                .params()
                .get("ROLE")
                .is_some_and(|role| role.value().eq_ignore_ascii_case("OPT-PARTICIPANT")),
        })
        .collect();
    guests.dedup_by(|a, b| a.who.email.eq_ignore_ascii_case(&b.who.email));
    Some(Invitation {
        uid: event.value("UID").unwrap_or_default().to_string(),
        sequence: event
            .value("SEQUENCE")
            .and_then(|value| value.trim().parse().ok())
            .unwrap_or(0),
        method: if cancelled { Method::Cancel } else { method },
        summary: text_of(&event, "SUMMARY").unwrap_or_default(),
        when,
        location: text_of(&event, "LOCATION"),
        description: text_of(&event, "DESCRIPTION"),
        organizer,
        guests,
        repeats: event
            .value("RRULE")
            .and_then(|rule| recurrence::in_words(rule, start_year(&when))),
        occurrence: event
            .properties
            .get("RECURRENCE-ID")
            .map(|property| occurrence_of(property, &zones)),
    })
}

/// The occurrence a `RECURRENCE-ID` names, kept both as the organizer
/// wrote it and as an instant.
fn occurrence_of(property: &Property, zones: &Zones) -> Occurrence {
    let mut written = String::new();
    for parameter in property.params().values() {
        written.push_str(&format!(
            ";{}={}",
            parameter.key().to_ascii_uppercase(),
            reply::parameter(parameter.value())
        ));
    }
    written.push(':');
    written.push_str(property.value().trim());
    let at = match DatePerhapsTime::from_property(property) {
        Some(DatePerhapsTime::DateTime(at)) => zones.instant(&at),
        Some(DatePerhapsTime::Date(day)) => zone::local_midnight(day),
        None => None,
    };
    Occurrence { written, at }
}

/// The year the event starts in, so a repeat that ends in it needs no year.
fn start_year(when: &Option<When>) -> Option<i32> {
    use chrono::Datelike;
    match when.as_ref()? {
        When::Days { first, .. } => Some(first.year()),
        When::At { starts_at, .. } => chrono::DateTime::from_timestamp_millis(*starts_at)
            .map(|at| at.with_timezone(&chrono::Local).year()),
    }
}

/// The properties of one `VEVENT`. `icalendar`'s `Component` trait cannot
/// be a trait object, and the crate types a block as an event or leaves it
/// unknown depending on how the file spelled `BEGIN`, so this holds the two
/// maps both shapes give and the rest of the module reads only these.
struct Block<'a> {
    properties: &'a BTreeMap<String, Property>,
    multi: &'a BTreeMap<String, Vec<Property>>,
}

impl Block<'_> {
    fn of<C: Component>(component: &C) -> Block<'_> {
        Block {
            properties: component.properties(),
            multi: component.multi_properties(),
        }
    }

    fn value(&self, key: &str) -> Option<&str> {
        Some(self.properties.get(key)?.value())
    }
}

/// The `VEVENT` inside a component, whether the crate typed it as an event
/// or left it as an unknown block.
fn event_of(component: &CalendarComponent) -> Option<Block<'_>> {
    match component {
        CalendarComponent::Event(event) => Some(Block::of(event)),
        CalendarComponent::Other(other)
            if other.component_kind().eq_ignore_ascii_case("VEVENT") =>
        {
            Some(Block::of(other))
        }
        _ => None,
    }
}

/// Mailers send iCalendar with lone newlines, with a byte-order mark, and
/// with lowercase `begin:vevent`. The grammar wants none of that, so this
/// puts the text back in shape before the parser sees it: CRLF line ends,
/// no mark, and property names in capitals, which is how the rest of this
/// module looks them up.
fn repair(ics: &str) -> String {
    let mut out = String::with_capacity(ics.len() + 64);
    for line in ics.trim_start_matches('\u{feff}').split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        match property_name(line) {
            // BEGIN and END name a block, and the name is looked up too.
            Some(name) if name == "BEGIN" || name == "END" => {
                out.push_str(&line.to_ascii_uppercase());
            }
            Some(name) => {
                out.push_str(&name);
                out.push_str(&line[name.len()..]);
            }
            None => out.push_str(line),
        }
        out.push_str("\r\n");
    }
    out
}

/// The property name a line opens with, in capitals, or `None` when the
/// line continues the one before it or holds no name at all.
fn property_name(line: &str) -> Option<String> {
    if line.starts_with([' ', '\t']) {
        return None;
    }
    let end = line.find([';', ':'])?;
    let name = &line[..end];
    (!name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
        .then(|| name.to_ascii_uppercase())
}

/// A property's value with the control characters taken out. Outlook sends
/// a stray `\u{0}` often enough to be worth guarding, and a label with one
/// in it draws a box in GTK.
fn text_of(event: &Block<'_>, key: &str) -> Option<String> {
    let value: String = event
        .value(key)?
        .chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect();
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// The address in an `ORGANIZER` or `ATTENDEE`, with the `CN` parameter as
/// its name. The value is a `mailto:` URI in every invitation worth
/// showing, but Outlook sometimes sends a bare address.
fn address_of(property: &Property) -> Address {
    let email = property
        .value()
        .trim()
        .trim_start_matches("mailto:")
        .trim_start_matches("MAILTO:")
        .trim()
        .to_string();
    let name = property
        .params()
        .get("CN")
        .map(|cn| cn.value().trim().to_string())
        .filter(|cn| !cn.is_empty() && !cn.eq_ignore_ascii_case(&email));
    Address { name, email }
}

/// When the event runs, read from `DTSTART` with `DTEND` or `DURATION`.
fn when_of(event: &Block<'_>, zones: &Zones) -> Option<When> {
    let start = DatePerhapsTime::from_property(event.properties.get("DTSTART")?)?;
    let end = event
        .properties
        .get("DTEND")
        .and_then(DatePerhapsTime::from_property);
    let length = event.value("DURATION").and_then(recurrence::duration);
    match (&start, &end) {
        // An all-day event ends the day before its exclusive DTEND. One
        // that gives no DTEND covers its single day.
        (DatePerhapsTime::Date(first), Some(DatePerhapsTime::Date(after))) => Some(When::Days {
            first: *first,
            last: (*after - Duration::days(1)).max(*first),
        }),
        (DatePerhapsTime::Date(first), _) => {
            let last = length
                .map(|length| (*first + length - Duration::days(1)).max(*first))
                .unwrap_or(*first);
            Some(When::Days {
                first: *first,
                last,
            })
        }
        (DatePerhapsTime::DateTime(start), _) => {
            let starts_at = zones.instant(start)?;
            let ends_at = match end {
                Some(DatePerhapsTime::DateTime(end)) => zones.instant(&end),
                Some(DatePerhapsTime::Date(day)) => zone::local_midnight(day),
                None => length.map(|length| starts_at + length.num_milliseconds()),
            };
            Some(When::At {
                starts_at,
                ends_at: ends_at.filter(|end| *end >= starts_at),
            })
        }
    }
}

/// A naive time read as if it were in `offset` seconds east of UTC.
pub(crate) fn at_offset(naive: NaiveDateTime, offset: i32) -> Option<EpochMillis> {
    chrono::FixedOffset::east_opt(offset)?
        .from_local_datetime(&naive)
        .earliest()
        .map(|when| when.timestamp_millis())
}

/// The instant a `CalendarDateTime` names, for the forms that carry their
/// own zone. `WithTimezone` needs the calendar's `VTIMEZONE` blocks and
/// goes through [`Zones`] instead.
pub(crate) fn plain_instant(when: &CalendarDateTime) -> Option<EpochMillis> {
    match when {
        CalendarDateTime::Utc(at) => Some(at.timestamp_millis()),
        CalendarDateTime::Floating(naive) => chrono::Local
            .from_local_datetime(naive)
            .earliest()
            .map(|at| at.timestamp_millis()),
        CalendarDateTime::WithTimezone { .. } => None,
    }
}
