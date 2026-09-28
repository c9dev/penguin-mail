//! Answering an invitation, and asking what else the user has on, against
//! a stand-in Calendar API. No network: `wiremock` answers the calls and
//! checks what went out.

use mailrs_domain::calendar::{Access, Notify};
use mailrs_domain::invitation::Answer;
use mailrs_gmail::{
    Answered, EventFields, EventTime, GmailClient, GmailError, OAuthClient, Series,
};
use serde_json::{Value, json};
use wiremock::matchers::{body_json, body_partial_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const CALENDAR: &str = "/calendar/v3";
const UID: &str = "6k2v9d1qkq8p3nlo7a5fbe9gsk@google.com";
const EVENT: &str = "ev-1";

async fn mount_token(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"access_token": "at-1", "expires_in": 3600})),
        )
        .mount(server)
        .await;
}

fn client(server: &MockServer) -> GmailClient {
    let oauth = OAuthClient::new("cid", "secret").with_endpoints(
        format!("{}/auth", server.uri()),
        format!("{}/token", server.uri()),
    );
    GmailClient::new(oauth, "rt".into())
        .with_calendar_base_url(format!("{}{CALENDAR}", server.uri()))
}

/// The event Google made from the invitation, with three guests.
fn found() -> Value {
    json!({"items": [{
        "id": EVENT,
        "iCalUID": UID,
        "summary": "Q4 roadmap review",
        "organizer": {"email": "priya@fernwood.example"},
        "attendees": [
            {"email": "priya@fernwood.example", "responseStatus": "accepted",
             "organizer": true, "displayName": "Priya Raman"},
            {"email": "me@example.com", "responseStatus": "needsAction", "self": true},
            {"email": "jonas@fernwood.example", "responseStatus": "tentative"}
        ]
    }]})
}

async fn mount_search(server: &MockServer, body: Value) {
    Mock::given(method("GET"))
        .and(path(format!("{CALENDAR}/calendars/primary/events")))
        .and(query_param("iCalUID", UID))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}

#[tokio::test]
async fn an_answer_keeps_every_other_guest_as_google_has_them() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    mount_search(&server, found()).await;
    Mock::given(method("PATCH"))
        .and(path(format!("{CALENDAR}/calendars/primary/events/{EVENT}")))
        .and(query_param("sendUpdates", "all"))
        .and(body_json(json!({"attendees": [
            {"email": "priya@fernwood.example", "responseStatus": "accepted",
             "organizer": true, "displayName": "Priya Raman"},
            {"email": "me@example.com", "responseStatus": "declined", "self": true},
            {"email": "jonas@fernwood.example", "responseStatus": "tentative"}
        ]})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": EVENT})))
        .expect(1)
        .mount(&server)
        .await;

    let answered = client(&server)
        .answer_invitation(UID, "me@example.com", Answer::No, None, None)
        .await
        .unwrap();
    assert_eq!(answered, Answered::Done);
}

#[tokio::test]
async fn a_guest_google_left_off_the_list_is_added() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    mount_search(
        &server,
        json!({"items": [{"id": EVENT, "iCalUID": UID, "attendees": []}]}),
    )
    .await;
    let sent = std::sync::Arc::new(std::sync::Mutex::new(Value::Null));
    let seen = std::sync::Arc::clone(&sent);
    Mock::given(method("PATCH"))
        .and(path(format!("{CALENDAR}/calendars/primary/events/{EVENT}")))
        .respond_with(move |request: &Request| {
            *seen.lock().unwrap() = request.body_json().unwrap();
            ResponseTemplate::new(200).set_body_json(json!({"id": EVENT}))
        })
        .mount(&server)
        .await;

    client(&server)
        .answer_invitation(UID, "me@example.com", Answer::Maybe, None, None)
        .await
        .unwrap();
    assert_eq!(
        *sent.lock().unwrap(),
        json!({"attendees": [{"email": "me@example.com", "responseStatus": "tentative"}]})
    );
}

#[tokio::test]
async fn an_event_that_is_on_no_calendar_is_not_answered() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    mount_search(&server, json!({"items": []})).await;
    assert_eq!(
        client(&server)
            .answer_invitation(UID, "me@example.com", Answer::Yes, None, None)
            .await
            .unwrap(),
        Answered::NotOnCalendar
    );
}

#[tokio::test]
async fn a_missing_calendar_permission_is_reported_as_such() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    Mock::given(method("GET"))
        .and(path(format!("{CALENDAR}/calendars/primary/events")))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "error": {"code": 403, "status": "PERMISSION_DENIED",
                      "details": [{"reason": "ACCESS_TOKEN_SCOPE_INSUFFICIENT"}]}
        })))
        .mount(&server)
        .await;
    assert!(matches!(
        client(&server)
            .answer_invitation(UID, "me@example.com", Answer::Yes, None, None)
            .await,
        Err(GmailError::MissingScope)
    ));
}

#[tokio::test]
async fn only_what_takes_the_hour_counts_as_busy() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    Mock::given(method("GET"))
        .and(path(format!("{CALENDAR}/calendars/primary/events")))
        .and(query_param("timeMin", "2026-03-10T09:00:00+00:00"))
        .and(query_param("singleEvents", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": [
            {"iCalUID": "crit@google.com", "summary": "Design crit",
             "start": {"dateTime": "2026-03-10T09:30:00Z"}},
            {"iCalUID": "off@google.com", "summary": "Called off",
             "status": "cancelled", "start": {"dateTime": "2026-03-10T09:30:00Z"}},
            {"iCalUID": "away@google.com", "summary": "Jonas away",
             "start": {"date": "2026-03-10"}},
            {"iCalUID": "focus@google.com", "summary": "Focus time",
             "transparency": "transparent", "start": {"dateTime": "2026-03-10T09:00:00Z"}},
            {"iCalUID": "skip@google.com", "summary": "Declined already",
             "start": {"dateTime": "2026-03-10T09:15:00Z"},
             "attendees": [{"email": "me@example.com", "self": true,
                            "responseStatus": "declined"}]},
            {"iCalUID": "untitled@google.com",
             "start": {"dateTime": "2026-03-10T09:45:00Z"}}
        ]})))
        .mount(&server)
        .await;

    let busy = client(&server)
        .busy_between("2026-03-10T09:00:00+00:00", "2026-03-10T10:00:00+00:00")
        .await
        .unwrap();
    assert_eq!(
        busy.iter()
            .map(|held| held.summary.as_str())
            .collect::<Vec<_>>(),
        vec!["Design crit", "an untitled event"]
    );
    assert_eq!(busy[0].uid, "crit@google.com");
}

#[tokio::test]
async fn answering_one_occurrence_looks_its_instance_up_first() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    mount_search(
        &server,
        json!({"items": [{
            "id": EVENT,
            "iCalUID": UID,
            "summary": "Stand-up",
            "recurrence": ["RRULE:FREQ=WEEKLY;BYDAY=TU"],
            "attendees": [{"email": "me@example.com", "responseStatus": "needsAction",
                           "self": true}]
        }]}),
    )
    .await;
    Mock::given(method("GET"))
        .and(path(format!(
            "{CALENDAR}/calendars/primary/events/{EVENT}/instances"
        )))
        .and(query_param("originalStart", "2026-03-10T09:00:00+00:00"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": [
            {"id": "ev-1_20260310T090000Z", "recurringEventId": EVENT}
        ]})))
        .mount(&server)
        .await;
    let patched = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let seen = std::sync::Arc::clone(&patched);
    Mock::given(method("PATCH"))
        .and(path(format!(
            "{CALENDAR}/calendars/primary/events/ev-1_20260310T090000Z"
        )))
        .respond_with(move |request: &Request| {
            *seen.lock().unwrap() = request.url.path().to_string();
            ResponseTemplate::new(200).set_body_json(json!({"id": "ev-1_20260310T090000Z"}))
        })
        .mount(&server)
        .await;

    assert_eq!(
        client(&server)
            .answer_invitation(
                UID,
                "me@example.com",
                Answer::Yes,
                Some("2026-03-10T09:00:00+00:00"),
                None
            )
            .await
            .unwrap(),
        Answered::Done
    );
    assert!(
        patched.lock().unwrap().ends_with("ev-1_20260310T090000Z"),
        "the answer went on the occurrence, not the series"
    );
}

