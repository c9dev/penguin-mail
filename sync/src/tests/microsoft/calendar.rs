use chrono::{TimeZone, Utc};
use mailrs_domain::calendar::list::ListEdit;
use mailrs_domain::calendar::{Access, Attachment, Event, Notify, Status, occurrence_id};
use mailrs_domain::invitation::Answer;
use mailrs_graph::{
    Attendee, DateTimeZone, EmailAddress, GraphCalendar, GraphError, GraphEvent, PatternedRecurrence,
    RecurrencePattern, RecurrenceRange, ResponseStatus,
};

use super::outlook;
use crate::fake::Area;
use crate::services::microsoft::GraphApi;
use crate::{BackendError, CalendarService};

fn at(text: &str) -> DateTimeZone {
    DateTimeZone { date_time: text.into(), time_zone: "UTC".into() }
}

fn weekly_standup() -> GraphEvent {
    GraphEvent {
        id: "m1".into(),
        ical_uid: Some("standup@contoso".into()),
        etag: Some("W/\"1\"".into()),
        subject: Some("Standup".into()),
        start: Some(at("2026-10-05T09:00:00.0000000")),
        end: Some(at("2026-10-05T09:15:00.0000000")),
        kind: Some("seriesMaster".into()),
        recurrence: Some(PatternedRecurrence {
            pattern: RecurrencePattern { kind: "weekly".into(), interval: 1, days_of_week: vec!["monday".into()], ..Default::default() },
            range: RecurrenceRange { kind: "numbered".into(), start_date: "2026-10-05".into(), number_of_occurrences: 10, ..Default::default() },
        }),
        cancelled_occurrences: vec!["OID.m1.2026-10-12".into()],
        ..GraphEvent::default()
    }
}

fn occurrence(id: &str, start: &str, subject: &str, kind: &str) -> GraphEvent {
    GraphEvent {
        id: id.into(),
        subject: Some(subject.into()),
        start: Some(at(start)),
        end: Some(at(&start.replace("09:00", "09:15"))),
        kind: Some(kind.into()),
        series_master_id: Some("m1".into()),
        original_start: Some(format!("{}Z", &start[..19])),
        etag: Some("W/\"2\"".into()),
        ..GraphEvent::default()
    }
}

/// A weekly Monday 09:00 meeting in Lisbon, as Graph hands it over in
/// UTC: 08:00 while summer time lasts. Outlook names Lisbon's zone
/// "GMT Standard Time".
fn lisbon_meeting() -> GraphEvent {
    GraphEvent {
        id: "m2".into(),
        ical_uid: Some("lisbon@contoso".into()),
        subject: Some("Weekly".into()),
        start: Some(at("2026-10-05T08:00:00.0000000")),
        end: Some(at("2026-10-05T08:30:00.0000000")),
        kind: Some("seriesMaster".into()),
        original_start_time_zone: Some("GMT Standard Time".into()),
        recurrence: Some(PatternedRecurrence {
            pattern: RecurrencePattern { kind: "weekly".into(), interval: 1, days_of_week: vec!["monday".into()], ..Default::default() },
            range: RecurrenceRange { kind: "noEnd".into(), start_date: "2026-10-05".into(), ..Default::default() },
        }),
        cancelled_occurrences: vec!["OID.m2.2026-10-12".into()],
        ..GraphEvent::default()
    }
}

/// Graph names a series only through its occurrences, so a test that
/// wants the series reads one of them too.
fn first_of_lisbon(master: GraphEvent) -> (GraphEvent, GraphEvent) {
    let first = GraphEvent {
        id: "m2o1".into(),
        kind: Some("occurrence".into()),
        series_master_id: Some("m2".into()),
        start: master.start.clone(),
        end: master.end.clone(),
        ..GraphEvent::default()
    };
    (master, first)
}

async fn read_all(h: &super::Outlook) -> mailrs_domain::calendar::EventPage {
    let calendar = h.sync.services().calendar.clone().unwrap();
    calendar.event_changes("cal-1", None, None, crate::now_millis() - 365 * 86_400_000).await.unwrap()
}

fn millis(text: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(text).unwrap().timestamp_millis()
}

