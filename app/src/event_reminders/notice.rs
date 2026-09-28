//! What an event reminder's notification says and offers: the title,
//! the time in words ("In 10 minutes, 14:00 to 15:00"), the place, Join
//! for a video call, Snooze 5 Minutes, and a click that opens the event.
//! Plain data, so the words are tested here and the app only posts them.

use std::fmt::Display;

use chrono::{NaiveDate, TimeZone};
use mailrs_domain::EpochMillis;
use mailrs_domain::translate::{date_locale, fill, fill_plural, gettext};
use mailrs_store::event_reminders::Key;

use super::plan::{Due, MINUTE, notification_id};

/// The app actions a reminder's notification calls, each with the
/// reminder's [`target`] as its parameter.
pub const SHOW_EVENT: &str = "show-event";
pub const JOIN_EVENT: &str = "join-event";
pub const SNOOZE_REMINDER: &str = "snooze-reminder";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    /// Opens the event's video call.
    Join,
    Snooze,
}

impl Button {
    pub fn label(self) -> String {
        match self {
            Button::Join => gettext("Join"),
            Button::Snooze => gettext("Snooze 5 Minutes"),
        }
    }

    pub fn action(self) -> &'static str {
        match self {
            Button::Join => JOIN_EVENT,
            Button::Snooze => SNOOZE_REMINDER,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    /// Stable per reminder, so posting it again replaces it.
    pub id: String,
    pub title: String,
    pub body: String,
    pub buttons: Vec<Button>,
    /// The reminder as the actions' parameter.
    pub target: String,
}

pub fn notice<Z: TimeZone>(due: &Due, now: EpochMillis, zone: &Z) -> Notice
where
    Z::Offset: Display,
{
    let event = &due.occurrence.event;
    let title = match event.title.trim() {
        "" => gettext("(no title)"),
        title => title.to_string(),
    };
    let mut body = when_words(due, now, zone);
    if !event.place.trim().is_empty() {
        body.push('\n');
        body.push_str(event.place.trim());
    }
    let mut buttons = Vec::new();
    if event.conference.as_deref().is_some_and(joinable) {
        buttons.push(Button::Join);
    }
    buttons.push(Button::Snooze);
    Notice { id: notification_id(&due.key), title, body, buttons, target: target(&due.key) }
}

/// "In 10 minutes, 14:00 to 15:00", "Started 5 minutes ago, 14:00 to
/// 15:00", "Tomorrow, all day". A reminder on time counts down in
/// minutes, or in whole hours; anything further names the day.
pub fn when_words<Z: TimeZone>(due: &Due, now: EpochMillis, zone: &Z) -> String
where
    Z::Offset: Display,
{
    let today = date_in(now, zone);
    if due.occurrence.event.all_day {
        // The store keeps an all-day event from midnight UTC of its day.
        let day = chrono::DateTime::from_timestamp_millis(due.occurrence.start)
            .map(|at| at.date_naive())
            .unwrap_or(today);
        return fill(&gettext("{when}, all day"), &[("when", &day_words(day.max(today), today, zone))]);
    }
    let ahead = (due.starts - now + MINUTE - 1).div_euclid(MINUTE);
    let when = if ahead <= 0 {
        let ago = (now - due.starts).div_euclid(MINUTE);
        if ago < 1 {
            gettext("Now")
        } else {
            fill_plural(
                "Started {count} minute ago",
                "Started {count} minutes ago",
                ago as usize,
                &[("count", &ago.to_string())],
            )
        }
    } else if ahead < 60 {
        fill_plural("In {count} minute", "In {count} minutes", ahead as usize, &[("count", &ahead.to_string())])
    } else if ahead % 60 == 0 && ahead < 24 * 60 {
        let hours = ahead / 60;
        fill_plural("In {count} hour", "In {count} hours", hours as usize, &[("count", &hours.to_string())])
    } else {
        day_words(date_in(due.starts, zone), today, zone)
    };
    fill(
        &gettext("{when}, {start} to {end}"),
        &[("when", &when), ("start", &time_in(due.starts, zone)), ("end", &time_in(due.ends, zone))],
    )
}

/// The reminder as JSON, the parameter each notification action takes.
pub fn target(key: &Key) -> String {
    serde_json::to_string(key).unwrap_or_default()
}

pub fn key_of(target: &str) -> Option<Key> {
    serde_json::from_str(target).ok()
}

/// Whether a conference link is one Join may open. Google hands back
/// `https` links; anything else stays off the notification.
pub fn joinable(link: &str) -> bool {
    link.starts_with("https://")
}

fn date_in<Z: TimeZone>(at: EpochMillis, zone: &Z) -> NaiveDate {
    zone.timestamp_millis_opt(at).single().map(|t| t.date_naive()).unwrap_or_default()
}

fn time_in<Z: TimeZone>(at: EpochMillis, zone: &Z) -> String
where
    Z::Offset: Display,
{
    zone.timestamp_millis_opt(at)
        .single()
        .map(|t| crate::clock_format::time_text(t.time()))
        .unwrap_or_default()
}

/// "Today", "Tomorrow", or the weekday and date.
fn day_words<Z: TimeZone>(day: NaiveDate, today: NaiveDate, zone: &Z) -> String
where
    Z::Offset: Display,
{
    match (day - today).num_days() {
        ..=0 => gettext("Today"),
        1 => gettext("Tomorrow"),
        _ => day
            .and_hms_opt(12, 0, 0)
            .and_then(|noon| zone.from_local_datetime(&noon).earliest())
            .map(|noon| noon.format_localized(&gettext("%A %-d %B"), date_locale()).to_string())
            .unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use chrono_tz::Europe::Lisbon;
    use mailrs_domain::calendar::Event;

    const MIN: EpochMillis = MINUTE;
    const HOUR: EpochMillis = 60 * MIN;
    const DAY: EpochMillis = 24 * HOUR;
    // Wednesday 23 September 2026, 14:00 in Lisbon.
    const TWO_PM: EpochMillis = 1_790_168_400_000;

    fn due(title: &str, start: EpochMillis, end: EpochMillis) -> Due {
        mailrs_domain::translate::set_date_locale("en_US");
        let event = Event {
            calendar: "work".into(),
            id: "review".into(),
            title: title.into(),
            start,
            end,
            zone: "Europe/Lisbon".into(),
            ..Event::default()
        };
        Due {
            key: Key { account_id: 1, calendar: "work".into(), event: "review".into(), start, minutes: 10 },
            occurrence: mailrs_domain::calendar::Occurrence { account_id: 1, event: Arc::new(event), start, end },
            starts: start,
            ends: end,
        }
    }

    fn words(now: EpochMillis) -> String {
        when_words(&due("Review", TWO_PM, TWO_PM + HOUR), now, &Lisbon)
    }

    #[test]
    fn the_time_ahead_reads_in_minutes_then_hours_then_days() {
        assert_eq!(words(TWO_PM - 10 * MIN), "In 10 minutes, 14:00 to 15:00");
        assert_eq!(words(TWO_PM - 30_000), "In 1 minute, 14:00 to 15:00");
        assert_eq!(words(TWO_PM - HOUR), "In 1 hour, 14:00 to 15:00");
        assert_eq!(words(TWO_PM - 90 * MIN), "Today, 14:00 to 15:00");
        assert_eq!(words(TWO_PM - DAY), "Tomorrow, 14:00 to 15:00");
        assert_eq!(words(TWO_PM - 3 * DAY), "Wednesday 23 September, 14:00 to 15:00");
    }

    #[test]
    fn a_late_reminder_says_how_long_ago_the_event_started() {
        assert_eq!(words(TWO_PM + 20_000), "Now, 14:00 to 15:00");
        assert_eq!(words(TWO_PM + MIN), "Started 1 minute ago, 14:00 to 15:00");
        assert_eq!(words(TWO_PM + 20 * MIN), "Started 20 minutes ago, 14:00 to 15:00");
    }

    #[test]
    fn an_all_day_event_names_its_day() {
        // Thursday 24 September, from midnight in Lisbon.
        let mut offsite = due("Offsite", 1_790_204_400_000, 1_790_204_400_000 + DAY);
        offsite.occurrence.start = 1_790_208_000_000;
        offsite.occurrence.end = 1_790_208_000_000 + DAY;
        offsite.occurrence.event = Arc::new(Event { all_day: true, ..(*offsite.occurrence.event).clone() });
        // 17:00 on Wednesday.
        assert_eq!(when_words(&offsite, 1_790_179_200_000, &Lisbon), "Tomorrow, all day");
        assert_eq!(when_words(&offsite, 1_790_204_400_000 + HOUR, &Lisbon), "Today, all day");
    }

    #[test]
    fn the_body_puts_the_place_under_the_time() {
        let mut lunch = due("Lunch with Ana", TWO_PM, TWO_PM + HOUR);
        lunch.occurrence.event = Arc::new(Event { place: "Café Império".into(), ..(*lunch.occurrence.event).clone() });
        let shown = notice(&lunch, TWO_PM - 10 * MIN, &Lisbon);
        assert_eq!(shown.title, "Lunch with Ana");
        assert_eq!(shown.body, "In 10 minutes, 14:00 to 15:00\nCafé Império");
        assert_eq!(shown.id, notification_id(&lunch.key));
    }

    #[test]
    fn an_event_with_no_title_still_says_something() {
        assert_eq!(notice(&due("  ", TWO_PM, TWO_PM + HOUR), TWO_PM, &Lisbon).title, "(no title)");
    }

    #[test]
    fn join_shows_only_with_a_call_link() {
        let plain = due("Review", TWO_PM, TWO_PM + HOUR);
        assert_eq!(notice(&plain, TWO_PM, &Lisbon).buttons, vec![Button::Snooze]);
        let mut call = plain.clone();
        call.occurrence.event = Arc::new(Event {
            conference: Some("https://meet.google.com/abc-defg-hij".into()),
            ..(*call.occurrence.event).clone()
        });
        assert_eq!(notice(&call, TWO_PM, &Lisbon).buttons, vec![Button::Join, Button::Snooze]);
        let mut odd = plain;
        odd.occurrence.event = Arc::new(Event {
            conference: Some("javascript:alert(1)".into()),
            ..(*odd.occurrence.event).clone()
        });
        assert_eq!(notice(&odd, TWO_PM, &Lisbon).buttons, vec![Button::Snooze]);
    }

    #[test]
    fn a_target_carries_the_reminder_there_and_back() {
        let review = due("Review", TWO_PM, TWO_PM + HOUR);
        let shown = notice(&review, TWO_PM, &Lisbon);
        assert_eq!(key_of(&shown.target), Some(review.key));
        assert_eq!(key_of("not json"), None);
    }
}
