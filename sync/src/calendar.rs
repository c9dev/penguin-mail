//! What is on an account's calendars, when the user is free, and the
//! events the assistant makes, moves and deletes, for the assistant and
//! for anything else that reads a calendar without a window of its own.
//!
//! Once the local copy has read an account's primary calendar at least
//! once (`mailrs_store::calendar::synced`), every read here comes from
//! it: no network call, no quota spent, every calendar named.
//! A write goes into the copy's queue and [`crate::calendar_copy::CalendarCopy::send`]
//! is asked to send it right away rather than waiting for the next timer
//! tick, without making the caller wait on the network round
//! trip. Before the first read, or for a provider the copy cannot yet
//! reach, every call goes straight to the provider, as it always has.
//!
//! Every call needs the calendar permission, which sign-in leaves out.
//! Without it each one answers `Permitted::NeedsPermission`, as the
//! settings calls do, and the caller asks the user for it. A Google Cloud
//! project with the Calendar API switched off answers
//! `BackendError::ApiDisabled` inside `SyncError::Backend` instead, since no
//! permission would help there. An account whose provider has no calendar
//! answers `BackendError::Unsupported`.

use std::sync::Arc;

use chrono::{DateTime, NaiveDate};
use mailrs_domain::calendar as model;
use mailrs_domain::calendar::series::{RepeatScope, Step};
use mailrs_domain::invitation::Answer;
use mailrs_domain::{AccountId, EpochMillis};
use mailrs_gmail::{Event, EventFields, EventTime};
use mailrs_store::Db;
use mailrs_store::calendar as store;

use crate::calendar_copy::{CalendarCopy, new_event_id};
use crate::{Accounts, AnyCalendar, BackendError, CalendarService, Permitted, SyncError};

/// Why no calendar answers to a name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoPick {
    /// No calendar on the account has that name or id.
    Unknown,
    /// Only a calendar the account cannot write to has it. This carries
    /// the name as the calendar spells it.
    ReadOnly(String),
    /// More than one calendar the account can write to has that name.
    Several,
}

/// The calendar the account can write to that `wanted` names: its id, or
/// its name in any case, with spaces around it ignored. An id wins over a
/// name, so two calendars that share a name can still be told apart.
pub fn writable_named<'a>(
    calendars: &'a [model::Calendar],
    wanted: &str,
) -> Result<&'a model::Calendar, NoPick> {
    let wanted = wanted.trim();
    if let Some(calendar) = calendars.iter().find(|c| c.id == wanted) {
        return match calendar.access.can_write() {
            true => Ok(calendar),
            false => Err(NoPick::ReadOnly(calendar.name.clone())),
        };
    }
    let lower = wanted.to_lowercase();
    let named: Vec<&model::Calendar> = calendars
        .iter()
        .filter(|c| c.name.trim().to_lowercase() == lower)
        .collect();
    let writable: Vec<&model::Calendar> =
        named.iter().copied().filter(|c| c.access.can_write()).collect();
    match (writable.as_slice(), named.first()) {
        ([one], _) => Ok(*one),
        ([], Some(read_only)) => Err(NoPick::ReadOnly(read_only.name.clone())),
        ([], None) => Err(NoPick::Unknown),
        _ => Err(NoPick::Several),
    }
}

pub struct Calendar<A: Accounts> {
    accounts: Arc<A>,
    db: Db,
    copy: Arc<CalendarCopy<A>>,
}

impl<A: Accounts> Calendar<A> {
    pub fn new(accounts: Arc<A>, db: Db, copy: Arc<CalendarCopy<A>>) -> Self {
        Calendar { accounts, db, copy }
    }

    /// Every event that overlaps `from` to `to`, in the order they start,
    /// on every calendar the account keeps. The caller names each one, so
    /// only the clash line and free time narrow it down.
    pub async fn events(
        &self,
        account_id: AccountId,
        from: EpochMillis,
        to: EpochMillis,
    ) -> Result<Permitted<Vec<model::Occurrence>>, SyncError> {
        self.occurrences(account_id, from, to, store::CalendarScope::All).await
    }

