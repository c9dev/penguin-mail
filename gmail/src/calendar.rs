//! Answering an invitation through Google Calendar, and asking it what
//! else the user has on.
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

#[derive(Deserialize)]
struct EventList {
    #[serde(default)]
    items: Vec<Value>,
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
    pub async fn answer_invitation(
        &self,
        ical_uid: &str,
        me: &str,
        answer: Answer,
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
        let Some(id) = event.get("id").and_then(Value::as_str) else {
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
