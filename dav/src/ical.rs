//! One VCALENDAR resource and Penguin Mail's events. A resource holds one
//! event, or a series with its changed occurrences, and each VEVENT maps
//! to one neutral [`Event`]. A write changes only the properties whose
//! value the edit changed, on the one VEVENT it names, and leaves every
//! other line, known or not, as the server sent it; calcard keeps unknown
//! properties and parameters through a parse and a write.

use calcard::common::timezone::Tz;
use calcard::icalendar::{
    ICalendar, ICalendarAction, ICalendarClassification, ICalendarComponent, ICalendarComponentType,
    ICalendarEntry, ICalendarParameter, ICalendarParameterName, ICalendarParameterValue, ICalendarParticipationStatus,
    ICalendarProperty, ICalendarStatus, ICalendarTransparency, ICalendarValue,
};
use calcard::{Entry, Parser};
use chrono::{DateTime, NaiveDate, Utc};
use mailrs_domain::EpochMillis;
use mailrs_domain::calendar::{Attachment, Event, Guest, Notify, Reminder, ReminderMethod, Status, occurrence_id};
use mailrs_domain::invitation::Answer;

use crate::ids::resource_id;
use crate::zones::Zones;
use crate::{DavError, MOST_RESOURCE_BYTES};

const DAY: EpochMillis = 24 * 60 * 60 * 1000;
const PRODID: &str = "-//Penguin Mail//EN";

/// A resource as events.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReadResource {
    pub events: Vec<Event>,
    /// The master's id when the resource repeats or holds a changed
    /// occurrence, so the copy can drop the occurrences it no longer has.
    pub series: Option<String>,
}

pub(crate) fn parse(text: &str) -> Result<ICalendar, DavError> {
    if text.len() > MOST_RESOURCE_BYTES {
        return Err(DavError::TooLarge(MOST_RESOURCE_BYTES));
    }
    match Parser::new(text).entry() {
        Entry::ICalendar(ical) => Ok(ical),
        other => Err(DavError::Parse(format!("not a VCALENDAR: {other:?}"))),
    }
}

/// Whether a VEVENT in `text` has the UID `uid`, compared octet for octet.
pub(crate) fn holds_uid(text: &str, uid: &str) -> bool {
    parse(text).is_ok_and(|ical| events_of(&ical).any(|(_, comp)| text_of(comp, ICalendarProperty::Uid).as_deref() == Some(uid)))
}

fn events_of(ical: &ICalendar) -> impl Iterator<Item = (usize, &ICalendarComponent)> {
    ical.components.iter().enumerate().filter(|(_, c)| c.component_type == ICalendarComponentType::VEvent)
}

fn text_of(comp: &ICalendarComponent, property: ICalendarProperty) -> Option<String> {
    comp.property(&property).and_then(|e| e.values.first()).and_then(|v| v.as_text()).map(str::to_string)
}

/// When one DTSTART, DTEND or RECURRENCE-ID says, in the file's zones.
struct When {
    at: EpochMillis,
    zone: String,
    all_day: bool,
}

fn when_of(entry: &ICalendarEntry, zones: &Zones) -> Option<When> {
    let value = entry.values.first()?.as_partial_date_time()?;
    if value.hour.is_none() {
        let day = NaiveDate::from_ymd_opt(i32::from(value.year?), u32::from(value.month?), u32::from(value.day?))?;
        return Some(When { at: day.and_hms_opt(0, 0, 0)?.and_utc().timestamp_millis(), zone: "UTC".into(), all_day: true });
    }
    let result = value.to_date_time()?;
    let named = entry.tz_id().and_then(|id| zones.resolve(id));
    let tz = match (result.offset, named) {
        (Some(_), _) => Tz::UTC,
        (None, Some(zone)) => Tz::Tz(zone),
        // A floating time means the same clock time wherever the reader
        // is; UTC keeps it where the file put it.
        (None, None) => Tz::UTC,
    };
    let at = result.to_date_time_with_tz(tz)?.timestamp_millis();
    let zone = match (result.offset, named) {
        (None, Some(zone)) => zone.name().to_string(),
        _ => "UTC".to_string(),
    };
    Some(When { at, zone, all_day: false })
}

