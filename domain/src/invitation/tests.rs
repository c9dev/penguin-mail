//! The parser against the shapes real invitations arrive in. Each fixture
//! keeps the quirks of the mailer it came from: Google's folded lines and
//! IANA zone names, Outlook's Windows zone names and quoted parameters,
//! and the escaped commas both of them send.

use chrono::{Datelike, TimeZone, Timelike};

use super::*;

/// Joins lines with CRLF, the way a mail part carries them.
fn ics(lines: &[&str]) -> String {
    format!("{}\r\n", lines.join("\r\n"))
}

fn google_invite() -> String {
    ics(&[
        "BEGIN:VCALENDAR",
        "PRODID:-//Google Inc//Google Calendar 70.9054//EN",
        "VERSION:2.0",
        "CALSCALE:GREGORIAN",
        "METHOD:REQUEST",
        "BEGIN:VTIMEZONE",
        "TZID:Europe/Lisbon",
        "BEGIN:DAYLIGHT",
        "TZOFFSETFROM:+0000",
        "TZOFFSETTO:+0100",
        "TZNAME:WEST",
        "DTSTART:19700329T010000",
        "RRULE:FREQ=YEARLY;BYMONTH=3;BYDAY=-1SU",
        "END:DAYLIGHT",
        "BEGIN:STANDARD",
        "TZOFFSETFROM:+0100",
        "TZOFFSETTO:+0000",
        "TZNAME:WET",
        "DTSTART:19701025T020000",
        "RRULE:FREQ=YEARLY;BYMONTH=10;BYDAY=-1SU",
        "END:STANDARD",
        "END:VTIMEZONE",
        "BEGIN:VEVENT",
        "DTSTART;TZID=Europe/Lisbon:20260612T150000",
        "DTEND;TZID=Europe/Lisbon:20260612T160000",
        "DTSTAMP:20260601T090000Z",
        "ORGANIZER;CN=Priya Raman:mailto:priya@fernwood.example",
        "UID:6k2v9d1qkq8p3nlo7a5fbe9gsk@google.com",
        "ATTENDEE;CUTYPE=INDIVIDUAL;ROLE=REQ-PARTICIPANT;PARTSTAT=NEEDS-ACTION;RSVP=TRU",
        " E;CN=Dana Reyes;X-NUM-GUESTS=0:mailto:dana.reyes@example.com",
        "ATTENDEE;CUTYPE=INDIVIDUAL;ROLE=REQ-PARTICIPANT;PARTSTAT=ACCEPTED;CN=Priya Ra",
        " man;X-NUM-GUESTS=0:mailto:priya@fernwood.example",
        "ATTENDEE;CUTYPE=INDIVIDUAL;ROLE=OPT-PARTICIPANT;PARTSTAT=TENTATIVE;CN=Jonas W",
        " eber;X-NUM-GUESTS=0:mailto:jonas@fernwood.example",
        "CREATED:20260601T085959Z",
        "DESCRIPTION:Bring the draft\\, and the numbers from May.",
        "LAST-MODIFIED:20260601T090000Z",
        "LOCATION:Meeting Room 2\\, Fernwood HQ",
        "SEQUENCE:0",
        "STATUS:CONFIRMED",
        "SUMMARY:Q4 roadmap review",
        "TRANSP:OPAQUE",
        "END:VEVENT",
        "END:VCALENDAR",
    ])
}

