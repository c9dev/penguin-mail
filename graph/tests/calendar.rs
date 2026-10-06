mod common;

use mailrs_graph::{CALENDAR_COLORS, Response, nearest_color};
use serde_json::json;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn a_calendar_delta_asks_for_its_window_in_utc() {
    let server = MockServer::start().await;
    common::token_endpoint(&server, "r").await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me/calendars/cal-1/calendarView/delta"))
        .and(query_param("startDateTime", "2025-09-27T00:00:00Z"))
        .and(query_param("endDateTime", "2028-09-27T00:00:00Z"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "value": [{"id": "e1", "type": "occurrence", "seriesMasterId": "s1",
                       "originalStart": "2026-10-01T09:00:00Z", "@odata.etag": "W/\"abc\""}],
            "@odata.deltaLink": format!("{}/v1.0/me/calendars/cal-1/calendarView/delta?$deltatoken=x", server.uri()),
        })))
        .mount(&server)
        .await;
    let page = common::graph(&server)
        .calendar_view_delta(
            "cal-1",
            None,
            "2025-09-27T00:00:00Z",
            "2028-09-27T00:00:00Z",
        )
        .await
        .unwrap();
    assert_eq!(page.value[0].series_master_id.as_deref(), Some("s1"));
    assert_eq!(page.value[0].etag.as_deref(), Some("W/\"abc\""));
    let asked = server.received_requests().await.unwrap();
    let delta = asked
        .iter()
        .find(|r| r.url.path().ends_with("/delta"))
        .unwrap();
    let prefer: Vec<_> = delta
        .headers
        .get_all("prefer")
        .iter()
        .map(|v| v.to_str().unwrap().to_string())
        .collect();
    assert!(
        prefer.iter().any(|p| p == "outlook.timezone=\"UTC\""),
        "{prefer:?}"
    );
}

#[tokio::test]
async fn a_change_names_the_version_it_was_made_against() {
    let server = MockServer::start().await;
    common::token_endpoint(&server, "r").await;
    Mock::given(method("PATCH"))
        .and(path("/v1.0/me/events/e1"))
        .and(header("if-match", "W/\"abc\""))
        .respond_with(
            ResponseTemplate::new(412)
                .set_body_json(json!({"error": {"code": "ErrorIrresolvableConflict"}})),
        )
        .mount(&server)
        .await;
    let answer = common::graph(&server)
        .update_event("e1", &json!({"subject": "x"}), Some("W/\"abc\""))
        .await;
    assert!(matches!(
        answer,
        Err(mailrs_graph::GraphError::PreconditionFailed)
    ));
}

#[tokio::test]
async fn an_answer_tells_the_organizer() {
    let server = MockServer::start().await;
    common::token_endpoint(&server, "r").await;
    Mock::given(method("POST"))
        .and(path("/v1.0/me/events/e1/tentativelyAccept"))
        .respond_with(ResponseTemplate::new(202))
        .mount(&server)
        .await;
    common::graph(&server)
        .respond("e1", Response::Tentative, Some("Running late"))
        .await
        .unwrap();
    let asked = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&asked.last().unwrap().body).unwrap();
    assert_eq!(body["sendResponse"], true);
    assert_eq!(body["comment"], "Running late");
}

#[tokio::test]
async fn an_answer_with_no_note_sends_no_comment() {
    let server = MockServer::start().await;
    common::token_endpoint(&server, "r").await;
    Mock::given(method("POST"))
        .and(path("/v1.0/me/events/e1/accept"))
        .respond_with(ResponseTemplate::new(202))
        .mount(&server)
        .await;
    common::graph(&server)
        .respond("e1", Response::Accept, None)
        .await
        .unwrap();
    let asked = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&asked.last().unwrap().body).unwrap();
    assert!(body.get("comment").is_none());
}

#[tokio::test]
async fn one_calendars_view_pages_and_follows_its_own_links() {
    let server = MockServer::start().await;
    common::token_endpoint(&server, "r").await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me/calendars/cal-2/calendarView"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "value": [{"id": "e1"}],
            "@odata.nextLink": format!("{}/v1.0/me/calendars/cal-2/calendarView?$skip=100", server.uri()),
        })))
        .mount(&server)
        .await;
    let graph = common::graph(&server);
    let first = graph
        .calendar_view_of(
            "cal-2",
            "2026-10-01T00:00:00Z",
            "2026-11-01T00:00:00Z",
            None,
        )
        .await
        .unwrap();
    assert_eq!(first.value.len(), 1);
    let next = first.next_link.unwrap();
    graph
        .calendar_view_of("cal-2", "", "", Some(&next))
        .await
        .unwrap();
    let asked = server.received_requests().await.unwrap();
    let views: Vec<_> = asked
        .iter()
        .filter(|r| r.url.path().ends_with("/calendarView"))
        .collect();
    let query = views[0].url.query().unwrap();
    assert!(query.contains("startDateTime=2026-10-01") && query.contains("endDateTime=2026-11-01"));
    assert!(views[1].url.query().unwrap().contains("skip=100"));
    assert!(
        views[1]
            .headers
            .get_all("prefer")
            .iter()
            .any(|v| v == "outlook.timezone=\"UTC\"")
    );
}

