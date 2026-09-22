//! Answering an invitation through Google Calendar, asking it what else
//! the user has on, and the events on the primary calendar that the
//! assistant lists, creates, changes and deletes.
//!
//! The Gmail permission an account grants at sign-in says nothing about
//! calendars, so Google turns every call here down until the account
//! grants [`CALENDAR_SCOPE`] as well. The refusal arrives as
//! [`GmailError::MissingScope`], the same one erasing mail gives, and the
//! window asks the user for the permission the first time somebody presses
//! Yes, No or Maybe. Anyone who says no to the permission still has the
//! links Google puts in the message itself.
//!
//! The calls run against the Calendar API, not Gmail, so they spend
//! nothing from the account's Gmail budget.

use mailrs_domain::invitation::Answer;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::client::GmailClient;
use crate::error::GmailError;

/// Read and change the events on the account's calendars. Sign-in leaves
/// it out; the window asks for it when somebody answers an invitation.
pub const CALENDAR_SCOPE: &str = "https://www.googleapis.com/auth/calendar.events";

pub const CALENDAR_API_BASE: &str = "https://www.googleapis.com/calendar/v3";

/// What answering an invitation did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answered {
    /// Google Calendar recorded the answer and told the organizer.
    Done,
    /// The account's calendar holds no event with that UID, so there is
    /// nothing to answer. Mail that Google never put on the calendar,
    /// such as an invitation forwarded from somebody else, lands here.
    NotOnCalendar,
}

/// Something the user already has on while an invitation's event would
/// run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Busy {
    /// The event's iCalendar UID, so the caller can tell the invitation's
    /// own event from a clash with something else.
    pub uid: String,
    pub summary: String,
}

/// How a repeating event on the calendar repeats.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Series {
    /// The series' rule without its `RRULE:` prefix, such as
    /// `FREQ=WEEKLY;BYDAY=MO;COUNT=10`.
    pub rule: String,
    /// How many occurrences are still to come, counted from the instant
    /// the caller named. Only a rule that stops after a number of
    /// occurrences has one; a rule with an end date or none counts
    /// nothing.
    pub left: Option<u32>,
}

#[derive(Deserialize)]
struct EventList {
    #[serde(default)]
    items: Vec<Value>,
    #[serde(default, rename = "nextPageToken")]
    next_page_token: Option<String>,
}

/// Most events one listing reads, over however many pages that takes.
/// A window with more than this is too wide to be worth reading whole.
const MOST_EVENTS: usize = 500;

/// When an event starts or ends, in the two forms Google writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventTime {
    /// An instant, as an RFC 3339 timestamp.
    At(String),
    /// A whole day, as `YYYY-MM-DD`. An all-day event ends on the day
    /// after its last one, as iCalendar has it.
    Day(String),
}

impl EventTime {
    fn json(&self) -> Value {
        match self {
            EventTime::At(at) => json!({ "dateTime": at }),
            EventTime::Day(day) => json!({ "date": day }),
        }
    }

    fn read(value: Option<&Value>) -> Option<EventTime> {
        let value = value?;
        if let Some(at) = value.get("dateTime").and_then(Value::as_str) {
            return Some(EventTime::At(at.to_string()));
        }
        value
            .get("date")
            .and_then(Value::as_str)
            .map(|day| EventTime::Day(day.to_string()))
    }
}

/// One guest on an event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Guest {
    pub email: String,
    pub name: Option<String>,
    /// Google's word for the guest's answer: `needsAction`, `accepted`,
    /// `tentative` or `declined`.
    pub answer: String,
    /// This guest is the account itself.
    pub me: bool,
}

/// One event on the account's primary calendar.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Event {
    /// Google's id, which the calls that change an event take.
    pub id: String,
    /// The iCalendar UID an invitation for this event carries.
    pub uid: String,
    pub summary: String,
    pub start: Option<EventTime>,
    pub end: Option<EventTime>,
    pub location: String,
    pub description: String,
    pub organizer: Option<String>,
    pub guests: Vec<Guest>,
    pub cancelled: bool,
    /// Whether the event takes the user's time, by the same test a clash
    /// uses: not declined, not marked free, not cancelled, not all day.
    pub busy: bool,
    /// The event's page in Google Calendar.
    pub link: Option<String>,
}

/// What to write on an event. A field left `None` stays as it is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventFields {
    pub summary: Option<String>,
    pub start: Option<EventTime>,
    pub end: Option<EventTime>,
    pub location: Option<String>,
    pub description: Option<String>,
    /// The guests' addresses. This replaces the guest list, and a guest
    /// who stays on it keeps the answer they already gave.
    pub guests: Option<Vec<String>>,
}