fn outlook_invite() -> String {
    ics(&[
        "BEGIN:VCALENDAR",
        "METHOD:REQUEST",
        "PRODID:Microsoft Exchange Server 2010",
        "VERSION:2.0",
        "BEGIN:VTIMEZONE",
        "TZID:W. Europe Standard Time",
        "BEGIN:STANDARD",
        "DTSTART:16011028T030000",
        "TZOFFSETFROM:+0200",
        "TZOFFSETTO:+0100",
        "RRULE:FREQ=YEARLY;INTERVAL=1;BYDAY=-1SU;BYMONTH=10",
        "END:STANDARD",
        "BEGIN:DAYLIGHT",
        "DTSTART:16010325T020000",
        "TZOFFSETFROM:+0100",
        "TZOFFSETTO:+0200",
        "RRULE:FREQ=YEARLY;INTERVAL=1;BYDAY=-1SU;BYMONTH=3",
        "END:DAYLIGHT",
        "END:VTIMEZONE",
        "BEGIN:VEVENT",
        "ORGANIZER;CN=\"Weber, Jonas\":mailto:jonas@fernwood.example",
        "ATTENDEE;ROLE=REQ-PARTICIPANT;PARTSTAT=NEEDS-ACTION;RSVP=TRUE;CN=\"Reyes, Dana",
        " \":mailto:dana.reyes@example.com",
        "DESCRIPTION;LANGUAGE=en-GB:\\nDial in on the usual bridge.\\n",
        "SUMMARY;LANGUAGE=en-GB:Budget review\\, Q1",
        "DTSTART;TZID=W. Europe Standard Time:20260305T100000",
        "DTEND;TZID=W. Europe Standard Time:20260305T1130",
        " 00",
        "UID:040000008200E00074C5B7101A82E00800000000A0",
        "CLASS:PUBLIC",
        "PRIORITY:5",
        "DTSTAMP:20260220T101500Z",
        "TRANSP:OPAQUE",
        "STATUS:CONFIRMED",
        "SEQUENCE:0",
        "LOCATION;LANGUAGE=en-GB:Room 3\\, Floor 2",
        "X-MICROSOFT-CDO-APPT-SEQUENCE:0",
        "X-MICROSOFT-CDO-BUSYSTATUS:TENTATIVE",
        "END:VEVENT",
        "END:VCALENDAR",
    ])
}

#[test]
fn a_google_invitation_reads_end_to_end() {
    let invitation = read(&google_invite()).expect("the part holds an event");
    assert_eq!(invitation.summary, "Q4 roadmap review");
    assert_eq!(
        invitation.location.as_deref(),
        Some("Meeting Room 2, Fernwood HQ")
    );
    assert_eq!(
        invitation.description.as_deref(),
        Some("Bring the draft, and the numbers from May.")
    );
    assert_eq!(invitation.method, Method::Request);
    assert_eq!(invitation.sequence, 0);
    assert_eq!(invitation.uid, "6k2v9d1qkq8p3nlo7a5fbe9gsk@google.com");
    assert_eq!(
        invitation.organizer.as_ref().map(|o| o.display()),
        Some("Priya Raman")
    );
    // 15:00 Lisbon in June is 14:00 UTC.
    let When::At { starts_at, ends_at } = invitation.when.expect("the event has a start") else {
        panic!("a timed event is not all day");
    };
    let start = chrono::Utc.timestamp_millis_opt(starts_at).unwrap();
    assert_eq!((start.hour(), start.day(), start.month()), (14, 12, 6));
    assert_eq!(ends_at, Some(starts_at + 60 * 60 * 1000));
    let names: Vec<&str> = invitation.guests.iter().map(|g| g.who.display()).collect();
    assert_eq!(names, ["Dana Reyes", "Priya Raman", "Jonas Weber"]);
    let answers: Vec<Option<Answer>> = invitation.guests.iter().map(|g| g.answer).collect();
    assert_eq!(answers, [None, Some(Answer::Yes), Some(Answer::Maybe)]);
    assert!(invitation.guests[2].optional, "Jonas is optional");
    assert_eq!(invitation.repeats, None);
}

#[test]
fn an_outlook_invitation_reads_its_windows_time_zone() {
    let invitation = read(&outlook_invite()).expect("the part holds an event");
    assert_eq!(invitation.summary, "Budget review, Q1");
    assert_eq!(invitation.location.as_deref(), Some("Room 3, Floor 2"));
    assert_eq!(
        invitation.organizer.as_ref().map(|o| o.display()),
        Some("Weber, Jonas")
    );
    // 10:00 Berlin in March is 09:00 UTC, and the folded DTEND is 11:30.
    let When::At { starts_at, ends_at } = invitation.when.expect("the event has a start") else {
        panic!("a timed event is not all day");
    };
    let start = chrono::Utc.timestamp_millis_opt(starts_at).unwrap();
    assert_eq!((start.hour(), start.minute()), (9, 0));
    assert_eq!(ends_at, Some(starts_at + 90 * 60 * 1000));
    assert_eq!(invitation.guests[0].who.display(), "Reyes, Dana");
    assert_eq!(invitation.guests[0].who.email, "dana.reyes@example.com");
}

