//! Answering an invitation through Google Calendar, asking it what else
//! the user has on, and the events on the primary calendar that the
//! assistant lists, creates, changes and deletes.
//!
//! Sign-in asks for [`CALENDAR_SCOPE`] along with every other scope in
//! one consent. A person who leaves it unticked makes Google turn every
//! call here down; the refusal arrives as [`GmailError::MissingScope`],
//! the same one erasing mail gives, and the window offers to ask again
//! the first time somebody presses Yes, No or Maybe. Anyone who says no
//! to the permission still has the links Google puts in the message
//! itself.
//!
//! The calls run against the Calendar API, not Gmail, so they spend
//! nothing from the account's Gmail budget.

use mailrs_domain::EpochMillis;
use mailrs_domain::calendar::{self, Access, EventPage, Guest as CalendarGuest, Notify, Reminder, ReminderMethod, Status};
use mailrs_domain::invitation::Answer;
use serde_json::{Value, json};

use crate::client::GmailClient;
use crate::error::GmailError;

/// Read and change the events on the account's calendars. Sign-in asks
/// for it along with every other scope, in one consent.
pub const CALENDAR_SCOPE: &str = "https://www.googleapis.com/auth/calendar.events";

/// List the calendars on the account, so the calendar view can show
/// shared and subscribed ones next to the primary. Sign-in no longer
/// asks for it on its own: [`CALENDAR_LIST_WRITE_SCOPE`] covers it, and
/// [`crate::Granted::has`] still checks it for an account that granted
/// only this one.
pub const CALENDAR_LIST_SCOPE: &str =
    "https://www.googleapis.com/auth/calendar.calendarlist.readonly";

/// Read and change the account's calendar list: subscribe to a calendar
/// by its id or an ICS address, and set a calendar's colour and whether
/// Google's own list hides it.
pub const CALENDAR_LIST_WRITE_SCOPE: &str = "https://www.googleapis.com/auth/calendar.calendarlist";

/// Make, rename and delete the calendars the account owns.
pub const CALENDARS_SCOPE: &str = "https://www.googleapis.com/auth/calendar.calendars";

pub const CALENDAR_API_BASE: &str = "https://www.googleapis.com/calendar/v3";

/// Most pages [`GmailClient::calendar_list`] reads, over 250 calendars
/// each. Far more than any account holds; it stops a runaway loop from
/// reading forever against a server that never stops paging.
const MOST_CALENDAR_PAGES: u32 = 10;

/// Reads one page of an events listing: events, the ids of those deleted
/// outright, and the page and sync tokens when Google sent them.
fn event_page(calendar: &str, answer: &Value) -> EventPage {
    let mut out = EventPage {
        next_page: answer.get("nextPageToken").and_then(Value::as_str).map(str::to_string),
        next_sync: answer.get("nextSyncToken").and_then(Value::as_str).map(str::to_string),
        ..EventPage::default()
    };
    // Google leaves `timeZone` off an event that keeps the calendar's
    // zone, and names that zone once, at the top of the listing.
    let zone = answer.get("timeZone").and_then(Value::as_str).unwrap_or("UTC");
    for item in answer.get("items").and_then(Value::as_array).into_iter().flatten() {
        let cancelled = item.get("status").and_then(Value::as_str) == Some("cancelled");
        let occurrence = item.get("recurringEventId").is_some();
        match (cancelled, occurrence, item.get("id").and_then(Value::as_str)) {
            (true, false, Some(id)) => out.removed.push(id.to_string()),
            _ => out.events.push(google_event(calendar, item, None, zone)),
        }
    }
    out
}

