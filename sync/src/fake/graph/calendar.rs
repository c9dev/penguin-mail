//! The fake's calendar: calendars, events and their log.

use mailrs_graph::{
    Attendee, DateTimeZone, DeltaPage, GraphCalendar, GraphError, GraphEvent, ItemBody, Location,
    PatternedRecurrence, Page, Removed, Response, ResponseStatus, nearest_color,
};
use serde_json::Value;

use super::{
    Answer, Answered, Area, FakeGraph, GraphState, Logged, Pending, Round, link, read_link,
};

/// Events a calendar view answers in one page, as Graph's `$top` allows.
const VIEW_PAGE: usize = 100;

impl FakeGraph {
    /// Stores `event` in `calendar` as given (a test writes a series
    /// master, its occurrences and exceptions as Graph would hand them)
    /// and logs it.
    pub fn put_event(&self, calendar: &str, event: GraphEvent) {
        self.with(|s| {
            let id = event.id.clone();
            s.events.insert(id.clone(), (calendar.to_string(), event));
            log(s, calendar, &id);
        });
    }

    /// Adds `calendar` to the list.
    pub fn add_calendar(&self, calendar: GraphCalendar) {
        self.with(|s| s.calendars.push(calendar));
    }
}

fn log(s: &mut GraphState, calendar: &str, id: &str) {
    let seq = s.next_seq();
    s.event_log.push(Logged { seq, place: calendar.to_string(), id: id.to_string() });
}

/// A moment Graph wrote: RFC 3339, or a bare local time that the fake's
/// UTC requests leave in UTC.
fn instant(text: &str) -> Option<i64> {
    if let Ok(t) = chrono::DateTime::parse_from_rfc3339(text) {
        return Some(t.timestamp_millis());
    }
    chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S%.f")
        .ok()
        .map(|t| t.and_utc().timestamp_millis())
}

fn starts(event: &GraphEvent) -> Option<i64> {
    event.start.as_ref().and_then(|t| instant(&t.date_time))
}

/// Whether `event` overlaps the window. An event with no times counts.
fn within(event: &GraphEvent, start: &str, end: &str) -> bool {
    let (Some(from), Some(to)) = (instant(start), instant(end)) else { return true };
    let begins = starts(event);
    let ends = event.end.as_ref().and_then(|t| instant(&t.date_time)).or(begins);
    match (begins, ends) {
        (Some(b), Some(e)) => b < to && e >= from,
        _ => true,
    }
}

fn is_master(event: &GraphEvent) -> bool {
    event.kind.as_deref() == Some("seriesMaster")
}

fn removed_event(id: &str) -> GraphEvent {
    GraphEvent {
        id: id.to_string(),
        removed: Some(Removed { reason: Some("deleted".into()) }),
        ..GraphEvent::default()
    }
}

fn event_of<'a>(s: &'a GraphState, id: &str) -> Answer<&'a GraphEvent> {
    s.events.get(id).map(|(_, e)| e).ok_or(GraphError::NotFound)
}

fn tagged(s: &mut GraphState, event: &mut GraphEvent) {
    let seq = s.next_seq();
    event.etag = Some(format!("W/\"{seq}\""));
}

/// Writes the fields a create or update body sets onto `event`.
fn read_body(event: &mut GraphEvent, body: &Value) {
    fn field<T: serde::de::DeserializeOwned>(body: &Value, key: &str) -> Option<T> {
        serde_json::from_value(body.get(key)?.clone()).ok()
    }
    if let Some(subject) = body["subject"].as_str() {
        event.subject = Some(subject.into());
    }
    if let Some(start) = field::<DateTimeZone>(body, "start") {
        event.start = Some(start);
    }
    if let Some(end) = field::<DateTimeZone>(body, "end") {
        event.end = Some(end);
    }
    if let Some(all_day) = body["isAllDay"].as_bool() {
        event.is_all_day = all_day;
    }
    if let Some(show_as) = body["showAs"].as_str() {
        event.show_as = Some(show_as.into());
    }
    if let Some(place) = body["location"]["displayName"].as_str() {
        event.location = Some(Location { display_name: place.into() });
    }
    if let Some(text) = field::<ItemBody>(body, "body") {
        event.body = Some(text);
    }
    if let Some(attendees) = field::<Vec<Attendee>>(body, "attendees") {
        event.attendees = attendees;
    }
    if body.get("recurrence").is_some_and(Value::is_null) {
        event.recurrence = None;
        event.kind = Some("singleInstance".into());
    } else if let Some(recurrence) = field::<PatternedRecurrence>(body, "recurrence") {
        event.recurrence = Some(recurrence);
        event.kind = Some("seriesMaster".into());
    }
}