    /// The stretches of at least `length` inside `windows` that no busy
    /// event on a calendar the account owns touches, earliest first. A
    /// calendar the account only reads or is only told free/busy about
    /// never counts: it is not this account's time to give away.
    pub async fn free(
        &self,
        account_id: AccountId,
        windows: &[(EpochMillis, EpochMillis)],
        length: EpochMillis,
    ) -> Result<Permitted<Vec<(EpochMillis, EpochMillis)>>, SyncError> {
        let (Some(from), Some(to)) = (
            windows.iter().map(|w| w.0).min(),
            windows.iter().map(|w| w.1).max(),
        ) else {
            return Ok(Permitted::Done(Vec::new()));
        };
        let occurrences = match self.occurrences(account_id, from, to, store::CalendarScope::Owned).await? {
            Permitted::Done(occurrences) => occurrences,
            Permitted::NeedsPermission => return Ok(Permitted::NeedsPermission),
        };
        let busy: Vec<(EpochMillis, EpochMillis)> =
            occurrences.iter().filter(|o| o.event.blocks_time()).map(|o| (o.start, o.end)).collect();
        Ok(Permitted::Done(free_slots(&busy, windows, length)))
    }

    /// Every calendar on the account: from the copy once it has been
    /// read, else from the provider, which answers `NeedsPermission`
    /// until the account grants the calendar list.
    pub async fn calendars(
        &self,
        account_id: AccountId,
    ) -> Result<Permitted<Vec<model::Calendar>>, SyncError> {
        let held = self
            .db
            .read(move |c| {
                Ok(match store::synced(c, account_id)? {
                    true => Some(store::calendars(c, account_id)?),
                    false => None,
                })
            })
            .await?;
        match held {
            Some(list) => Ok(Permitted::Done(list)),
            None => permitted(self.calendar(account_id)?.calendars().await),
        }
    }

    /// Puts a new event on `calendar`, or the primary calendar when it
    /// names none, and invites its guests. Once the copy is reading the
    /// account, the event waits in the queue and comes back `pending`;
    /// `send` is kicked off at once so it does not sit there until the
    /// next tick.
    pub async fn create(
        &self,
        account_id: AccountId,
        calendar: Option<&str>,
        fields: &EventFields,
    ) -> Result<Permitted<model::Event>, SyncError> {
        let service = self.calendar(account_id)?;
        // A calendar other than the primary exists only in the copy, so a
        // copy never read is read now; the event must not fall back to
        // the primary because the copy was late.
        if calendar.is_some() && !self.synced(account_id).await? {
            if let Permitted::NeedsPermission = self.copy.refresh(account_id, crate::now_millis()).await? {
                return Ok(Permitted::NeedsPermission);
            }
            if !self.synced(account_id).await? {
                return Err(SyncError::NoCalendar(calendar.unwrap_or_default().to_string()));
            }
        }
        if self.synced(account_id).await? {
            let mut event = self.new_event(account_id, calendar, fields).await?;
            self.copy.save(account_id, event.clone()).await?;
            self.send_soon(account_id);
            // `save` marks its own copy of `event` pending; this one is
            // what the caller sees, so it carries the same word.
            event.pending = true;
            return Ok(Permitted::Done(event));
        }
        match service.create_event(fields).await {
            Ok(event) => Ok(Permitted::Done(live_event(&event))),
            Err(BackendError::NeedsPermission) => Ok(Permitted::NeedsPermission),
            Err(err) => Err(err.into()),
        }
    }

    /// Changes what `fields` sets on event `id` and tells its guests.
    /// When the copy holds a row for `id` the change goes into the
    /// queue, the same way `create` does. An occurrence id
    /// (`<series>_<start>`) names one occurrence of a series in the copy:
    /// the change queues as a changed occurrence, for that occurrence
    /// alone, as the window's "This event only" does. Any other id, such
    /// as one the live path gave before the copy's first read, goes
    /// straight to the provider.
    pub async fn update(
        &self,
        account_id: AccountId,
        id: &str,
        fields: &EventFields,
    ) -> Result<Permitted<model::Event>, SyncError> {
        let calendar = self.calendar(account_id)?;
        if self.synced(account_id).await? {
            let found = {
                let id = id.to_string();
                self.db.read(move |c| store::find_event(c, account_id, &id)).await?
            };
            if let Some(mut event) = found {
                apply_fields(&mut event, fields);
                self.copy.save(account_id, event.clone()).await?;
                self.send_soon(account_id);
                event.pending = true;
                return Ok(Permitted::Done(event));
            }
            if let Some(occurrence) = self.occurrence(account_id, id).await? {
                let mut edited = model::Event {
                    start: occurrence.start,
                    end: occurrence.end,
                    ..model::Event::clone(&occurrence.event)
                };
                apply_fields(&mut edited, fields);
                let steps = self
                    .copy
                    .change_steps(account_id, &occurrence, edited, Some(RepeatScope::This))
                    .await?;
                let saved = steps.iter().find_map(|step| match step {
                    Step::Save(event) => Some(model::Event { pending: true, ..event.clone() }),
                    _ => None,
                });
                if let Permitted::NeedsPermission = self.copy.apply(account_id, steps).await? {
                    return Ok(Permitted::NeedsPermission);
                }
                self.send_soon(account_id);
                return Ok(Permitted::Done(saved.expect("one occurrence changes in one save")));
            }
        }
        match calendar.update_event(id, fields).await {
            Ok(event) => Ok(Permitted::Done(live_event(&event))),
            Err(BackendError::NeedsPermission) => Ok(Permitted::NeedsPermission),
            Err(err) => Err(err.into()),
        }
    }