#[tokio::test]
async fn calendars_come_with_their_colour_and_access() {
    let h = outlook().await;
    h.fake.add_calendar(GraphCalendar { id: "cal-2".into(), name: "Holidays".into(), can_edit: false, ..GraphCalendar::default() });
    let list = h.sync.services().calendar.clone().unwrap().calendars().await.unwrap();
    let main = list.iter().find(|c| c.id == "cal-1").unwrap();
    assert!(main.primary && main.access == Access::Owner && main.color == "#0078d4");
    let holidays = list.iter().find(|c| c.id == "cal-2").unwrap();
    assert_eq!(holidays.access, Access::Reader);
    assert!(holidays.color.starts_with('#'));
}

#[tokio::test]
async fn a_series_arrives_once_with_its_rule_its_gaps_and_its_changed_occurrence() {
    let h = outlook().await;
    h.fake.put_event("cal-1", weekly_standup());
    h.fake.put_event("cal-1", occurrence("o1", "2026-10-05T09:00:00.0000000", "Standup", "occurrence"));
    h.fake.put_event("cal-1", occurrence("o3", "2026-10-19T09:00:00.0000000", "Standup, long one", "exception"));
    let page = read_all(&h).await;
    let master = page.events.iter().find(|e| e.id == "m1").unwrap();
    assert!(master.rules.contains(&"RRULE:FREQ=WEEKLY;INTERVAL=1;BYDAY=MO;COUNT=10".to_string()));
    assert!(master.rules.contains(&"EXDATE:20261012T090000Z".to_string()), "{:?}", master.rules);
    let changed = page.events.iter().find(|e| e.id == "o3").unwrap();
    assert_eq!(changed.series.as_deref(), Some("m1"));
    assert!(changed.original_start.is_some());
    assert!(page.events.iter().all(|e| e.id != "o1"), "a plain occurrence is the rule's to expand");
    assert!(page.next_sync.is_some());
}

/// B6: the series keeps Lisbon's clock, so the meeting stays at 09:00
/// there when the clocks go back on 2026-10-25.
#[tokio::test]
async fn a_weekly_lisbon_meeting_stays_at_nine_after_the_clocks_change() {
    let h = outlook().await;
    let (master, first) = first_of_lisbon(lisbon_meeting());
    h.fake.put_event("cal-1", master);
    h.fake.put_event("cal-1", first);
    let page = read_all(&h).await;
    let series = page.events.iter().find(|e| e.id == "m2").unwrap();
    assert_eq!(series.zone, "Europe/London", "the zone Outlook names, as the database spells it");
    assert!(series.rules.contains(&"EXDATE;TZID=Europe/London:20261012T090000".to_string()), "{:?}", series.rules);
    let lisbon: chrono_tz::Tz = "Europe/Lisbon".parse().unwrap();
    let times: Vec<_> = mailrs_domain::calendar::expand(series, millis("2026-10-01T00:00:00Z"), millis("2026-11-10T00:00:00Z"))
        .into_iter()
        .map(|(start, _)| Utc.timestamp_millis_opt(start).unwrap().with_timezone(&lisbon).format("%m-%d %H:%M").to_string())
        .collect();
    // The 12th is cancelled; the 26th is after the change.
    assert_eq!(times, ["10-05 09:00", "10-19 09:00", "10-26 09:00", "11-02 09:00", "11-09 09:00"]);
}

#[tokio::test]
async fn a_zone_outlook_names_that_the_table_lacks_reads_as_utc() {
    let h = outlook().await;
    let (master, first) = first_of_lisbon(GraphEvent { original_start_time_zone: Some("Mars Standard Time".into()), ..lisbon_meeting() });
    h.fake.put_event("cal-1", master);
    h.fake.put_event("cal-1", first);
    let page = read_all(&h).await;
    assert_eq!(page.events.iter().find(|e| e.id == "m2").unwrap().zone, "UTC");
}

#[tokio::test]
async fn a_series_made_here_is_written_in_its_own_zone() {
    let h = outlook().await;
    let calendar = h.sync.services().calendar.clone().unwrap();
    let local = Event {
        calendar: "cal-1".into(),
        id: "pmweekly".into(),
        title: "Weekly".into(),
        start: millis("2026-10-05T08:00:00Z"),
        end: millis("2026-10-05T08:30:00Z"),
        zone: "Europe/Lisbon".into(),
        busy: true,
        rules: vec!["RRULE:FREQ=WEEKLY;BYDAY=MO".into()],
        ..Event::default()
    };
    let made = calendar.put_event(&local, None, true, Notify::Guests).await.unwrap();
    let stored = h.fake.with(|s| s.events[&made.id].1.clone());
    let start = stored.start.unwrap();
    assert_eq!((start.date_time.as_str(), start.time_zone.as_str()), ("2026-10-05T09:00:00", "GMT Standard Time"));
    let recurrence = stored.recurrence.unwrap();
    assert_eq!(recurrence.range.recurrence_time_zone.as_deref(), Some("GMT Standard Time"));
    assert_eq!(recurrence.pattern.days_of_week, vec!["monday".to_string()]);
    assert_eq!(made.start, local.start, "the instant survives the round trip");
}

