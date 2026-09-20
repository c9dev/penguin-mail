//! Answering an invitation, and asking what else the user has on, against
//! a stand-in Calendar API. No network: `wiremock` answers the calls and
//! checks what went out.

use mailrs_domain::invitation::Answer;
use mailrs_gmail::{Answered, GmailClient, GmailError, OAuthClient};
use serde_json::{Value, json};
use wiremock::matchers::{body_json, method, path, query_param};
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
        .answer_invitation(UID, "me@example.com", Answer::No, None)
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
        .answer_invitation(UID, "me@example.com", Answer::Maybe, None)
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
            .answer_invitation(UID, "me@example.com", Answer::Yes, None)
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
            .answer_invitation(UID, "me@example.com", Answer::Yes, None)
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
                Some("2026-03-10T09:00:00+00:00")
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
        .answer_invitation(UID, "me@example.com", Answer::No, None)
        .await
        .unwrap();
    assert!(patched.lock().unwrap().ends_with(EVENT));
}
