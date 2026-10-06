//! A VCALENDAR resource read into Penguin Mail's events, and written back
//! with only the edited lines changed.

use mailrs_dav::ical::{answer, answer_scheduled, cancel_occurrence, read_resource, write_event, write_event_notifying};
use mailrs_domain::calendar::{Notify, Reminder, ReminderMethod, Status};
use mailrs_domain::invitation::Answer;

const ME: &[String] = &[];
const NOW: i64 = 1_790_000_000_000;

fn fixture(name: &str) -> String {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(path).unwrap().replace("\r\n", "\n").replace('\n', "\r\n")
}

fn me() -> Vec<String> {
    vec!["me@fastmail.com".to_string()]
}

/// 2026-10-05 09:30 in Lisbon, which is 08:30 UTC under summer time.
const MONDAY_0930: i64 = 1_791_189_000_000;

#[test]
fn a_series_reads_as_its_master_and_its_changed_occurrence() {
    let read = read_resource(&fixture("google-series.ics"), "/cal/work/", "/cal/work/standup.ics", "\"7\"", &me()).unwrap();
    assert_eq!(read.series.as_deref(), Some("standup"));
    let master = &read.events[0];
    assert_eq!(master.id, "standup");
    assert_eq!(master.uid, "standup-2026@google.com");
    assert_eq!(master.etag, "\"7\"");
    assert_eq!(master.calendar, "/cal/work/");
    assert_eq!(master.start, MONDAY_0930);
    assert_eq!(master.end - master.start, 15 * 60_000);
    assert_eq!(master.zone, "Europe/Lisbon");
    assert_eq!(master.place, "Room 4, second floor");
    assert_eq!(master.rules.len(), 2, "{:?}", master.rules);
    assert!(master.rules[0].starts_with("RRULE:FREQ=WEEKLY"));
    assert!(master.rules[1].starts_with("EXDATE;TZID=Europe/Lisbon:20261012T093000"));
    assert_eq!(master.my_answer, None, "NEEDS-ACTION is no answer yet");
    assert_eq!(master.organizer.as_deref(), Some("ann@example.com"));
    assert_eq!(master.reminders, Some(vec![Reminder { minutes: 10, method: ReminderMethod::Notification }]));
    let moved = &read.events[1];
    assert_eq!(moved.series.as_deref(), Some("standup"));
    assert_eq!(moved.original_start, Some(MONDAY_0930 + 2 * 24 * 3_600_000));
    assert_eq!(moved.id, "standup_20261007T083000Z");
    assert_eq!(moved.my_answer, Some(Answer::Yes));
}

#[test]
fn an_all_day_apple_event_is_free_private_and_two_days_long() {
    let read = read_resource(&fixture("apple-event.ics"), "/cal/home/", "/cal/home/9A4C.ics", "e1", ME).unwrap();
    let trip = &read.events[0];
    assert!(trip.all_day);
    assert_eq!(trip.zone, "UTC");
    assert_eq!(trip.end - trip.start, 2 * 24 * 3_600_000);
    assert!(!trip.busy);
    assert!(trip.private);
    assert_eq!(read.series, None);
}

#[test]
fn a_windows_zone_name_resolves_by_its_offsets() {
    let read = read_resource(&fixture("windows-zone.ics"), "/c/", "/c/w.ics", "e", ME).unwrap();
    let event = &read.events[0];
    // 14:00 at +02:00 on 20 October 2026.
    assert_eq!(event.start, 1_792_497_600_000);
    assert!(event.zone.starts_with("Europe/"), "{}", event.zone);
}

#[test]
fn an_edit_keeps_every_line_it_did_not_touch() {
    let text = fixture("google-series.ics");
    let read = read_resource(&text, "/cal/work/", "/cal/work/standup.ics", "\"7\"", &me()).unwrap();
    let mut master = read.events[0].clone();
    master.title = "Standup (short)".into();
    let written = write_event(Some(&text), &master, &me(), NOW).unwrap();
    for kept in [
        "X-WR-CALNAME:Work",
        "X-GOOGLE-CONFERENCE:https://meet.google.com/abc-defg-hij",
        "X-NUM-GUESTS=0",
        "X-WR-ALARMUID:1C3E0D6B-7F6A-4C1E-9A2B-3F1E8C9D0A11",
        "RECURRENCE-ID;TZID=Europe/Lisbon:20261007T093000",
        "SUMMARY:Standup\\, moved",
    ] {
        assert!(written.contains(kept), "lost {kept}:\n{written}");
    }
    assert!(written.contains("SUMMARY:Standup (short)"));
    assert_eq!(written.matches("BEGIN:VALARM").count(), 1, "an unchanged reminder is not rewritten");
    let again = read_resource(&written, "/cal/work/", "/cal/work/standup.ics", "\"8\"", &me()).unwrap();
    assert_eq!(again.events[0].title, "Standup (short)");
    assert_eq!(again.events[0].rules, read.events[0].rules);
}