#[tokio::test]
async fn answering_a_series_found_by_one_of_its_occurrences_answers_the_series() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    mount_search(
        &server,
        json!({"items": [{
            "id": "ev-1_20260310T090000Z",
            "recurringEventId": EVENT,
            "iCalUID": UID,
            "summary": "Stand-up"
        }]}),
    )
    .await;
    let patched = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let seen = std::sync::Arc::clone(&patched);
    Mock::given(method("PATCH"))
        .and(path(format!("{CALENDAR}/calendars/primary/events/{EVENT}")))
        .respond_with(move |request: &Request| {
            *seen.lock().unwrap() = request.url.path().to_string();
            ResponseTemplate::new(200).set_body_json(json!({"id": EVENT}))
        })
        .mount(&server)
        .await;

    client(&server)
        .answer_invitation(UID, "me@example.com", Answer::No, None, None)
        .await
        .unwrap();
    assert!(patched.lock().unwrap().ends_with(EVENT));
}

#[tokio::test]
async fn events_come_back_read_and_over_every_page() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    Mock::given(method("GET"))
        .and(path(format!("{CALENDAR}/calendars/primary/events")))
        .and(query_param("timeMin", "2026-03-10T00:00:00+00:00"))
        .and(query_param("singleEvents", "true"))
        .and(query_param("pageToken", "page-2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": [
            {"id": "ev-2", "summary": "Holiday", "start": {"date": "2026-03-11"},
             "end": {"date": "2026-03-12"}}
        ]})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{CALENDAR}/calendars/primary/events")))
        .and(query_param("timeMin", "2026-03-10T00:00:00+00:00"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "nextPageToken": "page-2",
            "items": [{
                "id": EVENT, "iCalUID": UID, "summary": "Design crit",
                "location": "Room 4", "htmlLink": "https://calendar.example/ev-1",
                "start": {"dateTime": "2026-03-10T09:30:00Z"},
                "end": {"dateTime": "2026-03-10T10:00:00Z"},
                "organizer": {"email": "priya@fernwood.example"},
                "attendees": [
                    {"email": "priya@fernwood.example", "displayName": "Priya",
                     "responseStatus": "accepted"},
                    {"email": "me@example.com", "self": true}
                ]
            }]
        })))
        .mount(&server)
        .await;

    let events = client(&server)
        .events_between("2026-03-10T00:00:00+00:00", "2026-03-12T00:00:00+00:00")
        .await
        .unwrap();
    assert_eq!(events.len(), 2, "the second page is read too");
    let crit = &events[0];
    assert_eq!(crit.id, EVENT);
    assert_eq!(crit.summary, "Design crit");
    assert_eq!(crit.location, "Room 4");
    assert_eq!(
        crit.start,
        Some(EventTime::At("2026-03-10T09:30:00Z".into()))
    );
    assert_eq!(crit.organizer.as_deref(), Some("priya@fernwood.example"));
    assert_eq!(crit.guests[0].name.as_deref(), Some("Priya"));
    assert_eq!(crit.guests[1].answer, "needsAction");
    assert!(crit.guests[1].me);
    assert!(crit.busy);
    let holiday = &events[1];
    assert_eq!(holiday.start, Some(EventTime::Day("2026-03-11".into())));
    assert!(!holiday.busy, "an all-day event leaves the hours open");
}