fn line_of(entry: &ICalendarEntry) -> String {
    let mut line = String::new();
    // Writing into a String cannot fail.
    let _ = entry.write_to(&mut line);
    line.replace("\r\n ", "").replace("\r\n\t", "").trim_end().to_string()
}

fn answer_of(entry: &ICalendarEntry) -> Option<Answer> {
    match entry.parameter(&ICalendarParameterName::Partstat)? {
        ICalendarParameterValue::Partstat(ICalendarParticipationStatus::Accepted) => Some(Answer::Yes),
        ICalendarParameterValue::Partstat(ICalendarParticipationStatus::Declined) => Some(Answer::No),
        ICalendarParameterValue::Partstat(ICalendarParticipationStatus::Tentative) => Some(Answer::Maybe),
        _ => None,
    }
}

fn address_of(entry: &ICalendarEntry) -> Option<String> {
    let value = entry.values.first()?;
    let text = value.as_text().map(str::to_string).or_else(|| match value {
        ICalendarValue::Uri(uri) => uri.as_str().map(str::to_string),
        _ => None,
    })?;
    let address = calcard::icalendar::utils::strip_mailto_scheme(&text).trim().to_string();
    (!address.is_empty()).then_some(address)
}

fn reminders_of(ical: &ICalendar, comp: &ICalendarComponent) -> Option<Vec<Reminder>> {
    let alarms: Vec<&ICalendarComponent> = comp
        .component_ids
        .iter()
        .filter_map(|id| ical.components.get(*id as usize))
        .filter(|c| c.component_type == ICalendarComponentType::VAlarm)
        .collect();
    if alarms.is_empty() {
        return None;
    }
    Some(
        alarms
            .iter()
            .filter_map(|alarm| {
                let seconds = match alarm.property(&ICalendarProperty::Trigger)?.values.first()? {
                    ICalendarValue::Duration(duration) => duration.as_seconds(),
                    _ => return None,
                };
                let method = match alarm.property(&ICalendarProperty::Action)?.values.first()? {
                    ICalendarValue::Action(ICalendarAction::Email) => ReminderMethod::Email,
                    _ => ReminderMethod::Notification,
                };
                Some(Reminder { minutes: u32::try_from(-seconds / 60).unwrap_or(0), method })
            })
            .collect(),
    )
}

/// The files an event links to: its `ATTACH` lines that name a URL. An
/// inline base64 attachment has no URL to open and stays in the file
/// unread.
fn attachments_of(comp: &ICalendarComponent) -> Vec<Attachment> {
    comp.properties(&ICalendarProperty::Attach)
        .filter_map(|entry| {
            let value = entry.values.first()?;
            let url = value.as_text().map(str::to_string).or_else(|| match value {
                ICalendarValue::Uri(uri) => uri.as_str().map(str::to_string),
                _ => None,
            })?;
            let text = |name: ICalendarParameterName| entry.parameter(&name).and_then(|v| v.as_text()).map(str::to_string);
            let title = text(ICalendarParameterName::Filename)
                .or_else(|| url.rsplit('/').next().filter(|s| !s.is_empty()).map(str::to_string))
                .unwrap_or_default();
            Some(Attachment { title, file_url: url, mime_type: text(ICalendarParameterName::Fmttype).unwrap_or_default(), ..Attachment::default() })
        })
        .collect()
}

