//! The account's calendar over Graph.
//!
//! Graph's calendar-view delta hands over single events, occurrences,
//! exceptions and now and then a series master. The adapter reads each
//! master once, stores the series with its `RRULE` and an `EXDATE` for
//! each occurrence Graph cancelled, drops the plain occurrences, which the
//! rule expands, and stores each exception as a changed occurrence. The
//! delta leaves an exception's `originalStart` out, so the adapter reads
//! it for each exception the round brings; without it the copy shows the
//! series' own occurrence beside the moved one. The window
//! runs one year back and two years ahead; once its end is a month closer
//! than that, the token answers `StateLost` and the copy reads the
//! calendar whole with the window moved on.
//!
//! A series keeps its own zone. Graph answers in UTC, so a weekly 09:00
//! meeting in Lisbon arrives as 08:00 and then 09:00 across the autumn
//! clock change; the copy expands a rule in the event's zone, so the
//! series is stored in the zone Outlook made it in, and its start stays
//! the true instant.

mod recurrence;
mod zones;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use chrono::{DateTime, NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;
use mailrs_domain::EpochMillis;
use mailrs_domain::calendar::{self as model, Access, Guest, Kind, Reminder, ReminderMethod, Status};
use mailrs_domain::invitation::Answer;
use mailrs_domain::translate::gettext;
use mailrs_graph::{DateTimeZone, GraphCalendar, GraphError, GraphEvent, Response};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{GraphApi, Microsoft, Service};
use crate::BackendError;
use crate::services::CalendarService;

const DAY: i64 = 86_400_000;
const WINDOW_AHEAD: i64 = 730 * DAY;
const MONTH: i64 = 30 * DAY;
/// Most series a token remembers, so a calendar of thousands of them
/// cannot grow it without bound.
const MOST_SERIES: usize = 500;
/// The shape of the sync token this build writes. Builds before 1 stored
/// exceptions without the start they replace; a token of an older shape
/// answers `StateLost`, so the copy reads the calendar whole once and
/// rewrites each exception.
const TOKEN_SHAPE: u32 = 1;

/// Where a read of the calendar stands, kept in the sync token and the
/// page token. Nothing outside this file reads either.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CalendarToken {
    /// The end of the window the delta link covers.
    end: i64,
    link: String,
    /// Series masters seen, which a removed occurrence sends back to.
    series: Vec<String>,
    /// Masters read in this round already.
    #[serde(default)]
    fresh: Vec<String>,
    /// [`TOKEN_SHAPE`] when the token was written; 0 before it existed.
    #[serde(default)]
    shape: u32,
}

/// An instant as Graph writes a time: `date_time` is a local time in
/// `time_zone`, which is UTC for every call the adapter makes.
fn millis(at: &DateTimeZone) -> Option<i64> {
    let naive = NaiveDateTime::parse_from_str(&at.date_time, "%Y-%m-%dT%H:%M:%S%.f").ok()?;
    let zone = zones::zone_named(&at.time_zone).unwrap_or(chrono_tz::UTC);
    let instant = zone.from_local_datetime(&naive).earliest()?;
    Some(instant.with_timezone(&Utc).timestamp_millis())
}

