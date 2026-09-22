//! The calendar tools, against the in-memory Gmail's calendar.

use chrono::{Datelike, Duration, Local, NaiveDate, Weekday};
use mailrs_domain::MessageBody;
use serde_json::{Value, json};

use super::super::Permission;
use super::super::fake::{Harness, ME, NOW, meta};
use super::harness;

/// A Monday a few weeks ahead, so every time the tests use lies in the
/// future and the working week around it is whole.
fn monday() -> NaiveDate {
    let mut day = Local::now().date_naive() + Duration::days(30);
    while day.weekday() != Weekday::Mon {
        day = day.succ_opt().expect("a next day");
    }
    day
}

fn at(day: NaiveDate, time: &str) -> String {
    format!("{}T{time}", day.format("%Y-%m-%d"))
}

async fn kite_day(h: &Harness, day: NaiveDate) -> Value {
    h.ok(
        "create_event",
        json!({
            "title": "Kite day",
            "start": at(day, "10:00"),
            "end": at(day, "11:30"),
            "attendees": ["Ann <ann@example.com>"],
            "location": "The hill",
        }),
    )
    .await
}

#[tokio::test]
async fn an_event_is_asked_about_made_and_listed_in_local_time() {
    let h = harness().await;
    let day = monday();
    let made = kite_day(&h, day).await;
    assert_eq!(made["account"], ME);
    assert_eq!(made["created"]["title"], "Kite day");
    assert_eq!(made["created"]["start"], at(day, "10:00"));
    assert_eq!(made["created"]["end"], at(day, "11:30"));
    {
        let asked = h.asked();
        assert_eq!(asked.questions.len(), 1);
        let question = &asked.questions[0];
        assert!(
            question.starts_with(&format!("Add “Kite day” to the calendar for {ME}, ")),
            "{question}"
        );
        assert!(
            question.ends_with("Google sends an invitation to ann@example.com."),
            "{question}"
        );
    }

    let listed = h
        .ok(
            "list_events",
            json!({"from": day.format("%Y-%m-%d").to_string(), "to": day.format("%Y-%m-%d").to_string()}),
        )
        .await;
    assert_eq!(listed["count"], 1, "a day as `to` counts in full");
    let event = &listed["events"][0];
    assert_eq!(event["title"], "Kite day");
    assert_eq!(event["weekday"], "Monday");
    assert_eq!(event["location"], "The hill");
    assert_eq!(event["guests"][0]["email"], "ann@example.com");
    assert_eq!(event["guests"][0]["answer"], "not yet");
    assert_eq!(event["all_day"], false);
}

#[tokio::test]
async fn an_all_day_event_names_its_last_day() {
    let h = harness().await;
    let day = monday();
    let last = day + Duration::days(2);
    let made = h
        .ok(
            "create_event",
            json!({
                "title": "Kite festival",
                "start": day.format("%Y-%m-%d").to_string(),
                "end": last.format("%Y-%m-%d").to_string(),
            }),
        )
        .await;
    assert_eq!(made["created"]["all_day"], true);
    assert_eq!(made["created"]["start"], day.format("%Y-%m-%d").to_string());
    assert_eq!(
        made["created"]["end"],
        last.format("%Y-%m-%d").to_string(),
        "the tools name the last day, not Google's day after"
    );
    assert_eq!(
        h.gmail.with(|i| i.events[0].end.clone()),
        Some(mailrs_gmail::EventTime::Day(
            (last + Duration::days(1)).format("%Y-%m-%d").to_string()
        ))
    );

    assert_eq!(
        h.run(
            "create_event",
            json!({"title": "Odd", "start": day.format("%Y-%m-%d").to_string(), "end": at(day, "10:00")}),
        )
        .await,
        Err("Give start and end both as times, or both as days for an all-day event.".into())
    );
    assert_eq!(
        h.run(
            "create_event",
            json!({"title": "Backwards", "start": at(day, "10:00"), "end": at(day, "09:00")}),
        )
        .await,
        Err("The event must end after it starts.".into())
    );
}

