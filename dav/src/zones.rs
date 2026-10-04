//! A VTIMEZONE's IANA zone. calcard knows a TZID that is an IANA name,
//! `X-LIC-LOCATION`, and Microsoft's `X-MICROSOFT-CDO-TZID`. A name it
//! does not know, such as Outlook's "W. Europe Standard Time", is matched
//! by the offsets its STANDARD and DAYLIGHT blocks move to, against every
//! IANA zone's offsets in January and July, preferring a zone whose city
//! the TZID names and then a zone in Europe, the Americas or Asia.

use std::collections::HashMap;

use calcard::common::timezone::Tz;
use calcard::icalendar::{ICalendar, ICalendarComponent, ICalendarComponentType, ICalendarProperty};
use chrono::{Offset, TimeZone, Utc};

pub(crate) struct Zones {
    named: HashMap<String, chrono_tz::Tz>,
}

impl Zones {
    pub(crate) fn of(ical: &ICalendar) -> Zones {
        let mut named = HashMap::new();
        for zone in ical.timezones() {
            let Some(tzid) = zone.property(&ICalendarProperty::Tzid).and_then(|e| e.values.first()).and_then(|v| v.as_text()) else {
                continue;
            };
            let found = match zone.timezone() {
                Some((_, Tz::Tz(tz))) => Some(tz),
                _ => by_offsets(ical, zone, tzid),
            };
            if let Some(tz) = found {
                named.insert(tzid.to_string(), tz);
            }
        }
        Zones { named }
    }

    /// The zone a TZID parameter names: one of this file's VTIMEZONEs, or
    /// an IANA name the file did not define.
    pub(crate) fn resolve(&self, tzid: &str) -> Option<chrono_tz::Tz> {
        self.named.get(tzid).copied().or_else(|| tzid.parse().ok())
    }
}

fn by_offsets(ical: &ICalendar, zone: &ICalendarComponent, tzid: &str) -> Option<chrono_tz::Tz> {
    let offset_of = |kind: ICalendarComponentType| {
        zone.component_ids
            .iter()
            .filter_map(|id| ical.components.get(*id as usize))
            .find(|c| c.component_type == kind)
            .and_then(|c| c.property(&ICalendarProperty::Tzoffsetto))
            .and_then(|entry| {
                let mut line = String::new();
                entry.write_to(&mut line).ok()?;
                seconds(line.rsplit(':').next()?.trim())
            })
    };
    let standard = offset_of(ICalendarComponentType::Standard)?;
    let daylight = offset_of(ICalendarComponentType::Daylight).unwrap_or(standard);
    let january = Utc.with_ymd_and_hms(2026, 1, 15, 12, 0, 0).single()?;
    let july = Utc.with_ymd_and_hms(2026, 7, 15, 12, 0, 0).single()?;
    let offsets = |tz: chrono_tz::Tz| {
        let at = |when: chrono::DateTime<Utc>| when.with_timezone(&tz).offset().fix().local_minus_utc();
        let (jan, jul) = (at(january), at(july));
        (jan.min(jul), jan.max(jul))
    };
    let wanted = (standard.min(daylight), standard.max(daylight));
    let matching: Vec<chrono_tz::Tz> = chrono_tz::TZ_VARIANTS.iter().copied().filter(|tz| offsets(*tz) == wanted).collect();
    let words = tzid.to_ascii_lowercase();
    matching
        .iter()
        .copied()
        .find(|tz| tz.name().rsplit('/').next().is_some_and(|city| words.contains(&city.to_ascii_lowercase().replace('_', " "))))
        .or_else(|| {
            ["Europe/", "America/", "Asia/", "Australia/", "Pacific/", "Africa/"]
                .iter()
                .find_map(|region| matching.iter().copied().find(|tz| tz.name().starts_with(region)))
        })
}

/// `+0100` or `-0530` in seconds east of UTC.
fn seconds(text: &str) -> Option<i32> {
    let (sign, digits) = match text.as_bytes().first()? {
        b'+' => (1, &text[1..]),
        b'-' => (-1, &text[1..]),
        _ => (1, text),
    };
    let hours: i32 = digits.get(0..2)?.parse().ok()?;
    let minutes: i32 = digits.get(2..4)?.parse().ok()?;
    Some(sign * (hours * 3600 + minutes * 60))
}
