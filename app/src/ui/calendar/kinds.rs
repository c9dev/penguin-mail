//! How the calendar draws and names the entries Google keeps to say where
//! the person is: out of office as a hatched block, focus time with a
//! target, a birthday as an all-day chip with a cake, and a working
//! location as a word under the day's heading rather than a block. No
//! widgets here, so each rule has a test.

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use mailrs_domain::EpochMillis;
use mailrs_domain::calendar::{Declines, Kind, Occurrence, Workplace};
use mailrs_domain::translate::{fill, gettext};

/// How a block looks for its kind of entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Look {
    Plain,
    /// Out of office: stripes across its hours.
    Away,
    /// Focus time: a target before the title.
    Focus,
    /// A birthday: a cake before the title.
    Birthday,
}

impl Look {
    /// The class `app/data/style.css` draws the look with.
    pub fn css_class(self) -> Option<&'static str> {
        match self {
            Look::Plain => None,
            Look::Away => Some("away"),
            Look::Focus => Some("focus"),
            Look::Birthday => Some("birthday"),
        }
    }

    /// The icon before the title.
    pub fn icon(self) -> Option<&'static str> {
        match self {
            Look::Focus => Some("penguin-mail-focus-symbolic"),
            Look::Birthday => Some("penguin-mail-cake-symbolic"),
            Look::Plain | Look::Away => None,
        }
    }
}

pub fn look(kind: &Kind) -> Look {
    match kind {
        Kind::OutOfOffice(_) => Look::Away,
        Kind::Focus(_) => Look::Focus,
        Kind::Birthday => Look::Birthday,
        Kind::Event | Kind::WorkingLocation(_) => Look::Plain,
    }
}

/// "Out of office", "Focus time", "Working location" or "Birthday", for
/// the accessible name and the popover; `None` for an ordinary event.
pub fn kind_words(kind: &Kind) -> Option<String> {
    match kind {
        Kind::Event => None,
        Kind::OutOfOffice(_) => Some(gettext("Out of office")),
        Kind::Focus(_) => Some(gettext("Focus time")),
        Kind::WorkingLocation(_) => Some(gettext("Working location")),
        Kind::Birthday => Some(gettext("Birthday")),
    }
}

/// "Home", "Office", or the building or place the person named.
pub fn place_words(place: &Workplace) -> String {
    match place {
        Workplace::Home => gettext("Home"),
        Workplace::Office(name) if name.trim().is_empty() => gettext("Office"),
        Workplace::Office(name) => name.clone(),
        Workplace::Elsewhere(name) if name.trim().is_empty() => gettext("Elsewhere"),
        Workplace::Elsewhere(name) => name.clone(),
    }
}

/// Whether the entry is drawn as a block. A working location is a word
/// under its day's heading instead.
pub fn on_grid(kind: &Kind) -> bool {
    !matches!(kind, Kind::WorkingLocation(_))
}

/// The working location of each of `days`, in order: the place words of
/// each working-location entry that touches the day, joined with a comma
/// when the person split the day, or `None` for a day with none. An
/// all-day entry counts on its own UTC dates, as the all-day row places
/// it; a timed one on the local days it touches in `zone`.
pub fn workplaces<Z: TimeZone>(occurrences: &[Occurrence], days: &[NaiveDate], zone: &Z) -> Vec<Option<String>> {
    let mut places: Vec<&Occurrence> = occurrences
        .iter()
        .filter(|o| matches!(o.event.kind, Kind::WorkingLocation(_)))
        .collect();
    places.sort_by_key(|o| o.start);
    days.iter()
        .map(|&day| {
            let mut names: Vec<String> = Vec::new();
            for o in places.iter().filter(|o| touches(o, day, zone)) {
                if let Kind::WorkingLocation(place) = &o.event.kind {
                    let name = place_words(place);
                    if !names.contains(&name) {
                        names.push(name);
                    }
                }
            }
            (!names.is_empty()).then(|| names.join(", "))
        })
        .collect()
}

