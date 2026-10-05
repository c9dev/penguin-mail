//! Outlook names time zones the Windows way ("GMT Standard Time"); the
//! copy expands a series in an IANA zone ("Europe/London"). The table is
//! CLDR's `windowsZones` for the zones Outlook offers, one IANA name per
//! row. A name that is not in it reads as UTC rather than as a guess.

use chrono_tz::Tz;

/// A Windows zone name and the IANA zone that has its rules.
const ZONES: &[(&str, &str)] = &[
    ("Dateline Standard Time", "Etc/GMT+12"),
    ("UTC-11", "Etc/GMT+11"),
    ("Aleutian Standard Time", "America/Adak"),
    ("Hawaiian Standard Time", "Pacific/Honolulu"),
    ("Marquesas Standard Time", "Pacific/Marquesas"),
    ("Alaskan Standard Time", "America/Anchorage"),
    ("UTC-09", "Etc/GMT+9"),
    ("Pacific Standard Time (Mexico)", "America/Tijuana"),
    ("UTC-08", "Etc/GMT+8"),
    ("Pacific Standard Time", "America/Los_Angeles"),
    ("US Mountain Standard Time", "America/Phoenix"),
    ("Mountain Standard Time (Mexico)", "America/Mazatlan"),
    ("Mountain Standard Time", "America/Denver"),
    ("Yukon Standard Time", "America/Whitehorse"),
    ("Central America Standard Time", "America/Guatemala"),
    ("Central Standard Time", "America/Chicago"),
    ("Easter Island Standard Time", "Pacific/Easter"),
    ("Central Standard Time (Mexico)", "America/Mexico_City"),
    ("Canada Central Standard Time", "America/Regina"),
    ("SA Pacific Standard Time", "America/Bogota"),
    ("Eastern Standard Time (Mexico)", "America/Cancun"),
    ("Eastern Standard Time", "America/New_York"),
    ("Haiti Standard Time", "America/Port-au-Prince"),
    ("Cuba Standard Time", "America/Havana"),
    ("US Eastern Standard Time", "America/Indiana/Indianapolis"),
    ("Turks And Caicos Standard Time", "America/Grand_Turk"),
    ("Paraguay Standard Time", "America/Asuncion"),
    ("Atlantic Standard Time", "America/Halifax"),
    ("Venezuela Standard Time", "America/Caracas"),
    ("Central Brazilian Standard Time", "America/Cuiaba"),
    ("SA Western Standard Time", "America/La_Paz"),
    ("Pacific SA Standard Time", "America/Santiago"),
    ("Newfoundland Standard Time", "America/St_Johns"),
    ("Tocantins Standard Time", "America/Araguaina"),
    ("E. South America Standard Time", "America/Sao_Paulo"),
    ("SA Eastern Standard Time", "America/Cayenne"),
    ("Argentina Standard Time", "America/Argentina/Buenos_Aires"),
    ("Greenland Standard Time", "America/Nuuk"),
    ("Montevideo Standard Time", "America/Montevideo"),
    ("Magallanes Standard Time", "America/Punta_Arenas"),
    ("Saint Pierre Standard Time", "America/Miquelon"),
    ("Bahia Standard Time", "America/Bahia"),
    ("UTC-02", "Etc/GMT+2"),
    ("Azores Standard Time", "Atlantic/Azores"),
    ("Cape Verde Standard Time", "Atlantic/Cape_Verde"),
    ("UTC", "Etc/UTC"),
    ("GMT Standard Time", "Europe/London"),
    ("Greenwich Standard Time", "Atlantic/Reykjavik"),
    ("Sao Tome Standard Time", "Africa/Sao_Tome"),
    ("Morocco Standard Time", "Africa/Casablanca"),
    ("W. Europe Standard Time", "Europe/Berlin"),
    ("Central Europe Standard Time", "Europe/Budapest"),
    ("Romance Standard Time", "Europe/Paris"),
    ("Central European Standard Time", "Europe/Warsaw"),
    ("W. Central Africa Standard Time", "Africa/Lagos"),
    ("GTB Standard Time", "Europe/Bucharest"),
    ("Middle East Standard Time", "Asia/Beirut"),
    ("Egypt Standard Time", "Africa/Cairo"),
    ("E. Europe Standard Time", "Europe/Chisinau"),
    ("Syria Standard Time", "Asia/Damascus"),
    ("West Bank Standard Time", "Asia/Hebron"),
    ("South Africa Standard Time", "Africa/Johannesburg"),
    ("FLE Standard Time", "Europe/Kyiv"),
    ("Israel Standard Time", "Asia/Jerusalem"),
    ("Kaliningrad Standard Time", "Europe/Kaliningrad"),
    ("Sudan Standard Time", "Africa/Khartoum"),
    ("Libya Standard Time", "Africa/Tripoli"),
    ("Namibia Standard Time", "Africa/Windhoek"),
    ("Jordan Standard Time", "Asia/Amman"),
    ("Arabic Standard Time", "Asia/Baghdad"),
    ("Turkey Standard Time", "Europe/Istanbul"),
    ("Arab Standard Time", "Asia/Riyadh"),
    ("Belarus Standard Time", "Europe/Minsk"),
    ("Russian Standard Time", "Europe/Moscow"),
    ("E. Africa Standard Time", "Africa/Nairobi"),
    ("Iran Standard Time", "Asia/Tehran"),
    ("Arabian Standard Time", "Asia/Dubai"),
    ("Astrakhan Standard Time", "Europe/Astrakhan"),
    ("Azerbaijan Standard Time", "Asia/Baku"),
    ("Russia Time Zone 3", "Europe/Samara"),
    ("Mauritius Standard Time", "Indian/Mauritius"),
    ("Saratov Standard Time", "Europe/Saratov"),
    ("Georgian Standard Time", "Asia/Tbilisi"),
    ("Caucasus Standard Time", "Asia/Yerevan"),
    ("Afghanistan Standard Time", "Asia/Kabul"),
    ("West Asia Standard Time", "Asia/Tashkent"),
    ("Ekaterinburg Standard Time", "Asia/Yekaterinburg"),
    ("Pakistan Standard Time", "Asia/Karachi"),
    ("India Standard Time", "Asia/Kolkata"),
    ("Sri Lanka Standard Time", "Asia/Colombo"),
    ("Nepal Standard Time", "Asia/Kathmandu"),
    ("Central Asia Standard Time", "Asia/Almaty"),
    ("Bangladesh Standard Time", "Asia/Dhaka"),
    ("Omsk Standard Time", "Asia/Omsk"),
    ("Myanmar Standard Time", "Asia/Yangon"),
    ("SE Asia Standard Time", "Asia/Bangkok"),
    ("Altai Standard Time", "Asia/Barnaul"),
    ("W. Mongolia Standard Time", "Asia/Hovd"),
    ("North Asia Standard Time", "Asia/Krasnoyarsk"),
    ("N. Central Asia Standard Time", "Asia/Novosibirsk"),
    ("Tomsk Standard Time", "Asia/Tomsk"),
    ("China Standard Time", "Asia/Shanghai"),
    ("North Asia East Standard Time", "Asia/Irkutsk"),
    ("Singapore Standard Time", "Asia/Singapore"),
    ("W. Australia Standard Time", "Australia/Perth"),
    ("Taipei Standard Time", "Asia/Taipei"),
    ("Ulaanbaatar Standard Time", "Asia/Ulaanbaatar"),
    ("Aus Central W. Standard Time", "Australia/Eucla"),
    ("Transbaikal Standard Time", "Asia/Chita"),
    ("Tokyo Standard Time", "Asia/Tokyo"),
    ("North Korea Standard Time", "Asia/Pyongyang"),
    ("Korea Standard Time", "Asia/Seoul"),
    ("Yakutsk Standard Time", "Asia/Yakutsk"),
    ("Cen. Australia Standard Time", "Australia/Adelaide"),
    ("AUS Central Standard Time", "Australia/Darwin"),
    ("E. Australia Standard Time", "Australia/Brisbane"),
    ("AUS Eastern Standard Time", "Australia/Sydney"),
    ("West Pacific Standard Time", "Pacific/Port_Moresby"),
    ("Tasmania Standard Time", "Australia/Hobart"),
    ("Vladivostok Standard Time", "Asia/Vladivostok"),
    ("Lord Howe Standard Time", "Australia/Lord_Howe"),
    ("Bougainville Standard Time", "Pacific/Bougainville"),
    ("Russia Time Zone 10", "Asia/Srednekolymsk"),
    ("Magadan Standard Time", "Asia/Magadan"),
    ("Norfolk Standard Time", "Pacific/Norfolk"),
    ("Sakhalin Standard Time", "Asia/Sakhalin"),
    ("Central Pacific Standard Time", "Pacific/Guadalcanal"),
    ("Russia Time Zone 11", "Asia/Kamchatka"),
    ("New Zealand Standard Time", "Pacific/Auckland"),
    ("UTC+12", "Etc/GMT-12"),
    ("Fiji Standard Time", "Pacific/Fiji"),
    ("Chatham Islands Standard Time", "Pacific/Chatham"),
    ("UTC+13", "Etc/GMT-13"),
    ("Tonga Standard Time", "Pacific/Tongatapu"),
    ("Samoa Standard Time", "Pacific/Apia"),
    ("Line Islands Standard Time", "Pacific/Kiritimati"),
];