#[test]
fn an_unknown_time_zone_falls_back_to_the_offset_the_file_carries() {
    let invitation = read(&ics(&[
        "BEGIN:VCALENDAR",
        "METHOD:REQUEST",
        "BEGIN:VTIMEZONE",
        "TZID:Customer Site Time",
        "BEGIN:STANDARD",
        "DTSTART:16011028T030000",
        "TZOFFSETFROM:+0300",
        "TZOFFSETTO:+0230",
        "END:STANDARD",
        "END:VTIMEZONE",
        "BEGIN:VEVENT",
        "UID:offset-1",
        "SUMMARY:Site visit",
        "DTSTART;TZID=Customer Site Time:20260401T120000",
        "DURATION:PT45M",
        "END:VEVENT",
        "END:VCALENDAR",
    ]))
    .expect("the part holds an event");
    let When::At { starts_at, ends_at } = invitation.when.expect("the event has a start") else {
        panic!("a timed event is not all day");
    };
    let start = chrono::Utc.timestamp_millis_opt(starts_at).unwrap();
    assert_eq!((start.hour(), start.minute()), (9, 30));
    assert_eq!(ends_at, Some(starts_at + 45 * 60 * 1000));
}

#[test]
fn an_all_day_event_keeps_the_days_it_covers() {
    let invitation = read(&ics(&[
        "BEGIN:VCALENDAR",
        "METHOD:REQUEST",
        "BEGIN:VEVENT",
        "UID:allday-1",
        "SUMMARY:Team offsite",
        "DTSTART;VALUE=DATE:20260714",
        "DTEND;VALUE=DATE:20260717",
        "END:VEVENT",
        "END:VCALENDAR",
    ]))
    .expect("the part holds an event");
    let when = invitation.when.expect("the event has a start");
    assert!(when.all_day());
    let When::Days { first, last } = when else {
        panic!("an all-day event keeps days");
    };
    // DTEND stops before the 17th, so the last day is the 16th.
    assert_eq!(first, chrono::NaiveDate::from_ymd_opt(2026, 7, 14).unwrap());
    assert_eq!(last, chrono::NaiveDate::from_ymd_opt(2026, 7, 16).unwrap());
}

#[test]
fn a_one_day_all_day_event_needs_no_end() {
    let invitation = read(&ics(&[
        "BEGIN:VCALENDAR",
        "BEGIN:VEVENT",
        "UID:allday-2",
        "DTSTART;VALUE=DATE:20260714",
        "SUMMARY:Public holiday",
        "END:VEVENT",
        "END:VCALENDAR",
    ]))
    .expect("the part holds an event");
    let When::Days { first, last } = invitation.when.expect("the event has a start") else {
        panic!("an all-day event keeps days");
    };
    assert_eq!(first, last);
}

#[test]
fn a_repeating_event_says_how_it_repeats() {
    let weekly = ics(&[
        "BEGIN:VCALENDAR",
        "METHOD:REQUEST",
        "BEGIN:VEVENT",
        "UID:weekly-1",
        "SUMMARY:Standup",
        "DTSTART:20260105T090000Z",
        "DTEND:20260105T091500Z",
        "RRULE:FREQ=WEEKLY;BYDAY=MO;UNTIL=20260630T090000Z",
        "END:VEVENT",
        "END:VCALENDAR",
    ]);
    assert_eq!(
        read(&weekly).unwrap().repeats.as_deref(),
        Some("Every Monday until 30 June")
    );
}

