//! JMAP Calendars (draft-ietf-jmap-calendars-32): the calendars, the
//! account's event changes, a page of events by query, and writes. Events
//! travel as JSCalendar JSON; `mailrs-sync` maps them onto the app's
//! event model.
//!
//! Draft 24, which Stalwart follows, defines the same account capability
//! as draft 32, so the shape checked here is the one they share. Draft 22
//! changed `minDateTime` and `maxDateTime` from LocalDateTime to
//! UTCDateTime; a server in an older shape counts as offering no
//! calendars, and its account keeps CalDAV.

mod scope;

use scope::Scope;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::mail::{ChangesResponse, Got, SetResponse};
use crate::session::CALENDARS;
use crate::{JmapClient, JmapError, Request, Session};

/// Named in `using`, it asks Cyrus for JSCalendar 2.0's forms, the ones
/// draft 32 cites.
pub const CYRUS_JSCALENDAR_BIS: &str = "https://cyrusimap.org/ns/jmap/jscalendarbis";

/// Whether an account's calendars capability has the members draft 32
/// defines, with UTC times.
pub fn fits(capability: &Value) -> bool {
    let utc = |key: &str| {
        capability
            .get(key)
            .and_then(Value::as_str)
            .is_some_and(|t| t.ends_with('Z'))
    };
    capability.get("maxCalendarsPerEvent").is_some()
        && capability.get("maxParticipantsPerEvent").is_some()
        && utc("minDateTime")
        && utc("maxDateTime")
        && capability
            .get("maxExpandedQueryDuration")
            .and_then(Value::as_str)
            .is_some()
        && capability
            .get("mayCreateCalendar")
            .and_then(Value::as_bool)
            .is_some()
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CalendarRights {
    pub may_read_items: bool,
    pub may_write_all: bool,
    pub may_write_own: bool,
    pub may_rsvp: bool,
    pub may_admin: bool,
    pub may_delete: bool,
}

fn visible() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JmapCalendar {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub is_default: bool,
    #[serde(default = "visible")]
    pub is_visible: bool,
    #[serde(default)]
    pub time_zone: Option<String>,
    #[serde(default)]
    pub my_rights: CalendarRights,
    /// The alerts of a timed event that uses the calendar's defaults, by id.
    #[serde(default)]
    pub default_alerts_with_time: Option<Map<String, Value>>,
    /// The same for an all-day event.
    #[serde(default)]
    pub default_alerts_without_time: Option<Map<String, Value>>,
}

/// Events as `CalendarEvent/get` answers them, whole JSCalendar objects.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CalendarEvents {
    pub state: String,
    pub list: Vec<Value>,
    pub not_found: Vec<String>,
}

/// One page of a query, with the CalendarEvent state the page was read at.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EventsPage {
    pub state: String,
    pub events: Vec<Value>,
    pub total: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EventWrite {
    Create(Map<String, Value>),
    /// A patch: each key a path into the event, each value what goes
    /// there, `null` taking it out.
    Update {
        id: String,
        patch: Map<String, Value>,
    },
    Destroy(String),
}

#[derive(Deserialize)]
struct Queried {
    #[serde(default)]
    total: Option<u64>,
}

fn calendar_request(session: &Session) -> Request {
    let mut request = Request::new();
    if session.capabilities.contains_key(CYRUS_JSCALENDAR_BIS) {
        request.also_using(CYRUS_JSCALENDAR_BIS);
    }
    request
}

impl JmapClient {
    pub async fn calendars(&self) -> Result<Vec<JmapCalendar>, JmapError> {
        let session = self.session().await?;
        let primary = session
            .account_for(CALENDARS)
            .ok_or_else(|| JmapError::UnknownCapability(CALENDARS.into()))?;
        let accounts = std::iter::once(primary)
            .chain(
                session
                    .accounts
                    .keys()
                    .map(String::as_str)
                    .filter(|id| *id != primary),
            )
            .filter(|id| {
                session.accounts[*id]
                    .account_capabilities
                    .get(CALENDARS)
                    .is_some_and(fits)
            });
        let mut calendars = Vec::new();
        for account in accounts {
            let scope = Scope::for_account(&session, account)?;
            let mut request = calendar_request(&session);
            let got = request.call(
                CALENDARS,
                "Calendar/get",
                json!({"accountId": account, "ids": null}),
            );
            let got: Got<JmapCalendar> = self.send(&request).await?.take(&got, "Calendar/get")?;
            calendars.extend(got.list.into_iter().map(|mut calendar| {
                calendar.id = scope.id(&calendar.id);
                if account != primary {
                    calendar.is_default = false;
                }
                if session.accounts[account].is_read_only {
                    calendar.my_rights.may_write_all = false;
                    calendar.my_rights.may_write_own = false;
                    calendar.my_rights.may_admin = false;
                    calendar.my_rights.may_delete = false;
                    calendar.my_rights.may_rsvp = false;
                }
                calendar
            }));
        }
        Ok(calendars)
    }

    /// The account's event changes since `since`, every calendar at once.
    pub async fn calendar_event_changes(
        &self,
        since: &str,
        max: usize,
    ) -> Result<ChangesResponse, JmapError> {
        let session = self.session().await?;
        let scope = Scope::of(&session, since)?;
        let account = &scope.account;
        let step = session.limits().max_objects_in_get.clamp(1, 500);
        let since = scope.raw(since)?;
        let mut request = calendar_request(&session);
        let changes = request.call(
            CALENDARS,
            "CalendarEvent/changes",
            json!({
                "accountId": account, "sinceState": since, "maxChanges": max.clamp(1, step)
            }),
        );
        let mut changes: ChangesResponse = self
            .send(&request)
            .await?
            .take(&changes, "CalendarEvent/changes")?;
        changes.old_state = scope.id(&changes.old_state);
        changes.new_state = scope.id(&changes.new_state);
        for ids in [
            &mut changes.created,
            &mut changes.updated,
            &mut changes.destroyed,
        ] {
            for id in ids {
                *id = scope.id(id);
            }
        }
        Ok(changes)
    }

