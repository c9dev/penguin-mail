//! The hours around an invitation, for the strip on its card: five whole
//! hours with the event's start two hours in, kept inside the event's
//! local day, the account's other events as blocks, and one line saying
//! whether anything else is on at that hour. The card draws what
//! [`build`] answers; the window reads the calendar's copy for it.

use std::collections::HashMap;
use std::fmt::Display;

use chrono::{NaiveDateTime, TimeDelta, TimeZone, Timelike};
use mailrs_domain::EpochMillis;
use mailrs_domain::calendar::Occurrence;
use mailrs_domain::translate::{date_locale, fill, fill_plural, gettext};

use crate::ui::calendar::layout::MOST_LANES;

/// How many hours the strip shows.
pub const HOURS: i64 = 5;

/// The strip is a grid of quarter hours.
pub const COLUMNS: i32 = 20;

/// Where the strip starts and ends: the hour the event starts in, less
/// two, for five hours, moved to stay inside the event's local day.
pub fn window<Tz: TimeZone>(start: EpochMillis, zone: &Tz) -> Option<(EpochMillis, EpochMillis)> {
    let local = zone.timestamp_millis_opt(start).single()?.naive_local();
    let day = local.date().and_hms_opt(0, 0, 0)?;
    let day_end = day + TimeDelta::days(1);
    let span = TimeDelta::hours(HOURS);
    let mut from = local.date().and_hms_opt(local.hour(), 0, 0)? - TimeDelta::hours(2);
    if from < day {
        from = day;
    }
    if from + span > day_end {
        from = day_end - span;
    }
    Some((instant(zone, from)?, instant(zone, from + span)?))
}

/// A local wall time as an instant. A time the clock skips in spring
/// takes the hour after it.
fn instant<Tz: TimeZone>(zone: &Tz, at: NaiveDateTime) -> Option<EpochMillis> {
    zone.from_local_datetime(&at)
        .earliest()
        .or_else(|| {
            zone.from_local_datetime(&(at + TimeDelta::hours(1)))
                .earliest()
        })
        .map(|t| t.timestamp_millis())
}

/// The part of the day the strip is about, which names its heading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    Morning,
    Afternoon,
    Evening,
}

impl Part {
    pub fn heading(self) -> String {
        match self {
            Part::Morning => gettext("Your morning"),
            Part::Afternoon => gettext("Your afternoon"),
            Part::Evening => gettext("Your evening"),
        }
    }
}

/// Before noon is the morning, until six the afternoon, then the evening.
pub fn part<Tz: TimeZone>(start: EpochMillis, zone: &Tz) -> Part {
    let hour = zone
        .timestamp_millis_opt(start)
        .single()
        .map_or(12, |t| t.hour());
    match hour {
        0..=11 => Part::Morning,
        12..=17 => Part::Afternoon,
        _ => Part::Evening,
    }
}

/// Whether anything else is on while the event runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Free,
    Clashes(Vec<String>),
}

impl Verdict {
    /// "Nothing else at that hour", or what clashes. Two clashes name
    /// both; more name the first and count the rest, as the clash line
    /// does, since the point is that the hour is taken.
    pub fn words(&self) -> String {
        let titles = match self {
            Verdict::Free => return gettext("Nothing else at that hour"),
            Verdict::Clashes(titles) => titles,
        };
        match titles.as_slice() {
            [] => gettext("Nothing else at that hour"),
            [one] => fill(&gettext("Clashes with {event}"), &[("event", one)]),
            [one, two] => fill(
                &gettext("Clashes with {event} and {other}"),
                &[("event", one), ("other", two)],
            ),
            [one, rest @ ..] => fill_plural(
                "Clashes with {event} and {count} more",
                "Clashes with {event} and {count} more",
                rest.len(),
                &[("event", one), ("count", &rest.len().to_string())],
            ),
        }
    }

    /// The CSS class that colours the line: green when free, the warning
    /// colour on a clash.
    pub fn tone(&self) -> &'static str {
        match self {
            Verdict::Free => "free",
            Verdict::Clashes(_) => "clash",
        }
    }
}

/// One event on the strip, placed as fractions of the strip's width.
#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub title: String,
    pub from: f64,
    pub to: f64,
    /// The row it sits in, when events overlap.
    pub lane: usize,
    /// `#rrggbb`, or empty for the accent.
    pub colour: String,
    /// The invitation's own event, drawn dashed.
    pub this: bool,
}