fn iso(at: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(at)
        .unwrap_or_default()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn utc_name(zone: Tz) -> String {
    match zone.name() {
        "Etc/UTC" => "UTC".to_string(),
        name => name.to_string(),
    }
}

fn answer(response: &str) -> Option<Answer> {
    match response {
        "accepted" => Some(Answer::Yes),
        "declined" => Some(Answer::No),
        "tentativelyAccepted" => Some(Answer::Maybe),
        _ => None,
    }
}

/// Where a series keeps its clock: the zone Outlook made it in. UTC for a
/// name the table lacks, and for anything that does not repeat.
fn series_zone(e: &GraphEvent) -> Tz {
    if e.recurrence.is_none() {
        return chrono_tz::UTC;
    }
    let named = e
        .recurrence
        .as_ref()
        .and_then(|r| r.range.recurrence_time_zone.as_deref())
        .or(e.original_start_time_zone.as_deref());
    named.and_then(zones::zone_named).unwrap_or(chrono_tz::UTC)
}

/// The midnight (UTC) that starts the day `instant` falls on in `zone`,
/// which is how the copy keeps an all-day event. Graph answers an all-day
/// event's midnight in UTC, so a zone east of Greenwich puts it on the
/// evening before.
fn day_of(instant: i64, zone: Tz) -> i64 {
    let Some(at) = DateTime::<Utc>::from_timestamp_millis(instant) else {
        return instant;
    };
    let day = at.with_timezone(&zone).date_naive();
    day.and_hms_opt(0, 0, 0).map_or(instant, |t| t.and_utc().timestamp_millis())
}

/// The `EXDATE` line for each occurrence Graph cancelled, named
/// `OID.<master id>.<YYYY-MM-DD>`, at the series' own time of day.
fn exdates(master: &GraphEvent, start: i64, all_day: bool, zone: Tz) -> Vec<String> {
    let Some(first) = DateTime::<Utc>::from_timestamp_millis(start) else {
        return Vec::new();
    };
    master
        .cancelled_occurrences
        .iter()
        .filter_map(|name| {
            let (_, day) = name.rsplit_once('.')?;
            let day = NaiveDate::parse_from_str(day, "%Y-%m-%d").ok()?;
            let date = day.format("%Y%m%d");
            Some(if all_day {
                format!("EXDATE;VALUE=DATE:{date}")
            } else if zone == chrono_tz::UTC {
                format!("EXDATE:{date}T{}Z", first.format("%H%M%S"))
            } else {
                format!("EXDATE;TZID={}:{date}T{}", utc_name(zone), first.with_timezone(&zone).format("%H%M%S"))
            })
        })
        .collect()
}

/// Outlook's named calendar colors, as `#rrggbb`.
fn color_of(calendar: &GraphCalendar) -> String {
    if let Some(hex) = calendar.hex_color.as_deref().filter(|h| h.starts_with('#') && h.len() == 7) {
        return hex.to_ascii_lowercase();
    }
    let named = calendar.color.as_deref().unwrap_or_default();
    mailrs_graph::CALENDAR_COLORS
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(named))
        .map_or("#0078d4", |(_, hex)| hex)
        .to_string()
}

fn calendar_of(c: &GraphCalendar) -> model::Calendar {
    model::Calendar {
        id: c.id.clone(),
        name: c.name.clone(),
        color: color_of(c),
        access: match (c.can_edit, c.is_default_calendar) {
            (true, true) => Access::Owner,
            (true, false) => Access::Writer,
            _ => Access::Reader,
        },
        primary: c.is_default_calendar,
        shown: true,
        reminders: Vec::new(),
        ..model::Calendar::default()
    }
}

/// An event's body as the copy keeps it: plain text.
fn description_of(e: &GraphEvent) -> String {
    e.body
        .as_ref()
        .map(|b| match b.content_type.as_str() {
            "html" => mailrs_mime::html::html_to_text(&b.content),
            _ => b.content.clone(),
        })
        .unwrap_or_default()
}

/// Trims an edit's `body` against the event Graph holds, since the copy
/// keeps less than Graph does. A description left as it was stays out, so
/// an HTML body, such as a Teams meeting's join block, is not rewritten as
/// plain text. A guest list left as it was stays out too; a changed one
/// keeps each remaining attendee's type (optional, a booked room) and
/// makes only the new ones required.
fn trim_edit(body: &mut Value, event: &model::Event, current: &GraphEvent, me: &str) {
    let Some(map) = body.as_object_mut() else {
        return;
    };
    if description_of(current) == event.description {
        map.remove("body");
    }
    let address = |a: &mailrs_graph::Attendee| a.email_address.address.clone().unwrap_or_default().to_ascii_lowercase();
    let kinds: std::collections::HashMap<String, String> =
        current.attendees.iter().map(|a| (address(a), a.kind.clone().unwrap_or_else(|| "required".into()))).collect();
    let Some(Value::Array(attendees)) = map.get_mut("attendees") else {
        return;
    };
    let sent: BTreeSet<String> = attendees
        .iter()
        .filter_map(|a| a["emailAddress"]["address"].as_str())
        .map(str::to_ascii_lowercase)
        .collect();
    let organizer = current.organizer.as_ref().and_then(|o| o.email_address.address.clone()).unwrap_or_default().to_ascii_lowercase();
    let held: BTreeSet<String> = kinds.keys().filter(|a| **a != organizer && !a.eq_ignore_ascii_case(me)).cloned().collect();
    if sent == held {
        map.remove("attendees");
        return;
    }
    for attendee in attendees.iter_mut() {
        let known = attendee["emailAddress"]["address"].as_str().map(str::to_ascii_lowercase).and_then(|a| kinds.get(&a).cloned());
        if let Some(kind) = known {
            attendee["type"] = json!(kind);
        }
    }
}