impl GmailClient {
    /// Points the calendar calls at another server. Tests use this.
    pub fn with_calendar_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.calendar_base_url = base_url.into();
        self
    }

    /// Every calendar on the account's list. Stops after
    /// [`MOST_CALENDAR_PAGES`] rather than page forever against a server
    /// that never runs out.
    pub async fn calendar_list(&self) -> Result<Vec<calendar::Calendar>, GmailError> {
        let url = format!("{}/users/me/calendarList", self.calendar_base_url);
        let mut list = Vec::new();
        let mut page: Option<String> = None;
        for _ in 0..MOST_CALENDAR_PAGES {
            let answer: Value = self
                .call_at(&url, |url| {
                    // Google leaves a hidden calendar off the list unless
                    // asked, and the copy would then drop it and its events
                    // rather than keep it under Hidden Calendars.
                    let mut query = vec![("maxResults", "250".to_string()), ("showHidden", "true".to_string())];
                    if let Some(token) = &page {
                        query.push(("pageToken", token.clone()));
                    }
                    self.http().get(url).query(&query)
                })
                .await?;
            list.extend(answer.get("items").and_then(Value::as_array).into_iter().flatten().map(google_calendar));
            match answer.get("nextPageToken").and_then(Value::as_str) {
                Some(next) => page = Some(next.to_string()),
                None => return Ok(list),
            }
        }
        tracing::warn!(pages = MOST_CALENDAR_PAGES, "stopped reading the calendar list early");
        Ok(list)
    }

    /// One page of what changed on `calendar` since `sync_token`, or of
    /// the whole calendar from `time_min` (RFC 3339) when there is no
    /// token. Repeating events come as their series, not as occurrences.
    pub async fn event_changes(
        &self,
        calendar: &str,
        sync_token: Option<&str>,
        page: Option<&str>,
        time_min: &str,
    ) -> Result<EventPage, GmailError> {
        let url = format!("{}/calendars/{}/events", self.calendar_base_url, encode(calendar));
        let answer: Value = self
            .call_at(&url, |url| {
                let mut query = vec![("showDeleted", "true"), ("maxResults", "250")];
                match sync_token {
                    Some(token) => query.push(("syncToken", token)),
                    None => query.push(("timeMin", time_min)),
                }
                if let Some(page) = page {
                    query.push(("pageToken", page));
                }
                self.http().get(url).query(&query)
            })
            .await?;
        Ok(event_page(calendar, &answer))
    }

    /// One page of the events of `calendar` that overlap `time_min` to
    /// `time_max` (RFC 3339), for a range older than the copy reaches. It
    /// is a read of its own: no sync token goes with it, since Google
    /// refuses one beside `timeMin` and `timeMax`. Google may still send a
    /// `nextSyncToken` on the last page; it belongs to this range's filter,
    /// so the caller must not keep it. Series come whole and cancelled events come marked, as in
    /// [`Self::event_changes`].
    pub async fn event_range(
        &self,
        calendar: &str,
        time_min: &str,
        time_max: &str,
        page: Option<&str>,
    ) -> Result<EventPage, GmailError> {
        let url = format!("{}/calendars/{}/events", self.calendar_base_url, encode(calendar));
        let answer: Value = self
            .call_at(&url, |url| {
                let mut query = vec![
                    ("showDeleted", "true"),
                    ("maxResults", "250"),
                    ("timeMin", time_min),
                    ("timeMax", time_max),
                ];
                if let Some(page) = page {
                    query.push(("pageToken", page));
                }
                self.http().get(url).query(&query)
            })
            .await?;
        Ok(event_page(calendar, &answer))
    }

    /// Creates `event` under its own id when `create`, or changes it to
    /// match, and mails the guests when `notify` says so. `etag` makes
    /// Google refuse the change with [`GmailError::Changed`] when the
    /// event moved on since.
    pub async fn put_event(
        &self,
        event: &calendar::Event,
        etag: Option<&str>,
        create: bool,
        notify: Notify,
    ) -> Result<calendar::Event, GmailError> {
        let base = format!("{}/calendars/{}/events", self.calendar_base_url, encode(&event.calendar));
        let body = event_json(event, create);
        // Google reads conferenceData only when told which version of it
        // the body speaks.
        // Without supportsAttachments Google ignores a change to the
        // attachments, so every write says it, whether the body carries
        // them or not.
        let mut query = vec![("sendUpdates", send_updates(notify)), ("supportsAttachments", "true")];
        if event.meet_request.is_some() {
            query.push(("conferenceDataVersion", "1"));
        }
        let answer: Value = if create {
            self.call_at(&base, |url| self.http().post(url).query(&query).json(&body)).await?
        } else {
            // An occurrence goes out as a PATCH on its own id, which
            // changes that occurrence alone. A 404 there means the event
            // is gone and a 400 turns the edit down; no retry follows,
            // since a PUT would clear the fields this body leaves out,
            // such as the Meet link and attachments.
            let url = format!("{base}/{}", encode(&event.id));
            self.call_at(&url, |url| {
                let mut request = self.http().patch(url).query(&query).json(&body);
                if let Some(etag) = etag {
                    request = request.header("If-Match", etag);
                }
                request
            })
            .await?
        };
        // The answer is the event alone, without the calendar's zone, so an
        // event Google gives no zone of its own keeps the one it went out
        // with.
        Ok(google_event(&event.calendar, &answer, None, &event.zone))
    }

    /// Imports `event` into its calendar under its own iCalendar UID, for
    /// a file the person chose to keep (`events.import`). Google matches
    /// on the UID, so importing the same file again updates the copy the
    /// first import made instead of adding a second one. No id goes out
    /// and no guest is invited: the import is a private copy, and mail on
    /// the person's behalf about somebody else's event would surprise
    /// everyone on it.
    pub async fn import_event(&self, event: &calendar::Event) -> Result<calendar::Event, GmailError> {
        let url = format!("{}/calendars/{}/events/import", self.calendar_base_url, encode(&event.calendar));
        let mut body = event_json(event, true);
        if let Some(fields) = body.as_object_mut() {
            for key in ["id", "attendees", "conferenceData"] {
                fields.remove(key);
            }
            fields.insert("iCalUID".to_string(), json!(event.uid));
            if event.sequence > 0 {
                fields.insert("sequence".to_string(), json!(event.sequence));
            }
        }
        let answer: Value = self.call_at(&url, |url| self.http().post(url).json(&body)).await?;
        Ok(google_event(&event.calendar, &answer, None, &event.zone))
    }

    /// Deletes an event, and mails its guests the cancellation when
    /// `notify` says so.
    pub async fn remove_event(
        &self,
        calendar: &str,
        id: &str,
        etag: Option<&str>,
        notify: Notify,
    ) -> Result<(), GmailError> {
        let url = format!("{}/calendars/{}/events/{}", self.calendar_base_url, encode(calendar), encode(id));
        self.call_at_empty(&url, |url| {
            let mut request = self.http().delete(url).query(&[("sendUpdates", send_updates(notify))]);
            if let Some(etag) = etag {
                request = request.header("If-Match", etag);
            }
            request
        })
        .await
    }

    /// Moves `event` from its calendar to `destination` (Google's
    /// `events.move`), and mails its guests when `notify` says so. Google
    /// moves a series whole, its changed occurrences with it. The answer
    /// is the event as it now stands on `destination`; one Google gives no
    /// zone of its own keeps the zone it had.
    pub async fn move_event(
        &self,
        event: &calendar::Event,
        destination: &str,
        notify: Notify,
    ) -> Result<calendar::Event, GmailError> {
        let url = format!(
            "{}/calendars/{}/events/{}/move",
            self.calendar_base_url,
            encode(&event.calendar),
            encode(&event.id)
        );
        let query = [("destination", destination), ("sendUpdates", send_updates(notify))];
        let answer: Value = self.call_at(&url, |url| self.http().post(url).query(&query)).await?;
        Ok(google_event(destination, &answer, None, &event.zone))
    }

    /// Answers event `id` on `calendar` as `me`, for an event the calendar
    /// copy already holds, and lets Google tell the organizer. `id` is the
    /// series' own id to answer every occurrence, or one occurrence's id
    /// (`<series>_<start in UTC>`) to answer that one alone: Google keeps
    /// the answer on that occurrence as a change of its own and leaves the
    /// rest of the series as it was. `note` becomes the guest's
    /// `comment`, which the organizer reads beside the answer; `None`
    /// leaves any earlier note as Google holds it.
    ///
    /// Two calls: a read, since a patch replaces the whole guest list and
    /// the other guests must go back as Google holds them, then the patch.
    /// Answers the event as Google now holds it.
    pub async fn answer_event(
        &self,
        calendar: &str,
        id: &str,
        me: &str,
        answer: Answer,
        note: Option<&str>,
    ) -> Result<calendar::Event, GmailError> {
        let url = format!("{}/calendars/{}/events/{}", self.calendar_base_url, encode(calendar), encode(id));
        let event: Value = self.call_at(&url, |url| self.http().get(url)).await?;
        let guests = answered(&event, me, answer, note);
        let written: Value = self
            .call_at(&url, |url| {
                self.http()
                    .patch(url)
                    .query(&[("sendUpdates", "all")])
                    .json(&json!({ "attendees": guests }))
            })
            .await?;
        Ok(google_event(calendar, &written, Some(me), ""))
    }
}