#[tokio::test]
async fn a_new_event_invites_its_guests() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    Mock::given(method("POST"))
        .and(path(format!("{CALENDAR}/calendars/primary/events")))
        .and(query_param("sendUpdates", "all"))
        .and(body_json(json!({
            "summary": "Kite day",
            "start": {"dateTime": "2026-03-14T10:00:00+00:00"},
            "end": {"date": "2026-03-15"},
            "attendees": [{"email": "ann@example.com"}]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "ev-9", "summary": "Kite day",
            "start": {"dateTime": "2026-03-14T10:00:00Z"}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let made = client(&server)
        .create_event(&EventFields {
            summary: Some("Kite day".into()),
            start: Some(EventTime::At("2026-03-14T10:00:00+00:00".into())),
            end: Some(EventTime::Day("2026-03-15".into())),
            guests: Some(vec!["ann@example.com".into()]),
            ..EventFields::default()
        })
        .await
        .unwrap();
    assert_eq!(made.id, "ev-9");
}

#[tokio::test]
async fn a_new_guest_list_keeps_the_answers_of_guests_who_stay() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    Mock::given(method("GET"))
        .and(path(format!("{CALENDAR}/calendars/primary/events/{EVENT}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": EVENT,
            "attendees": [
                {"email": "priya@fernwood.example", "responseStatus": "accepted"},
                {"email": "jonas@fernwood.example", "responseStatus": "declined"}
            ]
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path(format!("{CALENDAR}/calendars/primary/events/{EVENT}")))
        .and(query_param("sendUpdates", "all"))
        .and(body_json(json!({
            "location": "Room 5",
            "attendees": [
                {"email": "priya@fernwood.example", "responseStatus": "accepted"},
                {"email": "ann@example.com"}
            ]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": EVENT})))
        .expect(1)
        .mount(&server)
        .await;

    client(&server)
        .update_event(
            EVENT,
            &EventFields {
                location: Some("Room 5".into()),
                guests: Some(vec![
                    "Priya@Fernwood.example".into(),
                    "ann@example.com".into(),
                ]),
                ..EventFields::default()
            },
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn a_change_that_leaves_the_guests_alone_reads_nothing_first() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path(format!("{CALENDAR}/calendars/primary/events/{EVENT}")))
        .and(body_json(json!({"summary": "Design crit, moved"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": EVENT, "summary": "Design crit, moved"
        })))
        .mount(&server)
        .await;

    let changed = client(&server)
        .update_event(
            EVENT,
            &EventFields {
                summary: Some("Design crit, moved".into()),
                ..EventFields::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(changed.summary, "Design crit, moved");
}

#[tokio::test]
async fn deleting_an_event_tells_its_guests() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    Mock::given(method("DELETE"))
        .and(path(format!("{CALENDAR}/calendars/primary/events/{EVENT}")))
        .and(query_param("sendUpdates", "all"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    client(&server).delete_event(EVENT).await.unwrap();
}

#[tokio::test]
async fn a_calendar_api_switched_off_says_where_to_turn_it_on() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    Mock::given(method("GET"))
        .and(path(format!("{CALENDAR}/calendars/primary/events")))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "error": {"code": 403, "status": "PERMISSION_DENIED", "details": [{
                "reason": "SERVICE_DISABLED",
                "metadata": {
                    "serviceTitle": "Google Calendar API",
                    "activationUrl": "https://console.example/calendar"
                }
            }]}
        })))
        .mount(&server)
        .await;
    let listed = client(&server)
        .events_between("2026-03-10T00:00:00+00:00", "2026-03-11T00:00:00+00:00")
        .await;
    assert!(
        matches!(
            &listed,
            Err(GmailError::ApiDisabled { service, enable_url })
                if service == "Google Calendar API"
                    && enable_url == "https://console.example/calendar"
        ),
        "{listed:?}"
    );
}

#[tokio::test]
async fn a_series_found_by_one_occurrence_counts_what_is_left_of_it() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    mount_search(
        &server,
        json!({"items": [{"id": "ev-1_20260310T090000Z", "recurringEventId": EVENT}]}),
    )
    .await;
    Mock::given(method("GET"))
        .and(path(format!("{CALENDAR}/calendars/primary/events/{EVENT}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": EVENT,
            "recurrence": ["EXDATE:20260317T090000Z", "RRULE:FREQ=WEEKLY;BYDAY=TU;COUNT=10"]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!(
            "{CALENDAR}/calendars/primary/events/{EVENT}/instances"
        )))
        .and(query_param("pageToken", "p2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": [{"id": "c"}]})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!(
            "{CALENDAR}/calendars/primary/events/{EVENT}/instances"
        )))
        .and(query_param("timeMin", "2026-03-01T00:00:00+00:00"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [{"id": "a"}, {"id": "b"}],
            "nextPageToken": "p2"
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    let series = client(&server)
        .series(UID, "2026-03-01T00:00:00+00:00")
        .await
        .unwrap();
    assert_eq!(
        series,
        Some(Series {
            rule: "FREQ=WEEKLY;BYDAY=TU;COUNT=10".into(),
            left: Some(3),
        })
    );
}

#[tokio::test]
async fn a_series_with_an_end_date_counts_nothing() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    mount_search(
        &server,
        json!({"items": [{"id": EVENT, "recurrence": ["RRULE:FREQ=DAILY;UNTIL=20260331"]}]}),
    )
    .await;
    Mock::given(method("GET"))
        .and(path(format!(
            "{CALENDAR}/calendars/primary/events/{EVENT}/instances"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": []})))
        .expect(0)
        .mount(&server)
        .await;

    let series = client(&server)
        .series(UID, "2026-03-01T00:00:00+00:00")
        .await
        .unwrap();
    assert_eq!(
        series,
        Some(Series {
            rule: "FREQ=DAILY;UNTIL=20260331".into(),
            left: None,
        })
    );
}

#[tokio::test]
async fn an_event_that_does_not_repeat_has_no_series() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    mount_search(&server, found()).await;

    let series = client(&server)
        .series(UID, "2026-03-01T00:00:00+00:00")
        .await
        .unwrap();
    assert_eq!(series, None);
}

#[tokio::test]
async fn the_calendar_list_reads_each_calendar_with_its_access() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    Mock::given(method("GET"))
        .and(path(format!("{CALENDAR}/users/me/calendarList")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": [
            {"id": "me@example.com", "summary": "me@example.com", "summaryOverride": "Personal",
             "backgroundColor": "#e8660c", "accessRole": "owner", "timeZone": "Europe/Lisbon",
             "primary": true, "defaultReminders": [{"method": "popup", "minutes": 10}]},
            {"id": "pt.portuguese#holiday@group.v.calendar.google.com", "summary": "Holidays in Portugal",
             "backgroundColor": "#e01b24", "accessRole": "reader", "timeZone": "Europe/Lisbon"}
        ]})))
        .mount(&server)
        .await;
    let list = client(&server).calendar_list().await.unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list[0].name, "Personal");
    assert!(list[0].primary);
    assert_eq!(list[0].reminders.len(), 1);
    assert_eq!(list[1].access, Access::Reader);
}

#[tokio::test]
async fn a_change_page_maps_events_and_names_the_deleted_ones() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    Mock::given(method("GET"))
        .and(path(format!("{CALENDAR}/calendars/work/events")))
        .and(query_param("syncToken", "t1"))
        .and(query_param("showDeleted", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [
                {"id": "a", "iCalUID": "a@google.com", "etag": "\"3\"", "status": "confirmed",
                 "summary": "Sprint planning", "sequence": 3,
                 "start": {"dateTime": "2026-09-23T10:00:00+01:00", "timeZone": "Europe/Lisbon"},
                 "end": {"dateTime": "2026-09-23T11:30:00+01:00", "timeZone": "Europe/Lisbon"},
                 "recurrence": ["RRULE:FREQ=WEEKLY;BYDAY=WE"],
                 "hangoutLink": "https://meet.google.com/abc-defg-hij",
                 "attendees": [{"email": "me@example.com", "self": true, "responseStatus": "tentative"}]},
                {"id": "b", "status": "cancelled"},
                {"id": "c", "iCalUID": "c@google.com", "etag": "\"1\"", "status": "confirmed",
                 "summary": "Company holiday",
                 "start": {"date": "2026-09-24"}, "end": {"date": "2026-09-25"}}
            ],
            "nextSyncToken": "t2"
        })))
        .mount(&server)
        .await;
    let page = client(&server)
        .event_changes("work", Some("t1"), None, "2025-09-23T00:00:00Z")
        .await
        .unwrap();
    assert_eq!(page.removed, vec!["b".to_string()]);
    assert_eq!(page.next_sync.as_deref(), Some("t2"));
    let event = &page.events[0];
    assert_eq!(event.title, "Sprint planning");
    assert_eq!(event.zone, "Europe/Lisbon");
    assert_eq!(event.end - event.start, 90 * 60 * 1000);
    assert_eq!(event.rules, vec!["RRULE:FREQ=WEEKLY;BYDAY=WE".to_string()]);
    assert_eq!(event.conference.as_deref(), Some("https://meet.google.com/abc-defg-hij"));
    assert_eq!(event.my_answer, Some(Answer::Maybe));
    assert_eq!(event.sequence, 3, "the organizer's version, which a proposal must name");
    // Google writes an all-day holiday with no transparency, so its own
    // flag says busy; `Event::blocks_time` is what leaves the day open.
    assert!(page.events[1].busy);
}

/// An older range is a read of its own: it bounds the events by time, keeps
/// the series whole, asks for the cancelled ones, and never sends a sync
/// token, which Google refuses beside `timeMin` and `timeMax`.
#[tokio::test]
async fn an_older_range_is_read_by_time_and_sends_no_sync_token() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    Mock::given(method("GET"))
        .and(path(format!("{CALENDAR}/calendars/work/events")))
        .and(query_param("timeMin", "2024-09-01T00:00:00Z"))
        .and(query_param("timeMax", "2025-09-01T00:00:00Z"))
        .and(query_param("showDeleted", "true"))
        .and(query_param("pageToken", "p2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "timeZone": "Europe/Lisbon",
            "items": [
                {"id": "old", "iCalUID": "old@google.com", "etag": "\"1\"", "status": "confirmed",
                 "summary": "Last year's offsite",
                 "start": {"dateTime": "2024-10-03T10:00:00+01:00"},
                 "end": {"dateTime": "2024-10-03T11:00:00+01:00"}},
                {"id": "gone", "status": "cancelled"}
            ]
        })))
        .mount(&server)
        .await;
    let page = client(&server)
        .event_range("work", "2024-09-01T00:00:00Z", "2025-09-01T00:00:00Z", Some("p2"))
        .await
        .unwrap();
    assert_eq!(page.events.len(), 1);
    assert_eq!(page.events[0].title, "Last year's offsite");
    assert_eq!(page.events[0].zone, "Europe/Lisbon");
    assert_eq!(page.removed, vec!["gone".to_string()]);
    assert_eq!(page.next_sync, None);
    let sent = server.received_requests().await.unwrap();
    let events = sent.iter().find(|r| r.url.path().ends_with("/events")).unwrap();
    let query = events.url.query().unwrap_or_default();
    assert!(!query.contains("syncToken"), "a range read must not carry a sync token: {query}");
    assert!(!query.contains("singleEvents"), "a series must arrive whole: {query}");
}

#[tokio::test]
async fn an_expired_calendar_token_says_so() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    Mock::given(method("GET"))
        .and(path(format!("{CALENDAR}/calendars/work/events")))
        .respond_with(ResponseTemplate::new(410).set_body_json(json!({"error": {"code": 410, "message": "Sync token is no longer valid, a full sync is required."}})))
        .mount(&server)
        .await;
    let err = client(&server).event_changes("work", Some("old"), None, "x").await.unwrap_err();
    assert!(matches!(err, GmailError::ExpiredSyncToken));
}