#[tokio::test]
async fn a_repeat_outlook_cannot_hold_is_refused_in_plain_words() {
    let h = outlook().await;
    let calendar = h.sync.services().calendar.clone().unwrap();
    let hourly = Event { calendar: "cal-1".into(), id: "pmhour".into(), rules: vec!["RRULE:FREQ=HOURLY".into()], ..Event::default() };
    let answer = calendar.put_event(&hourly, None, true, Notify::Guests).await;
    assert!(matches!(answer, Err(BackendError::Refused(line)) if line.contains("Outlook cannot repeat")));
}

#[tokio::test]
async fn a_new_event_takes_graphs_id_and_names_its_own_as_the_transaction() {
    let h = outlook().await;
    let calendar = h.sync.services().calendar.clone().unwrap();
    let local = Event {
        calendar: "cal-1".into(),
        id: "pmabc".into(),
        title: "Dentist".into(),
        start: 1_790_000_000_000,
        end: 1_790_003_600_000,
        zone: "UTC".into(),
        busy: true,
        ..Event::default()
    };
    let made = calendar.put_event(&local, None, true, Notify::Guests).await.unwrap();
    assert_ne!(made.id, "pmabc");
    assert_eq!(made.title, "Dentist");
    assert!(h.fake.with(|s| s.transactions.contains_key("pmabc")));
}

#[tokio::test]
async fn an_edit_against_an_old_version_is_turned_down() {
    let h = outlook().await;
    h.fake.put_event("cal-1", GraphEvent { id: "e1".into(), etag: Some("W/\"9\"".into()), subject: Some("A".into()), ..GraphEvent::default() });
    let calendar = h.sync.services().calendar.clone().unwrap();
    let stale = Event { calendar: "cal-1".into(), id: "e1".into(), title: "B".into(), ..Event::default() };
    let answer = calendar.put_event(&stale, Some("W/\"1\""), false, Notify::Guests).await;
    assert!(matches!(answer, Err(BackendError::Changed)));
}

#[tokio::test]
async fn changing_one_occurrence_reaches_graphs_instance() {
    let h = outlook().await;
    let master = weekly_standup();
    h.fake.put_event("cal-1", master.clone());
    h.fake.put_event("cal-1", occurrence("o2", "2026-10-26T09:00:00.0000000", "Standup", "occurrence"));
    let page = read_all(&h).await;
    let series = page.events.iter().find(|e| e.id == "m1").unwrap();
    let original = millis("2026-10-26T09:00:00Z");
    let moved = Event {
        id: occurrence_id(series, original),
        series: Some("m1".into()),
        original_start: Some(original),
        title: "Standup, moved".into(),
        ..series.clone()
    };
    h.sync.services().calendar.clone().unwrap().put_event(&moved, None, false, Notify::Guests).await.unwrap();
    assert_eq!(h.fake.with(|s| s.events["o2"].1.subject.clone()).as_deref(), Some("Standup, moved"));
}

#[tokio::test]
async fn deleting_one_occurrence_deletes_graphs_instance() {
    let h = outlook().await;
    h.fake.put_event("cal-1", weekly_standup());
    h.fake.put_event("cal-1", occurrence("o2", "2026-10-26T09:00:00.0000000", "Standup", "occurrence"));
    let series = read_all(&h).await.events.into_iter().find(|e| e.id == "m1").unwrap();
    let id = occurrence_id(&series, millis("2026-10-26T09:00:00Z"));
    h.sync.services().calendar.clone().unwrap().remove_event("cal-1", &id, None, Notify::Guests).await.unwrap();
    assert!(h.fake.with(|s| !s.events.contains_key("o2")));
}