/// The event's guest list with this account's answer changed and every
/// other guest left as Google has them. A patch replaces the whole list,
/// so sending back less would drop the others.
fn answered(event: &Value, me: &str, answer: Answer, note: Option<&str>) -> Vec<Value> {
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
            if let Some(note) = note {
                fields.insert("comment".into(), json!(note));
            }
        }
    }
    if !found {
        let mut guest = json!({ "email": me, "responseStatus": answer.response_status() });
        if let Some(note) = note {
            guest["comment"] = json!(note);
        }
        guests.push(guest);
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

/// A calendar or event id in a URL path. Ids hold `@` and `#`, which
/// `byte_serialize` turns into `%40` and `%23`; it also turns a space
/// into `+`, which a URL path would read back as a literal plus, so that
/// one substitution is undone.
pub(crate) fn encode(part: &str) -> String {
    url::form_urlencoded::byte_serialize(part.as_bytes())
        .collect::<String>()
        .replace('+', "%20")
}

pub(crate) fn google_calendar(item: &Value) -> calendar::Calendar {
    let text = |key: &str| item.get(key).and_then(Value::as_str).unwrap_or_default().to_string();
    calendar::Calendar {
        id: text("id"),
        name: item
            .get("summaryOverride")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| text("summary")),
        color: text("backgroundColor"),
        access: Access::parse(&text("accessRole")),
        zone: item.get("timeZone").and_then(Value::as_str).unwrap_or("UTC").to_string(),
        primary: item.get("primary").and_then(Value::as_bool) == Some(true),
        shown: item.get("selected").and_then(Value::as_bool) != Some(false),
        // Google sends `hidden` only when it is true.
        hidden: item.get("hidden").and_then(Value::as_bool) == Some(true),
        reminders: reminders(item.get("defaultReminders")),
    }
}