fn refused(line: String) -> BackendError {
    BackendError::Refused(line)
}

impl<G: GraphApi> Microsoft<G> {
    fn event_of(&self, e: &GraphEvent, calendar: &str) -> model::Event {
        let zone = series_zone(e);
        let shown_in = e.original_start_time_zone.as_deref().and_then(zones::zone_named).unwrap_or(chrono_tz::UTC);
        let mut start = e.start.as_ref().and_then(millis).unwrap_or_default();
        let mut end = e.end.as_ref().and_then(millis).unwrap_or(start);
        if e.is_all_day {
            (start, end) = (day_of(start, shown_in), day_of(end, shown_in));
        }
        // A day has no clock to follow, so an all-day series expands in UTC.
        let zone = if e.is_all_day { chrono_tz::UTC } else { zone };
        let me = self.settings().address.to_ascii_lowercase();
        let organizer = e.organizer.as_ref().and_then(|o| o.email_address.address.clone());
        let mut rules: Vec<String> =
            e.recurrence.as_ref().and_then(|r| recurrence::rrule_of(r, zone, e.is_all_day)).into_iter().collect();
        rules.extend(exdates(e, start, e.is_all_day, zone));
        model::Event {
            calendar: calendar.to_string(),
            id: e.id.clone(),
            uid: e.ical_uid.clone().unwrap_or_default(),
            etag: e.etag.clone().unwrap_or_default(),
            start,
            end,
            zone: utc_name(zone),
            all_day: e.is_all_day,
            title: e.subject.clone().unwrap_or_default(),
            place: e.location.as_ref().map(|l| l.display_name.clone()).unwrap_or_default(),
            description: description_of(e),
            color: None,
            busy: e.show_as.as_deref() != Some("free"),
            status: if e.is_cancelled { Status::Cancelled } else { Status::Confirmed },
            private: matches!(e.sensitivity.as_deref(), Some("private" | "confidential")),
            guests: e
                .attendees
                .iter()
                .filter_map(|a| {
                    let email = a.email_address.address.clone()?;
                    Some(Guest {
                        me: email.eq_ignore_ascii_case(&me),
                        organizer: organizer.as_deref().is_some_and(|o| o.eq_ignore_ascii_case(&email)),
                        name: a.email_address.name.clone(),
                        answer: a.status.as_ref().and_then(|s| answer(&s.response)),
                        email,
                    })
                })
                .collect(),
            organizer,
            my_answer: (!e.is_organizer).then(|| e.response_status.as_ref().and_then(|s| answer(&s.response))).flatten(),
            reminders: Some(match e.is_reminder_on {
                Some(true) => vec![Reminder {
                    minutes: e.reminder_minutes_before_start.unwrap_or(15),
                    method: ReminderMethod::Notification,
                }],
                _ => Vec::new(),
            }),
            conference: e.online_meeting.as_ref().and_then(|m| m.join_url.clone()),
            rules,
            series: (e.kind.as_deref() == Some("exception")).then(|| e.series_master_id.clone()).flatten(),
            original_start: e
                .original_start
                .as_deref()
                .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
                .map(|t| t.timestamp_millis()),
            pending: false,
            meet_request: None,
            kind: if e.show_as.as_deref() == Some("oof") { Kind::OutOfOffice(Default::default()) } else { Kind::default() },
            ..model::Event::default()
        }
    }

