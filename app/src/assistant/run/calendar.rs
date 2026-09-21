//! The calendar tools: what is on, when the user is free, the events the
//! assistant makes, changes and deletes, and the answer to an invitation
//! in mail. The work goes through `mailrs_sync::Calendar` and
//! `mailrs_sync::Invitations`, the second of which the event card's
//! buttons use too.
//!
//! Times go in and come back as local time, `YYYY-MM-DDTHH:MM`, the shape
//! `remind_me` takes, and a plain `YYYY-MM-DD` stands for a whole day.

use chrono::{Datelike, Local, NaiveDate, NaiveTime, TimeZone, Weekday};
use mailrs_domain::invitation::Answer;
use mailrs_gmail::{Event, EventFields, EventTime};
use mailrs_sync::Told;
use mailrs_sync::calendar::at;

use super::*;

/// The longest stretch `find_free_time` looks through, in days.
const MOST_DAYS: i64 = 31;

/// The most free slots one answer lists.
const MOST_SLOTS: usize = 30;

/// A moment a calendar tool was given: an instant, or a whole day.
#[derive(Debug, Clone, Copy)]
enum Moment {
    At(EpochMillis),
    Day(NaiveDate),
}

fn moment(text: &str) -> Result<Moment, String> {
    match NaiveDate::parse_from_str(text.trim(), "%Y-%m-%d") {
        Ok(day) => Ok(Moment::Day(day)),
        Err(_) => instant(text).map(Moment::At),
    }
}

/// Local time `time` on `day`. A time the clocks skip that day, such as
/// one lost when summer time starts, moves to the next that exists.
fn on_day(day: NaiveDate, time: NaiveTime) -> Result<EpochMillis, String> {
    let naive = day.and_time(time);
    Local
        .from_local_datetime(&naive)
        .earliest()
        .or_else(|| {
            Local
                .from_local_datetime(&(naive + chrono::Duration::hours(1)))
                .earliest()
        })
        .map(|at| at.timestamp_millis())
        .ok_or_else(|| format!("{naive} does not exist here."))
}

fn midnight(day: NaiveDate) -> Result<EpochMillis, String> {
    on_day(day, NaiveTime::MIN)
}

/// The local day an instant falls on.
fn day_of(at: EpochMillis) -> Result<NaiveDate, String> {
    crate::format::local(at)
        .map(|at| at.date_naive())
        .ok_or_else(|| "That time is out of range.".to_string())
}

/// The span `from` and `to` name. A day given as `to` counts in full.
fn window(input: &Value) -> Result<(EpochMillis, EpochMillis), String> {
    let from = match moment(&required(input, "from")?)? {
        Moment::At(at) => at,
        Moment::Day(day) => midnight(day)?,
    };
    let to = match moment(&required(input, "to")?)? {
        Moment::At(at) => at,
        Moment::Day(day) => midnight(day.succ_opt().ok_or("That day is out of range.")?)?,
    };
    if to <= from {
        return Err("`to` must come after `from`.".into());
    }
    Ok((from, to))
}

/// What Google calls an answer, in the words the tools use.
fn answer_word(google: &str) -> &'static str {
    match google {
        "accepted" => "yes",
        "declined" => "no",
        "tentative" => "maybe",
        _ => "not yet",
    }
}

/// An event's start or end as the tools write it. An all-day event ends
/// on the day after its last one in Google's terms, and on its last day
/// in the tools', since that is how a person says it.
fn time_text(time: Option<&EventTime>, end: bool) -> Option<String> {
    match time? {
        EventTime::At(_) => mailrs_sync::calendar::instant(time?).map(local_text),
        EventTime::Day(day) => {
            let day = NaiveDate::parse_from_str(day, "%Y-%m-%d").ok()?;
            let day = if end { day.pred_opt()? } else { day };
            Some(day.format("%Y-%m-%d").to_string())
        }
    }
}

