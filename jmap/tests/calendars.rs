//! The calendar calls, as wiremock receives them.

mod common;

use mailrs_jmap::{CALENDARS, CYRUS_JSCALENDAR_BIS, EventWrite};
use serde_json::{Map, Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use common::{client, session_on};

/// A session offering calendars in draft 32's shape, and Cyrus's 2.0
/// switch when `bis`, answering `answer` to every call.
async fn answering(server: &MockServer, bis: bool, answer: Value) {
    let mut session = session_on(&server.uri(), json!({"maxCallsInRequest": 8}));
    session["capabilities"][CALENDARS] = json!({});
    session["primaryAccounts"][CALENDARS] = json!("A1");
    session["accounts"]["A1"]["accountCapabilities"][CALENDARS] = json!({
        "maxCalendarsPerEvent": null, "minDateTime": "1900-01-01T00:00:00Z", "maxDateTime": "2200-01-01T00:00:00Z",
        "maxExpandedQueryDuration": "P366D", "maxParticipantsPerEvent": null, "mayCreateCalendar": true
    });
    if bis {
        session["capabilities"][CYRUS_JSCALENDAR_BIS] = json!({});
    }
    Mock::given(method("GET"))
        .and(path("/jmap/session"))
        .respond_with(ResponseTemplate::new(200).set_body_json(session))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(answer))
        .mount(server)
        .await;
}

async fn sent(server: &MockServer) -> Value {
    let posted = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.method.as_str() == "POST")
        .unwrap();
    serde_json::from_slice(&posted.body).unwrap()
}

#[tokio::test]
async fn calendars_list_with_their_rights() {
    let server = MockServer::start().await;
    answering(&server, false, json!({"sessionState": "s1", "methodResponses": [
        ["Calendar/get", {"accountId": "A1", "state": "c1", "notFound": [], "list": [
            {"id": "cal-1", "name": "Personal", "color": "#e66100", "isDefault": true, "isVisible": true,
             "timeZone": "Europe/Lisbon", "myRights": {"mayReadItems": true, "mayWriteAll": true, "mayAdmin": true}}]}, "c0"]
    ]})).await;
    let calendars = client(&server).calendars().await.unwrap();
    assert_eq!(calendars[0].name, "Personal");
    assert!(calendars[0].is_default && calendars[0].my_rights.may_admin);
    assert_eq!(
        sent(&server).await["using"],
        json!(["urn:ietf:params:jmap:core", CALENDARS])
    );
}

#[tokio::test]
async fn cyrus_s_2_0_switch_rides_along_where_listed() {
    let server = MockServer::start().await;
    answering(
        &server,
        true,
        json!({"sessionState": "s1", "methodResponses": [
            ["Calendar/get", {"accountId": "A1", "state": "c1", "notFound": [], "list": []}, "c0"]
        ]}),
    )
    .await;
    client(&server).calendars().await.unwrap();
    assert!(
        sent(&server).await["using"]
            .as_array()
            .unwrap()
            .contains(&json!(CYRUS_JSCALENDAR_BIS))
    );
}

#[tokio::test]
async fn a_page_of_events_comes_by_back_reference_with_its_total() {
    let server = MockServer::start().await;
    answering(&server, false, json!({"sessionState": "s1", "methodResponses": [
        ["CalendarEvent/query", {"accountId": "A1", "queryState": "q", "canCalculateChanges": false, "position": 0, "ids": ["ev-1"], "total": 3}, "c0"],
        ["CalendarEvent/get", {"accountId": "A1", "state": "e7", "notFound": [], "list": [{"id": "ev-1", "title": "Rota"}]}, "c1"]
    ]})).await;
    let page = client(&server)
        .calendar_events_page(&json!({"inCalendar": "cal-1"}), 0, 200)
        .await
        .unwrap();
    assert_eq!(
        (page.state.as_str(), page.events.len(), page.total),
        ("e7", 1, Some(3))
    );
    let body = sent(&server).await;
    assert_eq!(
        body["methodCalls"][1][1]["#ids"],
        json!({"resultOf": "c0", "name": "CalendarEvent/query", "path": "/ids"})
    );
    assert_eq!(body["methodCalls"][0][1]["filter"]["inCalendar"], "cal-1");
    assert_eq!(body["methodCalls"][0][1]["calculateTotal"], true);
}