    /// The body Graph takes for `event`. A repeat Outlook cannot hold is a
    /// refusal; the series' own zone goes with its times, as a Windows
    /// name, so the rule follows that clock.
    fn body_of(&self, event: &model::Event, create: bool) -> Result<Value, BackendError> {
        let wanted: Tz = event.zone.parse().unwrap_or(chrono_tz::UTC);
        let (zone, name) = match zones::windows_name(wanted) {
            Some(name) if !event.all_day => (wanted, name),
            _ => (chrono_tz::UTC, "UTC"),
        };
        let local = |at: i64| -> String {
            let utc = DateTime::<Utc>::from_timestamp_millis(at).unwrap_or_default();
            utc.with_timezone(&zone).format("%Y-%m-%dT%H:%M:%S").to_string()
        };
        let me = self.settings().address.to_ascii_lowercase();
        let attendees: Vec<Value> = event
            .guests
            .iter()
            .filter(|g| !g.organizer && !g.me && !g.email.eq_ignore_ascii_case(&me))
            .map(|g| json!({ "emailAddress": { "address": g.email, "name": g.name }, "type": "required" }))
            .collect();
        let mut body = json!({
            "subject": event.title,
            "body": { "contentType": "text", "content": event.description },
            "start": { "dateTime": local(event.start), "timeZone": name },
            "end": { "dateTime": local(event.end), "timeZone": name },
            "isAllDay": event.all_day,
            "location": { "displayName": event.place },
            "showAs": match (&event.kind, event.busy) {
                (Kind::OutOfOffice(_), true) => "oof",
                (_, true) => "busy",
                (_, false) => "free",
            },
            "sensitivity": if event.private { "private" } else { "normal" },
            "attendees": attendees,
        });
        if let Some(reminders) = &event.reminders {
            body["isReminderOn"] = json!(!reminders.is_empty());
            if let Some(first) = reminders.first() {
                body["reminderMinutesBeforeStart"] = json!(first.minutes);
            }
        }
        if event.series.is_none() {
            let day = DateTime::<Utc>::from_timestamp_millis(event.start).unwrap_or_default().with_timezone(&zone).date_naive();
            let repeat = recurrence::recurrence_of(&event.rules, day, zone)
                .map_err(|_| refused(gettext("Outlook cannot repeat an event that way.")))?;
            match repeat {
                Some(mut repeat) => {
                    repeat.range.recurrence_time_zone = Some(name.to_string());
                    body["recurrence"] = serde_json::to_value(repeat).unwrap_or(Value::Null);
                }
                // A change that leaves no rule ends the repeat on Outlook's
                // side too; a patch without the key would keep it.
                None if !create => body["recurrence"] = Value::Null,
                None => {}
            }
        }
        if create {
            body["transactionId"] = json!(event.id);
        }
        Ok(body)
    }

    /// The event with iCalendar UID `uid` and the calendar that holds it,
    /// looking on `first` before the account's other calendars.
    async fn known_by_uid(&self, first: &str, uid: &str) -> Result<Option<(String, GraphEvent)>, BackendError> {
        let mut calendars = vec![first.to_string()];
        let mut listed = false;
        while let Some(calendar) = calendars.pop() {
            let found = self.graph().events_by_uid(&calendar, uid).await.map_err(|e| self.service(e))?;
            if let Some(known) = found.into_iter().find(|e| !matches!(e.kind.as_deref(), Some("occurrence" | "exception"))) {
                return Ok(Some((calendar, known)));
            }
            if !listed {
                listed = true;
                let all = self.graph().calendars().await.map_err(|e| self.service(e))?;
                calendars.extend(all.into_iter().map(|c| c.id).filter(|id| id != first));
            }
        }
        Ok(None)
    }

    fn service(&self, err: GraphError) -> BackendError {
        self.service_error(Service::Calendar, err)
    }

    /// Graph's instance of series `series` that started at `original`.
    async fn instance_of(&self, series: &str, original: i64) -> Result<GraphEvent, BackendError> {
        let found = self
            .graph()
            .instances(series, &iso(original - 60_000), &iso(original + DAY))
            .await
            .map_err(|e| self.service(e))?;
        found
            .into_iter()
            .find(|e| {
                e.original_start.as_deref().and_then(|t| DateTime::parse_from_rfc3339(t).ok()).map(|t| t.timestamp_millis())
                    == Some(original)
            })
            .ok_or(BackendError::NotFound)
    }