#[tokio::test]
async fn an_event_gone_from_graph_is_listed_as_removed_on_the_next_read() {
    let h = outlook().await;
    h.fake.put_event("cal-1", GraphEvent { id: "e1".into(), subject: Some("A".into()), start: Some(at("2026-10-05T10:00:00.0000000")), end: Some(at("2026-10-05T11:00:00.0000000")), ..GraphEvent::default() });
    let calendar = h.sync.services().calendar.clone().unwrap();
    let first = calendar.event_changes("cal-1", None, None, 0).await.unwrap();
    let token = first.next_sync.unwrap();
    h.fake.delete_event("e1", None).await.unwrap();
    let next = calendar.event_changes("cal-1", Some(&token), None, 0).await.unwrap();
    assert_eq!(next.removed, vec!["e1".to_string()]);
}

#[tokio::test]
async fn answering_an_invitation_tells_graph() {
    let h = outlook().await;
    h.fake.put_event("cal-1", GraphEvent { id: "i1".into(), ical_uid: Some("party@example".into()), ..GraphEvent::default() });
    let calendar = h.sync.services().calendar.clone().unwrap();
    calendar.answer_invitation("party@example", "me@outlook.com", Answer::Yes, None, Some("See you")).await.unwrap();
    assert_eq!(
        h.fake.with(|s| s.events["i1"].1.response_status.clone()),
        Some(ResponseStatus { response: "accepted".into() })
    );
    assert_eq!(h.fake.with(|s| s.responses[0].comment.clone()).as_deref(), Some("See you"));
    let none = calendar.answer_invitation("nothing@example", "me@outlook.com", Answer::No, None, None).await.unwrap();
    assert_eq!(none, mailrs_gmail::Answered::NotOnCalendar);
}

#[tokio::test]
async fn answering_an_event_on_the_calendar_answers_the_event() {
    let h = outlook().await;
    h.fake.put_event("cal-1", GraphEvent { id: "i1".into(), subject: Some("Party".into()), ..GraphEvent::default() });
    let calendar = h.sync.services().calendar.clone().unwrap();
    let held = calendar.answer_event("cal-1", "i1", "me@outlook.com", Answer::Maybe, None).await.unwrap();
    assert_eq!(held.id, "i1");
    assert_eq!(h.fake.with(|s| s.events["i1"].1.response_status.clone()), Some(ResponseStatus { response: "tentativelyAccepted".into() }));
}

#[tokio::test]
async fn an_organization_that_blocks_the_calendar_turns_it_off() {
    let h = outlook().await;
    h.fake.refuse(Area::Calendar, GraphError::AccessDenied { code: "ErrorAccessDenied".into() });
    let answer = h.sync.services().calendar.clone().unwrap().calendars().await;
    assert!(matches!(answer, Err(BackendError::Unsupported)));
    assert!(!h.sync.services().offers().calendar);
}

#[tokio::test]
async fn a_calendar_the_person_did_not_grant_asks_for_the_permission() {
    let h = outlook().await;
    h.fake.withhold("Calendars.ReadWrite");
    h.fake.refuse(Area::Calendar, GraphError::AccessDenied { code: "ErrorAccessDenied".into() });
    let answer = h.sync.services().calendar.clone().unwrap().calendars().await;
    assert!(matches!(answer, Err(BackendError::NeedsPermission)));
}

#[tokio::test]
async fn the_window_moves_on_once_a_month_has_passed() {
    let h = outlook().await;
    let calendar = h.sync.services().calendar.clone().unwrap();
    let old_end = crate::now_millis() + 700 * 86_400_000 - 40 * 86_400_000;
    let token = serde_json::json!({ "end": old_end, "link": "fake:delta:cal-1:0", "series": [] }).to_string();
    let answer = calendar.event_changes("cal-1", Some(&token), None, 0).await;
    assert!(matches!(answer, Err(BackendError::StateLost)));
}

#[tokio::test]
async fn a_cancelled_event_says_so_and_a_free_one_blocks_nothing() {
    let h = outlook().await;
    h.fake.put_event("cal-1", GraphEvent { id: "c1".into(), is_cancelled: true, start: Some(at("2026-10-05T10:00:00.0000000")), end: Some(at("2026-10-05T11:00:00.0000000")), ..GraphEvent::default() });
    h.fake.put_event("cal-1", GraphEvent { id: "f1".into(), show_as: Some("free".into()), start: Some(at("2026-10-05T12:00:00.0000000")), end: Some(at("2026-10-05T13:00:00.0000000")), ..GraphEvent::default() });
    let page = read_all(&h).await;
    assert_eq!(page.events.iter().find(|e| e.id == "c1").unwrap().status, Status::Cancelled);
    assert!(!page.events.iter().find(|e| e.id == "f1").unwrap().busy);
}