impl EventFields {
    /// The fields as the Calendar API takes them. `held` is the guest list
    /// the event has now, so a guest who stays keeps their answer.
    fn json(&self, held: &[Value]) -> Value {
        let mut body = serde_json::Map::new();
        let mut put = |key: &str, value: Value| {
            body.insert(key.to_string(), value);
        };
        if let Some(summary) = &self.summary {
            put("summary", json!(summary));
        }
        if let Some(start) = &self.start {
            put("start", start.json());
        }
        if let Some(end) = &self.end {
            put("end", end.json());
        }
        if let Some(location) = &self.location {
            put("location", json!(location));
        }
        if let Some(description) = &self.description {
            put("description", json!(description));
        }
        if let Some(guests) = &self.guests {
            let list: Vec<Value> = guests
                .iter()
                .map(|email| {
                    held.iter()
                        .find(|guest| {
                            guest
                                .get("email")
                                .and_then(Value::as_str)
                                .is_some_and(|held| held.eq_ignore_ascii_case(email))
                        })
                        .cloned()
                        .unwrap_or_else(|| json!({ "email": email }))
                })
                .collect();
            put("attendees", Value::Array(list));
        }
        Value::Object(body)
    }
}

impl GmailClient {
    /// Points the calendar calls at another server. Tests use this.
    pub fn with_calendar_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.calendar_base_url = base_url.into();
        self
    }

    /// Answers the event `ical_uid` names as `me`, and lets Google tell the
    /// organizer. Two calls: one to find the event Google made from the
    /// invitation, one to change this account's answer on it.
    ///
    /// `occurrence` is the start of the one occurrence to answer, as an
    /// RFC 3339 timestamp, for an invitation to a single occurrence of a
    /// repeating event. Without it the answer covers the series, which is
    /// what a single event and "all events" both want. Naming an
    /// occurrence costs a third call, since Google numbers the occurrences
    /// of a series itself and the id it gives one is not the UID.
    pub async fn answer_invitation(
        &self,
        ical_uid: &str,
        me: &str,
        answer: Answer,
        occurrence: Option<&str>,
    ) -> Result<Answered, GmailError> {
        let list: EventList = self
            .call_at(
                &format!("{}/calendars/primary/events", self.calendar_base_url),
                |url| {
                    self.http().get(url).query(&[
                        ("iCalUID", ical_uid),
                        ("maxResults", "1"),
                        ("showDeleted", "true"),
                    ])
                },
            )
            .await?;
        let Some(event) = list.items.into_iter().next() else {
            return Ok(Answered::NotOnCalendar);
        };
        let Some(id) = self.event_to_answer(&event, occurrence).await? else {
            return Ok(Answered::NotOnCalendar);
        };
        let guests = answered(&event, me, answer);
        let url = format!("{}/calendars/primary/events/{id}", self.calendar_base_url);
        let _: Value = self
            .call_at(&url, |url| {
                self.http()
                    .patch(url)
                    .query(&[("sendUpdates", "all")])
                    .json(&json!({ "attendees": guests }))
            })
            .await?;
        Ok(Answered::Done)
    }

    /// Which event the answer goes on: the series, or the one occurrence
    /// `occurrence` names. Google keeps a repeating event as one event
    /// with a rule, and hands out an id for an occurrence only when asked
    /// for the instances, so an answer to one occurrence looks that id up
    /// first. An answer to the series goes on the series even when the
    /// search turned up an occurrence of it.
    async fn event_to_answer(
        &self,
        event: &Value,
        occurrence: Option<&str>,
    ) -> Result<Option<String>, GmailError> {
        let text = |key: &str| event.get(key).and_then(Value::as_str);
        let Some(occurrence) = occurrence else {
            return Ok(text("recurringEventId")
                .or_else(|| text("id"))
                .map(str::to_string));
        };
        let Some(id) = text("id") else {
            return Ok(None);
        };
        if text("recurringEventId").is_some() {
            return Ok(Some(id.to_string()));
        }
        if event.get("recurrence").is_none() {
            return Ok(Some(id.to_string()));
        }
        let instances: EventList = self
            .call_at(
                &format!(
                    "{}/calendars/primary/events/{id}/instances",
                    self.calendar_base_url
                ),
                |url| {
                    self.http()
                        .get(url)
                        .query(&[("originalStart", occurrence), ("maxResults", "1")])
                },
            )
            .await?;
        Ok(instances
            .items
            .first()
            .and_then(|instance| instance.get("id"))
            .and_then(Value::as_str)
            .map(str::to_string))
    }

    /// How the repeating event `ical_uid` names repeats, as the calendar
    /// holds it. An invitation to one occurrence carries no rule of its
    /// own, so this is the only place the rest of the series shows up.
    /// `None` means the calendar has no such event or it does not repeat.
    ///
    /// One call finds the event, and a second reads the series when the
    /// search turned up an occurrence of it. A rule that stops after a
    /// number of occurrences costs one more: Google counts the ones still
    /// to come from `from`, an RFC 3339 timestamp, which saves this crate
    /// from expanding the rule itself.
    pub async fn series(&self, ical_uid: &str, from: &str) -> Result<Option<Series>, GmailError> {
        let events = format!("{}/calendars/primary/events", self.calendar_base_url);
        let list: EventList = self
            .call_at(&events, |url| {
                self.http()
                    .get(url)
                    .query(&[("iCalUID", ical_uid), ("maxResults", "1")])
            })
            .await?;
        let Some(mut event) = list.items.into_iter().next() else {
            return Ok(None);
        };
        if let Some(parent) = event.get("recurringEventId").and_then(Value::as_str) {
            let url = format!("{events}/{parent}");
            event = self.call_at(&url, |url| self.http().get(url)).await?;
        }
        let Some(rule) = rule_of(&event) else {
            return Ok(None);
        };
        let counted = rule
            .split(';')
            .any(|part| part.trim().to_ascii_uppercase().starts_with("COUNT="));
        let id = event.get("id").and_then(Value::as_str);
        let left = match (counted, id) {
            (true, Some(id)) => Some(self.instances_from(&format!("{events}/{id}"), from).await?),
            _ => None,
        };
        Ok(Some(Series { rule, left }))
    }

    /// How many occurrences of the series at `url` start at or after
    /// `from`, over as many pages as that takes.
    async fn instances_from(&self, url: &str, from: &str) -> Result<u32, GmailError> {
        let url = format!("{url}/instances");
        let mut count = 0;
        let mut page: Option<String> = None;
        loop {
            let list: EventList = self
                .call_at(&url, |url| {
                    let mut query = vec![("timeMin", from), ("maxResults", "250")];
                    if let Some(token) = &page {
                        query.push(("pageToken", token));
                    }
                    self.http().get(url).query(&query)
                })
                .await?;
            count += list.items.len();
            match list.next_page_token {
                Some(token) if count < MOST_EVENTS => page = Some(token),
                _ => break,
            }
        }
        Ok(count as u32)
    }

    /// What the account's primary calendar holds between `from` and `to`,
    /// both RFC 3339 timestamps. One call, and only the events that would
    /// keep the user from another meeting: an event they declined, one
    /// they marked free, a cancelled one and an all-day one all leave the
    /// hours they cover open.
    pub async fn busy_between(&self, from: &str, to: &str) -> Result<Vec<Busy>, GmailError> {
        let list: EventList = self
            .call_at(
                &format!("{}/calendars/primary/events", self.calendar_base_url),
                |url| {
                    self.http().get(url).query(&[
                        ("timeMin", from),
                        ("timeMax", to),
                        ("singleEvents", "true"),
                        ("orderBy", "startTime"),
                        ("maxResults", "10"),
                    ])
                },
            )
            .await?;
        Ok(list
            .items
            .iter()
            .filter(|event| busy(event))
            .map(busy_of)
            .collect())
    }

    /// Every event on the primary calendar that overlaps `from` to `to`,
    /// both RFC 3339 timestamps, in the order they start. A repeating
    /// event comes back as one event per occurrence.
    pub async fn events_between(&self, from: &str, to: &str) -> Result<Vec<Event>, GmailError> {
        let url = format!("{}/calendars/primary/events", self.calendar_base_url);
        let mut events = Vec::new();
        let mut page: Option<String> = None;
        loop {
            let list: EventList = self
                .call_at(&url, |url| {
                    let mut query = vec![
                        ("timeMin", from),
                        ("timeMax", to),
                        ("singleEvents", "true"),
                        ("orderBy", "startTime"),
                        ("maxResults", "250"),
                    ];
                    if let Some(token) = &page {
                        query.push(("pageToken", token));
                    }
                    self.http().get(url).query(&query)
                })
                .await?;
            events.extend(list.items.iter().map(event_of));
            match list.next_page_token {
                Some(token) if events.len() < MOST_EVENTS => page = Some(token),
                _ => break,
            }
        }
        events.truncate(MOST_EVENTS);
        Ok(events)
    }

    /// Puts a new event on the primary calendar and invites its guests.
    pub async fn create_event(&self, fields: &EventFields) -> Result<Event, GmailError> {
        let url = format!("{}/calendars/primary/events", self.calendar_base_url);
        let event: Value = self
            .call_at(&url, |url| {
                self.http()
                    .post(url)
                    .query(&[("sendUpdates", "all")])
                    .json(&fields.json(&[]))
            })
            .await?;
        Ok(event_of(&event))
    }

    /// Changes the fields `fields` sets on event `id` and tells its
    /// guests. A new guest list costs one more call first, to read the
    /// answers the guests who stay have already given.
    pub async fn update_event(&self, id: &str, fields: &EventFields) -> Result<Event, GmailError> {
        let url = format!("{}/calendars/primary/events/{id}", self.calendar_base_url);
        let held: Vec<Value> = match fields.guests {
            Some(_) => {
                let event: Value = self.call_at(&url, |url| self.http().get(url)).await?;
                event
                    .get("attendees")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
            }
            None => Vec::new(),
        };
        let event: Value = self
            .call_at(&url, |url| {
                self.http()
                    .patch(url)
                    .query(&[("sendUpdates", "all")])
                    .json(&fields.json(&held))
            })
            .await?;
        Ok(event_of(&event))
    }

    /// Takes event `id` off the primary calendar and tells its guests.
    pub async fn delete_event(&self, id: &str) -> Result<(), GmailError> {
        let url = format!("{}/calendars/primary/events/{id}", self.calendar_base_url);
        self.call_at_empty(&url, |url| {
            self.http().delete(url).query(&[("sendUpdates", "all")])
        })
        .await
    }
}