/// What the card draws.
#[derive(Debug, Clone, PartialEq)]
pub struct Strip {
    pub heading: String,
    /// Each whole hour after the strip's first edge, at its place along
    /// the strip, with the hour in words ("14:00").
    pub hours: Vec<(f64, String)>,
    pub blocks: Vec<Block>,
    pub lanes: usize,
    pub verdict: Verdict,
}

/// The invitation the strip is for. For a series this is the occurrence
/// Show in Calendar opens, so the card's two ways into the calendar agree.
pub struct Asked<'a> {
    pub uid: &'a str,
    pub start: EpochMillis,
    pub end: EpochMillis,
}

/// Whether an event on the copy takes the hour, as the clash line counts
/// it: one that blocks time and is not the invitation's own event, whose
/// UID a calendar may write in either case.
pub fn counts(o: &Occurrence, uid: &str) -> bool {
    o.event.blocks_time() && !o.event.uid.eq_ignore_ascii_case(uid)
}

fn named(title: &str) -> String {
    if title.trim().is_empty() {
        gettext("Untitled event")
    } else {
        title.to_string()
    }
}

/// The strip for `asked` over `span`, from the account's occurrences
/// around it and its calendars' colours by calendar id.
pub fn build<Tz: TimeZone>(
    asked: &Asked,
    span: (EpochMillis, EpochMillis),
    others: &[Occurrence],
    colours: &HashMap<String, String>,
    zone: &Tz,
) -> Strip
where
    Tz::Offset: Display,
{
    let (from, to) = span;
    let width = (to - from).max(1) as f64;
    let at = |t: EpochMillis| (t.clamp(from, to) - from) as f64 / width;
    let colour_of = |o: &Occurrence| {
        o.event
            .color
            .clone()
            .or_else(|| colours.get(&o.event.calendar).cloned())
            .unwrap_or_default()
    };
    let own_colour = others
        .iter()
        .find(|o| o.event.uid.eq_ignore_ascii_case(asked.uid))
        .map(colour_of)
        .unwrap_or_default();
    let mut shown: Vec<&Occurrence> = others
        .iter()
        .filter(|o| counts(o, asked.uid) && o.start < to && o.end > from)
        .collect();
    shown.sort_by_key(|o| (o.start, std::cmp::Reverse(o.end)));
    let mut blocks = vec![Block {
        title: gettext("This meeting"),
        from: at(asked.start),
        to: at(asked.end),
        lane: 0,
        colour: own_colour,
        this: true,
    }];
    // Each lane remembers where its last event ends. The meeting holds
    // the top lane, so what overlaps it stacks underneath. A block with
    // no lane left among the four is not drawn; the verdict line still
    // names it.
    let mut ends: Vec<Vec<(EpochMillis, EpochMillis)>> = vec![vec![(asked.start, asked.end)]];
    for o in &shown {
        let free = |lane: &Vec<(EpochMillis, EpochMillis)>| {
            lane.iter()
                .all(|(start, end)| o.start >= *end || o.end <= *start)
        };
        let lane = match ends.iter().position(free) {
            Some(lane) => lane,
            None if ends.len() < MOST_LANES => {
                ends.push(Vec::new());
                ends.len() - 1
            }
            None => continue,
        };
        ends[lane].push((o.start, o.end));
        blocks.push(Block {
            title: named(&o.event.title),
            from: at(o.start),
            to: at(o.end),
            lane,
            colour: colour_of(o),
            this: false,
        });
    }
    let clashing: Vec<String> = others
        .iter()
        .filter(|o| counts(o, asked.uid) && o.start < asked.end && o.end > asked.start)
        .map(|o| named(&o.event.title))
        .collect();
    Strip {
        heading: part(asked.start, zone).heading(),
        hours: hour_marks(from, to, zone),
        lanes: ends.len(),
        blocks,
        verdict: if clashing.is_empty() {
            Verdict::Free
        } else {
            Verdict::Clashes(clashing)
        },
    }
}

fn hour_marks<Tz: TimeZone>(from: EpochMillis, to: EpochMillis, zone: &Tz) -> Vec<(f64, String)>
where
    Tz::Offset: Display,
{
    let width = (to - from).max(1) as f64;
    let pattern = gettext("%H:%M");
    (1..HOURS)
        .map(|n| from + n * 3_600_000)
        .filter(|at| *at < to)
        .filter_map(|at| {
            let local = zone.timestamp_millis_opt(at).single()?;
            Some((
                (at - from) as f64 / width,
                local.format_localized(&pattern, date_locale()).to_string(),
            ))
        })
        .collect()
}

