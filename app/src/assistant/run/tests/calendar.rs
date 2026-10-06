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

/// How many events `list_events` finds on `day`.
async fn listed_on(h: &Harness, day: NaiveDate) -> u64 {
    let day = day.format("%Y-%m-%d").to_string();
    let listed = h.ok("list_events", json!({"from": day, "to": day})).await;
    listed["count"].as_u64().unwrap_or_default()
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
        // The same words serve a Google, Microsoft or CalDAV account.
        assert!(
            question.ends_with("Guests get an invitation: ann@example.com."),
            "{question}"
        );
        assert!(!question.contains("Google"), "{question}");
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
    h.copy.send(h.account_id).await.expect("the queue goes out");
    let after_last = (last + Duration::days(1)).and_hms_opt(0, 0, 0).expect("midnight").and_utc().timestamp_millis();
    assert_eq!(
        h.gmail.with(|i| i.calendar_events.iter().map(|e| (e.all_day, e.end)).collect::<Vec<_>>()),
        [(true, after_last)],
        "the calendar keeps the day after the last"
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
    assert_eq!(listed_on(&h, day).await, 1, "a no keeps the event");

    h.effects.asked.borrow_mut().approves = true;
    assert_eq!(
        h.ok("delete_event", json!({"id": id, "title": "Kite day"}))
            .await,
        json!({"account": ME, "deleted": id})
    );
    assert_eq!(listed_on(&h, day).await, 0);
    assert_eq!(
        h.asked().questions.last().map(String::as_str),
        Some(
            format!("Delete “Kite day” from the calendar for {ME}? Its guests are told.").as_str()
        )
    );
}

/// The confirmation says who gets mail from the same rule the window's
/// question uses: nobody for an event with no other guests, and the new
/// guests' invitation when a change adds one.
#[tokio::test]
async fn the_confirmation_says_who_hears_of_a_change() {
    let h = harness().await;
    let day = monday();
    let made = h
        .ok("create_event", json!({"title": "Dentist", "start": at(day, "09:00"), "end": at(day, "10:00")}))
        .await;
    let id = made["created"]["id"].as_str().expect("an id").to_string();

    h.ok("update_event", json!({"id": id, "current_title": "Dentist", "attendees": ["bo@example.com"]})).await;
    let invited = h.asked().questions.last().cloned().unwrap_or_default();
    assert!(
        invited.starts_with(&format!(
            "Change “Dentist” on the calendar for {ME}? The new guests get their invitation, and the others get an update."
        )),
        "{invited}"
    );

    let kite = kite_day(&h, day).await;
    let kite = kite["created"]["id"].as_str().expect("an id").to_string();
    h.ok("update_event", json!({"id": kite, "current_title": "Kite day", "title": "Kite day"})).await;
    let unchanged = h.asked().questions.last().cloned().unwrap_or_default();
    assert!(unchanged.starts_with(&format!("Change “Kite day” on the calendar for {ME}?\n\n")), "{unchanged}");

    let solo = h
        .ok("create_event", json!({"title": "Solo", "start": at(day, "12:00"), "end": at(day, "13:00")}))
        .await;
    let solo = solo["created"]["id"].as_str().expect("an id").to_string();
    h.ok("delete_event", json!({"id": solo, "title": "Solo"})).await;
    assert_eq!(
        h.asked().questions.last().map(String::as_str),
        Some(format!("Delete “Solo” from the calendar for {ME}?").as_str())
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
        i.messages
            .insert("m8".into(), mailrs_sync::fake::meta("m8", "t8", NOW, &["INBOX"]));
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

#[tokio::test]
async fn once_the_copy_has_read_the_account_listing_events_uses_it() {
    use chrono::TimeZone;
    let h = harness().await;
    let day = monday();
    let start = Local
        .from_local_datetime(&day.and_hms_opt(10, 0, 0).expect("a time"))
        .earliest()
        .expect("a real local time")
        .timestamp_millis();
    h.gmail.with(|s| {
        s.calendars = vec![mailrs_domain::calendar::Calendar {
            id: "primary".into(),
            name: "Personal".into(),
            color: String::new(),
            access: mailrs_domain::calendar::Access::Owner,
            zone: "UTC".into(),
            primary: true,
            shown: true,
            hidden: false,
            reminders: Vec::new(),
        }];
    });
    h.gmail.put_calendar_event(mailrs_domain::calendar::Event {
        calendar: "primary".into(),
        id: "a".into(),
        title: "Kite day".into(),
        zone: "UTC".into(),
        start,
        end: start + 60 * 60_000,
        busy: true,
        ..mailrs_domain::calendar::Event::default()
    });
    let copy = mailrs_sync::calendar_copy::CalendarCopy::new(
        std::sync::Arc::clone(&h.tools.modules.accounts),
        h.db.clone(),
    );
    copy.refresh(h.account_id, start).await.unwrap();

    let listed = h
        .ok(
            "list_events",
            json!({"from": day.format("%Y-%m-%d").to_string(), "to": day.format("%Y-%m-%d").to_string()}),
        )
        .await;
    assert_eq!(listed["count"], 1);
    assert_eq!(listed["events"][0]["title"], "Kite day");
    assert_eq!(listed["events"][0]["calendar"], "Personal");
    assert_eq!(listed["events"][0]["pending"], false);
}

/// Reads `events` into a fresh copy of the harness account's primary
/// calendar, so the tools answer from the copy.
async fn copy_of(h: &Harness, events: Vec<mailrs_domain::calendar::Event>, now: i64) {
    h.gmail.with(|s| {
        s.calendars = vec![mailrs_domain::calendar::Calendar {
            id: "primary".into(),
            name: "Personal".into(),
            access: mailrs_domain::calendar::Access::Owner,
            zone: "UTC".into(),
            primary: true,
            shown: true,
            ..mailrs_domain::calendar::Calendar::default()
        }];
    });
    for event in events {
        h.gmail.put_calendar_event(event);
    }
    let copy = mailrs_sync::calendar_copy::CalendarCopy::new(
        std::sync::Arc::clone(&h.tools.modules.accounts),
        h.db.clone(),
    );
    copy.refresh(h.account_id, now).await.unwrap();
}

fn local_millis(day: NaiveDate, hour: u32) -> i64 {
    use chrono::TimeZone;
    Local
        .from_local_datetime(&day.and_hms_opt(hour, 0, 0).expect("a time"))
        .earliest()
        .expect("a real local time")
        .timestamp_millis()
}

#[tokio::test]
async fn each_occurrence_of_a_series_is_listed_under_an_id_of_its_own() {
    let h = harness().await;
    let day = monday();
    let start = local_millis(day, 10);
    let series = mailrs_domain::calendar::Event {
        calendar: "primary".into(),
        id: "standup".into(),
        title: "Stand-up".into(),
        zone: "UTC".into(),
        start,
        end: start + 15 * 60_000,
        busy: true,
        rules: vec!["RRULE:FREQ=DAILY;COUNT=5".into()],
        ..mailrs_domain::calendar::Event::default()
    };
    copy_of(&h, vec![series], start).await;

    let listed = h
        .ok(
            "list_events",
            json!({"from": day.format("%Y-%m-%d").to_string(), "to": (day + Duration::days(4)).format("%Y-%m-%d").to_string()}),
        )
        .await;
    assert_eq!(listed["count"], 5);
    let thursday = start + 3 * 24 * 60 * 60_000;
    let stamp = chrono::DateTime::from_timestamp_millis(thursday).unwrap().format("%Y%m%dT%H%M%SZ");
    assert_eq!(listed["events"][3]["id"], format!("standup_{stamp}"));
    assert_eq!(listed["events"][3]["repeats"], true);
}

#[tokio::test]
async fn a_declined_event_reads_as_free() {
    let h = harness().await;
    let day = monday();
    let start = local_millis(day, 14);
    let declined = mailrs_domain::calendar::Event {
        calendar: "primary".into(),
        id: "review".into(),
        title: "Client review".into(),
        zone: "UTC".into(),
        start,
        end: start + 60 * 60_000,
        busy: true,
        my_answer: Some(mailrs_domain::invitation::Answer::No),
        ..mailrs_domain::calendar::Event::default()
    };
    copy_of(&h, vec![declined], start).await;

    let listed = h
        .ok(
            "list_events",
            json!({"from": day.format("%Y-%m-%d").to_string(), "to": day.format("%Y-%m-%d").to_string()}),
        )
        .await;
    assert_eq!(listed["events"][0]["busy"], false);
}

#[tokio::test]
async fn a_calendar_tool_on_an_account_without_a_calendar_says_why() {
    let h = Harness::with_services(|_, services| services.calendar = None).await;
    let answer = h
        .run("list_events", json!({"from": "2026-03-10", "to": "2026-03-11"}))
        .await;
    assert_eq!(
        answer,
        Ok(json!({"unavailable": "Gmail has no calendar that other apps can reach."}))
    );
}

use mailrs_domain::calendar::{Access, Calendar};

const FAMILY: &str = "family@group.calendar.google.com";
const HOLIDAYS: &str = "pt.portuguese#holiday@group.v.calendar.google.com";

fn calendars() -> Vec<Calendar> {
    let one = |id: &str, name: &str, color: &str, access: Access| Calendar {
        id: id.into(),
        name: name.into(),
        color: color.into(),
        access,
        zone: "Europe/Lisbon".into(),
        primary: id == ME,
        shown: true,
        hidden: false,
        reminders: Vec::new(),
    };
    vec![
        one(ME, "Personal", "#e8660c", Access::Owner),
        one(FAMILY, "Family", "#9141ac", Access::Writer),
        one(HOLIDAYS, "Holidays in Portugal", "#e01b24", Access::Reader),
    ]
}

fn recital(day: NaiveDate, calendar: &str) -> Value {
    json!({
        "title": "Piano recital",
        "start": at(day, "18:00"),
        "end": at(day, "19:00"),
        "calendar": calendar,
    })
}

#[tokio::test]
async fn every_calendar_is_listed_with_its_colour_and_whether_it_takes_events() {
    let h = harness().await;
    h.read_calendars(calendars()).await;
    let account = h.account_id;
    h.db.write(move |c| mailrs_store::calendar::set_shown(c, account, HOLIDAYS, false))
        .await
        .unwrap();

    let listed = h.ok("list_calendars", json!({})).await;
    let mine = &listed["accounts"][0];
    assert_eq!(mine["account"], ME);
    let names: Vec<&str> = mine["calendars"]
        .as_array()
        .expect("a list")
        .iter()
        .map(|c| c["name"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(names, ["Personal", "Family", "Holidays in Portugal"]);
    assert_eq!(mine["calendars"][0]["primary"], true);
    assert_eq!(mine["calendars"][1]["writable"], true);
    assert_eq!(mine["calendars"][1]["id"], FAMILY);
    let holidays = &mine["calendars"][2];
    assert_eq!(holidays["writable"], false);
    assert_eq!(holidays["shown"], false);
    assert_eq!(holidays["color"], "#e01b24");
}

#[tokio::test]
async fn an_account_without_the_calendar_list_is_noted_and_asked_about_only_when_named() {
    let h = harness().await;
    h.gmail.withhold(mailrs_gmail::CALENDAR_LIST_SCOPE);

    let listed = h.ok("list_calendars", json!({})).await;
    assert_eq!(listed["accounts"][0]["calendars"], json!([]));
    let note = listed["accounts"][0]["note"].as_str().unwrap_or_default();
    assert!(note.contains("calendar permission"), "{note}");
    assert!(h.asked().permission_asked.is_empty(), "a list of every account asks nobody");

    assert_eq!(
        h.run("list_calendars", json!({"account": ME})).await,
        Err(format!(
            "Penguin Mail needs permission to use the calendar for {ME}. \
             The user was asked to grant it; try again once they have."
        ))
    );
    assert_eq!(h.asked().permission_asked, [(h.account_id, Permission::Calendar)]);
}

#[tokio::test]
async fn an_event_goes_on_the_calendar_named_in_any_case() {
    let h = harness().await;
    h.read_calendars(calendars()).await;
    let made = h.ok("create_event", recital(monday(), "family")).await;
    assert_eq!(made["created"]["calendar"], "Family");
    let question = h.asked().questions[0].clone();
    assert!(
        question.starts_with(&format!("Add “Piano recital” to the calendar “Family” for {ME}, ")),
        "{question}"
    );
    let (account, id) = (h.account_id, made["created"]["id"].as_str().expect("an id").to_string());
    let held = h
        .db
        .read(move |c| mailrs_store::calendar::event(c, account, FAMILY, &id))
        .await
        .unwrap();
    assert!(held.is_some(), "it lands on Family");
    // The queue's own background send can outrun this check (it starts as
    // soon as `create` saves the event, before this test asks for it
    // again), so pending is read from the tool's own answer, taken before
    // any send had a chance to run, rather than from a second store read.
    assert_eq!(made["created"]["pending"], true, "it waits in the queue");
}

#[tokio::test]
async fn an_account_with_no_calendar_that_takes_events_says_so() {
    let h = harness().await;
    let read_only = calendars()
        .into_iter()
        .map(|c| Calendar { access: Access::Reader, ..c })
        .collect();
    h.read_calendars(read_only).await;
    assert_eq!(
        h.run("create_event", recital(monday(), "Family")).await,
        Err(format!("“Family” on {ME} is read-only. No calendar on {ME} takes new events."))
    );
}

#[tokio::test]
async fn a_read_only_or_unknown_calendar_is_an_error_the_model_reads() {
    let h = harness().await;
    h.read_calendars(calendars()).await;
    let day = monday();
    assert_eq!(
        h.run("create_event", recital(day, "Holidays in Portugal")).await,
        Err(format!(
            "“Holidays in Portugal” on {ME} is read-only. Calendars that take events: Personal, Family."
        ))
    );
    assert_eq!(
        h.run("create_event", recital(day, "Work")).await,
        Err(format!("{ME} has no calendar called “Work”. Calendars that take events: Personal, Family."))
    );
    assert!(h.asked().questions.is_empty(), "nothing was put to the user");
}

#[tokio::test]
async fn naming_a_calendar_reads_the_copy_first() {
    let h = harness().await;
    // Google lists the calendars, but the copy has not been read, as in
    // the first minute after start.
    h.gmail.with(|s| s.calendars = calendars());
    let made = h.ok("create_event", recital(monday(), "Family")).await;
    assert_eq!(made["created"]["calendar"], "Family");
    let account = h.account_id;
    assert!(h.db.read(move |c| mailrs_store::calendar::synced(c, account)).await.unwrap());
}

#[tokio::test]
async fn an_account_with_no_calendar_service_lists_none_and_says_why_whether_or_not_it_is_named() {
    let h = Harness::with_services(|_, services| services.calendar = None).await;

    let listed = h.ok("list_calendars", json!({})).await;
    assert_eq!(listed["accounts"][0]["calendars"], json!([]));
    assert_eq!(
        listed["accounts"][0]["note"],
        "Gmail has no calendar that other apps can reach."
    );
    assert!(h.asked().permission_asked.is_empty());

    let named = h.ok("list_calendars", json!({"account": ME})).await;
    assert_eq!(named["accounts"][0]["calendars"], json!([]));
    assert_eq!(
        named["accounts"][0]["note"],
        "Gmail has no calendar that other apps can reach."
    );
    assert!(h.asked().permission_asked.is_empty(), "a permanent gap is not a permission to ask for");
}