/// The `RRULE` in an event's `recurrence` list, without its prefix. The
/// list also holds `EXDATE` and `RDATE` lines, which say nothing about how
/// the series runs on.
fn rule_of(event: &Value) -> Option<String> {
    event
        .get("recurrence")?
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .find_map(|line| {
            let (name, rule) = line.split_once(':')?;
            name.eq_ignore_ascii_case("RRULE")
                .then(|| rule.trim().to_string())
        })
}

/// The event as the tools read it, from Google's JSON.
fn event_of(event: &Value) -> Event {
    let text = |key: &str| {
        event
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let guests = event
        .get("attendees")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|guest| {
            let email = guest.get("email").and_then(Value::as_str)?;
            Some(Guest {
                email: email.to_string(),
                name: guest
                    .get("displayName")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                answer: guest
                    .get("responseStatus")
                    .and_then(Value::as_str)
                    .unwrap_or("needsAction")
                    .to_string(),
                me: guest.get("self").and_then(Value::as_bool) == Some(true),
            })
        })
        .collect();
    Event {
        id: text("id"),
        uid: text("iCalUID"),
        summary: text("summary"),
        start: EventTime::read(event.get("start")),
        end: EventTime::read(event.get("end")),
        location: text("location"),
        description: text("description"),
        organizer: event
            .pointer("/organizer/email")
            .and_then(Value::as_str)
            .map(str::to_string),
        guests,
        cancelled: text("status") == "cancelled",
        busy: busy(event),
        link: event
            .get("htmlLink")
            .and_then(Value::as_str)
            .map(str::to_string),
    }
}