    async fn changes_of(
        &self,
        calendar: &str,
        token: Option<&str>,
        page: Option<&str>,
        from: EpochMillis,
    ) -> Result<model::EventPage, BackendError> {
        let now = crate::now_millis();
        let read = |text: &str| serde_json::from_str::<CalendarToken>(text).map_err(|_| BackendError::StateLost);
        let mut at = match (page, token) {
            (Some(page), _) => read(page)?,
            (None, Some(token)) => {
                let held = read(token)?;
                // The window's end has come a month nearer, or an older
                // build stored the rows: read the calendar whole.
                if held.end - now < WINDOW_AHEAD - MONTH || held.shape < TOKEN_SHAPE {
                    return Err(BackendError::StateLost);
                }
                CalendarToken { fresh: Vec::new(), ..held }
            }
            (None, None) => CalendarToken {
                end: now + WINDOW_AHEAD,
                link: String::new(),
                series: Vec::new(),
                fresh: Vec::new(),
                shape: TOKEN_SHAPE,
            },
        };
        let (start, end) = (iso(from), iso(at.end));
        let link = (!at.link.is_empty()).then_some(at.link.as_str());
        let got = self.graph().calendar_view_delta(calendar, link, &start, &end).await.map_err(|e| self.service(e))?;
        let (mut events, mut removed) = (Vec::new(), Vec::new());
        let mut masters: BTreeSet<String> = BTreeSet::new();
        let mut exceptions = Vec::new();
        for e in &got.value {
            if e.removed.is_some() {
                removed.push(e.id.clone());
                // A removed occurrence changes its master's cancelled list,
                // and the delta does not name the master: read every series
                // this calendar holds again.
                masters.extend(at.series.iter().cloned());
                continue;
            }
            match e.kind.as_deref() {
                Some("occurrence") => masters.extend(e.series_master_id.clone()),
                Some("exception") => {
                    masters.extend(e.series_master_id.clone());
                    exceptions.push(e.clone());
                }
                // The master a delta names is read whole like any other,
                // so its cancelled occurrences come with it.
                Some("seriesMaster") => {
                    masters.insert(e.id.clone());
                }
                _ => events.push(self.event_of(e, calendar)),
            }
        }
        let gone = self.read_original_starts(&mut exceptions).await?;
        removed.extend(gone);
        events.extend(exceptions.iter().map(|e| self.event_of(e, calendar)));
        let unread: Vec<String> = masters.into_iter().filter(|m| !at.fresh.contains(m)).collect();
        for master in unread {
            match self.graph().event(&master).await {
                Ok(e) => events.push(self.event_of(&e, calendar)),
                Err(GraphError::NotFound) => removed.push(master.clone()),
                Err(err) => return Err(self.service(err)),
            }
            if !at.series.contains(&master) && at.series.len() < MOST_SERIES {
                at.series.push(master.clone());
            }
            at.fresh.push(master);
        }
        let held = |link: String, fresh: Vec<String>| serde_json::to_string(&CalendarToken { link, fresh, ..at.clone() }).ok();
        match (got.next_link, got.delta_link) {
            (Some(next), _) => {
                let fresh = at.fresh.clone();
                Ok(model::EventPage { events, removed, next_page: held(next, fresh), ..model::EventPage::default() })
            }
            (None, Some(delta)) => {
                Ok(model::EventPage { events, removed, next_sync: held(delta, Vec::new()), ..model::EventPage::default() })
            }
            (None, None) => Err(BackendError::StateLost),
        }
    }