#[tokio::test]
async fn a_write_against_an_older_version_is_refused_as_changed() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    Mock::given(method("PATCH"))
        .and(path(format!("{CALENDAR}/calendars/work/events/a")))
        .and(wiremock::matchers::header("If-Match", "\"2\""))
        .respond_with(ResponseTemplate::new(412))
        .mount(&server)
        .await;
    let event = mailrs_domain::calendar::Event {
        calendar: "work".into(),
        id: "a".into(),
        zone: "UTC".into(),
        ..Default::default()
    };
    let err = client(&server).put_event(&event, Some("\"2\""), false, Notify::Guests).await.unwrap_err();
    assert!(matches!(err, GmailError::Changed));
}

#[tokio::test]
async fn a_new_event_goes_out_with_its_own_id() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    Mock::given(method("POST"))
        .and(path(format!("{CALENDAR}/calendars/work/events")))
        .and(query_param("sendUpdates", "all"))
        .respond_with(|request: &Request| {
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            assert_eq!(body["id"], "pm0123abcd");
            ResponseTemplate::new(200).set_body_json(json!({
                "id": "pm0123abcd", "iCalUID": "pm0123abcd@google.com", "etag": "\"1\"",
                "summary": body["summary"], "start": body["start"], "end": body["end"]
            }))
        })
        .mount(&server)
        .await;
    let event = mailrs_domain::calendar::Event {
        calendar: "work".into(),
        id: "pm0123abcd".into(),
        title: "Lunch".into(),
        start: 1_790_000_000_000,
        end: 1_790_003_600_000,
        zone: "Europe/Lisbon".into(),
        ..Default::default()
    };
    let made = client(&server).put_event(&event, None, true, Notify::Guests).await.unwrap();
    assert_eq!(made.etag, "\"1\"");
    assert_eq!(made.title, "Lunch");
}

#[tokio::test]
async fn asking_for_a_meet_link_sends_a_create_request() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    Mock::given(method("POST"))
        .and(path(format!("{CALENDAR}/calendars/work/events")))
        .and(query_param("conferenceDataVersion", "1"))
        .and(query_param("sendUpdates", "all"))
        .respond_with(|request: &Request| {
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            assert_eq!(body.pointer("/conferenceData/createRequest/requestId"), Some(&json!("req0123")));
            assert_eq!(
                body.pointer("/conferenceData/createRequest/conferenceSolutionKey/type"),
                Some(&json!("hangoutsMeet"))
            );
            ResponseTemplate::new(200).set_body_json(json!({
                "id": body["id"], "etag": "\"1\"", "summary": body["summary"],
                "start": body["start"], "end": body["end"],
                "hangoutLink": "https://meet.google.com/abc-defg-hij"
            }))
        })
        .mount(&server)
        .await;
    let event = mailrs_domain::calendar::Event {
        calendar: "work".into(),
        id: "pm0123abcd".into(),
        title: "Planning".into(),
        start: 1_790_000_000_000,
        end: 1_790_003_600_000,
        zone: "Europe/Lisbon".into(),
        meet_request: Some("req0123".into()),
        ..Default::default()
    };
    let made = client(&server).put_event(&event, None, true, Notify::Guests).await.unwrap();
    assert_eq!(made.conference.as_deref(), Some("https://meet.google.com/abc-defg-hij"));
}

#[tokio::test]
async fn a_changed_occurrence_goes_out_as_a_patch_without_a_rule() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    Mock::given(method("PATCH"))
        .and(path(format!("{CALENDAR}/calendars/work/events/standup_20260923T080000Z")))
        .respond_with(|request: &Request| {
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            assert!(body.get("recurrence").is_none(), "Google refuses a rule on one occurrence");
            assert!(body.get("id").is_none());
            assert!(request.headers.get("If-Match").is_none());
            ResponseTemplate::new(200).set_body_json(json!({
                "id": "standup_20260923T080000Z", "etag": "\"1\"", "recurringEventId": "standup",
                "originalStartTime": {"dateTime": "2026-09-23T08:00:00Z"},
                "start": body["start"], "end": body["end"]
            }))
        })
        .mount(&server)
        .await;
    let event = mailrs_domain::calendar::Event {
        calendar: "work".into(),
        id: "standup_20260923T080000Z".into(),
        zone: "Europe/Lisbon".into(),
        start: 1_790_154_000_000,
        end: 1_790_154_900_000,
        series: Some("standup".into()),
        original_start: Some(1_790_150_400_000),
        ..Default::default()
    };
    let made = client(&server).put_event(&event, None, false, Notify::Guests).await.unwrap();
    assert_eq!(made.series.as_deref(), Some("standup"));
}

#[tokio::test]
async fn a_series_saved_without_rules_stops_repeating_on_google() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    Mock::given(method("PATCH"))
        .and(path(format!("{CALENDAR}/calendars/work/events/standup")))
        .respond_with(|request: &Request| {
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            // A PATCH that leaves recurrence out keeps Google's rule.
            assert_eq!(body.get("recurrence"), Some(&json!([])));
            ResponseTemplate::new(200).set_body_json(json!({
                "id": "standup", "etag": "\"8\"",
                "start": body["start"], "end": body["end"]
            }))
        })
        .expect(1)
        .mount(&server)
        .await;
    let event = mailrs_domain::calendar::Event {
        calendar: "work".into(),
        id: "standup".into(),
        zone: "Europe/Lisbon".into(),
        start: 1_790_150_400_000,
        end: 1_790_151_300_000,
        ..Default::default()
    };
    let made = client(&server).put_event(&event, Some("\"7\""), false, Notify::Guests).await.unwrap();
    assert!(made.rules.is_empty());
}

#[tokio::test]
async fn an_event_colour_goes_out_as_googles_colour_id() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    Mock::given(method("PATCH"))
        .and(path(format!("{CALENDAR}/calendars/work/events/a")))
        .and(body_partial_json(json!({"colorId": "6"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "a", "colorId": "6"})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path(format!("{CALENDAR}/calendars/work/events/b")))
        .and(body_partial_json(json!({"colorId": null})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "b"})))
        .expect(1)
        .mount(&server)
        .await;
    let base = mailrs_domain::calendar::Event { calendar: "work".into(), zone: "UTC".into(), ..Default::default() };
    let tangerine = mailrs_domain::calendar::Event { id: "a".into(), color: Some("#F4511E".into()), ..base.clone() };
    let made = client(&server).put_event(&tangerine, None, false, Notify::Guests).await.unwrap();
    assert_eq!(made.color.as_deref(), Some("#f4511e"));
    let plain = mailrs_domain::calendar::Event { id: "b".into(), color: None, ..base };
    client(&server).put_event(&plain, None, false, Notify::Guests).await.unwrap();
}

/// A guest's change, as the series change hands it over: every field as
/// Google has it, and the guest's reminders, colour and busy.
fn attended(id: &str, series: Option<&str>) -> mailrs_domain::calendar::Event {
    use mailrs_domain::calendar::{Guest, Reminder, ReminderMethod};
    mailrs_domain::calendar::Event {
        calendar: "work".into(),
        id: id.into(),
        title: "Stand-up".into(),
        zone: "Europe/Lisbon".into(),
        start: 1_790_150_400_000,
        end: 1_790_151_300_000,
        rules: if series.is_none() { vec!["RRULE:FREQ=DAILY;COUNT=10".into()] } else { Vec::new() },
        series: series.map(str::to_string),
        guests: vec![
            Guest { email: "rita@example.com".into(), organizer: true, ..Guest::default() },
            Guest { email: "me@example.com".into(), me: true, ..Guest::default() },
        ],
        reminders: Some(vec![Reminder { minutes: 30, method: ReminderMethod::Notification }]),
        color: Some("#f4511e".into()),
        busy: false,
        ..Default::default()
    }
}