/// One VEVENT as an event, without the ids the resource gives it.
fn event_of(ical: &ICalendar, comp: &ICalendarComponent, zones: &Zones, me: &[String]) -> Option<Event> {
    let start = when_of(comp.property(&ICalendarProperty::Dtstart)?, zones)?;
    let end = match comp.property(&ICalendarProperty::Dtend).and_then(|e| when_of(e, zones)) {
        Some(end) => end.at,
        None => match comp.property(&ICalendarProperty::Duration).and_then(|e| e.values.first()) {
            Some(ICalendarValue::Duration(d)) => start.at + d.as_seconds() * 1000,
            _ if start.all_day => start.at + DAY,
            _ => start.at,
        },
    };
    let organizer = comp.property(&ICalendarProperty::Organizer).and_then(address_of);
    let is_me = |address: &str| me.iter().any(|m| m.eq_ignore_ascii_case(address));
    let guests: Vec<Guest> = comp
        .properties(&ICalendarProperty::Attendee)
        .filter_map(|entry| {
            let email = address_of(entry)?;
            Some(Guest {
                name: entry.parameter(&ICalendarParameterName::Cn).and_then(|v| v.as_text()).map(str::to_string),
                answer: answer_of(entry),
                organizer: organizer.as_deref().is_some_and(|o| o.eq_ignore_ascii_case(&email)),
                me: is_me(&email),
                email,
            })
        })
        .collect();
    let my_answer = guests.iter().find(|g| g.me).and_then(|g| g.answer);
    let rules = comp
        .entries
        .iter()
        .filter(|e| matches!(e.name, ICalendarProperty::Rrule | ICalendarProperty::Exdate | ICalendarProperty::Rdate))
        .map(|e| mailrs_domain::calendar::rename_zone(&line_of(e), &|tzid| zones.resolve(tzid).map(|tz| tz.name().to_string())))
        .collect();
    Some(Event {
        uid: comp.uid().unwrap_or_default().to_string(),
        start: start.at,
        end,
        zone: start.zone,
        all_day: start.all_day,
        title: text_of(comp, ICalendarProperty::Summary).unwrap_or_default(),
        place: text_of(comp, ICalendarProperty::Location).unwrap_or_default(),
        description: text_of(comp, ICalendarProperty::Description).unwrap_or_default(),
        color: text_of(comp, ICalendarProperty::Color).filter(|c| c.starts_with('#') && c.len() == 7).map(|c| c.to_ascii_lowercase()),
        busy: comp.transparency() != Some(&ICalendarTransparency::Transparent),
        status: match comp.status() {
            Some(ICalendarStatus::Cancelled) => Status::Cancelled,
            Some(ICalendarStatus::Tentative) => Status::Tentative,
            _ => Status::Confirmed,
        },
        private: matches!(
            comp.property(&ICalendarProperty::Class).and_then(|e| e.values.first()),
            Some(ICalendarValue::Classification(ICalendarClassification::Private | ICalendarClassification::Confidential))
        ),
        organizer,
        guests,
        my_answer,
        reminders: reminders_of(ical, comp),
        conference: text_of(comp, ICalendarProperty::Conference)
            .or_else(|| text_of(comp, ICalendarProperty::Other("X-GOOGLE-CONFERENCE".into()))),
        rules,
        sequence: comp.property(&ICalendarProperty::Sequence).and_then(|e| e.values.first()).and_then(|v| v.as_integer()).unwrap_or(0),
        // `Some` even when empty: the copy reads `None` as "not read yet".
        attachments: Some(attachments_of(comp)),
        ..Event::default()
    })
}

/// The instant a VEVENT's RECURRENCE-ID names; `None` for a master.
fn recurrence_of(comp: &ICalendarComponent, zones: &Zones) -> Option<EpochMillis> {
    when_of(comp.property(&ICalendarProperty::RecurrenceId)?, zones).map(|w| w.at)
}