    /// `ids`, a request per `maxObjectsInGet`.
    pub async fn calendar_events(&self, ids: &[String]) -> Result<CalendarEvents, JmapError> {
        let session = self.session().await?;
        let scope = Scope::of(
            &session,
            ids.first().map(String::as_str).unwrap_or_default(),
        )?;
        let account = &scope.account;
        let step = session.limits().max_objects_in_get.clamp(1, 500);
        let ids = ids
            .iter()
            .map(|id| scope.raw(id))
            .collect::<Result<Vec<_>, _>>()?;
        let mut all = CalendarEvents::default();
        for chunk in ids.chunks(step) {
            let mut request = calendar_request(&session);
            let got = request.call(
                CALENDARS,
                "CalendarEvent/get",
                json!({ "accountId": account, "ids": chunk }),
            );
            let got: Got<Value> = self.send(&request).await?.take(&got, "CalendarEvent/get")?;
            all.state = scope.id(&got.state);
            all.list
                .extend(got.list.into_iter().map(|e| scope.event(e)));
            all.not_found
                .extend(got.not_found.iter().map(|id| scope.id(id)));
        }
        Ok(all)
    }

    /// The events `filter` matches from `position`, at most `limit`,
    /// soonest first, fetched in the same request.
    pub async fn calendar_events_page(
        &self,
        filter: &Value,
        position: u64,
        limit: usize,
    ) -> Result<EventsPage, JmapError> {
        let session = self.session().await?;
        let scope = Scope::of(&session, scope::calendar(filter).unwrap_or_default())?;
        let account = &scope.account;
        let step = session.limits().max_objects_in_get.clamp(1, 500);
        let filter = scope.filter(filter)?;
        let mut request = calendar_request(&session);
        let query = request.call(
            CALENDARS,
            "CalendarEvent/query",
            json!({
                "accountId": account,
                "filter": filter,
                "sort": [{ "property": "start", "isAscending": true }],
                "position": position,
                "limit": limit.clamp(1, step),
                "calculateTotal": true
            }),
        );
        let got = request.call(
            CALENDARS,
            "CalendarEvent/get",
            json!({
                "accountId": account, "#ids": query.result_of("CalendarEvent/query", "/ids")
            }),
        );
        let mut answer = self.send(&request).await?;
        let queried: Queried = answer.take(&query, "CalendarEvent/query")?;
        let events: Got<Value> = answer.take(&got, "CalendarEvent/get")?;
        Ok(EventsPage {
            state: scope.id(&events.state),
            events: events
                .list
                .into_iter()
                .map(|event| scope.event(event))
                .collect(),
            total: queried.total,
        })
    }

    /// Writes one event. `send` asks the server to mail the guests the
    /// iTIP the change calls for. With `state`, the server refuses the
    /// write with `stateMismatch` once any event changed since that state.
    pub async fn set_calendar_event(
        &self,
        write: &EventWrite,
        send: bool,
        state: Option<&str>,
    ) -> Result<Option<Value>, JmapError> {
        let session = self.session().await?;
        let hint = match write {
            EventWrite::Create(fields) => fields
                .get("calendarIds")
                .and_then(Value::as_object)
                .and_then(|ids| ids.keys().next())
                .map(String::as_str)
                .unwrap_or_default(),
            EventWrite::Update { id, .. } | EventWrite::Destroy(id) => id.as_str(),
        };
        let scope = Scope::of(&session, hint)?;
        let account = &scope.account;
        let write = match write {
            EventWrite::Create(fields) => EventWrite::Create(scope.fields(fields)?),
            EventWrite::Update { id, patch } => EventWrite::Update {
                id: scope.raw(id)?,
                patch: scope.fields(patch)?,
            },
            EventWrite::Destroy(id) => EventWrite::Destroy(scope.raw(id)?),
        };
        let write = &write;
        let mut request = calendar_request(&session);
        let mut args = json!({ "accountId": account, "sendSchedulingMessages": send });
        if let Some(state) = state {
            args["ifInState"] = json!(scope.raw(state)?);
        }
        let key = match write {
            EventWrite::Create(object) => {
                args["create"] = json!({ "e": object });
                "e"
            }
            EventWrite::Update { id, patch } => {
                args["update"] = json!({ id.clone(): patch });
                id
            }
            EventWrite::Destroy(id) => {
                args["destroy"] = json!([id]);
                id
            }
        };
        let set = request.call(CALENDARS, "CalendarEvent/set", args);
        let mut said: SetResponse = self.send(&request).await?.take(&set, "CalendarEvent/set")?;
        if let Some(refused) = said
            .not_created
            .as_ref()
            .and_then(|n| n.get(key))
            .or_else(|| said.refusal(key))
        {
            return Err(refused.error());
        }
        match write {
            EventWrite::Create(_) => said
                .created
                .and_then(|mut created| created.remove(key))
                .map(|event| Some(scope.event(event)))
                .ok_or_else(|| JmapError::Decode("the server created no event".into())),
            EventWrite::Update { .. } => Ok(said
                .updated
                .as_mut()
                .and_then(|u| u.remove(key))
                .flatten()
                .map(|event| scope.event(event))),
            EventWrite::Destroy(_) => Ok(None),
        }
    }
}