#[test]
fn recurrence_words_cover_the_rules_organizers_send() {
    let say = |rule: &str| recurrence::in_words(rule, Some(2026));
    assert_eq!(say("FREQ=DAILY").as_deref(), Some("Every day"));
    assert_eq!(
        say("FREQ=DAILY;INTERVAL=3").as_deref(),
        Some("Every 3 days")
    );
    assert_eq!(
        say("FREQ=WEEKLY;BYDAY=MO,WE,FR").as_deref(),
        Some("Every Monday, Wednesday and Friday")
    );
    assert_eq!(
        say("FREQ=WEEKLY;INTERVAL=2;BYDAY=TH").as_deref(),
        Some("Every 2 weeks on Thursday")
    );
    assert_eq!(
        say("FREQ=MONTHLY;BYMONTHDAY=15").as_deref(),
        Some("Every month on the 15th")
    );
    assert_eq!(
        say("FREQ=MONTHLY;BYDAY=2TU").as_deref(),
        Some("Every month on the second Tuesday")
    );
    assert_eq!(
        say("FREQ=MONTHLY;BYDAY=-1FR").as_deref(),
        Some("Every month on the last Friday")
    );
    assert_eq!(say("FREQ=YEARLY").as_deref(), Some("Every year"));
    assert_eq!(
        say("FREQ=WEEKLY;BYDAY=TU;COUNT=6").as_deref(),
        Some("Every Tuesday, 6 times")
    );
    // A rule that ends in another year says which.
    assert_eq!(
        say("FREQ=WEEKLY;BYDAY=MO;UNTIL=20270301").as_deref(),
        Some("Every Monday until 1 March 2027")
    );
    assert_eq!(say("FREQ=FORTNIGHTLY"), None);
    assert_eq!(say("nonsense"), None);
}

#[test]
fn durations_read_as_lengths() {
    assert_eq!(recurrence::duration("PT1H30M"), Some(Duration::minutes(90)));
    assert_eq!(recurrence::duration("P1D"), Some(Duration::days(1)));
    assert_eq!(recurrence::duration("P1W"), Some(Duration::days(7)));
    assert_eq!(recurrence::duration("-PT15M"), Some(Duration::minutes(-15)));
    assert_eq!(recurrence::duration("PT"), Some(Duration::zero()));
    assert_eq!(recurrence::duration("1H"), None);
    assert_eq!(recurrence::duration("PT1X"), None);
    assert_eq!(recurrence::duration("PT99"), None);
}

#[test]
fn a_cancellation_says_the_event_is_off() {
    let invitation = read(&ics(&[
        "BEGIN:VCALENDAR",
        "METHOD:CANCEL",
        "BEGIN:VEVENT",
        "UID:6k2v9d1qkq8p3nlo7a5fbe9gsk@google.com",
        "SEQUENCE:3",
        "STATUS:CANCELLED",
        "SUMMARY:Q4 roadmap review",
        "DTSTART:20260612T140000Z",
        "END:VEVENT",
        "END:VCALENDAR",
    ]))
    .expect("the part holds an event");
    assert!(invitation.cancelled());
    assert_eq!(invitation.method, Method::Cancel);
    assert_eq!(invitation.sequence, 3);
}

#[test]
fn a_status_of_cancelled_counts_even_without_the_method() {
    let invitation = read(&ics(&[
        "BEGIN:VCALENDAR",
        "METHOD:REQUEST",
        "BEGIN:VEVENT",
        "UID:cancel-2",
        "STATUS:CANCELLED",
        "DTSTART:20260612T140000Z",
        "END:VEVENT",
        "END:VCALENDAR",
    ]))
    .expect("the part holds an event");
    assert!(invitation.cancelled());
}

#[test]
fn an_update_carries_a_higher_sequence_under_the_same_uid() {
    let first = read(&google_invite()).unwrap();
    let second = read(&ics(&[
        "BEGIN:VCALENDAR",
        "METHOD:REQUEST",
        "BEGIN:VEVENT",
        "UID:6k2v9d1qkq8p3nlo7a5fbe9gsk@google.com",
        "SEQUENCE:2",
        "SUMMARY:Q4 roadmap review",
        "DTSTART;TZID=Europe/Lisbon:20260613T150000",
        "DTEND;TZID=Europe/Lisbon:20260613T160000",
        "END:VEVENT",
        "END:VCALENDAR",
    ]))
    .unwrap();
    assert_eq!(first.uid, second.uid);
    assert!(second.sequence > first.sequence);
    assert_ne!(first.when, second.when);
}