pub fn read_resource(text: &str, calendar: &str, href: &str, etag: &str, me: &[String]) -> Result<ReadResource, DavError> {
    let ical = parse(text)?;
    let zones = Zones::of(&ical);
    let id = resource_id(href);
    let mut master: Option<Event> = None;
    let mut changed: Vec<(EpochMillis, Event)> = Vec::new();
    for (_, comp) in events_of(&ical) {
        let Some(mut event) = event_of(&ical, comp, &zones, me) else { continue };
        event.calendar = calendar.to_string();
        event.etag = etag.to_string();
        match recurrence_of(comp, &zones) {
            None => {
                event.id = id.clone();
                master = Some(event);
            }
            Some(original) => changed.push((original, event)),
        }
    }
    let series = (!changed.is_empty() || master.as_ref().is_some_and(|m| !m.rules.is_empty())).then(|| id.clone());
    // A resource of changed occurrences alone, as an invitation to one
    // occurrence leaves, still names them after a series of its own id.
    let named_after = master.clone().unwrap_or_else(|| Event {
        id: id.clone(),
        all_day: changed.first().is_some_and(|(_, e)| e.all_day),
        ..Event::default()
    });
    let mut events: Vec<Event> = master.into_iter().collect();
    for (original, mut event) in changed {
        event.id = occurrence_id(&named_after, original);
        event.series = Some(id.clone());
        event.original_start = Some(original);
        events.push(event);
    }
    Ok(ReadResource { events, series })
}

/// The zone of a calendar's `calendar-timezone` property.
pub fn calendar_zone(vtimezone: &str) -> Option<String> {
    let ical = parse(vtimezone).ok()?;
    let zones = Zones::of(&ical);
    ical.timezones()
        .filter_map(|zone| zone.property(&ICalendarProperty::Tzid)?.values.first()?.as_text())
        .find_map(|tzid| zones.resolve(tzid))
        .map(|tz| tz.name().to_string())
}

/// Text escaped for an iCalendar TEXT value (RFC 5545 section 3.3.11).
fn escape(text: &str) -> String {
    text.replace('\\', "\\\\").replace(';', "\\;").replace(',', "\\,").replace('\n', "\\n")
}

/// The entries `lines` hold, as calcard reads them inside a VEVENT. This
/// is how a new value is built: written as a line, read back as an entry,
/// so no value type is built by hand.
fn entries(lines: &[String]) -> Result<Vec<ICalendarEntry>, DavError> {
    if lines.is_empty() {
        return Ok(Vec::new());
    }
    let text = format!("BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\n{}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n", lines.join("\r\n"));
    let ical = parse(&text)?;
    Ok(ical.components.get(1).map(|c| c.entries.clone()).unwrap_or_default())
}

/// The components a snippet holds after its VCALENDAR, such as a VALARM.
fn components(text: &str) -> Result<Vec<ICalendarComponent>, DavError> {
    let ical = parse(&format!("BEGIN:VCALENDAR\r\n{text}END:VCALENDAR\r\n"))?;
    Ok(ical.components.into_iter().skip(1).collect())
}

/// Puts `lines` in place of every entry of `property` on component `at`.
fn replace(ical: &mut ICalendar, at: usize, property: ICalendarProperty, lines: &[String]) -> Result<(), DavError> {
    let new = entries(lines)?;
    let comp = &mut ical.components[at];
    comp.entries.retain(|e| e.name != property);
    comp.entries.extend(new);
    Ok(())
}

/// A DTSTART-shaped line for `at`: a date for an all-day event, the local
/// time with its TZID for a zoned one, UTC otherwise.
fn time_line(name: &str, at: EpochMillis, zone: &str, all_day: bool) -> String {
    let Some(utc) = DateTime::<Utc>::from_timestamp_millis(at) else {
        return format!("{name}:19700101T000000Z");
    };
    if all_day {
        return format!("{name};VALUE=DATE:{}", utc.format("%Y%m%d"));
    }
    match zone.parse::<chrono_tz::Tz>() {
        Ok(tz) if zone != "UTC" => format!("{name};TZID={zone}:{}", utc.with_timezone(&tz).format("%Y%m%dT%H%M%S")),
        _ => format!("{name}:{}", utc.format("%Y%m%dT%H%M%SZ")),
    }
}