#[tokio::test]
async fn free_time_skips_what_is_booked_nights_and_weekends() {
    let h = harness().await;
    let day = monday();
    kite_day(&h, day).await;

    let free = h
        .ok(
            "find_free_time",
            json!({
                "from": (day - Duration::days(2)).format("%Y-%m-%d").to_string(),
                "to": day.format("%Y-%m-%d").to_string(),
                "minutes": 60,
            }),
        )
        .await;
    let slots: Vec<(String, String)> = free["free"]
        .as_array()
        .expect("slots")
        .iter()
        .map(|s| {
            (
                s["start"].as_str().unwrap_or_default().to_string(),
                s["end"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    assert_eq!(
        slots,
        [
            (at(day, "09:00"), at(day, "10:00")),
            (at(day, "11:30"), at(day, "18:00")),
        ],
        "Saturday and Sunday are left out, and so is the kite day"
    );

    let longer = h
        .ok(
            "find_free_time",
            json!({
                "from": at(day, "08:00"),
                "to": at(day, "12:00"),
                "minutes": 90,
                "day_starts": "08:00",
            }),
        )
        .await;
    assert_eq!(longer["free"][0]["start"], at(day, "08:00"));
    assert_eq!(longer["free"][0]["minutes"], 120);
    assert_eq!(longer["free"].as_array().map(Vec::len), Some(1));
}

#[tokio::test]
async fn an_event_changes_and_goes_once_the_user_agrees() {
    let h = harness().await;
    let day = monday();
    let made = kite_day(&h, day).await;
    let id = made["created"]["id"].as_str().expect("an id").to_string();

    let changed = h
        .ok(
            "update_event",
            json!({
                "id": id,
                "current_title": "Kite day",
                "start": at(day, "14:00"),
                "end": at(day, "15:00"),
                "location": "The beach",
            }),
        )
        .await;
    assert_eq!(changed["updated"]["start"], at(day, "14:00"));
    assert_eq!(changed["updated"]["location"], "The beach");
    assert_eq!(changed["updated"]["title"], "Kite day");
    let question = h.asked().questions[1].clone();
    assert!(
        question.starts_with(&format!(
            "Change “Kite day” on the calendar for {ME}? Its guests are told."
        )),
        "{question}"
    );
    assert!(question.contains("Where: The beach"), "{question}");

    assert_eq!(
        h.run("update_event", json!({"id": id})).await,
        Err("Say what to change: a title, a time, a place, guests or a description.".into())
    );

    h.effects.asked.borrow_mut().approves = false;
    assert_eq!(
        h.run("delete_event", json!({"id": id, "title": "Kite day"}))
            .await,
        Err("The user declined.".into())
    );
    assert_eq!(h.gmail.with(|i| i.events.len()), 1, "a no keeps the event");

    h.effects.asked.borrow_mut().approves = true;
    assert_eq!(
        h.ok("delete_event", json!({"id": id, "title": "Kite day"}))
            .await,
        json!({"account": ME, "deleted": id})
    );
    assert!(h.gmail.with(|i| i.events.is_empty()));
    assert_eq!(
        h.asked().questions.last().map(String::as_str),
        Some(
            format!("Delete “Kite day” from the calendar for {ME}? Its guests are told.").as_str()
        )
    );
}

#[tokio::test]
async fn a_missing_calendar_permission_is_asked_for() {
    let h = harness().await;
    h.gmail.withhold(mailrs_gmail::CALENDAR_SCOPE);
    let day = monday().format("%Y-%m-%d").to_string();

    let answer = h.run("list_events", json!({"from": day, "to": day})).await;
    assert_eq!(
        answer,
        Err(format!(
            "Penguin Mail needs permission to use the calendar for {ME}. \
             The user was asked to grant it; try again once they have."
        ))
    );
    assert_eq!(
        h.asked().permission_asked,
        [(h.account_id, Permission::Calendar)]
    );
}

#[tokio::test]
async fn a_calendar_api_switched_off_is_explained_and_reported() {
    let h = harness().await;
    h.gmail
        .with(|i| i.calendar_off = Some("https://console.example/calendar".into()));
    let day = monday().format("%Y-%m-%d").to_string();

    let answer = h
        .run("list_events", json!({"from": day, "to": day}))
        .await
        .expect_err("Google refuses");
    assert!(
        answer.contains("Google Calendar API is switched off"),
        "{answer}"
    );
    assert!(
        answer.contains("https://console.example/calendar"),
        "{answer}"
    );
    let asked = h.asked();
    assert_eq!(
        asked.api_off,
        [(
            "Google Calendar API".to_string(),
            "https://console.example/calendar".to_string()
        )]
    );
    assert!(
        asked.permission_asked.is_empty(),
        "no permission would help"
    );
}

/// An invitation from Priya, as Google Calendar sends one.
fn invite() -> String {
    [
        "BEGIN:VCALENDAR",
        "METHOD:REQUEST",
        "BEGIN:VEVENT",
        "UID:roadmap@example.com",
        "SEQUENCE:0",
        "SUMMARY:Roadmap review",
        "DTSTART:20300310T090000Z",
        "ATTENDEE;PARTSTAT=NEEDS-ACTION;CN=Dana:mailto:dana@example.com",
        "ORGANIZER;CN=Priya:mailto:priya@example.com",
        "END:VEVENT",
        "END:VCALENDAR",
        "",
    ]
    .join("\r\n")
}

#[tokio::test]
async fn an_invitation_is_answered_after_asking() {
    let h = Harness::with(vec![meta(
        "m9",
        "t9",
        "priya@example.com",
        "Invitation: Roadmap review",
        NOW,
    )])
    .await;
    h.gmail.with(|i| {
        i.bodies.insert(
            "m9".into(),
            MessageBody {
                calendar: Some(invite()),
                ..MessageBody::default()
            },
        );
        i.bodies.insert("m8".into(), MessageBody::default());
    });

    let answered = h
        .ok(
            "answer_invitation",
            json!({"account": ME, "message_id": "m9", "answer": "yes"}),
        )
        .await;
    assert_eq!(answered["event"], "Roadmap review");
    assert_eq!(answered["answer"], "yes");
    assert_eq!(
        answered["went"], "The answer went to the organizer by email.",
        "the fake calendar holds no such event"
    );
    assert_eq!(h.asked().questions, ["Answer Yes to “Roadmap review”?"]);

    assert_eq!(
        h.run(
            "answer_invitation",
            json!({"account": ME, "message_id": "m8", "answer": "no"})
        )
        .await,
        Err("That message holds no invitation.".into())
    );
    assert_eq!(
        h.run(
            "answer_invitation",
            json!({"account": ME, "message_id": "m9", "answer": "perhaps"})
        )
        .await,
        Err("Unknown answer perhaps; use yes, no or maybe.".into())
    );
}