/// IANA zones that share the rules of a row above and are common enough
/// to be the zone of a new event: the Windows name a write sends for them.
const ALSO: &[(&str, &str)] = &[
    ("Europe/Lisbon", "GMT Standard Time"),
    ("Europe/Dublin", "GMT Standard Time"),
    ("Atlantic/Canary", "GMT Standard Time"),
    ("Europe/Madrid", "Romance Standard Time"),
    ("Europe/Brussels", "Romance Standard Time"),
    ("Europe/Copenhagen", "Romance Standard Time"),
    ("Europe/Amsterdam", "W. Europe Standard Time"),
    ("Europe/Rome", "W. Europe Standard Time"),
    ("Europe/Vienna", "W. Europe Standard Time"),
    ("Europe/Zurich", "W. Europe Standard Time"),
    ("Europe/Stockholm", "W. Europe Standard Time"),
    ("Europe/Oslo", "W. Europe Standard Time"),
    ("Europe/Prague", "Central Europe Standard Time"),
    ("Europe/Belgrade", "Central Europe Standard Time"),
    ("Europe/Athens", "GTB Standard Time"),
    ("Europe/Helsinki", "FLE Standard Time"),
    ("Europe/Kiev", "FLE Standard Time"),
    ("America/Toronto", "Eastern Standard Time"),
    ("America/Vancouver", "Pacific Standard Time"),
    ("America/Edmonton", "Mountain Standard Time"),
    ("America/Winnipeg", "Central Standard Time"),
    ("Asia/Calcutta", "India Standard Time"),
    ("Asia/Hong_Kong", "China Standard Time"),
    ("Australia/Melbourne", "AUS Eastern Standard Time"),
    ("America/Argentina/Buenos_Aires", "Argentina Standard Time"),
    ("UTC", "UTC"),
];