fn fresh() -> Result<ICalendar, DavError> {
    parse(&format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:{PRODID}\r\nEND:VCALENDAR\r\n"))
}

/// Adds an empty VEVENT for `event` under the VCALENDAR and answers its
/// index: its UID, and for a changed occurrence its RECURRENCE-ID in the
/// shape of the master's DTSTART.
fn add_vevent(ical: &mut ICalendar, event: &Event, master: Option<&Event>) -> Result<usize, DavError> {
    let uid = if event.uid.is_empty() { format!("{}@penguin-mail", event.id) } else { event.uid.clone() };
    let mut lines = vec![format!("UID:{uid}")];
    if let (Some(original), Some(master)) = (event.original_start, master) {
        lines.push(time_line("RECURRENCE-ID", original, &master.zone, master.all_day));
    }
    let comp = components(&format!("BEGIN:VEVENT\r\n{}\r\nEND:VEVENT\r\n", lines.join("\r\n")))?
        .into_iter()
        .next()
        .ok_or_else(|| DavError::Parse("an empty VEVENT did not read back".into()))?;
    ical.components.push(comp);
    let at = ical.components.len() - 1;
    ical.components[0].component_ids.push(at as u32);
    Ok(at)
}

/// The VEVENT `event` names: the master for an event with no original
/// start, else the one whose RECURRENCE-ID is that instant.
fn find(ical: &ICalendar, zones: &Zones, event: &Event) -> Option<usize> {
    events_of(ical)
        .find(|(_, comp)| recurrence_of(comp, zones) == event.original_start)
        .map(|(at, _)| at)
}

fn master_of(ical: &ICalendar, zones: &Zones, me: &[String]) -> Option<(usize, Event)> {
    events_of(ical)
        .find(|(_, comp)| recurrence_of(comp, zones).is_none())
        .and_then(|(at, comp)| event_of(ical, comp, zones, me).map(|e| (at, e)))
}

/// Whether the account may raise this VEVENT's SEQUENCE: it organizes the
/// event, or the event has no organizer or no guests. RFC 5546 leaves
/// SEQUENCE to the organizer, and a guest's raised number reaches the
/// organizer in the reply a scheduling server sends.
fn may_sequence(comp: &ICalendarComponent, me: &[String]) -> bool {
    let organizer = comp.property(&ICalendarProperty::Organizer).and_then(address_of);
    let guests = comp.properties(&ICalendarProperty::Attendee).next().is_some();
    match organizer {
        Some(organizer) if guests => me.iter().any(|m| m.eq_ignore_ascii_case(&organizer)),
        _ => true,
    }
}

/// DTSTAMP and LAST-MODIFIED to now, and SEQUENCE one up when `me` may
/// raise it (see [`may_sequence`]).
fn stamp(ical: &mut ICalendar, at: usize, now: EpochMillis, me: &[String]) -> Result<(), DavError> {
    let stamp = DateTime::<Utc>::from_timestamp_millis(now).unwrap_or_default().format("%Y%m%dT%H%M%SZ").to_string();
    replace(ical, at, ICalendarProperty::Dtstamp, &[format!("DTSTAMP:{stamp}")])?;
    replace(ical, at, ICalendarProperty::LastModified, &[format!("LAST-MODIFIED:{stamp}")])?;
    if !may_sequence(&ical.components[at], me) {
        return Ok(());
    }
    let sequence = ical.components[at]
        .property(&ICalendarProperty::Sequence)
        .and_then(|e| e.values.first())
        .and_then(|v| v.as_integer())
        .unwrap_or(0);
    replace(ical, at, ICalendarProperty::Sequence, &[format!("SEQUENCE:{}", sequence + 1)])
}

fn alarm_text(reminder: &Reminder) -> String {
    let action = match reminder.method {
        ReminderMethod::Email => "EMAIL",
        ReminderMethod::Notification => "DISPLAY",
    };
    format!("BEGIN:VALARM\r\nACTION:{action}\r\nTRIGGER:-PT{}M\r\nDESCRIPTION:Reminder\r\nEND:VALARM\r\n", reminder.minutes)
}

/// Writes what `event` changes onto the VEVENT at `at`, compared with
/// `before`, a read of that VEVENT. Answers whether anything changed.
fn patch(ical: &mut ICalendar, at: usize, before: &Event, event: &Event, me: &[String]) -> Result<bool, DavError> {
    let mut changed = false;
    let mut text = |ical: &mut ICalendar, property: ICalendarProperty, name: &str, old: &str, new: &str| -> Result<(), DavError> {
        if old != new {
            let lines: Vec<String> = if new.is_empty() { Vec::new() } else { vec![format!("{name}:{}", escape(new))] };
            replace(ical, at, property, &lines)?;
            changed = true;
        }
        Ok(())
    };
    text(ical, ICalendarProperty::Summary, "SUMMARY", &before.title, &event.title)?;
    text(ical, ICalendarProperty::Location, "LOCATION", &before.place, &event.place)?;
    text(ical, ICalendarProperty::Description, "DESCRIPTION", &before.description, &event.description)?;
    if (before.start, before.end, &before.zone, before.all_day) != (event.start, event.end, &event.zone, event.all_day) {
        replace(ical, at, ICalendarProperty::Dtstart, &[time_line("DTSTART", event.start, &event.zone, event.all_day)])?;
        replace(ical, at, ICalendarProperty::Dtend, &[time_line("DTEND", event.end, &event.zone, event.all_day)])?;
        replace(ical, at, ICalendarProperty::Duration, &[])?;
        changed = true;
    }
    if event.original_start.is_none() && before.rules != event.rules {
        for property in [ICalendarProperty::Rrule, ICalendarProperty::Exdate, ICalendarProperty::Rdate] {
            replace(ical, at, property, &[])?;
        }
        let new = entries(&event.rules)?;
        ical.components[at].entries.extend(new);
        changed = true;
    }
    if before.busy != event.busy {
        let value = if event.busy { "OPAQUE" } else { "TRANSPARENT" };
        replace(ical, at, ICalendarProperty::Transp, &[format!("TRANSP:{value}")])?;
        changed = true;
    }
    if before.status != event.status {
        let value = match event.status {
            Status::Confirmed => "CONFIRMED",
            Status::Tentative => "TENTATIVE",
            Status::Cancelled => "CANCELLED",
        };
        replace(ical, at, ICalendarProperty::Status, &[format!("STATUS:{value}")])?;
        changed = true;
    }
    if before.private != event.private {
        let value = if event.private { "PRIVATE" } else { "PUBLIC" };
        replace(ical, at, ICalendarProperty::Class, &[format!("CLASS:{value}")])?;
        changed = true;
    }
    if before.color != event.color {
        let lines: Vec<String> = event.color.iter().map(|c| format!("COLOR:{c}")).collect();
        replace(ical, at, ICalendarProperty::Color, &lines)?;
        changed = true;
    }
    changed |= patch_guests(ical, at, before, event, me)?;
    if before.reminders != event.reminders {
        let old: Vec<u32> = ical.components[at]
            .component_ids
            .iter()
            .copied()
            .filter(|id| ical.components.get(*id as usize).is_some_and(|c| c.component_type == ICalendarComponentType::VAlarm))
            .collect();
        ical.remove_component_ids(&old);
        // remove_component_ids renumbers; find the VEVENT again by its
        // place among the events, which removing alarms does not change.
        let at = find_again(ical, before)?;
        let text: String = event.reminders.iter().flatten().map(alarm_text).collect();
        for alarm in components(&text)? {
            ical.components.push(alarm);
            let id = (ical.components.len() - 1) as u32;
            ical.components[at].component_ids.push(id);
        }
        changed = true;
    }
    Ok(changed)
}

fn find_again(ical: &ICalendar, before: &Event) -> Result<usize, DavError> {
    let zones = Zones::of(ical);
    find(ical, &zones, before).ok_or_else(|| DavError::Parse("the event left its resource".into()))
}

/// Adds and removes ATTENDEE lines as the guest list changed, keeping each
/// remaining guest's own line with its parameters, and sets the account's
/// own PARTSTAT, with SCHEDULE-AGENT=CLIENT so a server that schedules
/// does not send its own reply beside the one Penguin Mail mails.
fn patch_guests(ical: &mut ICalendar, at: usize, before: &Event, event: &Event, me: &[String]) -> Result<bool, DavError> {
    let emails = |e: &Event| -> Vec<String> { e.guests.iter().map(|g| g.email.to_ascii_lowercase()).collect() };
    let mut changed = false;
    if emails(before) != emails(event) {
        let wanted = emails(event);
        ical.components[at].entries.retain(|entry| {
            entry.name != ICalendarProperty::Attendee
                || address_of(entry).is_some_and(|a| wanted.contains(&a.to_ascii_lowercase()))
        });
        let had = emails(before);
        let lines: Vec<String> = event
            .guests
            .iter()
            .filter(|g| !had.contains(&g.email.to_ascii_lowercase()))
            .map(|g| match &g.name {
                Some(name) => format!("ATTENDEE;CN=\"{}\";PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:{}", name.replace('"', ""), g.email),
                None => format!("ATTENDEE;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:{}", g.email),
            })
            .collect();
        let new = entries(&lines)?;
        ical.components[at].entries.extend(new);
        if before.organizer.is_none()
            && !event.guests.is_empty()
            && let Some(first) = me.first()
        {
            replace(ical, at, ICalendarProperty::Organizer, &[format!("ORGANIZER:mailto:{first}")])?;
        }
        changed = true;
    }
    if before.my_answer != event.my_answer
        && let Some(answer) = event.my_answer
    {
        changed |= set_partstat(&mut ical.components[at], me, answer, true)?;
    }
    Ok(changed)
}

/// The `SCHEDULE-AGENT=CLIENT` parameter, read back from a line so no
/// parameter value is built by hand.
fn client_agent() -> Result<Option<ICalendarParameter>, DavError> {
    let donor = entries(&["ATTENDEE;SCHEDULE-AGENT=CLIENT:mailto:x@x".to_string()])?;
    Ok(donor.into_iter().next().and_then(|e| e.params.into_iter().find(|p| p.name == ICalendarParameterName::ScheduleAgent)))
}

/// Tells the server not to mail anyone about this VEVENT: every ATTENDEE
/// line takes `SCHEDULE-AGENT=CLIENT`. Answers whether a line changed.
fn schedule_by_client(comp: &mut ICalendarComponent) -> Result<bool, DavError> {
    let Some(agent) = client_agent()? else { return Ok(false) };
    let mut changed = false;
    for entry in comp.entries.iter_mut().filter(|e| e.name == ICalendarProperty::Attendee) {
        if !entry.params.iter().any(|p| p.name == ICalendarParameterName::ScheduleAgent) {
            entry.params.push(agent.clone());
            changed = true;
        }
    }
    Ok(changed)
}

fn partstat_word(answer: Answer) -> &'static str {
    match answer {
        Answer::Yes => "ACCEPTED",
        Answer::No => "DECLINED",
        Answer::Maybe => "TENTATIVE",
    }
}