#[tokio::test]
async fn a_guests_change_patches_only_reminders_colour_and_busy() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    for id in ["standup_20260923T080000Z", "standup"] {
        Mock::given(method("PATCH"))
            .and(path(format!("{CALENDAR}/calendars/work/events/{id}")))
            .and(query_param("sendUpdates", "all"))
            .respond_with(move |request: &Request| {
                let body: Value = serde_json::from_slice(&request.body).unwrap();
                assert_eq!(
                    body,
                    json!({
                        "transparency": "transparent",
                        "colorId": "6",
                        "reminders": {"useDefault": false, "overrides": [{"method": "popup", "minutes": 30}]},
                    }),
                    "a guest's PATCH carries no time, rule or guest list",
                );
                ResponseTemplate::new(200).set_body_json(json!({"id": id, "etag": "\"8\""}))
            })
            .expect(1)
            .mount(&server)
            .await;
    }
    let gmail = client(&server);
    gmail.put_event(&attended("standup_20260923T080000Z", Some("standup")), None, false, Notify::Guests).await.unwrap();
    gmail.put_event(&attended("standup", None), Some("\"7\""), false, Notify::Guests).await.unwrap();
}

fn moved_standup() -> mailrs_domain::calendar::Event {
    mailrs_domain::calendar::Event {
        calendar: "work".into(),
        id: "standup_20260923T080000Z".into(),
        zone: "Europe/Lisbon".into(),
        start: 1_790_150_400_000,
        end: 1_790_151_300_000,
        series: Some("standup".into()),
        original_start: Some(1_790_150_400_000),
        ..Default::default()
    }
}

/// Answers any GET or PUT with a 500, so a test sees a retry that should
/// not happen as a failure of its own.
async fn refuse_any_retry(server: &MockServer) {
    for verb in ["GET", "PUT"] {
        Mock::given(method(verb)).respond_with(ResponseTemplate::new(500)).expect(0).mount(server).await;
    }
}

#[tokio::test]
async fn an_occurrence_patch_answered_404_means_the_event_is_gone() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    Mock::given(method("PATCH"))
        .and(path(format!("{CALENDAR}/calendars/work/events/standup_20260923T080000Z")))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&server)
        .await;
    refuse_any_retry(&server).await;
    let err = client(&server).put_event(&moved_standup(), None, false, Notify::Guests).await.unwrap_err();
    assert!(matches!(err, GmailError::NotFound), "got {err:?}");
}

#[tokio::test]
async fn an_occurrence_patch_answered_400_turns_the_edit_down_with_googles_reason() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    Mock::given(method("PATCH"))
        .and(path(format!("{CALENDAR}/calendars/work/events/standup_20260923T080000Z")))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": {"code": 400, "message": "The specified time range is empty."}
        })))
        .expect(1)
        .mount(&server)
        .await;
    refuse_any_retry(&server).await;
    let err = client(&server).put_event(&moved_standup(), None, false, Notify::Guests).await.unwrap_err();
    assert!(matches!(err, GmailError::Http { status: 400, .. }), "got {err:?}");
    assert!(err.to_string().contains("The specified time range is empty."));
}

#[tokio::test]
async fn an_occurrence_patch_refused_for_a_stale_etag_says_changed() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    Mock::given(method("PATCH"))
        .and(path(format!("{CALENDAR}/calendars/work/events/standup_20260923T080000Z")))
        .and(header("If-Match", "\"1\""))
        .respond_with(ResponseTemplate::new(412))
        .mount(&server)
        .await;
    let event = mailrs_domain::calendar::Event {
        calendar: "work".into(),
        id: "standup_20260923T080000Z".into(),
        zone: "Europe/Lisbon".into(),
        series: Some("standup".into()),
        original_start: Some(1_790_150_400_000),
        ..Default::default()
    };
    let err = client(&server).put_event(&event, Some("\"1\""), false, Notify::Guests).await.unwrap_err();
    assert!(matches!(err, GmailError::Changed), "got {err:?}");
}

fn guest(email: &str) -> mailrs_domain::calendar::Guest {
    mailrs_domain::calendar::Guest { email: email.into(), ..Default::default() }
}

#[tokio::test]
async fn a_new_event_with_a_guest_goes_out_with_the_guest_and_invitations_on() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    Mock::given(method("POST"))
        .and(path(format!("{CALENDAR}/calendars/work/events")))
        .and(query_param("sendUpdates", "all"))
        .and(body_partial_json(json!({"attendees": [{"email": "ann@example.com"}]})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "pm0123abcd", "etag": "\"1\""})))
        .expect(1)
        .mount(&server)
        .await;
    let event = mailrs_domain::calendar::Event {
        calendar: "work".into(),
        id: "pm0123abcd".into(),
        title: "Planning".into(),
        guests: vec![guest("ann@example.com")],
        ..Default::default()
    };
    client(&server).put_event(&event, None, true, Notify::Guests).await.unwrap();
}

#[tokio::test]
async fn a_move_the_person_keeps_quiet_goes_out_with_updates_off() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    Mock::given(method("PATCH"))
        .and(path(format!("{CALENDAR}/calendars/work/events/review")))
        .and(query_param("sendUpdates", "none"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "review", "etag": "\"2\""})))
        .expect(1)
        .mount(&server)
        .await;
    let event = mailrs_domain::calendar::Event {
        calendar: "work".into(),
        id: "review".into(),
        etag: "\"1\"".into(),
        title: "Review".into(),
        guests: vec![guest("ann@example.com")],
        ..Default::default()
    };
    client(&server).put_event(&event, Some("\"1\""), false, Notify::Nobody).await.unwrap();
}

#[tokio::test]
async fn a_delete_sends_the_cancellation_only_when_asked() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    Mock::given(method("DELETE"))
        .and(path(format!("{CALENDAR}/calendars/work/events/told")))
        .and(query_param("sendUpdates", "all"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path(format!("{CALENDAR}/calendars/work/events/quiet")))
        .and(query_param("sendUpdates", "none"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let gmail = client(&server);
    gmail.remove_event("work", "told", None, Notify::Guests).await.unwrap();
    gmail.remove_event("work", "quiet", None, Notify::Nobody).await.unwrap();
}

/// Google leaves `timeZone` off an event's start and end when the event
/// keeps the calendar's own zone, which the listing names at its top.
#[tokio::test]
async fn an_event_without_a_zone_takes_the_calendar_s() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    Mock::given(method("GET"))
        .and(path(format!("{CALENDAR}/calendars/work/events")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "timeZone": "Europe/Lisbon",
            "items": [
                {"id": "a", "iCalUID": "a@google.com", "etag": "\"3\"", "status": "confirmed",
                 "summary": "Dentist",
                 "start": {"dateTime": "2026-07-14T10:00:00+01:00"},
                 "end": {"dateTime": "2026-07-14T11:00:00+01:00"}}
            ],
            "nextSyncToken": "t2"
        })))
        .mount(&server)
        .await;
    let page = client(&server)
        .event_changes("work", Some("t1"), None, "2025-09-23T00:00:00Z")
        .await
        .unwrap();
    assert_eq!(page.events[0].zone, "Europe/Lisbon");
}

