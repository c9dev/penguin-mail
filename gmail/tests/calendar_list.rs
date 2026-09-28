//! Changing the account's calendar list against a stand-in Calendar API:
//! making, renaming and deleting a calendar, its colour and whether
//! Google's own list hides it, and subscribing. `wiremock` checks every
//! request body.

use mailrs_domain::calendar::Access;
use mailrs_domain::calendar::list::ListEdit;
use mailrs_gmail::{GmailClient, GmailError, OAuthClient};
use serde_json::json;
use wiremock::matchers::{body_json, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const CALENDAR: &str = "/calendar/v3";
const HOLIDAYS: &str = "en.portuguese#holiday@group.v.calendar.google.com";

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

/// Google's list entry for a calendar the account owns.
fn entry(id: &str, name: &str, color: &str) -> serde_json::Value {
    json!({"id": id, "summary": name, "backgroundColor": color, "foregroundColor": "#000000",
           "accessRole": "owner", "timeZone": "Europe/Lisbon"})
}

#[tokio::test]
async fn the_list_asks_for_hidden_calendars_and_reads_the_flag() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    Mock::given(method("GET"))
        .and(path(format!("{CALENDAR}/users/me/calendarList")))
        .and(query_param("showHidden", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": [
            entry("me@example.com", "me@example.com", "#e8660c"),
            {"id": "team", "summary": "Team", "backgroundColor": "#16a766",
             "accessRole": "writer", "hidden": true}
        ]})))
        .expect(1)
        .mount(&server)
        .await;
    let list = client(&server).calendar_list().await.unwrap();
    assert!(!list[0].hidden);
    assert!(list[1].hidden, "Google sends `hidden` only when it is true");
}

#[tokio::test]
async fn a_new_calendar_is_made_then_coloured_on_the_list() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    Mock::given(method("POST"))
        .and(path(format!("{CALENDAR}/calendars")))
        .and(body_json(json!({"summary": "Climbing", "timeZone": "Europe/Lisbon"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "abc@group.calendar.google.com", "summary": "Climbing", "timeZone": "Europe/Lisbon"
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path(format!("{CALENDAR}/users/me/calendarList/abc%40group.calendar.google.com")))
        .and(query_param("colorRgbFormat", "true"))
        .and(body_json(json!({"backgroundColor": "#16a766", "foregroundColor": "#ffffff"})))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(entry("abc@group.calendar.google.com", "Climbing", "#16a766")),
        )
        .expect(1)
        .mount(&server)
        .await;
    let edit = ListEdit::Create { name: "Climbing".into(), color: "#16a766".into(), zone: "Europe/Lisbon".into() };
    let made = client(&server).edit_calendar_list("new:x", &edit).await.unwrap().unwrap();
    assert_eq!(made.id, "abc@group.calendar.google.com");
    assert_eq!(made.color, "#16a766");
    assert_eq!(made.access, Access::Owner);
}

#[tokio::test]
async fn a_new_calendar_without_a_zone_leaves_the_zone_to_google() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    Mock::given(method("POST"))
        .and(path(format!("{CALENDAR}/calendars")))
        .and(body_json(json!({"summary": "Climbing"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "abc", "summary": "Climbing"})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path(format!("{CALENDAR}/users/me/calendarList/abc")))
        .respond_with(ResponseTemplate::new(200).set_body_json(entry("abc", "Climbing", "#3f51b5")))
        .mount(&server)
        .await;
    let edit = ListEdit::Create { name: "Climbing".into(), color: "#3f51b5".into(), zone: String::new() };
    client(&server).edit_calendar_list("new:x", &edit).await.unwrap();
}

#[tokio::test]
async fn a_dark_colour_goes_out_with_white_text() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    Mock::given(method("PATCH"))
        .and(path(format!("{CALENDAR}/users/me/calendarList/work")))
        .and(query_param("colorRgbFormat", "true"))
        .and(body_json(json!({"backgroundColor": "#3f51b5", "foregroundColor": "#ffffff"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(entry("work", "Work", "#3f51b5")))
        .expect(1)
        .mount(&server)
        .await;
    let edit = ListEdit::Recolor { color: "#3f51b5".into() };
    let answered = client(&server).edit_calendar_list("work", &edit).await.unwrap();
    assert_eq!(answered.map(|c| c.color), Some("#3f51b5".into()));
}

/// A rename changes the calendar's own name and clears the account's
/// own name for it on the list, which Google shows over the new one.
/// `null` deletes a field in a PATCH; an empty string would set an empty
/// override.
#[tokio::test]
async fn renaming_patches_the_calendar_and_clears_the_list_name() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    Mock::given(method("PATCH"))
        .and(path(format!("{CALENDAR}/calendars/work")))
        .and(body_json(json!({"summary": "Office"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "work", "summary": "Office"})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path(format!("{CALENDAR}/users/me/calendarList/work")))
        .and(body_json(json!({"summaryOverride": null})))
        .respond_with(ResponseTemplate::new(200).set_body_json(entry("work", "Office", "#3f51b5")))
        .expect(1)
        .mount(&server)
        .await;
    let edit = ListEdit::Rename { name: "Office".into() };
    let renamed = client(&server).edit_calendar_list("work", &edit).await.unwrap();
    assert_eq!(renamed.map(|c| c.name), Some("Office".into()));
}

/// An account that granted the calendars but not the list keeps the
/// rename; only the override stays.
#[tokio::test]
async fn a_rename_without_the_list_permission_still_renames() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    Mock::given(method("PATCH"))
        .and(path(format!("{CALENDAR}/calendars/work")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "work", "summary": "Office"})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path(format!("{CALENDAR}/users/me/calendarList/work")))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "error": {"code": 403, "status": "PERMISSION_DENIED",
                      "details": [{"reason": "ACCESS_TOKEN_SCOPE_INSUFFICIENT"}]}
        })))
        .mount(&server)
        .await;
    let edit = ListEdit::Rename { name: "Office".into() };
    assert_eq!(client(&server).edit_calendar_list("work", &edit).await.unwrap(), None);
}