    /// Takes event `id` off the calendar and tells its guests, through
    /// the queue when the copy holds it or it names one occurrence of a
    /// series, which the queue cancels alone; straight to the provider for
    /// an id the copy does not know (see [`Self::update`]).
    pub async fn delete(
        &self,
        account_id: AccountId,
        id: &str,
    ) -> Result<Permitted<()>, SyncError> {
        let calendar = self.calendar(account_id)?;
        if self.synced(account_id).await? {
            let found = {
                let id = id.to_string();
                self.db.read(move |c| store::find_event(c, account_id, &id)).await?
            };
            if let Some(event) = found {
                self.copy.remove(account_id, &event.calendar, id).await?;
                self.send_soon(account_id);
                return Ok(Permitted::Done(()));
            }
            if let Some(occurrence) = self.occurrence(account_id, id).await? {
                let steps = self.copy.delete_steps(account_id, &occurrence, Some(RepeatScope::This)).await?;
                if let Permitted::NeedsPermission = self.copy.apply(account_id, steps).await? {
                    return Ok(Permitted::NeedsPermission);
                }
                self.send_soon(account_id);
                return Ok(Permitted::Done(()));
            }
        }
        permitted(calendar.delete_event(id).await)
    }

    /// Occurrences over `from` to `to`, from the copy once it has read
    /// the account's primary calendar, live otherwise. The live branch
    /// only ever sees that one calendar, so `scope` makes no difference
    /// to it.
    async fn occurrences(
        &self,
        account_id: AccountId,
        from: EpochMillis,
        to: EpochMillis,
        scope: store::CalendarScope,
    ) -> Result<Permitted<Vec<model::Occurrence>>, SyncError> {
        let calendar = self.calendar(account_id)?;
        if self.synced(account_id).await? {
            let occurrences = self.db.read(move |c| store::occurrences(c, &[account_id], from, to, scope)).await?;
            return Ok(Permitted::Done(occurrences));
        }
        match calendar.events_between(from, to).await {
            Ok(events) => Ok(Permitted::Done(events.iter().map(|e| live_occurrence(account_id, e)).collect())),
            Err(BackendError::NeedsPermission) => Ok(Permitted::NeedsPermission),
            Err(err) => Err(err.into()),
        }
    }

    /// The occurrence an occurrence id names (`<series>_<start>`, as
    /// [`model::Occurrence::id`] writes it), with the series as its event.
    /// `None` when `id` is not in that form, names no series in the copy,
    /// or names a start the series never reaches.
    async fn occurrence(&self, account_id: AccountId, id: &str) -> Result<Option<model::Occurrence>, SyncError> {
        let Some((series_id, start)) = model::split_occurrence_id(id) else {
            return Ok(None);
        };
        let series = {
            let series_id = series_id.to_string();
            self.db.read(move |c| store::find_event(c, account_id, &series_id)).await?
        };
        let Some(series) = series.filter(|s| !s.rules.is_empty()) else {
            return Ok(None);
        };
        if !model::expand(&series, start, start + 1).iter().any(|&(at, _)| at == start) {
            return Ok(None);
        }
        let end = start + (series.end - series.start);
        Ok(Some(model::Occurrence { account_id, event: Arc::new(series), start, end }))
    }

    async fn synced(&self, account_id: AccountId) -> Result<bool, SyncError> {
        Ok(self.db.read(move |c| store::synced(c, account_id)).await?)
    }