fn event_json(event: &Event) -> Value {
    const MAX_DESCRIPTION: usize = 1000;
    let all_day = matches!(event.start, Some(EventTime::Day(_)));
    let weekday = match &event.start {
        Some(EventTime::Day(day)) => NaiveDate::parse_from_str(day, "%Y-%m-%d")
            .ok()
            .map(|d| d.format("%A").to_string()),
        Some(time) => mailrs_sync::calendar::instant(time)
            .and_then(crate::format::local)
            .map(|at| at.format("%A").to_string()),
        None => None,
    };
    let mut description: String = event.description.chars().take(MAX_DESCRIPTION).collect();
    if event.description.chars().count() > MAX_DESCRIPTION {
        description.push_str("\n[cut short]");
    }
    let mine = event.guests.iter().find(|g| g.me);
    json!({
        "id": event.id,
        "title": event.summary,
        "start": time_text(event.start.as_ref(), false),
        "end": time_text(event.end.as_ref(), true),
        "weekday": weekday,
        "all_day": all_day,
        "location": event.location,
        "description": description,
        "organizer": event.organizer,
        "guests": event.guests.iter().map(|g| json!({
            "email": g.email,
            "name": g.name,
            "answer": answer_word(&g.answer),
        })).collect::<Vec<_>>(),
        "my_answer": mine.map(|g| answer_word(&g.answer)),
        "busy": event.busy,
        "cancelled": event.cancelled,
        "link": event.link,
    })
}

/// The guest addresses a tool gave, as addresses or "Name <address>".
fn guests(input: &Value) -> Option<Vec<String>> {
    let list = input.get("attendees")?.as_array()?;
    let joined = list
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    Some(
        compose::parse_recipients(&joined)
            .into_iter()
            .map(|a| a.email)
            .collect(),
    )
}

/// A start or an end as Google takes it. The tools name the last day of
/// an all-day event, and Google the day after it.
fn event_time(moment: Moment, end: bool) -> Result<EventTime, String> {
    match moment {
        Moment::At(instant) => at(instant).ok_or_else(|| "That time is out of range.".into()),
        Moment::Day(day) => {
            let day = match end {
                true => day.succ_opt().ok_or("That day is out of range.")?,
                false => day,
            };
            Ok(EventTime::Day(day.format("%Y-%m-%d").to_string()))
        }
    }
}

/// How a confirmation says when an event runs.
fn when_text(start: Moment, end: Moment) -> String {
    let day = |d: NaiveDate| d.format("%a %-d %b").to_string();
    let at = |t: EpochMillis| {
        crate::format::local(t)
            .map(|t| t.format("%a %-d %b %H:%M").to_string())
            .unwrap_or_default()
    };
    let hour = |t: EpochMillis| {
        crate::format::local(t)
            .map(|t| t.format("%H:%M").to_string())
            .unwrap_or_default()
    };
    match (start, end) {
        (Moment::Day(first), Moment::Day(last)) if first == last => day(first),
        (Moment::Day(first), Moment::Day(last)) => fill(
            &gettext("{first} to {last}"),
            &[("first", &day(first)), ("last", &day(last))],
        ),
        (Moment::At(from), Moment::At(to)) if day_of(from).ok() == day_of(to).ok() => fill(
            &gettext("{first} to {last}"),
            &[("first", &at(from)), ("last", &hour(to))],
        ),
        (Moment::At(from), Moment::At(to)) => fill(
            &gettext("{first} to {last}"),
            &[("first", &at(from)), ("last", &at(to))],
        ),
        _ => String::new(),
    }
}

impl<A: Accounts> Tools<A> {
    pub(super) async fn list_events(&self, input: &Value) -> ToolResult {
        let account = self.account_or_default(input)?;
        let (from, to) = window(input)?;
        let calendar = Arc::clone(&self.modules.calendar);
        let account_id = account.id;
        let events = self
            .permitted(&account, Permission::Calendar, async move {
                calendar.events(account_id, from, to).await
            })
            .await?;
        Ok(json!({
            "account": account.email,
            "count": events.len(),
            "events": events.iter().map(event_json).collect::<Vec<_>>(),
        }))
    }