#[test]
fn a_new_changed_occurrence_joins_the_resource_under_its_recurrence_id() {
    let text = fixture("google-series.ics");
    let read = read_resource(&text, "/cal/work/", "/cal/work/standup.ics", "\"7\"", &me()).unwrap();
    let master = &read.events[0];
    let original = master.start + 7 * 24 * 3_600_000 + 2 * 24 * 3_600_000; // Wednesday 14 October
    let mut moved = master.clone();
    moved.rules.clear();
    moved.series = Some(master.id.clone());
    moved.original_start = Some(original);
    moved.start = original + 3_600_000;
    moved.end = moved.start + 15 * 60_000;
    let written = write_event(Some(&text), &moved, &me(), NOW).unwrap();
    assert!(written.contains("RECURRENCE-ID;TZID=Europe/Lisbon:20261014T093000"), "{written}");
    let again = read_resource(&written, "/cal/work/", "/cal/work/standup.ics", "\"8\"", &me()).unwrap();
    assert_eq!(again.events.len(), 3);
}

#[test]
fn a_new_event_is_a_whole_calendar_with_its_zone() {
    let mut event = mailrs_domain::calendar::Event {
        calendar: "/cal/work/".into(),
        id: "pmabc".into(),
        uid: "pmabc@penguin-mail".into(),
        start: MONDAY_0930,
        end: MONDAY_0930 + 3_600_000,
        zone: "Europe/Lisbon".into(),
        title: "Lunch".into(),
        busy: true,
        status: Status::Confirmed,
        ..Default::default()
    };
    event.reminders = Some(vec![Reminder { minutes: 5, method: ReminderMethod::Notification }]);
    let written = write_event(None, &event, &me(), NOW).unwrap();
    assert!(written.starts_with("BEGIN:VCALENDAR\r\n"));
    assert!(written.contains("PRODID:-//Penguin Mail//EN"));
    assert!(written.contains("BEGIN:VTIMEZONE"), "a TZID the file names gets its VTIMEZONE");
    assert!(written.contains("DTSTART;TZID=Europe/Lisbon:20261005T093000"));
    assert!(written.contains("TRIGGER:-PT5M"));
    let read = read_resource(&written, "/cal/work/", "/cal/work/pmabc.ics", "e", &me()).unwrap();
    assert_eq!(read.events[0].start, MONDAY_0930);
}

#[test]
fn cancelling_an_occurrence_adds_an_exdate_and_drops_its_change() {
    let text = fixture("google-series.ics");
    let moved_original = MONDAY_0930 + 2 * 24 * 3_600_000;
    let written = cancel_occurrence(&text, moved_original, NOW).unwrap().expect("the series stays");
    assert!(written.contains("EXDATE;TZID=Europe/Lisbon:20261007T093000"), "{written}");
    assert!(!written.contains("RECURRENCE-ID"));
}

#[test]
fn an_answer_marks_my_attendee_and_keeps_the_server_quiet() {
    let text = fixture("google-series.ics");
    let written = answer(&text, &me(), Answer::No, NOW).unwrap().expect("I am a guest");
    assert!(written.contains("PARTSTAT=DECLINED"), "{written}");
    assert!(written.contains("SCHEDULE-AGENT=CLIENT"));
    assert!(written.contains("PARTSTAT=ACCEPTED:mailto:ann@example.com"), "Ann's answer stays hers");
    assert_eq!(answer(&text, &["nobody@example.com".to_string()], Answer::Yes, NOW).unwrap(), None);
}

#[test]
fn the_sequence_and_the_attachments_are_read() {
    let text = fixture("google-series.ics").replace(
        "SEQUENCE:2\r\n",
        "SEQUENCE:2\r\nATTACH;FMTTYPE=application/pdf;FILENAME=agenda.pdf:https://files.example.com/agenda.pdf\r\n",
    );
    let read = read_resource(&text, "/c/", "/c/standup.ics", "e", &me()).unwrap();
    assert_eq!(read.events[0].sequence, 2);
    let files = read.events[0].attachments.as_ref().expect("read, so Some");
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].file_url, "https://files.example.com/agenda.pdf");
    assert_eq!(files[0].mime_type, "application/pdf");
    assert_eq!(files[0].title, "agenda.pdf");
    let none = read_resource(&fixture("apple-event.ics"), "/c/", "/c/a.ics", "e", ME).unwrap();
    assert_eq!(none.events[0].attachments, Some(vec![]));
}