    /// Fills in the `originalStart` the calendar-view delta leaves out of
    /// each exception, from one `$batch` entry an exception, twenty to a
    /// request. A delta names an exception only when it changed, so a
    /// quiet round asks nothing. Takes out and answers the ids Graph no
    /// longer holds.
    async fn read_original_starts(&self, exceptions: &mut Vec<GraphEvent>) -> Result<Vec<String>, BackendError> {
        let missing: Vec<String> =
            exceptions.iter().filter(|e| e.original_start.is_none()).map(|e| e.id.clone()).collect();
        if missing.is_empty() {
            return Ok(Vec::new());
        }
        let found = self.graph().original_starts(&missing).await.map_err(|e| self.service(e))?;
        let mut gone = Vec::new();
        for (id, answer) in missing.into_iter().zip(found) {
            let held = match answer {
                Ok(held) => held,
                Err(GraphError::NotFound) => {
                    gone.push(id);
                    continue;
                }
                Err(err) => return Err(self.service(err)),
            };
            if let Some(e) = exceptions.iter_mut().find(|e| e.id == id) {
                e.original_start = held.original_start;
                e.original_start_time_zone = e.original_start_time_zone.take().or(held.original_start_time_zone);
            }
        }
        exceptions.retain(|e| !gone.contains(&e.id));
        Ok(gone)
    }
}

impl<G: GraphApi> CalendarService for Microsoft<G> {
    async fn calendars(&self) -> Result<Vec<model::Calendar>, BackendError> {
        let all = self.graph().calendars().await.map_err(|e| self.service(e))?;
        Ok(all.iter().map(calendar_of).collect())
    }

    async fn event_changes(
        &self,
        calendar: &str,
        token: Option<&str>,
        page: Option<&str>,
        from: EpochMillis,
    ) -> Result<model::EventPage, BackendError> {
        self.changes_of(calendar, token, page, from).await
    }

    async fn event_range(
        &self,
        calendar: &str,
        from: EpochMillis,
        to: EpochMillis,
        page: Option<&str>,
    ) -> Result<model::EventPage, BackendError> {
        let got = self
            .graph()
            .calendar_view_of(calendar, &iso(from), &iso(to), page)
            .await
            .map_err(|e| self.service(e))?;
        // The view lists occurrences, not their master, so each shows as
        // an event of its own rather than as a change to a series the
        // copy has not read.
        let events = got
            .value
            .iter()
            .map(|e| model::Event { series: None, original_start: None, ..self.event_of(e, calendar) })
            .collect();
        Ok(model::EventPage { events, next_page: got.next_link, ..model::EventPage::default() })
    }

    /// Graph mails the guests on every change an organizer makes and has
    /// no switch against it, so `notify` is not read: the window does not
    /// offer to send nobody for such an account (`Offers::quiet_changes`).
    async fn put_event(
        &self,
        event: &model::Event,
        etag: Option<&str>,
        create: bool,
        _notify: model::Notify,
    ) -> Result<model::Event, BackendError> {
        let calendar = event.calendar.as_str();
        let mut body = self.body_of(event, create)?;
        let written = if create {
            self.graph().create_event(calendar, &body).await
        } else if let Some((series, original)) = model::split_occurrence_id(&event.id) {
            // An occurrence no row holds yet: Graph has its own id for the
            // instance, and an instance takes no recurrence or version of
            // the series.
            let instance = self.instance_of(series, original).await?;
            if let Some(map) = body.as_object_mut() {
                map.remove("recurrence");
            }
            let current = self.graph().event(&instance.id).await.map_err(|e| self.service(e))?;
            trim_edit(&mut body, event, &current, &self.settings().address);
            self.graph().update_event(&instance.id, &body, None).await
        } else {
            let current = self.graph().event(&event.id).await.map_err(|e| self.service(e))?;
            trim_edit(&mut body, event, &current, &self.settings().address);
            self.graph().update_event(&event.id, &body, etag).await
        };
        Ok(self.event_of(&written.map_err(|e| self.service(e))?, calendar))
    }

    async fn remove_event(
        &self,
        _calendar: &str,
        id: &str,
        etag: Option<&str>,
        _notify: model::Notify,
    ) -> Result<(), BackendError> {
        if let Some((series, original)) = model::split_occurrence_id(id) {
            let instance = self.instance_of(series, original).await?;
            return self.graph().delete_event(&instance.id, None).await.map_err(|e| self.service(e));
        }
        self.graph().delete_event(id, etag).await.map_err(|e| self.service(e))
    }