pub(super) fn calendars(s: &mut GraphState) -> Answer<Vec<GraphCalendar>> {
    s.refuses(Area::Calendar)?;
    Ok(s.calendars.clone())
}

pub(super) fn calendar_view_delta(
    s: &mut GraphState,
    calendar: &str,
    link_text: Option<&str>,
    start: &str,
    end: &str,
) -> Answer<DeltaPage<GraphEvent>> {
    s.refuses(Area::Calendar)?;
    if !s.calendars.iter().any(|c| c.id == calendar) {
        return Err(GraphError::NotFound);
    }
    let s = &*s;
    Round { log: &s.event_log, expired_before: s.expired_before, now: s.seq, place: calendar }.run(
        link_text,
        || {
            s.events
                .values()
                .filter(|(c, e)| c == calendar && within(e, start, end))
                .map(|(_, e)| as_delta(e))
                .collect()
        },
        |id| s.events.get(id).filter(|(c, _)| c == calendar).map(|(_, e)| as_delta(e)),
        removed_event,
    )
}

/// `event` as a calendar-view delta hands it over. Graph leaves
/// `originalStart` out of an exception there, though `GET /events/{id}`
/// and `/instances` carry it, and it names a series master too, which a
/// plain calendar view does not.
fn as_delta(event: &GraphEvent) -> GraphEvent {
    match event.kind.as_deref() {
        Some("exception") => GraphEvent { original_start: None, ..event.clone() },
        _ => event.clone(),
    }
}

pub(super) fn event(s: &mut GraphState, id: &str) -> Answer<GraphEvent> {
    s.refuses(Area::Calendar)?;
    event_of(s, id).cloned()
}

pub(super) fn instances(s: &mut GraphState, series: &str, start: &str, end: &str) -> Answer<Vec<GraphEvent>> {
    s.refuses(Area::Calendar)?;
    Ok(s.events
        .values()
        .map(|(_, e)| e)
        .filter(|e| e.series_master_id.as_deref() == Some(series) && within(e, start, end))
        .cloned()
        .collect())
}

/// What Graph's `GET /events/{id}?$select=id,originalStart,originalStartTimeZone`
/// answers for each id, logged so a test can count the lookups.
pub(super) fn original_starts(s: &mut GraphState, ids: &[String]) -> Answer<Vec<Answer<GraphEvent>>> {
    s.refuses(Area::Calendar)?;
    s.start_lookups.push(ids.to_vec());
    Ok(ids
        .iter()
        .map(|id| {
            event_of(s, id).map(|e| GraphEvent {
                id: e.id.clone(),
                original_start: e.original_start.clone(),
                original_start_time_zone: e.original_start_time_zone.clone(),
                ..GraphEvent::default()
            })
        })
        .collect())
}

/// What Graph's `$batch` of `GET /events/{id}` answers, logged one entry a
/// request of twenty, as Graph takes them.
pub(super) fn events(s: &mut GraphState, ids: &[String]) -> Answer<Vec<Answer<GraphEvent>>> {
    s.refuses(Area::Calendar)?;
    s.master_reads.extend(ids.chunks(20).map(<[String]>::to_vec));
    Ok(ids.iter().map(|id| event_of(s, id).cloned()).collect())
}

pub(super) fn events_by_uid(s: &mut GraphState, calendar: &str, uid: &str) -> Answer<Vec<GraphEvent>> {
    s.refuses(Area::Calendar)?;
    if !s.calendars.iter().any(|c| c.id == calendar) {
        return Err(GraphError::NotFound);
    }
    Ok(s.events
        .values()
        .filter(|(on, e)| on == calendar && e.ical_uid.as_deref() == Some(uid))
        .map(|(_, e)| e)
        .cloned()
        .collect())
}