#[test]
fn an_edit_leaves_attach_lines_as_the_server_sent_them() {
    let text = fixture("google-series.ics").replace(
        "SEQUENCE:2\r\n",
        "SEQUENCE:2\r\nATTACH;FMTTYPE=application/pdf:https://files.example.com/agenda.pdf\r\n",
    );
    let read = read_resource(&text, "/c/", "/c/standup.ics", "e", &me()).unwrap();
    let mut master = read.events[0].clone();
    master.title = "Changed".into();
    let written = write_event(Some(&text), &master, &me(), NOW).unwrap();
    assert!(written.contains("ATTACH;FMTTYPE=application/pdf:https://files.example.com/agenda.pdf"), "{written}");
}

#[test]
fn telling_nobody_marks_every_attendee_client_scheduled() {
    let text = fixture("google-series.ics");
    let read = read_resource(&text, "/c/", "/c/standup.ics", "e", &me()).unwrap();
    let mut master = read.events[0].clone();
    master.title = "Changed".into();
    let quiet = write_event_notifying(Some(&text), &master, &me(), NOW, Notify::Nobody).unwrap();
    assert_eq!(quiet.matches("SCHEDULE-AGENT=CLIENT").count(), 2, "{quiet}");
    let loud = write_event_notifying(Some(&text), &master, &me(), NOW, Notify::Guests).unwrap();
    assert!(!loud.contains("SCHEDULE-AGENT"), "{loud}");
}

#[test]
fn an_answer_for_a_scheduling_server_leaves_the_scheduling_to_it() {
    let text = fixture("google-series.ics");
    let written = answer_scheduled(&text, &me(), Answer::Yes, NOW, true).unwrap().expect("I am a guest");
    assert!(written.contains("PARTSTAT=ACCEPTED"), "{written}");
    assert!(!written.contains("SCHEDULE-AGENT"), "{written}");
}

#[test]
fn an_edit_by_the_organizer_raises_the_sequence() {
    let text = fixture("google-series.ics");
    let ann = vec!["ann@example.com".to_string()];
    let read = read_resource(&text, "/c/", "/c/standup.ics", "e", &ann).unwrap();
    let mut master = read.events[0].clone();
    master.title = "Standup (short)".into();
    let written = write_event(Some(&text), &master, &ann, NOW).unwrap();
    assert_eq!(written.matches("SEQUENCE:2").count(), 0, "{written}");
    assert_eq!(written.matches("SEQUENCE:3").count(), 2, "{written}");
}

#[test]
fn an_edit_by_a_guest_keeps_the_organizer_sequence() {
    let text = fixture("google-series.ics");
    let read = read_resource(&text, "/c/", "/c/standup.ics", "e", &me()).unwrap();
    let mut master = read.events[0].clone();
    master.title = "Standup (short)".into();
    let written = write_event(Some(&text), &master, &me(), NOW).unwrap();
    assert!(written.contains("SUMMARY:Standup (short)"), "{written}");
    assert_eq!(written.matches("SEQUENCE:2").count(), 1, "{written}");
    assert_eq!(written.matches("SEQUENCE:3").count(), 1, "{written}");
}

#[test]
fn an_answer_keeps_the_organizer_sequence() {
    let text = fixture("google-series.ics");
    let written = answer_scheduled(&text, &me(), Answer::Yes, NOW, true).unwrap().expect("I am a guest");
    assert_eq!(written.matches("SEQUENCE:2").count(), 1, "{written}");
    assert_eq!(written.matches("SEQUENCE:3").count(), 1, "{written}");
    assert!(!written.contains("SEQUENCE:4"), "{written}");
}

#[test]
fn an_edit_to_an_event_without_guests_raises_the_sequence() {
    let text = fixture("apple-event.ics");
    let read = read_resource(&text, "/c/", "/c/a.ics", "e", ME).unwrap();
    let mut event = read.events[0].clone();
    event.title = "Renamed".into();
    let written = write_event(Some(&text), &event, ME, NOW).unwrap();
    let before = read.events[0].sequence;
    let after = read_resource(&written, "/c/", "/c/a.ics", "e", ME).unwrap().events[0].sequence;
    assert_eq!(after, before + 1, "{written}");
}

#[test]
fn an_outlook_exdate_in_a_windows_zone_keeps_the_series_repeating() {
    let text = fixture("windows-zone.ics").replace(
        "SUMMARY:Call\r\n",
        "SUMMARY:Call\r\nRRULE:FREQ=WEEKLY;COUNT=4\r\nEXDATE;TZID=\"W. Europe Standard Time\":20261027T140000\r\n",
    );
    let read = read_resource(&text, "/c/", "/c/w.ics", "e", ME).unwrap();
    let event = &read.events[0];
    assert!(event.rules[1].starts_with(&format!("EXDATE;TZID={}:", event.zone)), "{:?}", event.rules);
    let week = 7 * 24 * 3_600_000;
    let shown = mailrs_domain::calendar::expand(event, event.start, event.start + 5 * week);
    assert_eq!(shown.len(), 3, "four weeks less the one skipped: {shown:?}");
}