#[test]
fn a_reply_carries_the_answer_the_guest_gave() {
    let invitation = read(&ics(&[
        "BEGIN:VCALENDAR",
        "METHOD:REPLY",
        "BEGIN:VEVENT",
        "UID:reply-1",
        "SUMMARY:Q4 roadmap review",
        "DTSTART:20260612T140000Z",
        "ATTENDEE;PARTSTAT=DECLINED;CN=Jonas Weber:mailto:jonas@fernwood.example",
        "ORGANIZER:mailto:priya@fernwood.example",
        "END:VEVENT",
        "END:VCALENDAR",
    ]))
    .expect("the part holds an event");
    assert_eq!(invitation.method, Method::Reply);
    assert_eq!(invitation.guests[0].answer, Some(Answer::No));
    // An organizer with no CN shows its address.
    assert_eq!(
        invitation.organizer.as_ref().map(|o| o.display()),
        Some("priya@fernwood.example")
    );
}

#[test]
fn the_user_finds_themselves_among_the_guests() {
    let invitation = read(&google_invite()).unwrap();
    let me = vec!["DANA.REYES@example.com".to_string()];
    assert_eq!(
        invitation.me(&me).map(|g| g.who.email.as_str()),
        Some("dana.reyes@example.com")
    );
    assert!(invitation.me(&["nobody@example.com".to_string()]).is_none());
}

#[test]
fn malformed_parts_give_nothing_back_and_never_panic() {
    let broken = [
        "",
        "   ",
        "not a calendar at all",
        "BEGIN:VCALENDAR",
        "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nSUMMARY:cut off halfway",
        "BEGIN:VCALENDAR\r\nBEGIN:VTODO\r\nUID:t1\r\nEND:VTODO\r\nEND:VCALENDAR\r\n",
        "BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n",
        "\u{0}\u{1}\u{2}",
        "BEGIN:VCALENDAR\nBEGIN:VEVENT\nDTSTART:not-a-date\nUID:1\nEND:VEVENT\nEND:VCALENDAR\n",
    ];
    for text in broken {
        let read_it = std::panic::catch_unwind(|| read(text));
        assert!(read_it.is_ok(), "panicked on {text:?}");
        if let Ok(Some(invitation)) = read_it {
            // The one shape that does parse is the date that will not: it
            // still gives an event, just without a time.
            assert!(invitation.when.is_none(), "{text:?}");
        }
    }
}

#[test]
fn a_bare_newline_and_a_byte_order_mark_do_not_stop_the_parser() {
    let invitation = read(
        "\u{feff}begin:vcalendar\nmethod:request\nbegin:vevent\nuid:lf-1\nsummary:Lunch\n\
         dtstart:20260612T110000Z\nend:vevent\nend:vcalendar\n",
    )
    .expect("the part holds an event");
    assert_eq!(invitation.summary, "Lunch");
    assert_eq!(invitation.uid, "lf-1");
}

#[test]
fn control_characters_never_reach_a_label() {
    let invitation = read(&ics(&[
        "BEGIN:VCALENDAR",
        "BEGIN:VEVENT",
        "UID:ctl-1",
        "SUMMARY:Review\u{0}\u{7} meeting",
        "DTSTART:20260612T140000Z",
        "END:VEVENT",
        "END:VCALENDAR",
    ]))
    .expect("the part holds an event");
    assert_eq!(invitation.summary, "Review meeting");
}

#[test]
fn an_answer_survives_a_round_trip_through_its_stored_form() {
    for answer in Answer::ALL {
        assert_eq!(answer.as_str().parse::<Answer>().unwrap(), answer);
    }
    assert!("later".parse::<Answer>().is_err());
}