pub(super) fn create_event(s: &mut GraphState, calendar: &str, body: &Value) -> Answer<GraphEvent> {
    s.refuses(Area::Calendar)?;
    if !s.calendars.iter().any(|c| c.id == calendar) {
        return Err(GraphError::NotFound);
    }
    s.event_bodies.push(body.clone());
    let transaction = body["transactionId"].as_str();
    if let Some(made) = transaction.and_then(|t| s.transactions.get(t)).and_then(|id| s.events.get(id)) {
        return Ok(made.1.clone());
    }
    let id = s.new_id("event");
    let mut event = GraphEvent {
        id: id.clone(),
        ical_uid: Some(format!("{id}@fake.outlook")),
        kind: Some("singleInstance".into()),
        is_organizer: true,
        response_status: Some(ResponseStatus { response: "organizer".into() }),
        ..GraphEvent::default()
    };
    read_body(&mut event, body);
    tagged(s, &mut event);
    if let Some(t) = transaction {
        s.transactions.insert(t.to_string(), id.clone());
    }
    s.events.insert(id.clone(), (calendar.to_string(), event.clone()));
    log(s, calendar, &id);
    Ok(event)
}

/// Refuses a change made against an etag that is no longer the event's.
fn check_etag(event: &GraphEvent, etag: Option<&str>) -> Answer<()> {
    match etag {
        Some(given) if event.etag.as_deref() != Some(given) => Err(GraphError::PreconditionFailed),
        _ => Ok(()),
    }
}

pub(super) fn update_event(s: &mut GraphState, id: &str, body: &Value, etag: Option<&str>) -> Answer<GraphEvent> {
    s.refuses(Area::Calendar)?;
    let (calendar, mut event) = s.events.get(id).cloned().ok_or(GraphError::NotFound)?;
    check_etag(&event, etag)?;
    s.event_bodies.push(body.clone());
    read_body(&mut event, body);
    tagged(s, &mut event);
    s.events.insert(id.to_string(), (calendar.clone(), event.clone()));
    log(s, &calendar, id);
    Ok(event)
}

pub(super) fn delete_event(s: &mut GraphState, id: &str, etag: Option<&str>) -> Answer<()> {
    s.refuses(Area::Calendar)?;
    check_etag(event_of(s, id)?, etag)?;
    if let Some((calendar, _)) = s.events.remove(id) {
        log(s, &calendar, id);
    }
    Ok(())
}

pub(super) fn respond(s: &mut GraphState, id: &str, response: Response, comment: Option<&str>) -> Answer<()> {
    s.refuses(Area::Calendar)?;
    let (calendar, event) = s.events.get_mut(id).ok_or(GraphError::NotFound)?;
    let text = match response {
        Response::Accept => "accepted",
        Response::Tentative => "tentativelyAccepted",
        Response::Decline => "declined",
    };
    event.response_status = Some(ResponseStatus { response: text.into() });
    let calendar = calendar.clone();
    s.responses.push(Answered {
        event: id.to_string(),
        response,
        comment: comment.filter(|c| !c.is_empty()).map(str::to_string),
    });
    log(s, &calendar, id);
    Ok(())
}

pub(super) fn calendar_view_of(
    s: &mut GraphState,
    calendar: &str,
    start: &str,
    end: &str,
    link_text: Option<&str>,
) -> Answer<Page<GraphEvent>> {
    s.refuses(Area::Calendar)?;
    let (calendar, start, end, offset, slot) = match link_text {
        Some(text) => {
            let (kind, place, _, offset) = read_link(text).ok_or_else(|| GraphError::Decode("bad link".into()))?;
            let slot: usize = place.parse().map_err(|_| GraphError::Decode("bad link".into()))?;
            match (kind.as_str(), s.pending.get(slot)) {
                ("view", Some(Pending::View { calendar, start, end })) => {
                    (calendar.clone(), start.clone(), end.clone(), offset, Some(slot))
                }
                _ => return Err(GraphError::Decode("bad link".into())),
            }
        }
        None => (calendar.to_string(), start.to_string(), end.to_string(), 0, None),
    };
    if !s.calendars.iter().any(|c| c.id == calendar) {
        return Err(GraphError::NotFound);
    }
    let mut found: Vec<&GraphEvent> = s
        .events
        .values()
        .filter(|(c, e)| *c == calendar && !is_master(e) && within(e, &start, &end))
        .map(|(_, e)| e)
        .collect();
    found.sort_by_key(|e| starts(e));
    let value: Vec<GraphEvent> = found.iter().skip(offset).take(VIEW_PAGE).map(|e| (*e).clone()).collect();
    let more = offset + value.len() < found.len();
    let next_link = if more {
        let slot = match slot {
            Some(slot) => slot.to_string(),
            None => s.remember(Pending::View { calendar, start, end }),
        };
        Some(link("view", &slot, 0, Some(offset + value.len())))
    } else {
        None
    };
    Ok(Page { value, next_link })
}