    /// Finds the event by its UID, on the calendar asked for first and
    /// then on the others, and updates it there, or makes it with no
    /// attendees, so importing a file mails nobody. An update leaves the
    /// guests and the repeat as Outlook holds them: a file names neither
    /// the guests of a meeting the person organizes nor, for one
    /// occurrence, the series it belongs to. Such a file, one with no
    /// repeat for an event that repeats, leaves the series alone, rather
    /// than turning it into one event at that occurrence's time.
    async fn import_event(&self, event: &model::Event) -> Result<model::Event, BackendError> {
        let Some((calendar, known)) = self.known_by_uid(&event.calendar, &event.uid).await? else {
            let quiet = model::Event { guests: Vec::new(), ..event.clone() };
            let body = self.body_of(&quiet, true)?;
            let made = self.graph().create_event(&event.calendar, &body).await.map_err(|e| self.service(e))?;
            return Ok(self.event_of(&made, &event.calendar));
        };
        if known.recurrence.is_some() && event.rules.is_empty() {
            return Ok(self.event_of(&known, &calendar));
        }
        let mut body = self.body_of(event, false)?;
        if let Some(map) = body.as_object_mut() {
            map.remove("attendees");
            map.remove("recurrence");
        }
        let written = self.graph().update_event(&known.id, &body, None).await.map_err(|e| self.service(e))?;
        Ok(self.event_of(&written, &calendar))
    }

    async fn upload_attachment(
        &self,
        _file: &model::Attachment,
        _sent: Arc<AtomicU64>,
    ) -> Result<model::Attachment, BackendError> {
        Err(refused(gettext("Outlook cannot attach a file to an event.")))
    }

    async fn share_file(&self, _file_id: &str, _email: &str) -> Result<(), BackendError> {
        Err(refused(gettext("Outlook has no files to share.")))
    }

    async fn move_event(
        &self,
        _event: &model::Event,
        _destination: &str,
        _notify: model::Notify,
    ) -> Result<model::Event, BackendError> {
        Err(refused(gettext("Outlook cannot move an event to another calendar.")))
    }

    async fn answer_event(
        &self,
        calendar: &str,
        id: &str,
        _me: &str,
        answer: Answer,
        note: Option<&str>,
    ) -> Result<model::Event, BackendError> {
        let target = match model::split_occurrence_id(id) {
            Some((series, original)) => self.instance_of(series, original).await?.id,
            None => id.to_string(),
        };
        self.graph().respond(&target, response_of(answer), note).await.map_err(|e| self.service(e))?;
        let held = self.graph().event(&target).await.map_err(|e| self.service(e))?;
        Ok(self.event_of(&held, calendar))
    }

    async fn edit_list(
        &self,
        calendar: &str,
        edit: &model::list::ListEdit,
    ) -> Result<Option<model::Calendar>, BackendError> {
        use model::list::ListEdit;
        let graph = self.graph();
        let made = match edit {
            ListEdit::Create { name, color, .. } => graph.create_calendar(name, color).await,
            ListEdit::Rename { name } => graph.update_calendar(calendar, &json!({ "name": name })).await,
            ListEdit::Recolor { color } => graph.update_calendar(calendar, &json!({ "color": color })).await,
            ListEdit::Delete => return graph.delete_calendar(calendar).await.map(|()| None).map_err(|e| self.service(e)),
            // What Outlook has no call for is a refusal, not `Unsupported`:
            // that answer would keep the change in the queue for good.
            ListEdit::Unsubscribe => return Err(refused(gettext("Outlook cannot take a calendar off the list."))),
            ListEdit::Hide { .. } => return Err(refused(gettext("Outlook cannot hide a calendar on all your devices."))),
            ListEdit::Subscribe { .. } => return Err(refused(gettext("Outlook cannot subscribe to a calendar by its address."))),
            ListEdit::Add => return Err(refused(gettext("Outlook has no other calendars to add."))),
        };
        Ok(Some(calendar_of(&made.map_err(|e| self.service(e))?)))
    }
}

fn response_of(answer: Answer) -> Response {
    match answer {
        Answer::Yes => Response::Accept,
        Answer::No => Response::Decline,
        Answer::Maybe => Response::Tentative,
    }
}