/// The IANA zone for a zone name Outlook sent: a Windows name from the
/// table, or an IANA name as it stands. `None` for any other name.
pub(super) fn zone_named(name: &str) -> Option<Tz> {
    let name = name.trim();
    if let Some((_, iana)) = ZONES.iter().find(|(windows, _)| windows.eq_ignore_ascii_case(name)) {
        return iana.parse().ok();
    }
    name.parse().ok()
}

/// The Windows name Graph takes for the IANA zone `zone`, or `None` for a
/// zone the table does not know, which a write then sends as UTC.
pub(super) fn windows_name(zone: Tz) -> Option<&'static str> {
    let iana = zone.name();
    if iana == "UTC" || iana == "Etc/UTC" {
        return Some("UTC");
    }
    let exact = ZONES.iter().find(|(_, i)| *i == iana).map(|(w, _)| *w);
    exact.or_else(|| ALSO.iter().find(|(i, _)| *i == iana).map(|(_, w)| *w))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_row_names_a_zone_the_database_has() {
        for (windows, iana) in ZONES {
            assert!(iana.parse::<Tz>().is_ok(), "{windows} -> {iana}");
        }
        for (iana, windows) in ALSO {
            assert!(iana.parse::<Tz>().is_ok(), "{iana}");
            assert!(ZONES.iter().any(|(w, _)| w == windows) || *windows == "UTC", "{windows}");
        }
    }

    #[test]
    fn a_windows_name_reads_as_its_iana_zone_and_back() {
        assert_eq!(zone_named("GMT Standard Time"), Some(chrono_tz::Europe::London));
        assert_eq!(zone_named("Europe/Lisbon"), Some(chrono_tz::Europe::Lisbon));
        assert_eq!(zone_named("Not A Zone"), None);
        assert_eq!(windows_name(chrono_tz::Europe::Lisbon), Some("GMT Standard Time"));
        assert_eq!(windows_name(chrono_tz::America::New_York), Some("Eastern Standard Time"));
        assert_eq!(windows_name(chrono_tz::Africa::Accra), None);
    }

    #[test]
    fn lisbon_and_the_zone_it_maps_to_agree_across_the_october_clock_change() {
        use chrono::{TimeZone, Utc};
        let after = Utc.with_ymd_and_hms(2026, 10, 26, 9, 0, 0).unwrap();
        let (lisbon, mapped) = (after.with_timezone(&chrono_tz::Europe::Lisbon), after.with_timezone(&zone_named("GMT Standard Time").unwrap()));
        assert_eq!(lisbon.naive_local(), mapped.naive_local());
    }
}