pub(super) fn create_calendar(s: &mut GraphState, name: &str, hex: &str) -> Answer<GraphCalendar> {
    s.refuses(Area::Calendar)?;
    let made = GraphCalendar {
        id: s.new_id("calendar"),
        name: name.into(),
        hex_color: Some(hex.into()),
        color: Some(nearest_color(hex).into()),
        can_edit: true,
        ..GraphCalendar::default()
    };
    s.calendars.push(made.clone());
    Ok(made)
}

pub(super) fn update_calendar(s: &mut GraphState, id: &str, body: &Value) -> Answer<GraphCalendar> {
    s.refuses(Area::Calendar)?;
    let calendar = s.calendars.iter_mut().find(|c| c.id == id).ok_or(GraphError::NotFound)?;
    if let Some(name) = body["name"].as_str() {
        calendar.name = name.into();
    }
    if let Some(color) = body["color"].as_str() {
        match color.starts_with('#') {
            true => {
                calendar.hex_color = Some(color.into());
                calendar.color = Some(nearest_color(color).into());
            }
            false => calendar.color = Some(color.into()),
        }
    }
    Ok(calendar.clone())
}

pub(super) fn delete_calendar(s: &mut GraphState, id: &str) -> Answer<()> {
    s.refuses(Area::Calendar)?;
    let at = s.calendars.iter().position(|c| c.id == id).ok_or(GraphError::NotFound)?;
    // Outlook keeps the default calendar for good.
    if s.calendars[at].is_default_calendar {
        return Err(GraphError::Conflict);
    }
    s.calendars.remove(at);
    let doomed: Vec<String> = s.events.iter().filter(|(_, (c, _))| c == id).map(|(k, _)| k.clone()).collect();
    for event in doomed {
        s.events.remove(&event);
        log(s, id, &event);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use mailrs_graph::{GraphError, Response};

    use crate::fake::FakeGraph;
    use crate::services::microsoft::GraphApi;

    #[tokio::test]
    async fn a_create_retried_with_its_transaction_makes_one_event() {
        let fake = FakeGraph::new();
        let body = serde_json::json!({"subject": "Dentist", "transactionId": "pm123",
            "start": {"dateTime": "2026-10-01T09:00:00", "timeZone": "UTC"},
            "end": {"dateTime": "2026-10-01T10:00:00", "timeZone": "UTC"}});
        let first = fake.create_event("cal-1", &body).await.unwrap();
        let again = fake.create_event("cal-1", &body).await.unwrap();
        assert_eq!(first.id, again.id);
        assert_eq!(fake.with(|s| s.events.len()), 1);
    }

    #[tokio::test]
    async fn a_change_against_an_old_etag_is_refused() {
        let fake = FakeGraph::new();
        let made = fake.create_event("cal-1", &serde_json::json!({"subject": "A"})).await.unwrap();
        let old = made.etag.clone();
        fake.update_event(&made.id, &serde_json::json!({"subject": "B"}), old.as_deref()).await.unwrap();
        let stale = fake.update_event(&made.id, &serde_json::json!({"subject": "C"}), old.as_deref()).await;
        assert!(matches!(stale, Err(GraphError::PreconditionFailed)));
    }

    #[tokio::test]
    async fn an_answer_keeps_its_comment() {
        let fake = FakeGraph::new();
        let made = fake.create_event("cal-1", &serde_json::json!({"subject": "A"})).await.unwrap();
        fake.respond(&made.id, Response::Decline, Some("Away")).await.unwrap();
        let held = fake.event(&made.id).await.unwrap();
        assert_eq!(held.response_status.unwrap().response, "declined");
        assert_eq!(fake.with(|s| s.responses[0].comment.clone()), Some("Away".to_string()));
    }

    #[tokio::test]
    async fn one_calendars_view_pages_through_its_link() {
        let fake = FakeGraph::new();
        for minute in 1..=120 {
            let body = serde_json::json!({"subject": "E",
                "start": {"dateTime": format!("2026-10-01T{:02}:{:02}:00", minute / 60, minute % 60), "timeZone": "UTC"},
                "end": {"dateTime": format!("2026-10-01T{:02}:{:02}:30", minute / 60, minute % 60), "timeZone": "UTC"}});
            fake.create_event("cal-1", &body).await.unwrap();
        }
        let (start, end) = ("2026-10-01T00:00:00Z", "2026-10-02T00:00:00Z");
        let first = fake.calendar_view_of("cal-1", start, end, None).await.unwrap();
        assert_eq!(first.value.len(), 100);
        let second = fake.calendar_view_of("cal-1", start, end, first.next_link.as_deref()).await.unwrap();
        assert_eq!(second.value.len(), 20);
        assert!(second.next_link.is_none());
    }

    #[tokio::test]
    async fn a_calendar_can_be_added_renamed_recolored_and_deleted() {
        let fake = FakeGraph::new();
        let made = fake.create_calendar("Work", "#87d28e").await.unwrap();
        assert_eq!(made.color.as_deref(), Some("lightGreen"));
        let renamed = fake
            .update_calendar(&made.id, &serde_json::json!({"name": "Job", "color": "#f19696"}))
            .await
            .unwrap();
        assert_eq!((renamed.name.as_str(), renamed.color.as_deref()), ("Job", Some("lightRed")));
        fake.create_event(&made.id, &serde_json::json!({"subject": "A"})).await.unwrap();
        fake.delete_calendar(&made.id).await.unwrap();
        assert_eq!(fake.calendars().await.unwrap().len(), 1);
        assert_eq!(fake.with(|s| s.events.len()), 0, "its events go with it");
        assert!(matches!(fake.delete_calendar(&made.id).await, Err(GraphError::NotFound)));
    }

    #[tokio::test]
    async fn a_delta_names_the_series_master_as_graph_does() {
        let fake = FakeGraph::new();
        let master = fake
            .create_event(
                "cal-1",
                &serde_json::json!({"subject": "Standup",
                    "start": {"dateTime": "2026-10-01T09:00:00", "timeZone": "UTC"},
                    "end": {"dateTime": "2026-10-01T09:15:00", "timeZone": "UTC"},
                    "recurrence": {"pattern": {"type": "daily", "interval": 1},
                        "range": {"type": "noEnd", "startDate": "2026-10-01"}}}),
            )
            .await
            .unwrap();
        assert_eq!(master.kind.as_deref(), Some("seriesMaster"));
        let page = fake
            .calendar_view_delta("cal-1", None, "2026-09-01T00:00:00Z", "2026-12-01T00:00:00Z")
            .await
            .unwrap();
        assert_eq!(page.value.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), [master.id.as_str()]);
    }

    #[tokio::test]
    async fn a_delta_leaves_out_an_exceptions_original_start_and_a_get_has_it() {
        let fake = FakeGraph::new();
        fake.put_event(
            "cal-1",
            mailrs_graph::GraphEvent {
                id: "x1".into(),
                kind: Some("exception".into()),
                series_master_id: Some("m1".into()),
                original_start: Some("2026-10-05T08:00:00Z".into()),
                ..Default::default()
            },
        );
        let page = fake
            .calendar_view_delta("cal-1", None, "2026-09-01T00:00:00Z", "2026-12-01T00:00:00Z")
            .await
            .unwrap();
        assert_eq!(page.value[0].original_start, None);
        let held = fake.event("x1").await.unwrap();
        assert_eq!(held.original_start.as_deref(), Some("2026-10-05T08:00:00Z"));
    }
}
