//! The next event, for the card at the foot of the mail sidebar: which
//! one shows, and in which words. The card shows an event from three
//! hours before it starts. One under way shows until it ends, unless
//! another starts within a quarter of an hour, which then comes first,
//! since that is where the person has to be next.

use std::collections::HashMap;
use std::fmt::Display;

use chrono::TimeZone;
use mailrs_domain::calendar::{Occurrence, Status};
use mailrs_domain::invitation::Answer;
use mailrs_domain::translate::{fill, gettext};
use mailrs_domain::{AccountId, EpochMillis};

use super::words::clock_words;

/// How far ahead the card looks.
pub const AHEAD: EpochMillis = 3 * 60 * 60 * 1_000;
/// How close the next event must be to show over one under way.
pub const SOON: EpochMillis = 15 * 60 * 1_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NextUp {
    /// Starts within [`AHEAD`].
    Soon(Occurrence),
    /// Under way now.
    Now(Occurrence),
}

impl NextUp {
    pub fn occurrence(&self) -> &Occurrence {
        match self {
            NextUp::Soon(o) | NextUp::Now(o) => o,
        }
    }
}

/// The event the card shows at `now`, from today's timed events the
/// person has not declined and that are not cancelled. `day_ends` is the
/// local midnight after `now`. A free (`busy: false`) event still shows:
/// only an all-day, declined or cancelled one, or a working location,
/// stays out.
pub fn next_up(
    occurrences: &[Occurrence],
    now: EpochMillis,
    day_ends: EpochMillis,
) -> Option<NextUp> {
    let counts = |o: &&Occurrence| {
        !o.event.all_day
            && super::kinds::on_grid(&o.event.kind)
            && o.event.status != Status::Cancelled
            && o.event.my_answer != Some(Answer::No)
    };
    let soon = occurrences
        .iter()
        .filter(counts)
        .filter(|o| o.start > now && o.start - now <= AHEAD && o.start < day_ends)
        .min_by_key(|o| (o.start, o.event.title.clone()));
    let under_way = occurrences
        .iter()
        .filter(counts)
        .filter(|o| o.start <= now && o.end > now)
        .min_by_key(|o| (o.end, o.event.title.clone()));
    match (under_way, soon) {
        (Some(_), Some(next)) if next.start - now <= SOON => Some(NextUp::Soon(next.clone())),
        (Some(current), _) => Some(NextUp::Now(current.clone())),
        (None, Some(next)) => Some(NextUp::Soon(next.clone())),
        (None, None) => None,
    }
}

/// The card's two lines: "Next · in 20 min" dimmed, then "Sprint planning
/// · 10:00" bold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Words {
    pub when: String,
    pub what: String,
}

pub fn words<Tz: TimeZone>(next: &NextUp, now: EpochMillis, zone: &Tz) -> Words
where
    Tz::Offset: Display,
{
    let o = next.occurrence();
    let title = if o.event.title.trim().is_empty() {
        gettext("Untitled event")
    } else {
        o.event.title.clone()
    };
    let what = fill(
        &gettext("{title} · {start}"),
        &[("title", &title), ("start", &clock_words(o.start, zone))],
    );
    let when = match next {
        NextUp::Now(o) => fill(&gettext("Now · until {end}"), &[("end", &clock_words(o.end, zone))]),
        NextUp::Soon(o) => {
            // Rounded up, so the card never says "in 0 min" before the
            // start.
            let minutes = (o.start - now + 59_999) / 60_000;
            let (hours, minutes) = (minutes / 60, minutes % 60);
            match (hours, minutes) {
                (0, m) => fill(&gettext("Next · in {minutes} min"), &[("minutes", &m.to_string())]),
                (h, 0) => fill(&gettext("Next · in {hours} h"), &[("hours", &h.to_string())]),
                (h, m) => fill(
                    &gettext("Next · in {hours} h {minutes} min"),
                    &[("hours", &h.to_string()), ("minutes", &m.to_string())],
                ),
            }
        }
    };
    Words { when, what }
}

/// What the card says out loud: both lines.
pub fn spoken(words: &Words) -> String {
    fill(&gettext("{when}: {what}"), &[("when", &words.when), ("what", &words.what)])
}

