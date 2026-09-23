//! The events on each account's primary Google calendar, for the
//! assistant: what is on, when the user is free, and the events it makes,
//! moves and deletes.
//!
//! Every call needs the calendar permission, which sign-in leaves out.
//! Without it each one answers `Permitted::NeedsPermission`, as the
//! settings calls do, and the caller asks the user for it. A Google Cloud
//! project with the Calendar API switched off answers
//! `GmailError::ApiDisabled` inside `SyncError::Backend` instead, since no
//! permission would help there.

use std::sync::Arc;

use chrono::{DateTime, NaiveDate};
use mailrs_domain::{AccountId, EpochMillis};
use mailrs_gmail::{Event, EventFields, EventTime};

use crate::{AccountSync, Accounts, BackendError, Permitted, SyncError};

pub struct Calendar<A: Accounts> {
    accounts: Arc<A>,
}

impl<A: Accounts> Calendar<A> {
    pub fn new(accounts: Arc<A>) -> Self {
        Calendar { accounts }
    }

    /// Every event that overlaps `from` to `to`, in the order they start.
    pub async fn events(
        &self,
        account_id: AccountId,
        from: EpochMillis,
        to: EpochMillis,
    ) -> Result<Permitted<Vec<Event>>, SyncError> {
        let sync = self.sync(account_id)?;
        permitted(sync.events_between(from, to).await)
    }

    /// The stretches of at least `length` inside `windows` that no busy
    /// event touches, earliest first. The windows are the hours worth
    /// offering, such as the working day on each of several days, so a
    /// free night never comes back as a slot. One call to Google covers
    /// them all.
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
        let events = match self.events(account_id, from, to).await? {
            Permitted::Done(events) => events,
            Permitted::NeedsPermission => return Ok(Permitted::NeedsPermission),
        };
        let busy: Vec<(EpochMillis, EpochMillis)> = events
            .iter()
            .filter(|event| event.busy)
            .filter_map(span)
            .collect();
        Ok(Permitted::Done(free_slots(&busy, windows, length)))
    }

    /// Puts a new event on the calendar and invites its guests.
    pub async fn create(
        &self,
        account_id: AccountId,
        fields: &EventFields,
    ) -> Result<Permitted<Event>, SyncError> {
        let sync = self.sync(account_id)?;
        permitted(sync.create_event(fields).await)
    }

    /// Changes what `fields` sets on event `id` and tells its guests.
    pub async fn update(
        &self,
        account_id: AccountId,
        id: &str,
        fields: &EventFields,
    ) -> Result<Permitted<Event>, SyncError> {
        let sync = self.sync(account_id)?;
        permitted(sync.update_event(id, fields).await)
    }

    /// Takes event `id` off the calendar and tells its guests.
    pub async fn delete(
        &self,
        account_id: AccountId,
        id: &str,
    ) -> Result<Permitted<()>, SyncError> {
        let sync = self.sync(account_id)?;
        permitted(sync.delete_event(id).await)
    }

    fn sync(&self, account_id: AccountId) -> Result<Arc<AccountSync>, SyncError> {
        self.accounts
            .account(account_id)
            .ok_or(SyncError::UnknownAccount(account_id))
    }
}

/// A calendar answer with the missing permission turned into a value the
/// caller matches on.
fn permitted<T>(answer: Result<T, SyncError>) -> Result<Permitted<T>, SyncError> {
    match answer {
        Ok(value) => Ok(Permitted::Done(value)),
        Err(SyncError::Backend(BackendError::NeedsPermission)) => Ok(Permitted::NeedsPermission),
        Err(err) => Err(err),
    }
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