#[tokio::test]
async fn unsubscribing_takes_the_calendar_off_the_list() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    Mock::given(method("DELETE"))
        .and(path(format!("{CALENDAR}/users/me/calendarList/en.portuguese%23holiday%40group.v.calendar.google.com")))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    assert_eq!(client(&server).edit_calendar_list(HOLIDAYS, &ListEdit::Unsubscribe).await.unwrap(), None);
}

#[tokio::test]
async fn deleting_deletes_the_calendar_itself() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    Mock::given(method("DELETE"))
        .and(path(format!("{CALENDAR}/calendars/work")))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    assert_eq!(client(&server).edit_calendar_list("work", &ListEdit::Delete).await.unwrap(), None);
}

#[tokio::test]
async fn deleting_a_calendar_already_gone_says_not_found() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    Mock::given(method("DELETE"))
        .and(path(format!("{CALENDAR}/calendars/work")))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({"error": {"code": 404, "message": "Not Found"}})))
        .mount(&server)
        .await;
    let gone = client(&server).edit_calendar_list("work", &ListEdit::Delete).await;
    assert!(matches!(gone, Err(GmailError::NotFound)), "{gone:?}");
}

#[tokio::test]
async fn hiding_sets_googles_hidden_flag() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    Mock::given(method("PATCH"))
        .and(path(format!("{CALENDAR}/users/me/calendarList/team")))
        .and(body_json(json!({"hidden": true})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "team", "summary": "Team",
            "accessRole": "writer", "hidden": true})))
        .expect(1)
        .mount(&server)
        .await;
    client(&server).edit_calendar_list("team", &ListEdit::Hide { hidden: true }).await.unwrap();
}

#[tokio::test]
async fn showing_again_clears_hidden_and_selects_it() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    Mock::given(method("PATCH"))
        .and(path(format!("{CALENDAR}/users/me/calendarList/team")))
        .and(body_json(json!({"hidden": false, "selected": true})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "team", "summary": "Team",
            "accessRole": "writer", "selected": true})))
        .expect(1)
        .mount(&server)
        .await;
    client(&server).edit_calendar_list("team", &ListEdit::Hide { hidden: false }).await.unwrap();
}

#[tokio::test]
async fn subscribing_puts_the_address_on_the_list() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    Mock::given(method("POST"))
        .and(path(format!("{CALENDAR}/users/me/calendarList")))
        .and(body_json(json!({"id": "https://example.com/team.ics", "selected": true})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "q8v3@import.calendar.google.com", "summary": "Team fixtures",
            "backgroundColor": "#9e69af", "accessRole": "reader", "selected": true
        })))
        .expect(1)
        .mount(&server)
        .await;
    let edit = ListEdit::Subscribe { url: "https://example.com/team.ics".into() };
    let added = client(&server).edit_calendar_list("new:x", &edit).await.unwrap().unwrap();
    assert_eq!(added.id, "q8v3@import.calendar.google.com");
    assert_eq!(added.access, Access::Reader);
}

#[tokio::test]
async fn a_holiday_calendar_goes_on_the_list_by_its_id() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    Mock::given(method("POST"))
        .and(path(format!("{CALENDAR}/users/me/calendarList")))
        .and(body_json(json!({"id": HOLIDAYS, "selected": true})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": HOLIDAYS, "summary": "Holidays in Portugal", "accessRole": "reader"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let added = client(&server).edit_calendar_list(HOLIDAYS, &ListEdit::Add).await.unwrap().unwrap();
    assert_eq!(added.name, "Holidays in Portugal");
}