#[tokio::test]
async fn a_write_says_whether_to_mail_the_guests() {
    let server = MockServer::start().await;
    answering(&server, false, json!({"sessionState": "s1", "methodResponses": [
        ["CalendarEvent/set", {"accountId": "A1", "oldState": "e1", "newState": "e2", "created": {"e": {"id": "ev-9", "uid": "u-9"}}}, "c0"]
    ]})).await;
    let mut object = Map::new();
    object.insert("title".into(), json!("Rota"));
    let created = client(&server)
        .set_calendar_event(&EventWrite::Create(object), false, None)
        .await
        .unwrap();
    assert_eq!(created.unwrap()["id"], "ev-9");
    let body = sent(&server).await;
    assert_eq!(body["methodCalls"][0][1]["sendSchedulingMessages"], false);
    assert_eq!(body["methodCalls"][0][1]["create"]["e"]["title"], "Rota");
    assert!(
        body["methodCalls"][0][1].get("ifInState").is_none(),
        "no state, no ifInState"
    );
}

#[tokio::test]
async fn a_write_names_the_state_it_was_made_from() {
    let server = MockServer::start().await;
    answering(&server, false, json!({"sessionState": "s1", "methodResponses": [
        ["CalendarEvent/set", {"accountId": "A1", "oldState": "e1", "newState": "e2", "updated": {"ev-1": null}}, "c0"]
    ]})).await;
    let write = EventWrite::Update {
        id: "ev-1".into(),
        patch: Map::new(),
    };
    client(&server)
        .set_calendar_event(&write, false, Some("e1"))
        .await
        .unwrap();
    assert_eq!(sent(&server).await["methodCalls"][0][1]["ifInState"], "e1");
}

#[tokio::test]
async fn a_refused_update_says_why() {
    let server = MockServer::start().await;
    answering(&server, false, json!({"sessionState": "s1", "methodResponses": [
        ["CalendarEvent/set", {"accountId": "A1", "notUpdated": {"ev-1": {"type": "noSupportedScheduleMethods"}}}, "c0"]
    ]})).await;
    let write = EventWrite::Update {
        id: "ev-1".into(),
        patch: Map::new(),
    };
    let refused = client(&server)
        .set_calendar_event(&write, true, None)
        .await
        .unwrap_err();
    assert!(
        matches!(refused, mailrs_jmap::JmapError::Method { ref kind, .. } if kind == "noSupportedScheduleMethods")
    );
}