fn reminders(list: Option<&Value>) -> Vec<Reminder> {
    list.and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|r| {
            Some(Reminder {
                minutes: u32::try_from(r.get("minutes")?.as_u64()?).ok()?,
                method: match r.get("method")?.as_str()? {
                    "email" => ReminderMethod::Email,
                    _ => ReminderMethod::Notification,
                },
            })
        })
        .collect()
}

/// Google's event as the neutral model has it. `me` names the account
/// for guests Google did not mark with `self`. `calendar_zone` is the zone
/// of the calendar the event is on, which a timed event without a
/// `timeZone` of its own keeps.
pub fn google_event(calendar: &str, item: &Value, me: Option<&str>, calendar_zone: &str) -> calendar::Event {
    let text = |key: &str| item.get(key).and_then(Value::as_str).unwrap_or_default().to_string();
    let (start, zone, all_day) = when(item.get("start"), calendar_zone);
    let (end, _, _) = when(item.get("end"), calendar_zone);
    let guests: Vec<CalendarGuest> = item
        .get("attendees")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|g| {
            let email = g.get("email")?.as_str()?.to_string();
            let me = g.get("self").and_then(Value::as_bool) == Some(true)
                || me.is_some_and(|me| me.eq_ignore_ascii_case(&email));
            Some(CalendarGuest {
                name: g.get("displayName").and_then(Value::as_str).map(str::to_string),
                answer: Answer::from_response_status(g.get("responseStatus").and_then(Value::as_str).unwrap_or("")),
                organizer: g.get("organizer").and_then(Value::as_bool) == Some(true),
                me,
                email,
            })
        })
        .collect();
    let overrides = item.pointer("/reminders/useDefault").and_then(Value::as_bool) == Some(false);
    calendar::Event {
        calendar: calendar.to_string(),
        id: text("id"),
        uid: text("iCalUID"),
        etag: text("etag"),
        start,
        end: end.max(start),
        zone,
        all_day,
        title: text("summary"),
        place: text("location"),
        description: text("description"),
        color: item.get("colorId").and_then(Value::as_str).and_then(calendar::event_color).map(str::to_string),
        // `busy` holds transparency alone. Declined and all-day events keep
        // the busy value Google sent, so a queued edit never marks them
        // free on the way back out.
        busy: item.get("transparency").and_then(Value::as_str) != Some("transparent"),
        status: Status::parse(&text("status")),
        private: matches!(item.get("visibility").and_then(Value::as_str), Some("private" | "confidential")),
        organizer: item.pointer("/organizer/email").and_then(Value::as_str).map(str::to_string),
        my_answer: guests.iter().find(|g| g.me).and_then(|g| g.answer),
        guests,
        sequence: item.get("sequence").and_then(Value::as_i64).unwrap_or(0),
        reminders: overrides.then(|| reminders(item.pointer("/reminders/overrides"))),
        conference: item
            .get("hangoutLink")
            .and_then(Value::as_str)
            .or_else(|| {
                item.pointer("/conferenceData/entryPoints")?
                    .as_array()?
                    .iter()
                    .find(|p| p.get("entryPointType").and_then(Value::as_str) == Some("video"))?
                    .get("uri")?
                    .as_str()
            })
            .map(str::to_string),
        rules: item
            .get("recurrence")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        series: item.get("recurringEventId").and_then(Value::as_str).map(str::to_string),
        original_start: item.get("originalStartTime").map(|t| when(Some(t), calendar_zone).0),
        pending: false,
        // A Meet request is something Penguin Mail asks for on a write;
        // Google's answer never needs to say one is still pending here.
        meet_request: None,
        kind: kind_of(item),
        // Google leaves the key off an event with no files, so an event
        // read here always knows its list, empty or not.
        attachments: Some(attachments_of(item)),
    }
}