/// A write's answer is one event with no calendar around it. When Google
/// leaves the zone off, the event keeps the zone it went out with.
#[tokio::test]
async fn a_written_event_without_a_zone_in_the_answer_keeps_its_own() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    Mock::given(method("PATCH"))
        .and(path(format!("{CALENDAR}/calendars/work/events/a")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "a", "iCalUID": "a@google.com", "etag": "\"4\"", "status": "confirmed",
            "summary": "Dentist",
            "start": {"dateTime": "2026-07-14T10:00:00+01:00"},
            "end": {"dateTime": "2026-07-14T11:00:00+01:00"}
        })))
        .mount(&server)
        .await;
    let event = mailrs_domain::calendar::Event {
        calendar: "work".into(),
        id: "a".into(),
        zone: "Europe/Lisbon".into(),
        ..Default::default()
    };
    let saved = client(&server).put_event(&event, None, false, Notify::Nobody).await.unwrap();
    assert_eq!(saved.zone, "Europe/Lisbon");
}

/// Moving an event to another calendar is Google's `events.move`: a POST
/// on the event under its old calendar, naming the new one, with the
/// person's choice of whether the guests hear of it. The answer is the
/// event on its new calendar.
#[tokio::test]
async fn moving_an_event_to_another_calendar_posts_to_its_move() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    Mock::given(method("POST"))
        .and(path(format!("{CALENDAR}/calendars/work/events/review/move")))
        .and(query_param("destination", "home@example.com"))
        .and(query_param("sendUpdates", "none"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({
                "id": "review",
                "etag": "\"7\"",
                "summary": "Review",
                "start": {"dateTime": "2026-10-01T09:00:00Z", "timeZone": "Europe/Lisbon"},
                "end": {"dateTime": "2026-10-01T10:00:00Z", "timeZone": "Europe/Lisbon"}
            })),
        )
        .expect(1)
        .mount(&server)
        .await;
    let review = mailrs_domain::calendar::Event {
        calendar: "work".into(),
        id: "review".into(),
        zone: "Europe/Lisbon".into(),
        ..Default::default()
    };
    let moved = client(&server).move_event(&review, "home@example.com", Notify::Nobody).await.unwrap();
    assert_eq!((moved.calendar.as_str(), moved.etag.as_str()), ("home@example.com", "\"7\""));
}

/// A guest who removes an invitation deletes only their own copy. Google
/// marks them as having declined, so nothing answers No first, and the
/// delete goes out with updates off: the organizer hears only what
/// Google itself tells them.
#[tokio::test]
async fn a_guests_removal_is_one_quiet_delete_and_no_answer() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    Mock::given(method("DELETE"))
        .and(path(format!("{CALENDAR}/calendars/primary/events/{EVENT}")))
        .and(query_param("sendUpdates", "none"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PATCH")).respond_with(ResponseTemplate::new(200)).expect(0).mount(&server).await;
    Mock::given(method("GET")).respond_with(ResponseTemplate::new(200)).expect(0).mount(&server).await;
    let invitation = mailrs_domain::calendar::Event {
        calendar: "primary".into(),
        id: EVENT.into(),
        guests: vec![
            mailrs_domain::calendar::Guest { email: "priya@fernwood.example".into(), organizer: true, ..guest("priya@fernwood.example") },
            mailrs_domain::calendar::Guest { me: true, ..guest("me@example.com") },
        ],
        ..Default::default()
    };
    let notify = mailrs_domain::calendar::removal_notify(&invitation, Notify::Guests);
    client(&server).remove_event(&invitation.calendar, &invitation.id, None, notify).await.unwrap();
}

/// One occurrence of a weekly series, by the id Google gives it before
/// anyone changes it: the series id and the original start in UTC.
const INSTANCE: &str = "standup_20261020T090000Z";

fn instance() -> Value {
    json!({
        "id": INSTANCE,
        "recurringEventId": "standup",
        "iCalUID": UID,
        "summary": "Stand-up",
        "originalStartTime": {"dateTime": "2026-10-20T09:00:00Z"},
        "start": {"dateTime": "2026-10-20T09:00:00Z", "timeZone": "UTC"},
        "end": {"dateTime": "2026-10-20T09:15:00Z", "timeZone": "UTC"},
        "attendees": [
            {"email": "priya@fernwood.example", "responseStatus": "accepted", "organizer": true,
             "comment": "Bring the numbers"},
            {"email": "me@example.com", "responseStatus": "needsAction", "self": true},
            {"email": "jonas@fernwood.example", "responseStatus": "tentative", "optional": true}
        ]
    })
}

async fn mount_instance(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path(format!("{CALENDAR}/calendars/primary/events/{INSTANCE}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(instance()))
        .expect(1)
        .mount(server)
        .await;
}

#[tokio::test]
async fn answering_one_occurrence_patches_that_instance_alone() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    mount_instance(&server).await;
    let mut answered = instance();
    answered["etag"] = json!("\"2\"");
    answered["attendees"][1]["responseStatus"] = json!("declined");
    Mock::given(method("PATCH"))
        .and(path(format!("{CALENDAR}/calendars/primary/events/{INSTANCE}")))
        .and(query_param("sendUpdates", "all"))
        .and(body_json(json!({"attendees": [
            {"email": "priya@fernwood.example", "responseStatus": "accepted", "organizer": true,
             "comment": "Bring the numbers"},
            {"email": "me@example.com", "responseStatus": "declined", "self": true},
            {"email": "jonas@fernwood.example", "responseStatus": "tentative", "optional": true}
        ]})))
        .respond_with(ResponseTemplate::new(200).set_body_json(answered))
        .expect(1)
        .mount(&server)
        .await;
    // The series itself is never written.
    Mock::given(method("PATCH"))
        .and(path(format!("{CALENDAR}/calendars/primary/events/standup")))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;

    let event = client(&server)
        .answer_event("primary", INSTANCE, "me@example.com", Answer::No, None)
        .await
        .unwrap();
    assert_eq!(
        (event.id.as_str(), event.series.as_deref(), event.etag.as_str(), event.my_answer),
        (INSTANCE, Some("standup"), "\"2\"", Some(Answer::No))
    );
}

#[tokio::test]
async fn a_note_goes_out_as_the_guests_own_comment() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    mount_instance(&server).await;
    Mock::given(method("PATCH"))
        .and(path(format!("{CALENDAR}/calendars/primary/events/{INSTANCE}")))
        .and(body_partial_json(json!({"attendees": [
            {"email": "priya@fernwood.example", "comment": "Bring the numbers"},
            {"email": "me@example.com", "responseStatus": "tentative",
             "comment": "Running ten minutes late"},
            {"email": "jonas@fernwood.example"}
        ]})))
        .respond_with(ResponseTemplate::new(200).set_body_json(instance()))
        .expect(1)
        .mount(&server)
        .await;

    client(&server)
        .answer_event("primary", INSTANCE, "me@example.com", Answer::Maybe, Some("Running ten minutes late"))
        .await
        .unwrap();
}

#[tokio::test]
async fn an_answer_found_by_its_uid_carries_the_note_too() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    mount_search(&server, found()).await;
    Mock::given(method("PATCH"))
        .and(path(format!("{CALENDAR}/calendars/primary/events/{EVENT}")))
        .and(body_partial_json(json!({"attendees": [
            {"email": "priya@fernwood.example"},
            {"email": "me@example.com", "responseStatus": "accepted", "comment": "See you there"},
            {"email": "jonas@fernwood.example"}
        ]})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": EVENT})))
        .expect(1)
        .mount(&server)
        .await;
    client(&server)
        .answer_invitation(UID, "me@example.com", Answer::Yes, None, Some("See you there"))
        .await
        .unwrap();
}

/// A change page holding one entry of each of Google's status types, as
/// `events.list` returns them (Calendar API reference, checked
/// 2026-09-28).
async fn typed_page() -> Vec<mailrs_domain::calendar::Event> {
    let server = MockServer::start().await;
    mount_token(&server).await;
    Mock::given(method("GET"))
        .and(path(format!("{CALENDAR}/calendars/primary/events")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "timeZone": "Europe/Lisbon",
            "items": [
                {"id": "ooo", "eventType": "outOfOffice", "summary": "Out of office",
                 "transparency": "opaque",
                 "start": {"dateTime": "2026-09-30T09:00:00+01:00"},
                 "end": {"dateTime": "2026-09-30T18:00:00+01:00"},
                 "outOfOfficeProperties": {"autoDeclineMode": "declineAllConflictingInvitations",
                                           "declineMessage": "Away until Thursday"}},
                {"id": "focus", "eventType": "focusTime", "summary": "Focus time",
                 "start": {"dateTime": "2026-09-29T14:00:00+01:00"},
                 "end": {"dateTime": "2026-09-29T16:00:00+01:00"},
                 "focusTimeProperties": {"autoDeclineMode": "declineOnlyNewConflictingInvitations",
                                         "chatStatus": "doNotDisturb", "declineMessage": "Heads down"}},
                {"id": "home", "eventType": "workingLocation", "summary": "Home",
                 "transparency": "transparent", "visibility": "public",
                 "start": {"date": "2026-09-28"}, "end": {"date": "2026-09-29"},
                 "workingLocationProperties": {"type": "homeOffice", "homeOffice": {}}},
                {"id": "office", "eventType": "workingLocation", "summary": "Office",
                 "start": {"date": "2026-09-29"}, "end": {"date": "2026-09-30"},
                 "workingLocationProperties": {"type": "officeLocation",
                     "officeLocation": {"buildingId": "lx-2", "label": "Lisbon HQ"}}},
                {"id": "cafe", "eventType": "workingLocation", "summary": "Café",
                 "start": {"date": "2026-10-01"}, "end": {"date": "2026-10-02"},
                 "workingLocationProperties": {"type": "customLocation",
                     "customLocation": {"label": "Café Tati"}}},
                {"id": "bday", "eventType": "birthday", "summary": "Ana's birthday",
                 "start": {"date": "2026-10-02"}, "end": {"date": "2026-10-03"},
                 "recurrence": ["RRULE:FREQ=YEARLY"],
                 "birthdayProperties": {"type": "birthday", "contact": "people/c123"}},
                {"id": "plain", "summary": "Lunch",
                 "start": {"dateTime": "2026-09-29T12:00:00+01:00"},
                 "end": {"dateTime": "2026-09-29T13:00:00+01:00"}}
            ],
            "nextSyncToken": "t2"
        })))
        .mount(&server)
        .await;
    client(&server).event_changes("primary", None, None, "2026-09-01T00:00:00Z").await.unwrap().events
}