    pub(super) async fn find_free_time(&self, input: &Value) -> ToolResult {
        let account = self.account_or_default(input)?;
        let (from, to) = window(input)?;
        let from = from.max(Local::now().timestamp_millis());
        let minutes = input
            .get("minutes")
            .and_then(Value::as_u64)
            .ok_or("`minutes` is missing")?
            .clamp(5, 24 * 60) as i64;
        let hour = |key: &str, fallback: NaiveTime| -> Result<NaiveTime, String> {
            match text(input, key) {
                None => Ok(fallback),
                Some(value) => NaiveTime::parse_from_str(&value, "%H:%M")
                    .map_err(|_| format!("Could not read the time of day {value}; use HH:MM.")),
            }
        };
        let starts = hour(
            "day_starts",
            NaiveTime::from_hms_opt(9, 0, 0).unwrap_or_default(),
        )?;
        let ends = hour(
            "day_ends",
            NaiveTime::from_hms_opt(18, 0, 0).unwrap_or_default(),
        )?;
        if ends <= starts {
            return Err("`day_ends` must come after `day_starts`.".into());
        }
        let weekends = flag(input, "weekends").unwrap_or(false);
        let mut windows = Vec::new();
        let (first, last) = (day_of(from)?, day_of(to)?);
        let mut day = first;
        while day <= last && (day - first).num_days() < MOST_DAYS {
            let rest = matches!(day.weekday(), Weekday::Sat | Weekday::Sun);
            if weekends || !rest {
                let open = on_day(day, starts)?.max(from);
                let close = on_day(day, ends)?.min(to);
                if close > open {
                    windows.push((open, close));
                }
            }
            day = day.succ_opt().ok_or("That day is out of range.")?;
        }
        let calendar = Arc::clone(&self.modules.calendar);
        let account_id = account.id;
        let length = minutes * 60 * 1000;
        let slots = self
            .permitted(&account, Permission::Calendar, async move {
                calendar.free(account_id, &windows, length).await
            })
            .await?;
        Ok(json!({
            "account": account.email,
            "free": slots.iter().take(MOST_SLOTS).map(|(start, end)| json!({
                "start": local_text(*start),
                "end": local_text(*end),
                "weekday": crate::format::local(*start).map(|t| t.format("%A").to_string()),
                "minutes": (end - start) / 60_000,
            })).collect::<Vec<_>>(),
        }))
    }

    pub(super) async fn create_event(&self, input: &Value) -> ToolResult {
        let account = self.account_or_default(input)?;
        let title = required(input, "title")?;
        let start = moment(&required(input, "start")?)?;
        let end = moment(&required(input, "end")?)?;
        let in_order = match (start, end) {
            (Moment::At(from), Moment::At(to)) => to > from,
            (Moment::Day(first), Moment::Day(last)) => last >= first,
            _ => {
                return Err(
                    "Give start and end both as times, or both as days for an all-day event."
                        .into(),
                );
            }
        };
        if !in_order {
            return Err("The event must end after it starts.".into());
        }
        let fields = EventFields {
            summary: Some(title.clone()),
            start: Some(event_time(start, false)?),
            end: Some(event_time(end, true)?),
            location: text(input, "location"),
            description: text(input, "description"),
            guests: guests(input),
        };
        let mut question = fill(
            &gettext("Add “{title}” to the calendar for {account}, {when}?"),
            &[
                ("title", &title),
                ("account", &account.email),
                ("when", &when_text(start, end)),
            ],
        );
        if let Some(invited) = fields.guests.as_ref().filter(|g| !g.is_empty()) {
            question.push_str("\n\n");
            question.push_str(&fill(
                &gettext("Google sends an invitation to {guests}."),
                &[("guests", &invited.join(", "))],
            ));
        }
        self.approve(&question).await?;
        let calendar = Arc::clone(&self.modules.calendar);
        let account_id = account.id;
        let made = self
            .permitted(&account, Permission::Calendar, async move {
                calendar.create(account_id, &fields).await
            })
            .await?;
        Ok(json!({"account": account.email, "created": event_json(&made)}))
    }

    pub(super) async fn update_event(&self, input: &Value) -> ToolResult {
        let account = self.account_or_default(input)?;
        let id = required(input, "id")?;
        let moment_of = |key: &str| -> Result<Option<Moment>, String> {
            text(input, key).map(|t| moment(&t)).transpose()
        };
        let (start, end) = (moment_of("start")?, moment_of("end")?);
        let fields = EventFields {
            summary: text(input, "title"),
            start: start.map(|m| event_time(m, false)).transpose()?,
            end: end.map(|m| event_time(m, true)).transpose()?,
            location: input
                .get("location")
                .and_then(Value::as_str)
                .map(str::to_string),
            description: input
                .get("description")
                .and_then(Value::as_str)
                .map(str::to_string),
            guests: guests(input),
        };
        if fields == EventFields::default() {
            return Err(
                "Say what to change: a title, a time, a place, guests or a description.".into(),
            );
        }
        let mut changes = Vec::new();
        let mut line = |label: String, value: &str| {
            changes.push(fill(&label, &[("value", value)]));
        };
        if let Some(title) = &fields.summary {
            line(gettext("Title: {value}"), title);
        }
        if let Some(start) = start {
            line(gettext("Starts: {value}"), &moment_text(start));
        }
        if let Some(end) = end {
            line(gettext("Ends: {value}"), &moment_text(end));
        }
        if let Some(location) = &fields.location {
            line(gettext("Where: {value}"), location);
        }
        if let Some(guests) = &fields.guests {
            line(gettext("Guests: {value}"), &guests.join(", "));
        }
        if let Some(description) = &fields.description {
            let preview: String = description.chars().take(160).collect();
            line(gettext("Description: {value}"), &preview);
        }
        let name = text(input, "current_title");
        let question = match &name {
            Some(title) => fill(
                &gettext("Change “{title}” on the calendar for {account}? Its guests are told."),
                &[("title", title), ("account", &account.email)],
            ),
            None => fill(
                &gettext("Change an event on the calendar for {account}? Its guests are told."),
                &[("account", &account.email)],
            ),
        };
        self.approve(&format!("{question}\n\n{}", changes.join("\n")))
            .await?;
        let calendar = Arc::clone(&self.modules.calendar);
        let account_id = account.id;
        let changed = self
            .permitted(&account, Permission::Calendar, async move {
                calendar.update(account_id, &id, &fields).await
            })
            .await?;
        Ok(json!({"account": account.email, "updated": event_json(&changed)}))
    }