/// Whether an event on the calendar takes the user's time. Google answers
/// with everything in the window, cancellations and all.
fn busy(event: &Value) -> bool {
    let is = |key: &str, value: &str| {
        event
            .get(key)
            .and_then(Value::as_str)
            .is_some_and(|held| held.eq_ignore_ascii_case(value))
    };
    if is("status", "cancelled") || is("transparency", "transparent") {
        return false;
    }
    // An all-day event marks the day rather than the hours in it.
    if event
        .get("start")
        .and_then(|start| start.get("date"))
        .is_some()
    {
        return false;
    }
    !event
        .get("attendees")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|guest| {
            guest.get("self").and_then(Value::as_bool) == Some(true)
                && guest.get("responseStatus").and_then(Value::as_str) == Some("declined")
        })
}

fn busy_of(event: &Value) -> Busy {
    let text = |key: &str| {
        event
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    Busy {
        uid: text("iCalUID"),
        summary: match text("summary") {
            summary if summary.trim().is_empty() => "an untitled event".to_string(),
            summary => summary,
        },
    }
}

/// The event's guest list with this account's answer changed and every
/// other guest left as Google has them. A patch replaces the whole list,
/// so sending back less would drop the others.
fn answered(event: &Value, me: &str, answer: Answer) -> Vec<Value> {
    let mut guests: Vec<Value> = event
        .get("attendees")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut found = false;
    for guest in &mut guests {
        if !is_me(guest, me) {
            continue;
        }
        found = true;
        if let Some(fields) = guest.as_object_mut() {
            fields.insert("responseStatus".into(), json!(answer.response_status()));
        }
    }
    if !found {
        guests.push(json!({ "email": me, "responseStatus": answer.response_status() }));
    }
    guests
}

/// Whether a guest entry is this account. Google marks it with `self`, and
/// an address match covers the entries it does not mark.
fn is_me(guest: &Value, me: &str) -> bool {
    if guest.get("self").and_then(Value::as_bool) == Some(true) {
        return true;
    }
    guest
        .get("email")
        .and_then(Value::as_str)
        .is_some_and(|email| email.eq_ignore_ascii_case(me))
}
