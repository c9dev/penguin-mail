//! A repeating all-day event must name the same days whatever zone the
//! machine runs in. This binary holds one test, since it sets the
//! process's `TZ` to a zone away from UTC before anything reads it.

use chrono::NaiveDate;
use mailrs_domain::EpochMillis;
use mailrs_domain::calendar::{Event, expand};

fn midnight(month: u32, day: u32) -> EpochMillis {
    NaiveDate::from_ymd_opt(2026, month, day)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc()
        .timestamp_millis()
}

#[test]
fn an_all_day_series_skips_and_adds_bare_dates_outside_utc() {
    // SAFETY: the only test in this binary, so no other thread reads the
    // environment while this one writes it.
    unsafe { std::env::set_var("TZ", "Europe/Lisbon") };
    let event = Event {
        start: midnight(7, 6),
        end: midnight(7, 7),
        zone: "UTC".into(),
        all_day: true,
        rules: vec![
            "RRULE:FREQ=WEEKLY;COUNT=3".into(),
            "EXDATE;VALUE=DATE:20260713".into(),
            "RDATE;VALUE=DATE:20260709".into(),
        ],
        ..Event::default()
    };
    let got = expand(&event, midnight(7, 1), midnight(8, 1));
    assert_eq!(
        got,
        vec![
            (midnight(7, 6), midnight(7, 7)),
            (midnight(7, 9), midnight(7, 10)),
            (midnight(7, 20), midnight(7, 21))
        ]
    );
}