    /// The neutral event a fresh `create` writes: on `calendar`, or the
    /// primary calendar when it names none, in that calendar's zone,
    /// under a new id, always busy, since an event the assistant makes is
    /// never a placeholder.
    async fn new_event(
        &self,
        account_id: AccountId,
        calendar: Option<&str>,
        fields: &EventFields,
    ) -> Result<model::Event, SyncError> {
        let target = self.target(account_id, calendar).await?;
        Ok(model::Event {
            calendar: target.id,
            id: new_event_id(),
            zone: target.zone,
            start: fields.start.as_ref().and_then(instant).unwrap_or_default(),
            end: fields.end.as_ref().and_then(instant).unwrap_or_default(),
            all_day: matches!(fields.start, Some(EventTime::Day(_))),
            title: fields.summary.clone().unwrap_or_default(),
            place: fields.location.clone().unwrap_or_default(),
            description: fields.description.clone().unwrap_or_default(),
            guests: guest_list(fields),
            busy: true,
            ..model::Event::default()
        })
    }

    /// The calendar a new event goes on: the one `calendar` names by id,
    /// or, when it names none, the one Google lists as `primary == true`
    /// (a real account's primary calendar is named by its address, not
    /// `primary`). Either must take events from the account; a calendar
    /// the account cannot write to, or an id naming none, is
    /// `SyncError::NoCalendar`. The no-primary fallback only guards a
    /// caller that races `synced` against a calendar list still being
    /// written.
    async fn target(&self, account_id: AccountId, calendar: Option<&str>) -> Result<model::Calendar, SyncError> {
        let calendars = self.db.read(move |c| store::calendars(c, account_id)).await?;
        let found = match calendar {
            Some(id) => calendars.into_iter().find(|c| c.id == id),
            None => Some(calendars.into_iter().find(|c| c.primary).unwrap_or_else(|| model::Calendar {
                id: "primary".to_string(),
                primary: true,
                access: model::Access::Owner,
                ..model::Calendar::default()
            })),
        };
        match found {
            Some(calendar) if calendar.access.can_write() => Ok(calendar),
            _ => Err(SyncError::NoCalendar(calendar.unwrap_or("primary").to_string())),
        }
    }

    /// Sends the account's queue right away rather than leaving a change
    /// made here to wait for the next tick. Spawned rather
    /// than awaited, so the assistant's own answer does not wait on the
    /// network round trip; a failed send just leaves the change queued
    /// for the next tick, as any other network failure does.
    fn send_soon(&self, account_id: AccountId) {
        let copy = Arc::clone(&self.copy);
        tokio::spawn(async move {
            if let Err(err) = copy.send(account_id).await {
                tracing::warn!(account = account_id, %err, "could not send a calendar change made here");
            }
        });
    }

    fn calendar(&self, account_id: AccountId) -> Result<AnyCalendar, SyncError> {
        self.accounts
            .services(account_id)
            .ok_or(SyncError::UnknownAccount(account_id))?
            .calendar
            .ok_or(SyncError::Backend(BackendError::Unsupported))
    }
}

/// A calendar answer with the missing permission turned into a value the
/// caller matches on.
fn permitted<T>(answer: Result<T, BackendError>) -> Result<Permitted<T>, SyncError> {
    match answer {
        Ok(value) => Ok(Permitted::Done(value)),
        Err(BackendError::NeedsPermission) => Ok(Permitted::NeedsPermission),
        Err(err) => Err(err.into()),
    }
}

/// The guests `EventFields` gives a fresh event, each with no answer of
/// their own yet.
fn guest_list(fields: &EventFields) -> Vec<model::Guest> {
    fields
        .guests
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(|email| model::Guest { email, ..model::Guest::default() })
        .collect()
}

/// Writes what `fields` sets onto `event`, leaving the rest as it was, the
/// way a Google patch would. A new start with no new end keeps the
/// event's length, since a person moving a meeting means to move all of
/// it.
fn apply_fields(event: &mut model::Event, fields: &EventFields) {
    if let Some(summary) = &fields.summary {
        event.title = summary.clone();
    }
    if let Some(start) = &fields.start {
        event.all_day = matches!(start, EventTime::Day(_));
        if let Some(at) = instant(start) {
            let length = event.end - event.start;
            event.start = at;
            event.end = at + length;
        }
    }
    if let Some(end) = &fields.end
        && let Some(at) = instant(end)
    {
        event.end = at;
    }
    if let Some(location) = &fields.location {
        event.place = location.clone();
    }
    if let Some(description) = &fields.description {
        event.description = description.clone();
    }
    if let Some(guests) = &fields.guests {
        event.guests = guests.iter().map(|email| model::Guest { email: email.clone(), ..model::Guest::default() }).collect();
    }
}