/// Sets PARTSTAT on the account's own ATTENDEE line, and
/// SCHEDULE-AGENT=CLIENT beside it when `by_client`, keeping its other
/// parameters. Without `by_client` an agent the line had is dropped, so
/// the server schedules. The parameters come from a line read back, so
/// no parameter value is built by hand.
fn set_partstat(comp: &mut ICalendarComponent, me: &[String], answer: Answer, by_client: bool) -> Result<bool, DavError> {
    let agent = if by_client { ";SCHEDULE-AGENT=CLIENT" } else { "" };
    let donor = entries(&[format!("ATTENDEE;PARTSTAT={}{agent}:mailto:x@x", partstat_word(answer))])?;
    let Some(donor) = donor.into_iter().next() else { return Ok(false) };
    let mut found = false;
    for entry in comp.entries.iter_mut().filter(|e| e.name == ICalendarProperty::Attendee) {
        if address_of(entry).is_some_and(|a| me.iter().any(|m| m.eq_ignore_ascii_case(&a))) {
            entry.params.retain(|p| !matches!(p.name, ICalendarParameterName::Partstat | ICalendarParameterName::ScheduleAgent));
            entry.params.extend(donor.params.iter().cloned());
            found = true;
        }
    }
    Ok(found)
}

/// Writes `event` so the server does not tell its guests: the same as
/// [`write_event_notifying`] with [`Notify::Nobody`].
pub fn write_event(existing: Option<&str>, event: &Event, me: &[String], now: EpochMillis) -> Result<String, DavError> {
    write_event_notifying(existing, event, me, now, Notify::Nobody)
}