#[tokio::test]
async fn shared_calendars_route_events_and_writes_to_their_own_accounts() {
    let server = MockServer::start().await;
    let mut session = session_on(&server.uri(), json!({}));
    let capability = json!({"maxCalendarsPerEvent": null, "maxParticipantsPerEvent": null,
        "minDateTime":"1900-01-01T00:00:00Z", "maxDateTime":"2200-01-01T00:00:00Z",
        "maxExpandedQueryDuration":"P366D", "mayCreateCalendar":true});
    session["capabilities"][CALENDARS] = json!({});
    session["primaryAccounts"][CALENDARS] = json!("A1");
    for account in ["A1", "B1", "C1"] {
        session["accounts"][account] = json!({"name":account,"isPersonal":account=="A1",
            "isReadOnly":account=="C1","accountCapabilities":{CALENDARS:capability}});
    }
    Mock::given(method("GET"))
        .and(path("/jmap/session"))
        .respond_with(ResponseTemplate::new(200).set_body_json(session))
        .mount(&server)
        .await;
    Mock::given(method("POST")).and(path("/api/"))
        .respond_with(|request: &wiremock::Request| {
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            let responses: Vec<Value> = body["methodCalls"].as_array().unwrap().iter().map(|call| {
                let account = call[1]["accountId"].as_str().unwrap();
                let response = match call[0].as_str().unwrap() {
                    "Calendar/get" => json!({"accountId":account,"state":"c1","notFound":[],"list":[
                        {"id":"cal","name":account,"isDefault":true,"myRights":{
                            "mayReadItems":true,"mayWriteAll":true,"mayAdmin":account=="A1"}}]}),
                    "CalendarEvent/query" => json!({"accountId":account,"queryState":"q1","canCalculateChanges":true,"position":0,"ids":["e1"],"total":1}),
                    "CalendarEvent/get" => json!({"accountId":account,"state":"s1","notFound":[],"list":[
                        {"id":"e1","calendarIds":{"cal":true},"title":account}]}),
                    "CalendarEvent/changes" => json!({"accountId":account,"oldState":"s1","newState":"s2","hasMoreChanges":false,"created":[],"updated":["e1"],"destroyed":[]}),
                    "CalendarEvent/set" => json!({"accountId":account,"oldState":"s1","newState":"s2","updated":{"e1":null},"created":{"e":{"id":"e1"}}}),
                    other => panic!("unexpected method {other}"),
                };
                json!([call[0], response, call[2]])
            }).collect();
            ResponseTemplate::new(200).set_body_json(json!({"sessionState":"s1","methodResponses":responses}))
        }).mount(&server).await;
    let client = client(&server);
    let calendars = client.calendars().await.unwrap();
    assert_eq!(calendars.len(), 3);
    assert_eq!(calendars[0].id, "cal", "existing personal IDs stay valid");
    let shared = calendars.iter().find(|c| c.name == "B1").unwrap();
    let reader = calendars.iter().find(|c| c.name == "C1").unwrap();
    assert_ne!(shared.id, calendars[0].id);
    assert_ne!(shared.id, reader.id);
    assert!(!shared.is_default && !reader.is_default);
    assert!(shared.my_rights.may_write_all && !reader.my_rights.may_write_all);
    let page = client
        .calendar_events_page(&json!({"inCalendar":shared.id}), 0, 10)
        .await
        .unwrap();
    assert_eq!(page.events[0]["title"], "B1");
    assert_eq!(
        page.events[0]["calendarIds"],
        json!({shared.id.clone():true})
    );
    let id = page.events[0]["id"].as_str().unwrap().to_string();
    let fetched = client
        .calendar_events(std::slice::from_ref(&id))
        .await
        .unwrap();
    assert_eq!(fetched.list[0]["title"], "B1");
    let changes = client
        .calendar_event_changes(&page.state, 10)
        .await
        .unwrap();
    assert_eq!(changes.updated.as_slice(), std::slice::from_ref(&id));
    client
        .set_calendar_event(
            &EventWrite::Update {
                id,
                patch: serde_json::from_value(json!({"title":"Changed"})).unwrap(),
            },
            false,
            Some(&page.state),
        )
        .await
        .unwrap();
    let created = client
        .set_calendar_event(
            &EventWrite::Create(
                serde_json::from_value(
                    json!({"calendarIds":{shared.id.clone():true},"title":"New"}),
                )
                .unwrap(),
            ),
            false,
            None,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(created["id"], page.events[0]["id"]);
    client
        .set_calendar_event(
            &EventWrite::Destroy(created["id"].as_str().unwrap().into()),
            false,
            None,
        )
        .await
        .unwrap();
    let count = server.received_requests().await.unwrap().len();
    let cross_account = client
        .set_calendar_event(
            &EventWrite::Update {
                id: "e1".into(),
                patch: serde_json::from_value(json!({"calendarIds":{shared.id.clone():true}}))
                    .unwrap(),
            },
            false,
            None,
        )
        .await;
    assert!(matches!(
        cross_account,
        Err(mailrs_jmap::JmapError::Forbidden(_))
    ));
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        count,
        "a cross-account move must not reach the server as a different calendar's id"
    );
    let requests = server.received_requests().await.unwrap();
    let calls: Vec<Value> = requests
        .iter()
        .filter(|r| r.method == "POST")
        .flat_map(|r| {
            serde_json::from_slice::<Value>(&r.body).unwrap()["methodCalls"]
                .as_array()
                .unwrap()
                .clone()
        })
        .collect();
    let query = calls
        .iter()
        .find(|c| c[0] == "CalendarEvent/query")
        .unwrap();
    assert_eq!(query[1]["accountId"], "B1");
    assert_eq!(query[1]["filter"]["inCalendar"], "cal");
    let changes = calls
        .iter()
        .find(|c| c[0] == "CalendarEvent/changes")
        .unwrap();
    assert_eq!(changes[1]["accountId"], "B1");
    assert_eq!(changes[1]["sinceState"], "s1");
    let set = calls.iter().find(|c| c[0] == "CalendarEvent/set").unwrap();
    assert_eq!(set[1]["accountId"], "B1");
    assert_eq!(set[1]["ifInState"], "s1");
    assert_eq!(set[1]["update"]["e1"], json!({"title":"Changed"}));
    let created = calls
        .iter()
        .find(|c| c[0] == "CalendarEvent/set" && c[1].get("create").is_some())
        .unwrap();
    assert_eq!(created[1]["accountId"], "B1");
    assert_eq!(
        created[1]["create"]["e"]["calendarIds"],
        json!({"cal":true})
    );
    let destroyed = calls
        .iter()
        .find(|c| c[0] == "CalendarEvent/set" && c[1].get("destroy").is_some())
        .unwrap();
    assert_eq!(destroyed[1]["accountId"], "B1");
    assert_eq!(destroyed[1]["destroy"], json!(["e1"]));
}