#[tokio::test]
async fn a_calendar_is_made_renamed_recolored_and_deleted() {
    let server = MockServer::start().await;
    common::token_endpoint(&server, "r").await;
    Mock::given(method("POST"))
        .and(path("/v1.0/me/calendars"))
        .respond_with(
            ResponseTemplate::new(201)
                .set_body_json(json!({"id": "c9", "name": "Trips", "color": "lightRed"})),
        )
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/v1.0/me/calendars/c9"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"id": "c9", "name": "Travel"})),
        )
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/v1.0/me/calendars/c9"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    let graph = common::graph(&server);
    let made = graph.create_calendar("Trips", "#f08888").await.unwrap();
    assert_eq!(made.id, "c9");
    graph
        .update_calendar("c9", &json!({"name": "Travel", "color": "#a0c8f0"}))
        .await
        .unwrap();
    graph.delete_calendar("c9").await.unwrap();
    let asked = server.received_requests().await.unwrap();
    let bodies: Vec<serde_json::Value> = asked
        .iter()
        .filter(|r| {
            matches!(r.method.as_str(), "POST" | "PATCH") && r.url.path().contains("/calendars")
        })
        .map(|r| serde_json::from_slice(&r.body).unwrap())
        .collect();
    assert_eq!(bodies[0], json!({"name": "Trips", "color": "lightRed"}));
    assert_eq!(bodies[1], json!({"name": "Travel", "color": "lightBlue"}));
}

#[test]
fn a_hex_maps_to_the_nearest_outlook_color() {
    for (name, hex) in CALENDAR_COLORS {
        assert_eq!(nearest_color(hex), name);
    }
    assert_eq!(nearest_color("not a color"), "lightBlue");
}

/// The calendar-view delta leaves `originalStart` out of an exception; a
/// GET of the event has it. Twenty-five exceptions take two `$batch` posts.
#[tokio::test]
async fn original_starts_come_in_batches_of_twenty() {
    let server = MockServer::start().await;
    common::token_endpoint(&server, "r").await;
    Mock::given(method("POST"))
        .and(path("/v1.0/$batch"))
        .respond_with(|request: &wiremock::Request| {
            let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
            let answers: Vec<serde_json::Value> = body["requests"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| {
                    let url = r["url"].as_str().unwrap();
                    let id = url.trim_start_matches("/me/events/").split('?').next().unwrap();
                    assert!(url.contains("%24select=id%2CoriginalStart%2CoriginalStartTimeZone"), "{url}");
                    match id {
                        "x24" => json!({"id": r["id"], "status": 404, "body": {"error": {"code": "ErrorItemNotFound"}}}),
                        _ => json!({"id": r["id"], "status": 200,
                                    "body": {"id": id, "originalStart": "2026-10-05T08:00:00Z"}}),
                    }
                })
                .collect();
            ResponseTemplate::new(200).set_body_json(json!({"responses": answers}))
        })
        .mount(&server)
        .await;
    let ids: Vec<String> = (0..25).map(|n| format!("x{n}")).collect();
    let found = common::graph(&server).original_starts(&ids).await.unwrap();
    assert_eq!(found.len(), 25);
    assert_eq!(found[3].as_ref().unwrap().id, "x3");
    assert_eq!(found[3].as_ref().unwrap().original_start.as_deref(), Some("2026-10-05T08:00:00Z"));
    assert!(matches!(found[24], Err(mailrs_graph::GraphError::NotFound)));
    let posts = server.received_requests().await.unwrap().iter().filter(|r| r.url.path() == "/v1.0/$batch").count();
    assert_eq!(posts, 2);
}

#[tokio::test]
async fn instances_ask_for_their_original_start() {
    let server = MockServer::start().await;
    common::token_endpoint(&server, "r").await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me/events/s1/instances"))
        .and(query_param("$select", "id,type,seriesMasterId,start,end,isAllDay,originalStart,originalStartTimeZone"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "value": [{"id": "x1", "originalStart": "2026-10-05T08:00:00Z"}],
        })))
        .mount(&server)
        .await;
    let found = common::graph(&server)
        .instances("s1", "2026-10-05T00:00:00Z", "2026-10-06T00:00:00Z")
        .await
        .unwrap();
    assert_eq!(found[0].original_start.as_deref(), Some("2026-10-05T08:00:00Z"));
}