pub fn write_event_notifying(existing: Option<&str>, event: &Event, me: &[String], now: EpochMillis, notify: Notify) -> Result<String, DavError> {
    let mut ical = match existing {
        Some(text) => parse(text)?,
        None => fresh()?,
    };
    let zones = Zones::of(&ical);
    let master = master_of(&ical, &zones, me).map(|(_, m)| m);
    let at = match find(&ical, &zones, event) {
        Some(at) => at,
        None => add_vevent(&mut ical, event, master.as_ref())?,
    };
    // A VEVENT just added reads as an event with nothing set, so every
    // field the event carries counts as changed and is written.
    let before = event_of(&ical, &ical.components[at], &zones, me).unwrap_or_else(|| Event {
        original_start: event.original_start,
        busy: true,
        ..Event::default()
    });
    let mut changed = patch(&mut ical, at, &before, event, me)?;
    if notify == Notify::Nobody {
        // After the patch, which may have renumbered the components.
        let at = find_again(&ical, event)?;
        changed |= schedule_by_client(&mut ical.components[at])?;
    }
    if changed || existing.is_none() {
        let at = find_again(&ical, event)?;
        stamp(&mut ical, at, now, me)?;
    }
    ical.add_missing_timezones();
    Ok(ical.to_string())
}

pub fn cancel_occurrence(existing: &str, original_start: EpochMillis, now: EpochMillis) -> Result<Option<String>, DavError> {
    let mut ical = parse(existing)?;
    let zones = Zones::of(&ical);
    let Some((master_at, master)) = master_of(&ical, &zones, &[]) else {
        return Ok(None);
    };
    let entry = entries(&[time_line("EXDATE", original_start, &master.zone, master.all_day)])?;
    ical.components[master_at].entries.extend(entry);
    let changed: Vec<u32> = events_of(&ical)
        .filter(|(_, comp)| recurrence_of(comp, &zones) == Some(original_start))
        .map(|(at, _)| at as u32)
        .collect();
    ical.remove_component_ids(&changed);
    let zones = Zones::of(&ical);
    if let Some((at, master)) = master_of(&ical, &zones, &[]) {
        // Only the organizer cancels an occurrence for everyone, so the
        // sequence goes up as for the organizer.
        let organizer: Vec<String> = master.organizer.into_iter().collect();
        stamp(&mut ical, at, now, &organizer)?;
    }
    Ok(Some(ical.to_string()))
}

pub fn answer(existing: &str, me: &[String], answer: Answer, now: EpochMillis) -> Result<Option<String>, DavError> {
    answer_scheduled(existing, me, answer, now, false)
}

/// [`answer`], for a server that mails the organizer itself when a PUT
/// changes an attendee's PARTSTAT (`server_schedules`): the line then
/// carries no `SCHEDULE-AGENT=CLIENT`, which would stop it.
pub fn answer_scheduled(existing: &str, me: &[String], answer: Answer, now: EpochMillis, server_schedules: bool) -> Result<Option<String>, DavError> {
    let mut ical = parse(existing)?;
    let mut any = false;
    let events: Vec<usize> = events_of(&ical).map(|(at, _)| at).collect();
    for at in events {
        if set_partstat(&mut ical.components[at], me, answer, !server_schedules)? {
            stamp(&mut ical, at, now, me)?;
            any = true;
        }
    }
    Ok(any.then(|| ical.to_string()))
}