/// Whether `o` covers part of `day`: an all-day occurrence by its UTC
/// dates, a timed one by `zone`'s wall clock.
fn touches<Z: TimeZone>(o: &Occurrence, day: NaiveDate, zone: &Z) -> bool {
    let local = |at: EpochMillis| DateTime::<Utc>::from_timestamp_millis(at).map(|t| t.with_timezone(zone).date_naive());
    let utc = |at: EpochMillis| DateTime::<Utc>::from_timestamp_millis(at).map(|t| t.date_naive());
    let read = |at: EpochMillis| if o.event.all_day { utc(at) } else { local(at) };
    // The end is exclusive, so an entry ending at midnight stays off the
    // day after.
    match (read(o.start), read((o.end - 1).max(o.start))) {
        (Some(first), Some(last)) => first <= day && day <= last,
        _ => false,
    }
}

/// What a screen reader says for a day's heading with a working location
/// under it: "Monday 21 September, working from Home".
pub fn heading_words(date: &str, place: Option<&str>) -> String {
    match place {
        Some(place) => fill(&gettext("{date}, working from {place}"), &[("date", date), ("place", place)]),
        None => date.to_string(),
    }
}

/// The popover's line about the entry's kind: what an out-of-office or
/// focus-time entry declines, or that Google's own apps make a working
/// location or a birthday, so it shows here read-only. `None` for an
/// ordinary event.
pub fn popover_words(kind: &Kind) -> Option<String> {
    let name = kind_words(kind)?;
    let declines = |meetings: Declines| match meetings {
        Declines::Nothing => name.clone(),
        Declines::New => fill(&gettext("{kind}. Declines new meetings."), &[("kind", &name)]),
        Declines::All => fill(&gettext("{kind}. Declines new and existing meetings."), &[("kind", &name)]),
    };
    Some(match kind {
        Kind::OutOfOffice(decline) | Kind::Focus(decline) => declines(decline.meetings),
        Kind::Birthday => gettext("Birthday. Google Calendar makes it from your contacts, so you can only change it there."),
        Kind::WorkingLocation(_) => gettext("Working location. Google Calendar sets it, so you can only change it there."),
        Kind::Event => return None,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use mailrs_domain::calendar::{Decline, Event};

    use super::*;

    const HOUR: EpochMillis = 3_600_000;

    fn day(d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 9, d).unwrap()
    }

    fn midnight(d: u32) -> EpochMillis {
        day(d).and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp_millis()
    }

    fn entry(kind: Kind, start: EpochMillis, end: EpochMillis, all_day: bool) -> Occurrence {
        let event = Event { kind, start, end, all_day, zone: "UTC".into(), ..Event::default() };
        Occurrence { account_id: 1, event: Arc::new(event), start, end }
    }

    fn away(meetings: Declines) -> Kind {
        Kind::OutOfOffice(Decline { meetings, message: String::new() })
    }

    #[test]
    fn each_kind_has_its_own_look() {
        assert_eq!(look(&Kind::Event), Look::Plain);
        assert_eq!(look(&away(Declines::All)), Look::Away);
        assert_eq!(look(&Kind::Focus(Decline::default())), Look::Focus);
        assert_eq!(look(&Kind::Birthday), Look::Birthday);
    }

    #[test]
    fn out_of_office_is_striped_and_the_other_two_carry_an_icon() {
        assert_eq!(Look::Away.css_class(), Some("away"));
        assert_eq!(Look::Away.icon(), None);
        assert_eq!(Look::Focus.icon(), Some("penguin-mail-focus-symbolic"));
        assert_eq!(Look::Birthday.icon(), Some("penguin-mail-cake-symbolic"));
        assert_eq!(Look::Plain.css_class(), None);
    }

    #[test]
    fn each_kind_names_itself_and_an_ordinary_event_says_nothing() {
        assert_eq!(kind_words(&away(Declines::Nothing)).as_deref(), Some("Out of office"));
        assert_eq!(kind_words(&Kind::Focus(Decline::default())).as_deref(), Some("Focus time"));
        assert_eq!(kind_words(&Kind::WorkingLocation(Workplace::Home)).as_deref(), Some("Working location"));
        assert_eq!(kind_words(&Kind::Birthday).as_deref(), Some("Birthday"));
        assert_eq!(kind_words(&Kind::Event), None);
    }

    #[test]
    fn a_place_reads_home_the_building_or_the_named_place() {
        assert_eq!(place_words(&Workplace::Home), "Home");
        assert_eq!(place_words(&Workplace::Office("Lisbon HQ".into())), "Lisbon HQ");
        assert_eq!(place_words(&Workplace::Office(String::new())), "Office");
        assert_eq!(place_words(&Workplace::Elsewhere("Café Tati".into())), "Café Tati");
    }

    #[test]
    fn only_a_working_location_stays_off_the_grid() {
        assert!(!on_grid(&Kind::WorkingLocation(Workplace::Home)));
        assert!(on_grid(&away(Declines::All)));
        assert!(on_grid(&Kind::Birthday));
        assert!(on_grid(&Kind::Event));
    }

    #[test]
    fn an_all_day_working_location_labels_its_own_day() {
        let home = entry(Kind::WorkingLocation(Workplace::Home), midnight(28), midnight(29), true);
        let days = [day(27), day(28), day(29)];
        assert_eq!(workplaces(&[home], &days, &Utc), vec![None, Some("Home".into()), None]);
    }

    #[test]
    fn a_split_day_names_both_places_in_order() {
        let office = Kind::WorkingLocation(Workplace::Office("Lisbon HQ".into()));
        let morning = entry(Kind::WorkingLocation(Workplace::Home), midnight(28) + 8 * HOUR, midnight(28) + 12 * HOUR, false);
        let afternoon = entry(office, midnight(28) + 13 * HOUR, midnight(28) + 18 * HOUR, false);
        assert_eq!(workplaces(&[afternoon, morning], &[day(28)], &Utc), vec![Some("Home, Lisbon HQ".into())]);
    }

    #[test]
    fn the_same_place_twice_in_a_day_is_named_once() {
        let a = entry(Kind::WorkingLocation(Workplace::Home), midnight(28) + 8 * HOUR, midnight(28) + 12 * HOUR, false);
        let b = entry(Kind::WorkingLocation(Workplace::Home), midnight(28) + 13 * HOUR, midnight(28) + 18 * HOUR, false);
        assert_eq!(workplaces(&[a, b], &[day(28)], &Utc), vec![Some("Home".into())]);
    }

    #[test]
    fn other_entries_name_no_workplace() {
        let meeting = entry(Kind::Event, midnight(28) + 9 * HOUR, midnight(28) + 10 * HOUR, false);
        assert_eq!(workplaces(&[meeting], &[day(28)], &Utc), vec![None]);
    }

    #[test]
    fn a_heading_with_a_place_says_where_the_person_works() {
        assert_eq!(heading_words("Monday 28 September", Some("Home")), "Monday 28 September, working from Home");
        assert_eq!(heading_words("Monday 28 September", None), "Monday 28 September");
    }

    #[test]
    fn the_popover_says_what_out_of_office_declines() {
        assert_eq!(
            popover_words(&away(Declines::All)).as_deref(),
            Some("Out of office. Declines new and existing meetings.")
        );
        assert_eq!(popover_words(&away(Declines::New)).as_deref(), Some("Out of office. Declines new meetings."));
        assert_eq!(popover_words(&away(Declines::Nothing)).as_deref(), Some("Out of office"));
    }

    #[test]
    fn the_popover_says_a_birthday_and_a_working_location_are_read_only() {
        assert_eq!(
            popover_words(&Kind::Birthday).as_deref(),
            Some("Birthday. Google Calendar makes it from your contacts, so you can only change it there.")
        );
        assert_eq!(
            popover_words(&Kind::WorkingLocation(Workplace::Home)).as_deref(),
            Some("Working location. Google Calendar sets it, so you can only change it there.")
        );
        assert_eq!(popover_words(&Kind::Event), None);
    }
}