/// The event's own colour, else its calendar's.
pub fn colour(o: &Occurrence, calendars: &HashMap<(AccountId, String), String>) -> String {
    o.event
        .color
        .clone()
        .or_else(|| calendars.get(&(o.account_id, o.event.calendar.clone())).cloned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use chrono::FixedOffset;
    use mailrs_domain::calendar::Event;

    const M: EpochMillis = 60_000;
    /// Wednesday 23 September 2026, 08:00 UTC.
    const NOW: EpochMillis = 1_790_150_400_000;
    /// The midnight after it.
    const DAY_ENDS: EpochMillis = 1_790_208_000_000;

    fn utc() -> FixedOffset {
        FixedOffset::east_opt(0).expect("a valid offset")
    }

    fn at(title: &str, start: EpochMillis, minutes: i64) -> Occurrence {
        Occurrence {
            account_id: 1,
            event: Arc::new(Event {
                calendar: "primary".into(),
                id: title.to_lowercase(),
                title: title.into(),
                busy: true,
                ..Event::default()
            }),
            start,
            end: start + minutes * M,
        }
    }

    fn title(found: &Option<NextUp>) -> Option<&str> {
        found.as_ref().map(|n| n.occurrence().event.title.as_str())
    }

    #[test]
    fn an_event_within_three_hours_shows_as_next() {
        let found = next_up(&[at("Sprint planning", NOW + 20 * M, 90)], NOW, DAY_ENDS);
        assert_eq!(title(&found), Some("Sprint planning"));
        let said = words(&found.expect("an event"), NOW, &utc());
        assert_eq!(said.when, "Next · in 20 min");
        assert_eq!(said.what, "Sprint planning · 08:20");
    }

    #[test]
    fn an_event_further_off_shows_no_card() {
        assert_eq!(next_up(&[at("Lunch", NOW + 181 * M, 60)], NOW, DAY_ENDS), None);
    }

    #[test]
    fn a_wait_past_an_hour_counts_in_hours() {
        let three = next_up(&[at("Lunch", NOW + 180 * M, 60)], NOW, DAY_ENDS).expect("an event");
        assert_eq!(words(&three, NOW, &utc()).when, "Next · in 3 h");
        let later = next_up(&[at("Lunch", NOW + 135 * M, 60)], NOW, DAY_ENDS).expect("an event");
        assert_eq!(words(&later, NOW, &utc()).when, "Next · in 2 h 15 min");
    }

    #[test]
    fn the_minute_rounds_up() {
        let found = next_up(&[at("Call", NOW + 19 * M + 1_000, 30)], NOW, DAY_ENDS).expect("an event");
        assert_eq!(words(&found, NOW, &utc()).when, "Next · in 20 min");
    }

    #[test]
    fn an_event_under_way_shows_until_it_ends() {
        let found = next_up(&[at("Stand-up", NOW - 10 * M, 30)], NOW, DAY_ENDS).expect("an event");
        assert!(matches!(found, NextUp::Now(_)));
        let said = words(&found, NOW, &utc());
        assert_eq!(said.when, "Now · until 08:20");
        assert_eq!(said.what, "Stand-up · 07:50");
    }

    #[test]
    fn the_next_event_wins_over_one_under_way_ten_minutes_before() {
        let workshop = at("Workshop", NOW - 60 * M, 120);
        let soon = next_up(&[workshop.clone(), at("Dentist", NOW + 10 * M, 60)], NOW, DAY_ENDS);
        assert_eq!(title(&soon), Some("Dentist"));
        let later = next_up(&[workshop, at("Dentist", NOW + 40 * M, 60)], NOW, DAY_ENDS);
        assert_eq!(title(&later), Some("Workshop"));
    }

    #[test]
    fn declined_all_day_and_tomorrow_stay_out() {
        // 23:00, with tomorrow's first event an hour and a half away.
        let late = DAY_ENDS - 60 * M;
        let mut declined = at("Retro", late + 20 * M, 30);
        Arc::make_mut(&mut declined.event).my_answer = Some(Answer::No);
        let mut all_day = at("Offsite", DAY_ENDS - 24 * 60 * M, 24 * 60);
        Arc::make_mut(&mut all_day.event).all_day = true;
        let tomorrow = at("Early", DAY_ENDS + 30 * M, 30);
        assert_eq!(next_up(&[declined, all_day, tomorrow], late, DAY_ENDS), None);
    }

    #[test]
    fn a_timed_working_location_is_not_the_next_event() {
        use mailrs_domain::calendar::{Kind, Workplace};
        let mut office = at("Office", NOW + 20 * M, 240);
        Arc::make_mut(&mut office.event).kind = Kind::WorkingLocation(Workplace::Office(String::new()));
        assert_eq!(next_up(&[office], NOW, DAY_ENDS), None);
    }

    #[test]
    fn a_cancelled_event_stays_out() {
        let mut cancelled = at("Retro", NOW + 20 * M, 30);
        Arc::make_mut(&mut cancelled.event).status = Status::Cancelled;
        assert_eq!(next_up(&[cancelled], NOW, DAY_ENDS), None);
    }

    #[test]
    fn a_free_event_still_shows() {
        let mut free = at("Focus time", NOW + 20 * M, 90);
        Arc::make_mut(&mut free.event).busy = false;
        assert_eq!(title(&next_up(&[free], NOW, DAY_ENDS)), Some("Focus time"));
    }
}
