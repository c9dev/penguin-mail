//! Calendars and events. Every call asks for times in UTC, so a start is
//! an instant the adapter reads without a zone table.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::GraphError;
use crate::http::{BatchRequest, DeltaPage, Graph, IMMUTABLE_IDS, Method, Page};
use crate::mail::with_query;
use crate::model::{DateTimeZone, EmailAddress, ItemBody, Recipient, Removed};

const UTC: &str = "outlook.timezone=\"UTC\"";
const PAGE_PREFER: &str = "odata.maxpagesize=50";

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct GraphCalendar {
    pub id: String,
    pub name: String,
    /// `#rrggbb` when the person picked a colour, else empty.
    pub hex_color: Option<String>,
    /// Outlook's named colour (`auto`, `lightBlue`, ...).
    pub color: Option<String>,
    pub can_edit: bool,
    pub is_default_calendar: bool,
    pub owner: Option<EmailAddress>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct ResponseStatus {
    /// `none`, `organizer`, `tentativelyAccepted`, `accepted`, `declined`,
    /// `notResponded`.
    pub response: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct Attendee {
    pub email_address: EmailAddress,
    pub status: Option<ResponseStatus>,
    #[serde(rename = "type")]
    pub kind: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct Location {
    pub display_name: String,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct OnlineMeeting {
    pub join_url: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct RecurrencePattern {
    /// `daily`, `weekly`, `absoluteMonthly`, `relativeMonthly`,
    /// `absoluteYearly`, `relativeYearly`.
    #[serde(rename = "type")]
    pub kind: String,
    pub interval: u32,
    #[serde(skip_serializing_if = "is_zero")]
    pub month: u32,
    #[serde(skip_serializing_if = "is_zero")]
    pub day_of_month: u32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub days_of_week: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_day_of_week: Option<String>,
    /// `first`, `second`, `third`, `fourth`, `last`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index: Option<String>,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct RecurrenceRange {
    /// `endDate`, `noEnd`, `numbered`.
    #[serde(rename = "type")]
    pub kind: String,
    /// `YYYY-MM-DD`.
    pub start_date: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_date: Option<String>,
    #[serde(skip_serializing_if = "is_zero")]
    pub number_of_occurrences: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recurrence_time_zone: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct PatternedRecurrence {
    pub pattern: RecurrencePattern,
    pub range: RecurrenceRange,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct GraphEvent {
    pub id: String,
    #[serde(rename = "iCalUId")]
    pub ical_uid: Option<String>,
    #[serde(rename = "@odata.etag")]
    pub etag: Option<String>,
    pub subject: Option<String>,
    pub body: Option<ItemBody>,
    pub start: Option<DateTimeZone>,
    pub end: Option<DateTimeZone>,
    pub is_all_day: bool,
    pub location: Option<Location>,
    /// `free`, `tentative`, `busy`, `oof`, `workingElsewhere`.
    pub show_as: Option<String>,
    pub is_cancelled: bool,
    /// `normal`, `personal`, `private`, `confidential`.
    pub sensitivity: Option<String>,
    pub organizer: Option<Recipient>,
    pub attendees: Vec<Attendee>,
    pub response_status: Option<ResponseStatus>,
    pub is_organizer: bool,
    pub is_reminder_on: Option<bool>,
    pub reminder_minutes_before_start: Option<u32>,
    pub online_meeting: Option<OnlineMeeting>,
    pub recurrence: Option<PatternedRecurrence>,
    /// `singleInstance`, `occurrence`, `exception`, `seriesMaster`.
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub series_master_id: Option<String>,
    /// For an occurrence or an exception, the start it had in the series,
    /// as an instant.
    pub original_start: Option<String>,
    /// The Windows (or IANA) name of the zone the event was made in,
    /// which a series' wall-clock times follow.
    pub original_start_time_zone: Option<String>,
    /// On a series master, the occurrences taken out of it, as Graph names
    /// them: `OID.<master id>.<YYYY-MM-DD>`.
    pub cancelled_occurrences: Vec<String>,
    #[serde(rename = "@removed")]
    pub removed: Option<Removed>,
}

/// The answers an invitation takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Response {
    Accept,
    Tentative,
    Decline,
}

impl Graph {
    pub async fn calendars(&self) -> Result<Vec<GraphCalendar>, GraphError> {
        let page: Page<GraphCalendar> = self
            .get(
                "me/calendars",
                &[
                    ("$top", "100"),
                    (
                        "$select",
                        "id,name,hexColor,color,canEdit,isDefaultCalendar,owner",
                    ),
                ],
            )
            .await?;
        Ok(page.value)
    }

    /// One page of `calendar`'s view between `start` and `end` (RFC 3339),
    /// as a delta round: single events, occurrences, exceptions and, now
    /// and then, a series master. An exception comes without its
    /// `originalStart`; [`Graph::original_starts`] reads it. `link`
    /// continues a round or starts the next.
    pub async fn calendar_view_delta(
        &self,
        calendar: &str,
        link: Option<&str>,
        start: &str,
        end: &str,
    ) -> Result<DeltaPage<GraphEvent>, GraphError> {
        if let Some(link) = link {
            return self.follow(link, &[UTC, PAGE_PREFER]).await;
        }
        self.get_with(
            &format!("me/calendars/{calendar}/calendarView/delta"),
            &[("startDateTime", start), ("endDateTime", end)],
            &[UTC, PAGE_PREFER],
        )
        .await
    }

    /// One event with its recurrence, such as a series master.
    pub async fn event(&self, id: &str) -> Result<GraphEvent, GraphError> {
        self.get_with(&format!("me/events/{id}"), &[], &[UTC]).await
    }

    /// The occurrences of series `series` between `start` and `end`, each
    /// with the start it has in the series. Graph was seen to answer
    /// `originalStart` here when `$select` names it.
    pub async fn instances(
        &self,
        series: &str,
        start: &str,
        end: &str,
    ) -> Result<Vec<GraphEvent>, GraphError> {
        let page: Page<GraphEvent> = self
            .get_with(
                &format!("me/events/{series}/instances"),
                &[
                    ("startDateTime", start),
                    ("endDateTime", end),
                    ("$select", "id,type,seriesMasterId,start,end,isAllDay,originalStart,originalStartTimeZone"),
                    ("$top", "100"),
                ],
                &[UTC],
            )
            .await?;
        Ok(page.value)
    }

    /// The `originalStart` of each event in `ids`, which the calendar-view
    /// delta leaves out of an exception. One `$batch` entry an event,
    /// twenty to a request; each answer is in the place of its id.
    pub async fn original_starts(
        &self,
        ids: &[String],
    ) -> Result<Vec<Result<GraphEvent, GraphError>>, GraphError> {
        let requests: Vec<BatchRequest> = ids
            .iter()
            .map(|id| {
                BatchRequest::get(with_query(
                    &format!("me/events/{id}"),
                    &[("$select", "id,originalStart,originalStartTimeZone")],
                ))
            })
            .collect();
        Ok(self
            .batch(&requests)
            .await?
            .into_iter()
            .map(|answer| {
                answer
                    .into_json::<GraphEvent>()
                    .and_then(|e| e.ok_or(GraphError::NotFound))
            })
            .collect())
    }

    /// Each event of `ids`, whole and in UTC, in `$batch` calls of 20, one
    /// answer per id in order; an event Graph no longer has answers
    /// `NotFound` in its place.
    pub async fn events(
        &self,
        ids: &[String],
    ) -> Result<Vec<Result<GraphEvent, GraphError>>, GraphError> {
        let requests: Vec<BatchRequest> = ids
            .iter()
            .map(|id| {
                BatchRequest::get(format!("me/events/{id}"))
                    .header("Prefer", &format!("{IMMUTABLE_IDS}, {UTC}"))
            })
            .collect();
        Ok(self
            .batch(&requests)
            .await?
            .into_iter()
            .map(|answer| {
                answer
                    .into_json::<GraphEvent>()
                    .and_then(|e| e.ok_or(GraphError::NotFound))
            })
            .collect())
    }

    /// The events on `calendar` with iCalendar UID `uid`: single events
    /// and series masters, since a list of events names no occurrence.
    pub async fn events_by_uid(
        &self,
        calendar: &str,
        uid: &str,
    ) -> Result<Vec<GraphEvent>, GraphError> {
        let filter = format!("iCalUId eq '{}'", uid.replace('\'', "''"));
        let page: Page<GraphEvent> = self
            .get_with(
                &format!("me/calendars/{calendar}/events"),
                &[("$filter", &filter)],
                &[UTC],
            )
            .await?;
        Ok(page.value)
    }

    pub async fn create_event(
        &self,
        calendar: &str,
        body: &Value,
    ) -> Result<GraphEvent, GraphError> {
        self.send(
            Method::Post,
            &format!("me/calendars/{calendar}/events"),
            Some(body),
            &[("Prefer", UTC)],
        )
        .await?
        .ok_or_else(|| GraphError::Decode("no event in the answer".into()))
    }

    pub async fn update_event(
        &self,
        id: &str,
        body: &Value,
        etag: Option<&str>,
    ) -> Result<GraphEvent, GraphError> {
        let mut headers = vec![("Prefer", UTC)];
        if let Some(etag) = etag {
            headers.push(("If-Match", etag));
        }
        self.send(
            Method::Patch,
            &format!("me/events/{id}"),
            Some(body),
            &headers,
        )
        .await?
        .ok_or_else(|| GraphError::Decode("no event in the answer".into()))
    }

    pub async fn delete_event(&self, id: &str, etag: Option<&str>) -> Result<(), GraphError> {
        let headers: Vec<(&str, &str)> = etag.map(|e| ("If-Match", e)).into_iter().collect();
        self.send::<Value>(Method::Delete, &format!("me/events/{id}"), None, &headers)
            .await
            .map(|_| ())
    }

    /// Answers an invitation and lets Graph tell the organizer, with the
    /// person's note when they wrote one.
    pub async fn respond(
        &self,
        id: &str,
        response: Response,
        comment: Option<&str>,
    ) -> Result<(), GraphError> {
        let verb = match response {
            Response::Accept => "accept",
            Response::Tentative => "tentativelyAccept",
            Response::Decline => "decline",
        };
        let mut body = json!({ "sendResponse": true });
        if let Some(comment) = comment.filter(|c| !c.is_empty()) {
            body["comment"] = comment.into();
        }
        self.send::<Value>(
            Method::Post,
            &format!("me/events/{id}/{verb}"),
            Some(&body),
            &[],
        )
        .await
        .map(|_| ())
    }

    /// One page of one calendar's view between `start` and `end` (RFC
    /// 3339). `link` is the next link the last page gave.
    pub async fn calendar_view_of(
        &self,
        calendar: &str,
        start: &str,
        end: &str,
        link: Option<&str>,
    ) -> Result<Page<GraphEvent>, GraphError> {
        if let Some(link) = link {
            return self.follow(link, &[UTC]).await;
        }
        self.get_with(
            &format!("me/calendars/{calendar}/calendarView"),
            &[
                ("startDateTime", start),
                ("endDateTime", end),
                ("$top", "100"),
            ],
            &[UTC],
        )
        .await
    }

    /// A calendar named `name`, in the Outlook color nearest `hex`.
    pub async fn create_calendar(
        &self,
        name: &str,
        hex: &str,
    ) -> Result<GraphCalendar, GraphError> {
        let body = json!({ "name": name, "color": nearest_color(hex) });
        self.send(Method::Post, "me/calendars", Some(&body), &[])
            .await?
            .ok_or_else(|| GraphError::Decode("no calendar in the answer".into()))
    }

    /// Changes a calendar's `name` or `color`. A `color` given as `#rrggbb`
    /// becomes the nearest named color, since Graph takes names only.
    pub async fn update_calendar(
        &self,
        id: &str,
        body: &Value,
    ) -> Result<GraphCalendar, GraphError> {
        let mut body = body.clone();
        if let Some(color) = body
            .get("color")
            .and_then(Value::as_str)
            .filter(|c| c.starts_with('#'))
        {
            body["color"] = nearest_color(color).into();
        }
        self.send(
            Method::Patch,
            &format!("me/calendars/{id}"),
            Some(&body),
            &[],
        )
        .await?
        .ok_or_else(|| GraphError::Decode("no calendar in the answer".into()))
    }

    pub async fn delete_calendar(&self, id: &str) -> Result<(), GraphError> {
        self.send::<Value>(Method::Delete, &format!("me/calendars/{id}"), None, &[])
            .await
            .map(|_| ())
    }
}

/// Outlook's named calendar colors and the hex each shows as. A name
/// Graph sends that is not here (`auto`) reads as the first.
pub const CALENDAR_COLORS: [(&str, &str); 9] = [
    ("lightBlue", "#a6d1f5"),
    ("lightGreen", "#87d28e"),
    ("lightOrange", "#fcab73"),
    ("lightGray", "#c0c0c0"),
    ("lightYellow", "#f4de5b"),
    ("lightTeal", "#87e1d7"),
    ("lightPink", "#ee9fc4"),
    ("lightBrown", "#d3b492"),
    ("lightRed", "#f19696"),
];

fn rgb(hex: &str) -> Option<[i32; 3]> {
    let digits = hex.strip_prefix('#')?;
    if digits.len() != 6 || !digits.is_ascii() {
        return None;
    }
    let part = |at: usize| i32::from_str_radix(&digits[at..at + 2], 16).ok();
    Some([part(0)?, part(2)?, part(4)?])
}

/// The Outlook color name nearest `hex` (`#rrggbb`) by distance in RGB,
/// `lightBlue` for text that is not a color.
pub fn nearest_color(hex: &str) -> &'static str {
    let Some(want) = rgb(hex) else {
        return CALENDAR_COLORS[0].0;
    };
    CALENDAR_COLORS
        .iter()
        .filter_map(|(name, value)| {
            let have = rgb(value)?;
            let distance: i32 = want.iter().zip(have).map(|(a, b)| (a - b) * (a - b)).sum();
            Some((distance, *name))
        })
        .min_by_key(|(distance, _)| *distance)
        .map_or(CALENDAR_COLORS[0].0, |(_, name)| name)
}