fn kind_of(events: &[mailrs_domain::calendar::Event], id: &str) -> mailrs_domain::calendar::Kind {
    events.iter().find(|e| e.id == id).unwrap().kind.clone()
}

#[tokio::test]
async fn an_out_of_office_entry_reads_with_what_it_declines() {
    use mailrs_domain::calendar::{Decline, Declines, Kind};
    let events = typed_page().await;
    assert_eq!(
        kind_of(&events, "ooo"),
        Kind::OutOfOffice(Decline { meetings: Declines::All, message: "Away until Thursday".into() })
    );
}

#[tokio::test]
async fn a_focus_time_entry_reads_with_what_it_declines() {
    use mailrs_domain::calendar::{Decline, Declines, Kind};
    let events = typed_page().await;
    assert_eq!(
        kind_of(&events, "focus"),
        Kind::Focus(Decline { meetings: Declines::New, message: "Heads down".into() })
    );
}

#[tokio::test]
async fn a_working_location_reads_as_home_an_office_or_a_named_place() {
    use mailrs_domain::calendar::{Kind, Workplace};
    let events = typed_page().await;
    assert_eq!(kind_of(&events, "home"), Kind::WorkingLocation(Workplace::Home));
    assert_eq!(kind_of(&events, "office"), Kind::WorkingLocation(Workplace::Office("Lisbon HQ".into())));
    assert_eq!(kind_of(&events, "cafe"), Kind::WorkingLocation(Workplace::Elsewhere("Café Tati".into())));
}

#[tokio::test]
async fn a_birthday_reads_as_a_birthday() {
    let events = typed_page().await;
    assert_eq!(kind_of(&events, "bday"), mailrs_domain::calendar::Kind::Birthday);
}

#[tokio::test]
async fn an_event_with_no_type_is_an_ordinary_event() {
    let events = typed_page().await;
    assert_eq!(kind_of(&events, "plain"), mailrs_domain::calendar::Kind::Event);
}

fn out_of_office(declines: mailrs_domain::calendar::Declines) -> mailrs_domain::calendar::Event {
    use mailrs_domain::calendar::{Decline, Kind};
    mailrs_domain::calendar::Event {
        calendar: "primary".into(),
        id: "pm0ooo".into(),
        title: "Out of office".into(),
        start: 1_790_000_000_000,
        end: 1_790_028_800_000,
        zone: "Europe/Lisbon".into(),
        busy: true,
        kind: Kind::OutOfOffice(Decline { meetings: declines, message: "Back on Monday".into() }),
        ..Default::default()
    }
}

#[tokio::test]
async fn a_new_out_of_office_goes_out_typed_with_what_it_declines() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    Mock::given(method("POST"))
        .and(path(format!("{CALENDAR}/calendars/primary/events")))
        .and(body_partial_json(json!({
            "eventType": "outOfOffice",
            "transparency": "opaque",
            "outOfOfficeProperties": {
                "autoDeclineMode": "declineAllConflictingInvitations",
                "declineMessage": "Back on Monday"
            }
        })))
        .respond_with(|request: &Request| {
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            // Google refuses guests, a place and a Meet link on an
            // out-of-office entry.
            assert!(body.get("attendees").is_none(), "{body}");
            assert!(body.get("location").is_none(), "{body}");
            assert!(body.get("conferenceData").is_none(), "{body}");
            ResponseTemplate::new(200).set_body_json(json!({
                "id": "pm0ooo", "etag": "\"1\"", "eventType": "outOfOffice",
                "summary": body["summary"], "start": body["start"], "end": body["end"],
                "outOfOfficeProperties": body["outOfOfficeProperties"]
            }))
        })
        .expect(1)
        .mount(&server)
        .await;
    let made = client(&server)
        .put_event(&out_of_office(mailrs_domain::calendar::Declines::All), None, true, Notify::Guests)
        .await
        .unwrap();
    assert_eq!(made.kind, out_of_office(mailrs_domain::calendar::Declines::All).kind);
}

#[tokio::test]
async fn a_new_focus_time_goes_out_typed() {
    use mailrs_domain::calendar::{Decline, Declines, Kind};
    let server = MockServer::start().await;
    mount_token(&server).await;
    Mock::given(method("POST"))
        .and(path(format!("{CALENDAR}/calendars/primary/events")))
        .and(body_partial_json(json!({
            "eventType": "focusTime",
            "focusTimeProperties": {"autoDeclineMode": "declineNone"}
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "pm0focus", "eventType": "focusTime"})))
        .expect(1)
        .mount(&server)
        .await;
    let event = mailrs_domain::calendar::Event {
        id: "pm0focus".into(),
        kind: Kind::Focus(Decline { meetings: Declines::Nothing, message: String::new() }),
        ..out_of_office(Declines::Nothing)
    };
    client(&server).put_event(&event, None, true, Notify::Guests).await.unwrap();
}

#[tokio::test]
async fn an_edit_of_an_out_of_office_leaves_its_type_alone() {
    // Google refuses a change of `eventType`, and a PATCH that repeats it
    // gains nothing, so only the properties go out.
    let server = MockServer::start().await;
    mount_token(&server).await;
    Mock::given(method("PATCH"))
        .and(path(format!("{CALENDAR}/calendars/primary/events/pm0ooo")))
        .respond_with(|request: &Request| {
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            assert!(body.get("eventType").is_none(), "{body}");
            assert_eq!(body.pointer("/outOfOfficeProperties/autoDeclineMode"), Some(&json!("declineOnlyNewConflictingInvitations")));
            ResponseTemplate::new(200).set_body_json(json!({"id": "pm0ooo", "eventType": "outOfOffice"}))
        })
        .expect(1)
        .mount(&server)
        .await;
    client(&server)
        .put_event(&out_of_office(mailrs_domain::calendar::Declines::New), Some("\"1\""), false, Notify::Guests)
        .await
        .unwrap();
}