/// An invitation to one occurrence of a weekly event, as Google sends one
/// when the organizer moves a single stand-up.
fn one_occurrence() -> String {
    ics(&[
        "BEGIN:VCALENDAR",
        "METHOD:REQUEST",
        "BEGIN:VEVENT",
        "UID:standup@google.com",
        "SEQUENCE:3",
        "RECURRENCE-ID;TZID=Europe/Lisbon:20260612T093000",
        "DTSTART;TZID=Europe/Lisbon:20260612T100000",
        "DTEND;TZID=Europe/Lisbon:20260612T103000",
        "SUMMARY:Stand-up",
        "ORGANIZER;CN=Priya Raman:mailto:priya@fernwood.example",
        "ATTENDEE;PARTSTAT=NEEDS-ACTION;CN=Dana Reyes:mailto:dana.reyes@example.com",
        "ATTENDEE;PARTSTAT=ACCEPTED;CN=Jonas Weber:mailto:jonas@fernwood.example",
        "END:VEVENT",
        "END:VCALENDAR",
    ])
}

fn me() -> Address {
    Address {
        name: Some("Dana Reyes".into()),
        email: "dana.reyes@example.com".into(),
    }
}

/// The properties of a written object, unfolded the way a reader unfolds
/// them: a continuation line opens with a space and joins the one before.
fn properties(object: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in object.split("\r\n").filter(|line| !line.is_empty()) {
        match line.strip_prefix(' ') {
            Some(rest) => out
                .last_mut()
                .expect("a fold follows a property")
                .push_str(rest),
            None => out.push(line.to_string()),
        }
    }
    out
}

#[test]
fn a_reply_carries_what_an_organizer_matches_it_against() {
    let invitation = read(&google_invite()).unwrap();
    let object = reply(
        &invitation,
        &me(),
        Answer::Yes,
        Scope::Series,
        1_780_000_000_000,
    );
    let lines = properties(&object);

    assert!(lines.contains(&"METHOD:REPLY".to_string()));
    assert!(lines.contains(&"UID:6k2v9d1qkq8p3nlo7a5fbe9gsk@google.com".to_string()));
    assert!(lines.contains(&format!("SEQUENCE:{}", invitation.sequence)));
    assert!(lines.contains(&"ORGANIZER;CN=Priya Raman:mailto:priya@fernwood.example".to_string()));
    assert!(lines.iter().any(|line| line.starts_with("DTSTAMP:")));
    assert_eq!(lines.first().map(String::as_str), Some("BEGIN:VCALENDAR"));
    assert_eq!(lines.last().map(String::as_str), Some("END:VCALENDAR"));
}

#[test]
fn a_reply_names_the_one_person_answering() {
    let invitation = read(&google_invite()).unwrap();
    for answer in Answer::ALL {
        let object = reply(&invitation, &me(), answer, Scope::Series, 1_780_000_000_000);
        let attendees: Vec<String> = properties(&object)
            .into_iter()
            .filter(|line| line.starts_with("ATTENDEE"))
            .collect();
        assert_eq!(
            attendees,
            vec![format!(
                "ATTENDEE;PARTSTAT={};CN=Dana Reyes:mailto:dana.reyes@example.com",
                answer.partstat()
            )]
        );
    }
}

#[test]
fn a_reply_ends_every_line_with_crlf_and_folds_at_75_octets() {
    let long = Address {
        name: Some("Dana Reyes of the Fernwood Planning and Scheduling Office".into()),
        email: "dana.reyes.planning.scheduling@example.com".into(),
    };
    let invitation = read(&google_invite()).unwrap();
    let object = reply(
        &invitation,
        &long,
        Answer::Maybe,
        Scope::Series,
        1_780_000_000_000,
    );

    assert!(object.ends_with("END:VCALENDAR\r\n"));
    assert!(!object.contains("\n\r"));
    for line in object.split("\r\n") {
        assert!(line.len() <= 75, "{line:?} runs past 75 octets");
        assert!(!line.contains('\n'), "a lone newline in {line:?}");
    }
    assert!(
        object.contains("\r\n "),
        "the long attendee should have been folded"
    );
    // Folding loses nothing: unfolding gives the address back whole.
    assert!(
        properties(&object)
            .iter()
            .any(|line| line.ends_with("mailto:dana.reyes.planning.scheduling@example.com"))
    );
}