#[tokio::test]
async fn an_event_range_pages_one_calendar() {
    let h = outlook().await;
    for n in 0..120 {
        let (start, end) = (format!("2026-10-05T{:02}:{:02}:00.0000000", n / 60, n % 60), format!("2026-10-05T{:02}:{:02}:30.0000000", n / 60, n % 60));
        h.fake.put_event("cal-1", GraphEvent { id: format!("r{n}"), start: Some(at(&start)), end: Some(at(&end)), ..GraphEvent::default() });
    }
    let calendar = h.sync.services().calendar.clone().unwrap();
    let (from, to) = (millis("2026-10-05T00:00:00Z"), millis("2026-10-06T00:00:00Z"));
    let first = calendar.event_range("cal-1", from, to, None).await.unwrap();
    assert_eq!(first.events.len(), 100);
    let second = calendar.event_range("cal-1", from, to, first.next_page.as_deref()).await.unwrap();
    assert_eq!((second.events.len(), second.next_page), (20, None));
}

#[tokio::test]
async fn busy_time_leaves_out_free_cancelled_declined_and_all_day_events() {
    let h = outlook().await;
    let timed = |id: &str, hour: u32| GraphEvent {
        id: id.into(),
        subject: Some(id.into()),
        ical_uid: Some(format!("{id}@x")),
        start: Some(at(&format!("2026-10-05T{hour:02}:00:00.0000000"))),
        end: Some(at(&format!("2026-10-05T{hour:02}:30:00.0000000"))),
        ..GraphEvent::default()
    };
    h.fake.put_event("cal-1", timed("busy", 9));
    h.fake.put_event("cal-1", GraphEvent { show_as: Some("free".into()), ..timed("free", 10) });
    h.fake.put_event("cal-1", GraphEvent { is_cancelled: true, ..timed("cancelled", 11) });
    h.fake.put_event("cal-1", GraphEvent { response_status: Some(ResponseStatus { response: "declined".into() }), ..timed("declined", 12) });
    h.fake.put_event("cal-1", GraphEvent { is_all_day: true, ..timed("allday", 13) });
    let calendar = h.sync.services().calendar.clone().unwrap();
    let busy = calendar.busy_between(millis("2026-10-05T00:00:00Z"), millis("2026-10-06T00:00:00Z")).await.unwrap();
    assert_eq!(busy.iter().map(|b| b.summary.as_str()).collect::<Vec<_>>(), ["busy"]);
}

#[tokio::test]
async fn a_series_says_how_it_repeats_and_how_many_are_left() {
    let h = outlook().await;
    h.fake.put_event("cal-1", weekly_standup());
    let calendar = h.sync.services().calendar.clone().unwrap();
    let series = calendar.series("standup@contoso", millis("2026-10-14T00:00:00Z")).await.unwrap().unwrap();
    assert_eq!(series.rule, "FREQ=WEEKLY;INTERVAL=1;BYDAY=MO;COUNT=10");
    // Ten Mondays from the 5th, less the 12th, which Graph cancelled, less
    // the 5th, which is before the 14th: eight remain.
    assert_eq!(series.left, Some(8));
    assert!(calendar.series("nothing@x", 0).await.unwrap().is_none());
}

#[tokio::test]
async fn the_assistants_events_come_from_the_default_calendar() {
    let h = outlook().await;
    let calendar = h.sync.services().calendar.clone().unwrap();
    let fields = mailrs_gmail::EventFields {
        summary: Some("Lunch".into()),
        start: Some(mailrs_gmail::EventTime::At("2026-10-06T12:00:00Z".into())),
        end: Some(mailrs_gmail::EventTime::At("2026-10-06T13:00:00Z".into())),
        guests: Some(vec!["ana@example.com".into()]),
        ..mailrs_gmail::EventFields::default()
    };
    let made = calendar.create_event(&fields).await.unwrap();
    assert_eq!(made.summary, "Lunch");
    assert_eq!(h.fake.with(|s| s.events[&made.id].0.clone()), "cal-1");
    let changed = calendar
        .update_event(&made.id, &mailrs_gmail::EventFields { summary: Some("Late lunch".into()), ..Default::default() })
        .await
        .unwrap();
    assert_eq!(changed.summary, "Late lunch");
    let listed = calendar.events_between(millis("2026-10-06T00:00:00Z"), millis("2026-10-07T00:00:00Z")).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].guests.len(), 1);
    calendar.delete_event(&made.id).await.unwrap();
    assert!(h.fake.with(|s| s.events.is_empty()));
}