#[tokio::test]
async fn an_imported_event_goes_out_under_its_uid_with_no_guests() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    let sent = std::sync::Arc::new(std::sync::Mutex::new(Value::Null));
    let seen = std::sync::Arc::clone(&sent);
    Mock::given(method("POST"))
        .and(path(format!("{CALENDAR}/calendars/work/events/import")))
        .respond_with(move |request: &Request| {
            *seen.lock().unwrap() = request.body_json().unwrap();
            ResponseTemplate::new(200).set_body_json(json!({
                "id": "gen1", "iCalUID": "ticket-4471@rail.example", "summary": "Coach 4",
                "start": {"dateTime": "2026-11-05T08:30:00Z", "timeZone": "Europe/Lisbon"},
                "end": {"dateTime": "2026-11-05T11:30:00Z", "timeZone": "Europe/Lisbon"}
            }))
        })
        .expect(1)
        .mount(&server)
        .await;
    let event = mailrs_domain::calendar::Event {
        calendar: "work".into(),
        uid: "ticket-4471@rail.example".into(),
        title: "Coach 4".into(),
        zone: "Europe/Lisbon".into(),
        start: 1_793_867_400_000,
        end: 1_793_878_200_000,
        guests: vec![guest("ann@example.com")],
        rules: vec!["RRULE:FREQ=WEEKLY;COUNT=4".into()],
        ..Default::default()
    };

    let made = client(&server).import_event(&event).await.unwrap();

    assert_eq!((made.id.as_str(), made.uid.as_str()), ("gen1", "ticket-4471@rail.example"));
    let body = sent.lock().unwrap().clone();
    assert_eq!(body["iCalUID"], "ticket-4471@rail.example");
    assert_eq!(body["start"]["timeZone"], "Europe/Lisbon");
    assert_eq!(body["recurrence"], json!(["RRULE:FREQ=WEEKLY;COUNT=4"]));
    // The import names no id of its own, or Google would file a second
    // event beside the one the UID already matches, and it invites nobody.
    assert!(body.get("id").is_none() && body.get("attendees").is_none());
}

fn drive_file(title: &str, mime: &str, id: &str) -> mailrs_domain::calendar::Attachment {
    mailrs_domain::calendar::Attachment {
        title: title.into(),
        file_url: format!("https://drive.google.com/file/d/{id}/view"),
        mime_type: mime.into(),
        icon_link: "https://drive-thirdparty.googleusercontent.com/16/type/application/pdf".into(),
        file_id: id.into(),
        waiting: None,
    }
}

#[test]
fn an_event_reads_its_attachments() {
    let item = json!({
        "id": "ev", "start": {"dateTime": "2026-09-23T08:00:00Z"}, "end": {"dateTime": "2026-09-23T09:00:00Z"},
        "attachments": [{
            "fileUrl": "https://drive.google.com/file/d/1abc/view",
            "title": "Agenda.pdf",
            "mimeType": "application/pdf",
            "iconLink": "https://drive-thirdparty.googleusercontent.com/16/type/application/pdf",
            "fileId": "1abc"
        }]
    });
    let event = mailrs_gmail::google_event("work", &item, None, "UTC");
    assert_eq!(event.attachments, Some(vec![drive_file("Agenda.pdf", "application/pdf", "1abc")]));
}

#[test]
fn an_event_without_attachments_reads_an_empty_list() {
    let item = json!({"id": "ev", "start": {"date": "2026-09-23"}, "end": {"date": "2026-09-24"}});
    let event = mailrs_gmail::google_event("work", &item, None, "UTC");
    assert_eq!(event.attachments, Some(Vec::new()));
}

#[tokio::test]
async fn a_change_sends_the_attachments_back_with_supports_attachments() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    Mock::given(method("PATCH"))
        .and(path(format!("{CALENDAR}/calendars/work/events/review")))
        .and(query_param("supportsAttachments", "true"))
        .and(body_partial_json(json!({"attachments": [
            {"fileUrl": "https://drive.google.com/file/d/1abc/view", "title": "Agenda.pdf", "mimeType": "application/pdf"},
            {"fileUrl": "https://drive.google.com/file/d/2def/view", "title": "Budget", "mimeType": "application/vnd.google-apps.spreadsheet"}
        ]})))
        .respond_with(|request: &Request| {
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            // Google sets fileId and iconLink itself for a Drive file.
            assert!(body["attachments"][0].get("fileId").is_none());
            ResponseTemplate::new(200).set_body_json(json!({
                "id": "review", "etag": "\"9\"", "start": body["start"], "end": body["end"],
                "attachments": body["attachments"]
            }))
        })
        .expect(1)
        .mount(&server)
        .await;
    let event = mailrs_domain::calendar::Event {
        calendar: "work".into(),
        id: "review".into(),
        zone: "UTC".into(),
        start: 1_790_150_400_000,
        end: 1_790_151_300_000,
        attachments: Some(vec![
            drive_file("Agenda.pdf", "application/pdf", "1abc"),
            drive_file("Budget", "application/vnd.google-apps.spreadsheet", "2def"),
        ]),
        ..Default::default()
    };
    let made = client(&server).put_event(&event, Some("\"8\""), false, Notify::Guests).await.unwrap();
    assert_eq!(made.attachments.map(|a| a.len()), Some(2));
}

#[tokio::test]
async fn a_change_with_unknown_attachments_leaves_googles_list_alone() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    Mock::given(method("PATCH"))
        .and(path(format!("{CALENDAR}/calendars/work/events/review")))
        .and(query_param("supportsAttachments", "true"))
        .respond_with(|request: &Request| {
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            assert!(body.get("attachments").is_none(), "an unread list must not go out as empty");
            ResponseTemplate::new(200).set_body_json(json!({"id": "review", "start": body["start"], "end": body["end"]}))
        })
        .expect(1)
        .mount(&server)
        .await;
    let event = mailrs_domain::calendar::Event {
        calendar: "work".into(),
        id: "review".into(),
        zone: "UTC".into(),
        start: 1_790_150_400_000,
        end: 1_790_151_300_000,
        attachments: None,
        ..Default::default()
    };
    client(&server).put_event(&event, None, false, Notify::Guests).await.unwrap();
}

#[tokio::test]
async fn a_new_event_goes_out_with_its_attachments() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    Mock::given(method("POST"))
        .and(path(format!("{CALENDAR}/calendars/work/events")))
        .and(query_param("supportsAttachments", "true"))
        .and(body_partial_json(json!({"attachments": [
            {"fileUrl": "https://drive.google.com/file/d/1abc/view", "title": "Agenda.pdf", "mimeType": "application/pdf"}
        ]})))
        .respond_with(|request: &Request| {
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            ResponseTemplate::new(200).set_body_json(json!({"id": body["id"], "start": body["start"], "end": body["end"]}))
        })
        .expect(1)
        .mount(&server)
        .await;
    let event = mailrs_domain::calendar::Event {
        calendar: "work".into(),
        id: "pmnew0123".into(),
        zone: "UTC".into(),
        start: 1_790_150_400_000,
        end: 1_790_151_300_000,
        attachments: Some(vec![drive_file("Agenda.pdf", "application/pdf", "1abc")]),
        ..Default::default()
    };
    client(&server).put_event(&event, None, true, Notify::Guests).await.unwrap();
}