/// The grid columns a block takes: its first quarter hour and how many it
/// spans, at least one.
pub fn columns(from: f64, to: f64) -> (i32, i32) {
    let first = ((from * f64::from(COLUMNS)).round() as i32).min(COLUMNS - 1);
    let last = ((to * f64::from(COLUMNS)).round() as i32).clamp(first + 1, COLUMNS);
    (first, last - first)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use super::*;
    use chrono::FixedOffset;
    use mailrs_domain::EpochMillis;
    use mailrs_domain::calendar::{Event, Occurrence};
    use mailrs_domain::invitation::Answer;

    const H: EpochMillis = 3_600_000;
    const M: EpochMillis = 60_000;
    /// Wednesday 23 September 2026, 00:00 UTC.
    const DAY: EpochMillis = 1_790_121_600_000;

    fn utc() -> FixedOffset {
        FixedOffset::east_opt(0).expect("a valid offset")
    }

    fn busy(title: &str, start: EpochMillis, end: EpochMillis) -> Occurrence {
        Occurrence {
            account_id: 1,
            event: Arc::new(Event {
                calendar: "primary".into(),
                id: title.to_lowercase(),
                uid: format!("{}@example.com", title.to_lowercase().replace(' ', "-")),
                title: title.into(),
                busy: true,
                ..Event::default()
            }),
            start,
            end,
        }
    }

    fn asked(start: EpochMillis, end: EpochMillis) -> Asked<'static> {
        Asked {
            uid: "review@example.com",
            start,
            end,
        }
    }

    fn afternoon(others: &[Occurrence]) -> Strip {
        build(
            &asked(DAY + 15 * H, DAY + 16 * H),
            (DAY + 13 * H, DAY + 18 * H),
            others,
            &HashMap::new(),
            &utc(),
        )
    }

    #[test]
    fn the_strip_holds_five_hours_with_the_event_two_hours_in() {
        assert_eq!(
            window(DAY + 15 * H, &utc()),
            Some((DAY + 13 * H, DAY + 18 * H))
        );
        assert_eq!(
            window(DAY + 15 * H + 30 * M, &utc()),
            Some((DAY + 13 * H, DAY + 18 * H))
        );
    }

    #[test]
    fn a_late_event_keeps_the_strip_inside_its_day() {
        assert_eq!(
            window(DAY + 23 * H + 30 * M, &utc()),
            Some((DAY + 19 * H, DAY + 24 * H))
        );
        assert_eq!(window(DAY + 30 * M, &utc()), Some((DAY, DAY + 5 * H)));
    }

    #[test]
    fn the_strip_follows_the_local_day() {
        let lisbon_summer = FixedOffset::east_opt(3_600).expect("a valid offset");
        // 14:00 UTC is 15:00 in Lisbon in September.
        assert_eq!(
            window(DAY + 14 * H, &lisbon_summer),
            Some((DAY + 12 * H, DAY + 17 * H))
        );
    }

    #[test]
    fn the_heading_follows_the_time_of_day() {
        assert_eq!(part(DAY + 9 * H, &utc()), Part::Morning);
        assert_eq!(part(DAY + 15 * H, &utc()), Part::Afternoon);
        assert_eq!(part(DAY + 19 * H, &utc()), Part::Evening);
        assert_eq!(Part::Afternoon.heading(), "Your afternoon");
    }

    #[test]
    fn a_free_hour_says_so_and_draws_the_neighbours() {
        let strip = afternoon(&[
            busy("Lunch with Ana", DAY + 13 * H, DAY + 14 * H),
            busy("Retro", DAY + 16 * H + 30 * M, DAY + 17 * H + 30 * M),
        ]);
        assert_eq!(strip.verdict, Verdict::Free);
        assert_eq!(strip.verdict.words(), "Nothing else at that hour");
        let drawn: Vec<(&str, f64, f64, bool)> = strip
            .blocks
            .iter()
            .map(|b| (b.title.as_str(), b.from, b.to, b.this))
            .collect();
        assert_eq!(
            drawn,
            [
                ("This meeting", 0.4, 0.6, true),
                ("Lunch with Ana", 0.0, 0.2, false),
                ("Retro", 0.7, 0.9, false)
            ]
        );
        let hours: Vec<&str> = strip.hours.iter().map(|(_, h)| h.as_str()).collect();
        assert_eq!(hours, ["14:00", "15:00", "16:00", "17:00"]);
        assert_eq!(strip.lanes, 1);
    }

    #[test]
    fn an_event_in_the_same_hour_is_a_clash() {
        let strip = afternoon(&[busy(
            "Dentist",
            DAY + 15 * H + 30 * M,
            DAY + 16 * H + 30 * M,
        )]);
        assert_eq!(strip.verdict.words(), "Clashes with Dentist");
        assert_eq!(strip.verdict.tone(), "clash");
        assert_eq!(
            strip.lanes, 2,
            "the clash sits under the meeting, not over it"
        );
        assert_eq!(strip.blocks[0].lane, 0, "the meeting keeps the top lane");
    }

    #[test]
    fn the_meeting_keeps_the_top_lane_under_an_earlier_overlap() {
        // Starting first, the dentist would take the top lane if the
        // strip placed events by start alone.
        let strip = afternoon(&[
            busy("Dentist", DAY + 14 * H + 30 * M, DAY + 15 * H + 30 * M),
            busy("Lunch with Ana", DAY + 13 * H, DAY + 14 * H),
        ]);
        let lanes: Vec<(&str, usize)> = strip
            .blocks
            .iter()
            .map(|b| (b.title.as_str(), b.lane))
            .collect();
        assert_eq!(
            lanes,
            [("This meeting", 0), ("Lunch with Ana", 0), ("Dentist", 1)]
        );
    }

    #[test]
    fn a_crowded_hour_draws_four_rows_and_names_every_clash() {
        let crowd: Vec<Occurrence> = (0..6)
            .map(|n| busy(&format!("Call {n}"), DAY + 15 * H, DAY + 16 * H))
            .collect();
        let strip = afternoon(&crowd);
        assert_eq!(strip.lanes, 4);
        assert_eq!(strip.blocks.len(), 4);
        assert_eq!(
            strip.verdict,
            Verdict::Clashes((0..6).map(|n| format!("Call {n}")).collect())
        );
    }

    #[test]
    fn declined_free_all_day_and_the_invitation_itself_leave_the_hour_open() {
        let mut declined = busy("Gym", DAY + 15 * H, DAY + 16 * H);
        Arc::make_mut(&mut declined.event).my_answer = Some(Answer::No);
        let mut free = busy("Focus", DAY + 15 * H, DAY + 16 * H);
        Arc::make_mut(&mut free.event).busy = false;
        let mut all_day = busy("Offsite", DAY, DAY + 24 * H);
        Arc::make_mut(&mut all_day.event).all_day = true;
        let mut itself = busy("Design review", DAY + 15 * H, DAY + 16 * H);
        Arc::make_mut(&mut itself.event).uid = "REVIEW@example.com".into();
        let strip = afternoon(&[declined, free, all_day, itself]);
        assert_eq!(strip.verdict, Verdict::Free);
        assert_eq!(strip.blocks.len(), 1, "only the invitation's own block");
    }

    #[test]
    fn more_clashes_name_the_first_and_count_the_rest() {
        assert_eq!(
            Verdict::Clashes(vec!["A".into(), "B".into()]).words(),
            "Clashes with A and B"
        );
        assert_eq!(
            Verdict::Clashes(vec!["A".into(), "B".into(), "C".into()]).words(),
            "Clashes with A and 2 more"
        );
    }

    #[test]
    fn the_invitation_takes_its_calendar_colour_from_the_copy() {
        let mut own = busy("Design review", DAY + 15 * H, DAY + 16 * H);
        let event = Arc::make_mut(&mut own.event);
        event.uid = "review@example.com".into();
        event.calendar = "team".into();
        let colours = HashMap::from([("team".to_string(), "#9141ac".to_string())]);
        let strip = build(
            &asked(DAY + 15 * H, DAY + 16 * H),
            (DAY + 13 * H, DAY + 18 * H),
            &[own],
            &colours,
            &utc(),
        );
        assert_eq!(strip.blocks[0].colour, "#9141ac");
    }

    #[test]
    fn a_block_takes_the_quarter_hours_it_covers() {
        assert_eq!(columns(0.0, 0.2), (0, 4));
        assert_eq!(columns(0.4, 0.6), (8, 4));
        assert_eq!(columns(0.99, 1.0), (19, 1));
        assert_eq!(columns(0.5, 0.5), (10, 1));
    }
}