/// Google's word for a guest's answer, read into the neutral model.
/// `needsAction` and anything unrecognised are "not yet answered".
fn guest_answer(word: &str) -> Option<Answer> {
    match word {
        "accepted" => Some(Answer::Yes),
        "declined" => Some(Answer::No),
        "tentative" => Some(Answer::Maybe),
        _ => None,
    }
}

/// A live Google event, read into the neutral model the copy would give
/// it: on the account's `"primary"` calendar, since that is the only one
/// the live calls ever reach.
fn live_event(event: &Event) -> model::Event {
    let (start, end) = span(event).unwrap_or_default();
    let guests: Vec<model::Guest> = event
        .guests
        .iter()
        .map(|g| model::Guest {
            email: g.email.clone(),
            name: g.name.clone(),
            answer: guest_answer(&g.answer),
            organizer: event.organizer.as_deref() == Some(g.email.as_str()),
            me: g.me,
        })
        .collect();
    let my_answer = event.guests.iter().find(|g| g.me).and_then(|g| guest_answer(&g.answer));
    model::Event {
        calendar: "primary".to_string(),
        id: event.id.clone(),
        uid: event.uid.clone(),
        start,
        end,
        all_day: matches!(event.start, Some(EventTime::Day(_))),
        title: event.summary.clone(),
        place: event.location.clone(),
        description: event.description.clone(),
        busy: event.busy,
        status: if event.cancelled { model::Status::Cancelled } else { model::Status::Confirmed },
        organizer: event.organizer.clone(),
        guests,
        my_answer,
        ..model::Event::default()
    }
}

fn live_occurrence(account_id: AccountId, event: &Event) -> model::Occurrence {
    let event = live_event(event);
    let (start, end) = (event.start, event.end);
    model::Occurrence { account_id, event: Arc::new(event), start, end }
}

/// An instant as the Calendar API writes one, in UTC.
pub fn at(instant: EpochMillis) -> Option<EventTime> {
    DateTime::from_timestamp_millis(instant).map(|at| EventTime::At(at.to_rfc3339()))
}

/// When an event time falls. A whole day counts from midnight UTC, which
/// is close enough for the one use this has outside a test: all-day
/// events never make the user busy.
pub fn instant(time: &EventTime) -> Option<EpochMillis> {
    match time {
        EventTime::At(at) => DateTime::parse_from_rfc3339(at)
            .ok()
            .map(|at| at.timestamp_millis()),
        EventTime::Day(day) => NaiveDate::parse_from_str(day, "%Y-%m-%d")
            .ok()?
            .and_hms_opt(0, 0, 0)
            .map(|midnight| midnight.and_utc().timestamp_millis()),
    }
}

/// When an event starts and ends. An event with no end of its own takes
/// no time, as Google treats one.
pub fn span(event: &Event) -> Option<(EpochMillis, EpochMillis)> {
    let start = instant(event.start.as_ref()?)?;
    let end = event.end.as_ref().and_then(instant).unwrap_or(start);
    Some((start, end.max(start)))
}

/// The gaps of at least `length` inside each window that no busy span
/// overlaps, earliest first. Busy spans may overlap one another and run
/// past a window's edges.
pub fn free_slots(
    busy: &[(EpochMillis, EpochMillis)],
    windows: &[(EpochMillis, EpochMillis)],
    length: EpochMillis,
) -> Vec<(EpochMillis, EpochMillis)> {
    let mut busy = busy.to_vec();
    busy.sort();
    let mut windows = windows.to_vec();
    windows.sort();
    let mut slots = Vec::new();
    for (from, to) in windows {
        let mut cursor = from;
        for &(starts, ends) in &busy {
            if ends <= cursor || starts >= to {
                continue;
            }
            if starts - cursor >= length {
                slots.push((cursor, starts));
            }
            cursor = cursor.max(ends);
        }
        if to - cursor >= length {
            slots.push((cursor, to));
        }
    }
    slots
}
