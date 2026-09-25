//! The calendar tools: what is on, when the user is free, the events the
//! assistant makes, changes and deletes, and the answer to an invitation
//! in mail. The work goes through `mailrs_sync::Calendar` and
//! `mailrs_sync::Invitations`, the second of which the event card's
//! buttons use too.
//!
//! Times go in and come back as local time, `YYYY-MM-DDTHH:MM`, the shape
//! `remind_me` takes, and a plain `YYYY-MM-DD` stands for a whole day.

use std::collections::HashMap;

use chrono::{DateTime, Datelike, Local, NaiveDate, NaiveTime, TimeZone, Utc, Weekday};
use mailrs_domain::calendar as model;
use mailrs_domain::invitation::Answer;
use mailrs_gmail::{EventFields, EventTime};
use mailrs_sync::Told;
use mailrs_sync::calendar::at;

use super::*;

/// A day, in milliseconds.
const DAY: EpochMillis = 24 * 60 * 60 * 1000;

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

/// What the tools call an answer nobody has given yet.
fn answer_word(answer: Option<Answer>) -> &'static str {
    match answer {
        Some(Answer::Yes) => "yes",
        Some(Answer::No) => "no",
        Some(Answer::Maybe) => "maybe",
        None => "not yet",
    }
}

/// Wraps a single event `create` or `update` hands back into the
/// occurrence `event_json` reads, so both tools share the one formatter.
fn as_occurrence(account_id: AccountId, event: model::Event) -> model::Occurrence {
    let (start, end) = (event.start, event.end);
    model::Occurrence { account_id, event: std::sync::Arc::new(event), start, end }
}

/// The UTC date an instant falls on, for an all-day event: a date means
/// the same date wherever the reader is (reconcile.md Task 9 item 7), so
/// this reads UTC rather than local time.
fn utc_date(at: EpochMillis) -> Option<NaiveDate> {
    DateTime::<Utc>::from_timestamp_millis(at).map(|at| at.date_naive())
}