    pub(super) async fn delete_event(&self, input: &Value) -> ToolResult {
        let account = self.account_or_default(input)?;
        let id = required(input, "id")?;
        let question = match text(input, "title") {
            Some(title) => fill(
                &gettext("Delete “{title}” from the calendar for {account}? Its guests are told."),
                &[("title", &title), ("account", &account.email)],
            ),
            None => fill(
                &gettext("Delete an event from the calendar for {account}? Its guests are told."),
                &[("account", &account.email)],
            ),
        };
        self.approve(&question).await?;
        let calendar = Arc::clone(&self.modules.calendar);
        let (account_id, gone) = (account.id, id.clone());
        self.permitted(&account, Permission::Calendar, async move {
            calendar.delete(account_id, &gone).await
        })
        .await?;
        Ok(json!({"account": account.email, "deleted": id}))
    }

    pub(super) async fn answer_invitation(&self, input: &Value) -> ToolResult {
        let (account, sync) = self.sync_for(&required(input, "account")?)?;
        let message_id = required(input, "message_id")?;
        let (answer, said) = match required(input, "answer")?.to_lowercase().as_str() {
            "yes" => (Answer::Yes, gettext("Yes")),
            "no" => (Answer::No, gettext("No")),
            "maybe" => (Answer::Maybe, gettext("Maybe")),
            other => return Err(format!("Unknown answer {other}; use yes, no or maybe.")),
        };
        // The question names the event, so the body is read before asking.
        // Answering reads it again from the cache.
        let id = message_id.clone();
        let body = self.call(async move { sync.body(&id).await }).await?;
        let invitation = body
            .calendar
            .as_deref()
            .and_then(mailrs_domain::invitation::read)
            .ok_or("That message holds no invitation.")?;
        self.approve(&fill(
            &gettext("Answer {answer} to “{title}”?"),
            &[("answer", &said), ("title", &invitation.summary)],
        ))
        .await?;
        let invitations = Arc::clone(&self.modules.invitations);
        let (account_id, me) = (account.id, account.email.clone());
        let now = Local::now().timestamp_millis();
        let answered = self
            .away(async move {
                invitations
                    .answer_message(account_id, &message_id, &me, answer, now)
                    .await
            })
            .await?
            .map_err(|err| err.to_string())?;
        let Some((invitation, sent)) = answered else {
            return Err(
                "That invitation waits on no answer: it is a cancellation or a reply.".into(),
            );
        };
        let went = match sent.told {
            Told::Calendar => "Google Calendar recorded the answer and told the organizer.",
            Told::Organizer => "The answer went to the organizer by email.",
            Told::Nobody => {
                return Err(
                    "The invitation names no organizer, so there is nobody to answer.".into(),
                );
            }
        };
        let mut result = json!({
            "event": invitation.summary,
            "answer": required(input, "answer")?.to_lowercase(),
            "went": went,
        });
        if sent.needs_permission {
            self.effects
                .ask_permission(account.id, Permission::Calendar);
            result["note"] = json!(
                "The user's own calendar was not marked, since Penguin Mail lacks the calendar permission. The user was asked to grant it."
            );
        }
        if let Some(off) = &sent.api_off {
            self.effects.explain_api_off(&off.service, &off.enable_url);
            result["note"] = json!(format!(
                "The user's own calendar was not marked, since the {} is switched off in the Google Cloud project. The user was shown where to turn it on.",
                off.service
            ));
        }
        Ok(result)
    }
}

/// A start or an end on its own, as a confirmation says it.
fn moment_text(moment: Moment) -> String {
    match moment {
        Moment::Day(day) => day.format("%a %-d %b").to_string(),
        Moment::At(at) => crate::format::local(at)
            .map(|t| t.format("%a %-d %b %H:%M").to_string())
            .unwrap_or_default(),
    }
}