/// The files linked to Google's event.
fn attachments_of(item: &Value) -> Vec<calendar::Attachment> {
    let text = |file: &Value, key: &str| file.get(key).and_then(Value::as_str).unwrap_or_default().to_string();
    item.get("attachments")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|file| calendar::Attachment {
            title: text(file, "title"),
            file_url: text(file, "fileUrl"),
            mime_type: text(file, "mimeType"),
            icon_link: text(file, "iconLink"),
            file_id: text(file, "fileId"),
            // What only this computer knows of a file comes back when the
            // store keeps it (`mailrs_domain::calendar::keep_local`).
            ..calendar::Attachment::default()
        })
        .collect()
}

/// The attachments as a write sends them. Google fills `fileId` and
/// `iconLink` for a Drive file itself (the reference calls the id
/// read-only), so only the link, title and type go out. A file still
/// waiting to upload has no link yet and stays out.
fn attachments_json(list: &[calendar::Attachment]) -> Value {
    json!(list
        .iter()
        .filter(|file| file.waiting.is_none() && !file.file_url.is_empty())
        .map(|file| json!({"fileUrl": file.file_url, "title": file.title, "mimeType": file.mime_type}))
        .collect::<Vec<_>>())
}

/// What sort of entry Google's event is, from its `eventType` and the
/// properties object that type carries. An unknown type, such as
/// `fromGmail`, reads as an ordinary event.
fn kind_of(item: &Value) -> calendar::Kind {
    let text = |pointer: &str| item.pointer(pointer).and_then(Value::as_str).unwrap_or_default().to_string();
    let decline = |properties: &str| calendar::Decline {
        meetings: calendar::Declines::from_google(&text(&format!("/{properties}/autoDeclineMode"))),
        message: text(&format!("/{properties}/declineMessage")),
    };
    match item.get("eventType").and_then(Value::as_str) {
        Some("outOfOffice") => calendar::Kind::OutOfOffice(decline("outOfOfficeProperties")),
        Some("focusTime") => calendar::Kind::Focus(decline("focusTimeProperties")),
        Some("birthday") => calendar::Kind::Birthday,
        Some("workingLocation") => {
            // An office without a label still has its building's id; the
            // person picked it from Google's list, so either names it.
            let office = || {
                Some(text("/workingLocationProperties/officeLocation/label"))
                    .filter(|label| !label.is_empty())
                    .unwrap_or_else(|| text("/workingLocationProperties/officeLocation/buildingId"))
            };
            let place = match text("/workingLocationProperties/type").as_str() {
                "officeLocation" => calendar::Workplace::Office(office()),
                "customLocation" => calendar::Workplace::Elsewhere(text("/workingLocationProperties/customLocation/label")),
                _ => calendar::Workplace::Home,
            };
            calendar::Kind::WorkingLocation(place)
        }
        _ => calendar::Kind::Event,
    }
}