#[test]
fn a_reply_to_one_occurrence_says_which_one() {
    let invitation = read(&one_occurrence()).unwrap();
    let occurrence = invitation.occurrence.as_ref().expect("one occurrence");
    assert_eq!(occurrence.written, ";TZID=Europe/Lisbon:20260612T093000");

    let one = reply(
        &invitation,
        &me(),
        Answer::No,
        Scope::Occurrence,
        1_780_000_000_000,
    );
    assert!(
        properties(&one).contains(&"RECURRENCE-ID;TZID=Europe/Lisbon:20260612T093000".to_string())
    );

    // The series is the same event with no occurrence named.
    let series = reply(
        &invitation,
        &me(),
        Answer::No,
        Scope::Series,
        1_780_000_000_000,
    );
    assert!(!series.contains("RECURRENCE-ID"));
    assert!(properties(&series).contains(&"UID:standup@google.com".to_string()));
}

#[test]
fn a_reply_answers_in_utc_whichever_zone_the_invitation_named() {
    let invitation = read(&outlook_invite()).unwrap();
    let lines = properties(&reply(
        &invitation,
        &me(),
        Answer::Yes,
        Scope::Series,
        1_780_000_000_000,
    ));
    assert!(lines.contains(&"DTSTART:20260305T090000Z".to_string()));
    assert!(lines.contains(&"DTEND:20260305T103000Z".to_string()));
    assert!(lines.contains(&"SUMMARY:Budget review\\, Q1".to_string()));
}

#[test]
fn an_all_day_reply_keeps_the_days_the_event_covers() {
    let invitation = read(&ics(&[
        "BEGIN:VCALENDAR",
        "METHOD:REQUEST",
        "BEGIN:VEVENT",
        "UID:offsite-1",
        "DTSTART;VALUE=DATE:20260612",
        "DTEND;VALUE=DATE:20260614",
        "SUMMARY:Offsite",
        "END:VEVENT",
        "END:VCALENDAR",
    ]))
    .unwrap();
    let lines = properties(&reply(
        &invitation,
        &me(),
        Answer::Yes,
        Scope::Series,
        1_780_000_000_000,
    ));
    assert!(lines.contains(&"DTSTART;VALUE=DATE:20260612".to_string()));
    assert!(lines.contains(&"DTEND;VALUE=DATE:20260614".to_string()));
}

#[test]
fn a_counter_proposes_another_time_for_the_same_event() {
    let invitation = read(&google_invite()).unwrap();
    let proposed = When::At {
        starts_at: 1_781_000_000_000,
        ends_at: Some(1_781_003_600_000),
    };
    let lines = properties(&counter(
        &invitation,
        &me(),
        &proposed,
        Scope::Series,
        1_780_000_000_000,
    ));
    assert!(lines.contains(&"METHOD:COUNTER".to_string()));
    assert!(lines.contains(&"UID:6k2v9d1qkq8p3nlo7a5fbe9gsk@google.com".to_string()));
    assert!(lines.contains(&"DTSTART:20260609T101320Z".to_string()));
    assert!(lines.contains(&"DTEND:20260609T111320Z".to_string()));
    // A proposal says nothing about coming, so the answer stays open.
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("ATTENDEE;PARTSTAT=TENTATIVE"))
    );
}

#[test]
fn a_name_that_would_end_a_parameter_is_quoted() {
    let invitation = read(&outlook_invite()).unwrap();
    let lines = properties(&reply(
        &invitation,
        &Address {
            name: Some("Reyes, Dana".into()),
            email: "dana.reyes@example.com".into(),
        },
        Answer::Yes,
        Scope::Series,
        1_780_000_000_000,
    ));
    assert!(lines.contains(
        &"ATTENDEE;PARTSTAT=ACCEPTED;CN=\"Reyes, Dana\":mailto:dana.reyes@example.com".to_string()
    ));
    assert!(
        lines.contains(&"ORGANIZER;CN=\"Weber, Jonas\":mailto:jonas@fernwood.example".to_string())
    );
}