/// One occurrence, in the shape the calendar tools write. `calendar_name`
/// is looked up once per call, from the account's own calendar list, and
/// keyed by id (reconcile.md Task 9 item 6).
fn event_json(occurrence: &model::Occurrence, calendar_name: &HashMap<String, String>) -> Value {
    const MAX_DESCRIPTION: usize = 1000;
    let event = &occurrence.event;
    let (start_text, end_text, weekday) = if event.all_day {
        // The neutral end sits at midnight UTC after the last day; the
        // tools name that last day itself, the way a person says it.
        let start_day = utc_date(occurrence.start);
        let end_day = utc_date(occurrence.end - DAY).or(start_day);
        (
            start_day.map(|d| d.format("%Y-%m-%d").to_string()),
            end_day.map(|d| d.format("%Y-%m-%d").to_string()),
            start_day.map(|d| d.format("%A").to_string()),
        )
    } else {
        (
            Some(local_text(occurrence.start)),
            Some(local_text(occurrence.end)),
            crate::format::local(occurrence.start).map(|at| at.format("%A").to_string()),
        )
    };
    let mut description: String = event.description.chars().take(MAX_DESCRIPTION).collect();
    if event.description.chars().count() > MAX_DESCRIPTION {
        description.push_str("\n[cut short]");
    }
    json!({
        // An occurrence of a series has an id of its own, so a change the
        // model makes to it reaches that occurrence and not the series.
        "id": occurrence.id(),
        "repeats": !event.rules.is_empty() || event.series.is_some(),
        "title": event.title,
        "start": start_text,
        "end": end_text,
        "weekday": weekday,
        "all_day": event.all_day,
        "location": event.place,
        "description": description,
        "organizer": event.organizer,
        "guests": event.guests.iter().map(|g| json!({
            "email": g.email,
            "name": g.name,
            "answer": answer_word(g.answer),
        })).collect::<Vec<_>>(),
        "my_answer": answer_word(event.my_answer),
        "busy": event.busy,
        "cancelled": event.status == model::Status::Cancelled,
        "link": Value::Null,
        "calendar": calendar_name.get(&event.calendar).cloned().unwrap_or_else(|| event.calendar.clone()),
        "pending": event.pending,
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
    let day = |d: NaiveDate| {
        d.format_localized(&gettext("%a %-d %b"), date_locale())
            .to_string()
    };
    let at = |t: EpochMillis| {
        crate::format::local(t)
            .map(|t| {
                t.format_localized(&gettext("%a %-d %b %H:%M"), date_locale())
                    .to_string()
            })
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
        if let Some(answer) = self.unavailable(&account, Missing::Calendar) {
            return Ok(answer);
        }
        let (from, to) = window(input)?;
        let calendar = Arc::clone(&self.modules.calendar);
        let account_id = account.id;
        let events = self
            .permitted(&account, Permission::Calendar, async move {
                calendar.events(account_id, from, to).await
            })
            .await?;
        let names = self.calendar_names(account_id).await?;
        Ok(json!({
            "account": account.email,
            "count": events.len(),
            "events": events.iter().map(|o| event_json(o, &names)).collect::<Vec<_>>(),
        }))
    }

    /// Each of the account's calendars, by id, for `event_json`'s
    /// "calendar" key (reconcile.md Task 9 item 6). Empty before the
    /// local copy has ever read the account's calendar list, which
    /// leaves `event_json` to fall back to the raw id.
    async fn calendar_names(&self, account_id: AccountId) -> Result<HashMap<String, String>, String> {
        let names = self
            .read(move |c| mailrs_store::calendar::calendars(c, account_id))
            .await?;
        Ok(names.into_iter().map(|c| (c.id, c.name)).collect())
    }

    pub(super) async fn find_free_time(&self, input: &Value) -> ToolResult {
        let account = self.account_or_default(input)?;
        if let Some(answer) = self.unavailable(&account, Missing::Calendar) {
            return Ok(answer);
        }
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

    pub(super) async fn create_event<'a>(&'a self, input: &'a Value) -> Result<Plan<'a>, String> {
        let account = self.account_or_default(input)?;
        if let Some(answer) = self.unavailable(&account, Missing::Calendar) {
            return Ok(Plan::without_asking(async move { Ok(answer) }));
        }
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
        Ok(Plan::ask(question, async move {
            let calendar = Arc::clone(&self.modules.calendar);
            let account_id = account.id;
            let made = self
                .permitted(&account, Permission::Calendar, async move {
                    calendar.create(account_id, &fields).await
                })
                .await?;
            let names = self.calendar_names(account_id).await?;
            let occurrence = as_occurrence(account_id, made);
            Ok(json!({"account": account.email, "created": event_json(&occurrence, &names)}))
        }))
    }

    pub(super) async fn update_event<'a>(&'a self, input: &'a Value) -> Result<Plan<'a>, String> {
        let account = self.account_or_default(input)?;
        if let Some(answer) = self.unavailable(&account, Missing::Calendar) {
            return Ok(Plan::without_asking(async move { Ok(answer) }));
        }
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
        let question = format!("{question}\n\n{}", changes.join("\n"));
        Ok(Plan::ask(question, async move {
            let calendar = Arc::clone(&self.modules.calendar);
            let account_id = account.id;
            let changed = self
                .permitted(&account, Permission::Calendar, async move {
                    calendar.update(account_id, &id, &fields).await
                })
                .await?;
            let names = self.calendar_names(account_id).await?;
            let occurrence = as_occurrence(account_id, changed);
            Ok(json!({"account": account.email, "updated": event_json(&occurrence, &names)}))
        }))
    }

    pub(super) async fn delete_event<'a>(&'a self, input: &'a Value) -> Result<Plan<'a>, String> {
        let account = self.account_or_default(input)?;
        if let Some(answer) = self.unavailable(&account, Missing::Calendar) {
            return Ok(Plan::without_asking(async move { Ok(answer) }));
        }
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
        Ok(Plan::ask(question, async move {
            let calendar = Arc::clone(&self.modules.calendar);
            let (account_id, gone) = (account.id, id.clone());
            self.permitted(&account, Permission::Calendar, async move {
                calendar.delete(account_id, &gone).await
            })
            .await?;
            Ok(json!({"account": account.email, "deleted": id}))
        }))
    }

    pub(super) async fn answer_invitation<'a>(
        &'a self,
        input: &'a Value,
    ) -> Result<Plan<'a>, String> {
        let (account, sync) = self.sync_for(&required(input, "account")?)?;
        let message_id = required(input, "message_id")?;
        let key = required(input, "answer")?.to_lowercase();
        let answer = Answer::ALL
            .into_iter()
            .find(|a| a.as_str() == key)
            .ok_or_else(|| format!("Unknown answer {key}; use yes, no or maybe."))?;
        // The question names the event, so the body is read before asking.
        // Answering reads it again from the cache.
        let id = message_id.clone();
        let body = self.call(async move { sync.body(&id).await }).await?;
        let invitation = body
            .calendar
            .as_deref()
            .and_then(mailrs_domain::invitation::read)
            .ok_or("That message holds no invitation.")?;
        let question = fill(
            &gettext("Answer {answer} to “{title}”?"),
            &[("answer", &answer.label()), ("title", &invitation.summary)],
        );
        Ok(Plan::ask(
            question,
            self.answer(account, message_id, answer),
        ))
    }

    /// Sends the answer the person allowed, and says where it went.
    async fn answer(&self, account: Account, message_id: String, answer: Answer) -> ToolResult {
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
            "answer": answer.as_str(),
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
        Moment::Day(day) => day
            .format_localized(&gettext("%a %-d %b"), date_locale())
            .to_string(),
        Moment::At(at) => crate::format::local(at)
            .map(|t| {
                t.format_localized(&gettext("%a %-d %b %H:%M"), date_locale())
                    .to_string()
            })
            .unwrap_or_default(),
    }
}