/// Google's `sendUpdates` for a write. Without the parameter Google
/// mails nobody, so a new event with guests must say `all` or they never
/// hear of it.
fn send_updates(notify: Notify) -> &'static str {
    match notify {
        Notify::Guests => "all",
        Notify::Nobody => "none",
    }
}

/// An event time as an instant, its zone, and whether it is a whole day.
/// A time without a `timeZone` is in `calendar_zone`: Google's `dateTime`
/// carries only an offset, which says nothing about summer time.
fn when(time: Option<&Value>, calendar_zone: &str) -> (EpochMillis, String, bool) {
    let Some(time) = time else {
        return (0, "UTC".into(), false);
    };
    let fallback = if calendar_zone.is_empty() { "UTC" } else { calendar_zone };
    let zone = time
        .get("timeZone")
        .and_then(Value::as_str)
        .filter(|zone| !zone.is_empty())
        .unwrap_or(fallback)
        .to_string();
    if let Some(at) = time.get("dateTime").and_then(Value::as_str) {
        let at = chrono::DateTime::parse_from_rfc3339(at).map(|a| a.timestamp_millis()).unwrap_or(0);
        return (at, zone, false);
    }
    let day = time
        .get("date")
        .and_then(Value::as_str)
        .and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|d| d.and_utc().timestamp_millis())
        .unwrap_or(0);
    (day, "UTC".into(), true)
}

/// A guest's change to someone else's event: only the fields that are
/// the guest's own. The time, rules and guests belong to the organizer,
/// and a guest's PATCH that carried them could move the series for every
/// guest, since it goes out with `sendUpdates=all`.
fn own_fields_json(event: &calendar::Event) -> Value {
    let mut body = json!({ "transparency": transparency(event) });
    own_fields_into(&mut body, event, false);
    body
}

fn transparency(event: &calendar::Event) -> &'static str {
    if event.busy { "opaque" } else { "transparent" }
}

