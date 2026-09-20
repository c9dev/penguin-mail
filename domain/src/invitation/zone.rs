//! Turning a `TZID` into a real time zone.
//!
//! Google writes IANA names, which `chrono-tz` knows. Outlook writes
//! Windows names such as `W. Europe Standard Time`, which it does not, so
//! those go through a table first. Anything left over falls back to the
//! offset the invitation's own `VTIMEZONE` block carries, and then to this
//! computer's zone, which at least puts the event on the right day.

use std::collections::HashMap;

use chrono::{NaiveDate, TimeZone};
use icalendar::{CalendarComponent, CalendarDateTime, Component};

use crate::EpochMillis;

use super::{at_offset, plain_instant};

/// The `VTIMEZONE` blocks of one invitation: a `TZID` to the offset its
/// standard time keeps, in seconds east of UTC.
pub(crate) struct Zones {
    offsets: HashMap<String, i32>,
}

impl Zones {
    pub(crate) fn read(calendar: &icalendar::Calendar) -> Zones {
        let mut offsets = HashMap::new();
        for component in &calendar.components {
            let CalendarComponent::Other(block) = component else {
                continue;
            };
            if !block.component_kind().eq_ignore_ascii_case("VTIMEZONE") {
                continue;
            }
            let Some(tzid) = block.property_value("TZID") else {
                continue;
            };
            // STANDARD comes first when a zone has both, because reading a
            // summer time as winter time is an hour out and reading it the
            // other way puts an autumn meeting an hour early.
            let offset = rule_offset(block, "STANDARD").or_else(|| rule_offset(block, "DAYLIGHT"));
            if let Some(offset) = offset {
                offsets.insert(tzid.trim().to_string(), offset);
            }
        }
        Zones { offsets }
    }

    /// The instant a start or end names.
    pub(crate) fn instant(&self, when: &CalendarDateTime) -> Option<EpochMillis> {
        let CalendarDateTime::WithTimezone { date_time, tzid } = when else {
            return plain_instant(when);
        };
        if let Some(zone) = named(tzid) {
            return zone
                .from_local_datetime(date_time)
                .earliest()
                .map(|at| at.timestamp_millis());
        }
        match self.offsets.get(tzid.trim()) {
            Some(offset) => at_offset(*date_time, *offset),
            None => chrono::Local
                .from_local_datetime(date_time)
                .earliest()
                .map(|at| at.timestamp_millis()),
        }
    }
}

/// The offset a `VTIMEZONE`'s `STANDARD` or `DAYLIGHT` rule switches to.
fn rule_offset<C: Component>(block: &C, kind: &str) -> Option<i32> {
    let rule = block
        .components()
        .iter()
        .find(|c| c.component_kind().eq_ignore_ascii_case(kind))?;
    utc_offset(rule.property_value("TZOFFSETTO")?)
}

/// Seconds east of UTC for an iCalendar offset such as `+0100` or `-053000`.
fn utc_offset(text: &str) -> Option<i32> {
    let text = text.trim();
    let (sign, digits) = match text.as_bytes().first()? {
        b'+' => (1, &text[1..]),
        b'-' => (-1, &text[1..]),
        _ => (1, text),
    };
    if digits.len() < 4 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let hours: i32 = digits[..2].parse().ok()?;
    let minutes: i32 = digits[2..4].parse().ok()?;
    let seconds: i32 = if digits.len() >= 6 {
        digits[4..6].parse().ok()?
    } else {
        0
    };
    Some(sign * (hours * 3600 + minutes * 60 + seconds))
}

/// The zone a `TZID` names, by its IANA name or its Windows one.
fn named(tzid: &str) -> Option<chrono_tz::Tz> {
    let tzid = tzid.trim();
    if let Ok(zone) = tzid.parse::<chrono_tz::Tz>() {
        return Some(zone);
    }
    // Outlook prefixes a Windows name with the country it picked, as in
    // "(UTC+01:00) Amsterdam, Berlin". The name after the offset is the
    // one the table knows.
    let bare = tzid.rsplit(')').next().unwrap_or(tzid).trim();
    WINDOWS_ZONES
        .iter()
        .find(|(windows, _)| windows.eq_ignore_ascii_case(bare))
        .and_then(|(_, iana)| iana.parse().ok())
}

/// Local midnight at the start of `date`, for an all-day event.
pub(crate) fn local_midnight(date: NaiveDate) -> Option<EpochMillis> {
    chrono::Local
        .from_local_datetime(&date.and_hms_opt(0, 0, 0)?)
        .earliest()
        .map(|at| at.timestamp_millis())
}

/// Windows time zone names to IANA ones, from the CLDR mapping. Only the
/// zones Outlook sends most are here; an unlisted one falls back to the
/// `VTIMEZONE` offset, which is right outside the daylight-saving weeks.
const WINDOWS_ZONES: [(&str, &str); 40] = [
    ("Dateline Standard Time", "Etc/GMT+12"),
    ("Hawaiian Standard Time", "Pacific/Honolulu"),
    ("Alaskan Standard Time", "America/Anchorage"),
    ("Pacific Standard Time", "America/Los_Angeles"),
    ("Pacific Standard Time (Mexico)", "America/Tijuana"),
    ("US Mountain Standard Time", "America/Phoenix"),
    ("Mountain Standard Time", "America/Denver"),
    ("Central Standard Time", "America/Chicago"),
    ("Central Standard Time (Mexico)", "America/Mexico_City"),
    ("Canada Central Standard Time", "America/Regina"),
    ("Eastern Standard Time", "America/New_York"),
    ("US Eastern Standard Time", "America/Indianapolis"),
    ("Atlantic Standard Time", "America/Halifax"),
    ("SA Pacific Standard Time", "America/Bogota"),
    ("SA Eastern Standard Time", "America/Cayenne"),
    ("E. South America Standard Time", "America/Sao_Paulo"),
    ("Argentina Standard Time", "America/Buenos_Aires"),
    ("Greenwich Standard Time", "Atlantic/Reykjavik"),
    ("UTC", "Etc/UTC"),
    ("GMT Standard Time", "Europe/London"),
    ("W. Europe Standard Time", "Europe/Berlin"),
    ("Central Europe Standard Time", "Europe/Budapest"),
    ("Romance Standard Time", "Europe/Paris"),
    ("Central European Standard Time", "Europe/Warsaw"),
    ("W. Central Africa Standard Time", "Africa/Lagos"),
    ("GTB Standard Time", "Europe/Bucharest"),
    ("South Africa Standard Time", "Africa/Johannesburg"),
    ("FLE Standard Time", "Europe/Kiev"),
    ("Israel Standard Time", "Asia/Jerusalem"),
    ("E. Africa Standard Time", "Africa/Nairobi"),
    ("Russian Standard Time", "Europe/Moscow"),
    ("Arabian Standard Time", "Asia/Dubai"),
    ("India Standard Time", "Asia/Calcutta"),
    ("SE Asia Standard Time", "Asia/Bangkok"),
    ("China Standard Time", "Asia/Shanghai"),
    ("Singapore Standard Time", "Asia/Singapore"),
    ("W. Australia Standard Time", "Australia/Perth"),
    ("Tokyo Standard Time", "Asia/Tokyo"),
    ("AUS Eastern Standard Time", "Australia/Sydney"),
    ("New Zealand Standard Time", "Pacific/Auckland"),
];