#[tokio::test]
async fn importing_a_known_uid_updates_it_and_a_new_one_invites_nobody() {
    let h = outlook().await;
    h.fake.put_event("cal-1", GraphEvent { id: "k1".into(), ical_uid: Some("known@x".into()), subject: Some("Old".into()), ..GraphEvent::default() });
    let calendar = h.sync.services().calendar.clone().unwrap();
    let known = Event { calendar: "cal-1".into(), uid: "known@x".into(), title: "New".into(), ..Event::default() };
    let kept = calendar.import_event(&known).await.unwrap();
    assert_eq!((kept.id.as_str(), kept.title.as_str()), ("k1", "New"));
    assert_eq!(h.fake.with(|s| s.events.len()), 1);
    let guest = mailrs_domain::calendar::Guest { email: "bo@example.com".into(), ..Default::default() };
    let fresh = Event { calendar: "cal-1".into(), id: "pmx".into(), uid: "fresh@x".into(), title: "Fresh".into(), guests: vec![guest], ..Event::default() };
    let made = calendar.import_event(&fresh).await.unwrap();
    assert!(h.fake.with(|s| s.events[&made.id].1.attendees.is_empty()));
}

#[tokio::test]
async fn the_calendar_list_can_be_made_renamed_recolored_and_deleted() {
    let h = outlook().await;
    let calendar = h.sync.services().calendar.clone().unwrap();
    let made = calendar
        .edit_list("", &ListEdit::Create { name: "Work".into(), color: "#87d28e".into(), zone: String::new() })
        .await
        .unwrap()
        .unwrap();
    assert_eq!((made.name.as_str(), made.color.as_str()), ("Work", "#87d28e"));
    let renamed = calendar.edit_list(&made.id, &ListEdit::Rename { name: "Job".into() }).await.unwrap().unwrap();
    assert_eq!(renamed.name, "Job");
    let colored = calendar.edit_list(&made.id, &ListEdit::Recolor { color: "#f19696".into() }).await.unwrap().unwrap();
    assert_eq!(colored.color, "#f19696");
    assert!(calendar.edit_list(&made.id, &ListEdit::Delete).await.unwrap().is_none());
    assert_eq!(h.fake.with(|s| s.calendars.len()), 1);
}

/// An `Unsupported` answer keeps a queued change for good, and every later
/// calendar change of the account waits behind it. What Outlook can never
/// do is a refusal, which drops the change and says why.
#[tokio::test]
async fn what_outlook_cannot_do_is_refused_never_unsupported() {
    let h = outlook().await;
    let calendar = h.sync.services().calendar.clone().unwrap();
    let plain = |answer: Result<(), BackendError>| match answer {
        Err(BackendError::Refused(line)) => assert!(!line.is_empty()),
        other => panic!("expected a refusal, got {other:?}"),
    };
    let event = Event { calendar: "cal-1".into(), id: "e".into(), ..Event::default() };
    plain(calendar.move_event(&event, "cal-2", Notify::Guests).await.map(|_| ()));
    plain(calendar.upload_attachment(&Attachment::default(), Default::default()).await.map(|_| ()));
    plain(calendar.share_file("f", "a@b.c").await);
    for edit in [
        ListEdit::Unsubscribe,
        ListEdit::Hide { hidden: true },
        ListEdit::Subscribe { url: "https://example.com/a.ics".into() },
        ListEdit::Add,
    ] {
        plain(calendar.edit_list("cal-1", &edit).await.map(|_| ()));
    }
}

#[tokio::test]
async fn graph_mails_guests_whatever_the_person_chose_so_nobody_is_not_offered() {
    let h = outlook().await;
    assert!(!h.sync.services().offers().quiet_changes);
    let calendar = h.sync.services().calendar.clone().unwrap();
    let guest = |email: &str| Attendee { email_address: EmailAddress { address: Some(email.into()), name: None }, ..Attendee::default() };
    h.fake.put_event("cal-1", GraphEvent { id: "g1".into(), attendees: vec![guest("ana@example.com")], ..GraphEvent::default() });
    // A change asked to stay quiet still goes through; Graph does the mailing.
    let held = Event { calendar: "cal-1".into(), id: "g1".into(), title: "Moved".into(), ..Event::default() };
    calendar.put_event(&held, None, false, Notify::Nobody).await.unwrap();
    assert_eq!(h.fake.with(|s| s.events["g1"].1.subject.clone()).as_deref(), Some("Moved"));
}