/// The reminders and colour, which every change writes.
fn own_fields_into(body: &mut Value, event: &calendar::Event, create: bool) {
    if let Some(reminders) = &event.reminders {
        body["reminders"] = json!({
            "useDefault": false,
            "overrides": reminders.iter().map(|r| json!({
                "method": match r.method { ReminderMethod::Email => "email", ReminderMethod::Notification => "popup" },
                "minutes": r.minutes,
            })).collect::<Vec<_>>(),
        });
    }
    match event.color.as_deref().and_then(calendar::color_id) {
        Some(id) => body["colorId"] = json!(id),
        // A patch with no colour clears the event's own, so it takes the
        // calendar's again. A new event has nothing to clear.
        None if !create => body["colorId"] = Value::Null,
        None => {}
    }
}

/// What Penguin Mail writes on an event. Fields the model does not hold
/// are left out, so a patch keeps whatever Google has for them.
fn event_json(event: &calendar::Event, create: bool) -> Value {
    if !create && event.limited() {
        return own_fields_json(event);
    }
    let time = |at: EpochMillis| {
        if event.all_day {
            let day = chrono::DateTime::from_timestamp_millis(at).map(|d| d.format("%Y-%m-%d").to_string());
            json!({ "date": day })
        } else {
            let at = chrono::DateTime::from_timestamp_millis(at).map(|d| d.to_rfc3339());
            let mut value = json!({ "dateTime": at });
            if !event.zone.is_empty() {
                value["timeZone"] = json!(event.zone);
            }
            value
        }
    };
    let mut body = json!({
        "transparency": transparency(event),
        "summary": event.title,
        "description": event.description,
        "start": time(event.start),
        "end": time(event.end),
        "visibility": if event.private { "private" } else { "default" },
    });
    if let Some(decline) = event.kind.decline() {
        // Google refuses guests, a place and a Meet link on out of office
        // and focus time, and takes the type only when the entry is made.
        if create {
            body["eventType"] = json!(event.kind.as_google());
        }
        let properties = match event.kind {
            calendar::Kind::Focus(_) => "focusTimeProperties",
            _ => "outOfOfficeProperties",
        };
        body["transparency"] = json!("opaque");
        body[properties] = json!({
            "autoDeclineMode": decline.meetings.as_google(),
            "declineMessage": decline.message,
        });
        if event.series.is_none() && !(create && event.rules.is_empty()) {
            body["recurrence"] = json!(event.rules);
        }
        own_fields_into(&mut body, event, create);
        if create {
            body["id"] = json!(event.id);
        }
        return body;
    }
    body["location"] = json!(event.place);
    body["attendees"] = json!(event.guests.iter().map(|g| {
            let mut guest = json!({ "email": g.email });
            if let Some(status) = g.answer.map(Answer::response_status) {
                guest["responseStatus"] = json!(status);
            }
            if let Some(name) = &g.name {
                guest["displayName"] = json!(name);
            }
            guest
        }).collect::<Vec<_>>());
    // Google refuses a recurrence rule on a changed occurrence. A patch
    // that leaves recurrence out keeps the rule Google has, so a series
    // saved with no rules must say so with an empty list. A new event
    // with no rules has nothing to say.
    if event.series.is_none() && !(create && event.rules.is_empty()) {
        body["recurrence"] = json!(event.rules);
    }
    own_fields_into(&mut body, event, create);
    // An unread list stays off the body: a PATCH without the key keeps
    // Google's files, and an empty list would take them all off.
    if let Some(list) = &event.attachments {
        body["attachments"] = attachments_json(list);
    }
    if let Some(request) = &event.meet_request {
        body["conferenceData"] = json!({
            "createRequest": {
                "requestId": request,
                "conferenceSolutionKey": {"type": "hangoutsMeet"},
            }
        });
    }
    if create {
        body["id"] = json!(event.id);
    }
    body
}
