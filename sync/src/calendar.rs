//! What is on an account's calendars, when the user is free, and the
//! events the assistant makes, moves and deletes, for the assistant and
//! for anything else that reads a calendar without a window of its own.
//!
//! Every read comes from the local copy: no network call, no quota
//! spent, every calendar named. A read before the copy has read the
//! account once waits for that first read
//! ([`crate::calendar_copy::CalendarCopy::ready`]). A write goes into the
//! copy's queue and [`crate::calendar_copy::CalendarCopy::send`] is asked
//! to send it right away rather than waiting for the next timer tick,
//! without making the caller wait on the network round trip.
//!
//! Every call needs the calendar permission. Without it each one answers
//! `Permitted::NeedsPermission`, as the settings calls do, and the caller
//! asks the user for it. A Google Cloud project with the Calendar API
//! switched off answers `BackendError::ApiDisabled` inside
//! `SyncError::Backend` instead, since no permission would help there. An
//! account whose provider has no calendar answers
//! `BackendError::Unsupported`.

use std::sync::Arc;

use mailrs_domain::calendar as model;
use mailrs_domain::calendar::EventEdit;
use mailrs_domain::calendar::series::{RepeatScope, Step};
use mailrs_domain::{AccountId, EpochMillis};
use mailrs_store::Db;
use mailrs_store::calendar as store;

use crate::calendar_copy::event_change::{self, Changed, Choice, Edit, EventChange, Question, Undo};
use crate::calendar_copy::{CalendarCopy, new_event_id};
use crate::{Accounts, AnyCalendar, BackendError, CalendarService, Permitted, Spot, SyncError};
use mailrs_domain::invitation::Invitation;

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

/// Where the events of a calendar file went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Added {
    /// The name of the calendar they went on, for the card's line. Empty
    /// when the account gave no calendar list to name it from.
    pub calendar: String,
    /// Where each event sits, in the order the file listed them, for
    /// Show in Calendar.
    pub spots: Vec<Spot>,
    /// Events the file lists that could not go: no UID to match on, or no
    /// start time.
    pub skipped: usize,
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

    /// Every calendar on the account, as the copy lists them. Without the
    /// calendar list permission the copy holds the primary calendar alone,
    /// which would read as every calendar the account has, so the answer
    /// is `NeedsPermission` instead.
    pub async fn calendars(
        &self,
        account_id: AccountId,
    ) -> Result<Permitted<Vec<model::Calendar>>, SyncError> {
        let withheld = self
            .accounts
            .services(account_id)
            .ok_or(SyncError::UnknownAccount(account_id))?
            .withheld();
        if withheld.calendar_list {
            return Ok(Permitted::NeedsPermission);
        }
        if let Permitted::NeedsPermission = self.ready(account_id).await? {
            return Ok(Permitted::NeedsPermission);
        }
        let list = self.db.read(move |c| store::calendars(c, account_id)).await?;
        Ok(Permitted::Done(list))
    }

    /// Adds the events of a calendar file to `calendar`, or the primary
    /// calendar when it names none. Straight to the provider, not through
    /// the queue: the person pressed Add to Calendar and the card says
    /// where the event went, so a failure has to reach them now. The
    /// provider matches each event on its UID, so the same file added
    /// again updates what the first time made. The events go into the
    /// copy too, so Show in Calendar works at once.
    ///
    /// A calendar the account cannot write to, or one it does not have,
    /// is `SyncError::NoCalendar`.
    pub async fn import(
        &self,
        account_id: AccountId,
        calendar: Option<&str>,
        events: &[Invitation],
    ) -> Result<Permitted<Added>, SyncError> {
        let service = self.calendar(account_id)?;
        if let Permitted::NeedsPermission = self.ready(account_id).await? {
            return Ok(Permitted::NeedsPermission);
        }
        let target = self.target(account_id, calendar).await?;
        let mut saved = Vec::new();
        let mut skipped = 0;
        for invitation in events {
            let Some(event) = invitation.to_event(&target.id, &target.zone) else {
                skipped += 1;
                continue;
            };
            match service.import_event(&event).await {
                Ok(made) => saved.push(made),
                Err(BackendError::NeedsPermission) => return Ok(Permitted::NeedsPermission),
                Err(err) => return Err(err.into()),
            }
        }
        let (rows, now) = (saved.clone(), crate::now_millis());
        self.db.write(move |c| store::save_events(c, account_id, &rows, now)).await?;
        let spots = saved
            .iter()
            .map(|event| Spot {
                account_id,
                calendar: event.calendar.clone(),
                id: event.id.clone(),
                start: event.start,
            })
            .collect();
        Ok(Permitted::Done(Added { calendar: target.name, spots, skipped }))
    }

    /// Puts a new event made of `edit` on `calendar`, or the primary
    /// calendar when it names none, and invites its guests. The event
    /// waits in the copy's queue and comes back `pending`; `send` is kicked
    /// off at once so it does not sit there until the next tick.
    pub async fn create(
        &self,
        account_id: AccountId,
        calendar: Option<&str>,
        edit: &EventEdit,
    ) -> Result<Permitted<model::Event>, SyncError> {
        if let Permitted::NeedsPermission = self.ready(account_id).await? {
            return Ok(Permitted::NeedsPermission);
        }
        let event = self.new_event(account_id, calendar, edit).await?;
        let change = EventChange::New(event.clone());
        let choice = Choice { scope: None, notify: model::Notify::Guests };
        if let Permitted::NeedsPermission = self.copy.change(account_id, change, choice, Undo::Skip).await? {
            return Ok(Permitted::NeedsPermission);
        }
        self.copy.send_soon(account_id);
        Ok(Permitted::Done(model::Event { pending: true, ..event }))
    }

    /// Changes what `edit` sets on event `id`, through the copy's queue,
    /// the same way `create` does. The guests hear of it when the window
    /// would tell them ([`Self::question`]). An occurrence id
    /// (`<series>_<start>`) names one occurrence of a series in the copy:
    /// the change queues as a changed occurrence, for that occurrence
    /// alone, as the window's "This event only" does. An id the copy does
    /// not hold is `BackendError::NotFound`.
    pub async fn update(
        &self,
        account_id: AccountId,
        id: &str,
        edit: &EventEdit,
    ) -> Result<Permitted<model::Event>, SyncError> {
        if let Permitted::NeedsPermission = self.ready(account_id).await? {
            return Ok(Permitted::NeedsPermission);
        }
        let Permitted::Done(changed) = self.write(account_id, id, Some(edit)).await? else {
            return Ok(Permitted::NeedsPermission);
        };
        let Changed::Queued(steps) = changed else {
            return Err(SyncError::Backend(BackendError::NotFound));
        };
        steps
            .into_iter()
            .find_map(|step| match step {
                Step::Save(event) => Some(Permitted::Done(model::Event { pending: true, ..event })),
                _ => None,
            })
            .ok_or(SyncError::Backend(BackendError::NotFound))
    }

    /// Takes event `id` off the calendar through the queue, with a
    /// cancellation to its guests when the window would send one. An
    /// occurrence id names one occurrence of a series, which the queue
    /// cancels alone (see [`Self::update`]).
    pub async fn delete(&self, account_id: AccountId, id: &str) -> Result<Permitted<()>, SyncError> {
        if let Permitted::NeedsPermission = self.ready(account_id).await? {
            return Ok(Permitted::NeedsPermission);
        }
        match self.write(account_id, id, None).await? {
            Permitted::Done(_) => Ok(Permitted::Done(())),
            Permitted::NeedsPermission => Ok(Permitted::NeedsPermission),
        }
    }

    /// What the window would ask before changing event `id` by `edit`, or
    /// deleting it when `edit` is `None`, for the assistant to say in its
    /// confirmation who hears of it. Read from the copy as it stands, with
    /// no wait for a first read.
    pub async fn question(
        &self,
        account_id: AccountId,
        id: &str,
        edit: Option<&EventEdit>,
    ) -> Result<Question, SyncError> {
        let (change, _) = self.change_of(account_id, id, edit).await?;
        Ok(event_change::confirmation(&change, self.always_mails(account_id)?))
    }

    /// Writes the assistant's change of event `id` and starts a send. The
    /// person agreed to the confirmation [`Self::question`] words, so the
    /// guests hear of it when that says they do.
    async fn write(
        &self,
        account_id: AccountId,
        id: &str,
        edit: Option<&EventEdit>,
    ) -> Result<Permitted<Changed>, SyncError> {
        let (change, scope) = self.change_of(account_id, id, edit).await?;
        let notify = match event_change::confirmation(&change, self.always_mails(account_id)?).guests_hear() {
            true => model::Notify::Guests,
            false => model::Notify::Nobody,
        };
        let written = self.copy.change(account_id, change, Choice { scope, notify }, Undo::Skip).await?;
        if let Permitted::Done(_) = written {
            self.copy.send_soon(account_id);
        }
        Ok(written)
    }

    /// The change `edit` makes to event `id`, or its removal when `edit` is
    /// `None`, with the repeat scope it covers: the event as it is for an
    /// id the copy holds, or that occurrence alone for an occurrence id.
    async fn change_of(
        &self,
        account_id: AccountId,
        id: &str,
        edit: Option<&EventEdit>,
    ) -> Result<(EventChange, Option<RepeatScope>), SyncError> {
        let found = {
            let id = id.to_string();
            self.db.read(move |c| store::find_event(c, account_id, &id)).await?
        };
        let (occurrence, scope) = match found {
            Some(event) => {
                let (start, end) = (event.start, event.end);
                (model::Occurrence { account_id, event: Arc::new(event), start, end }, None)
            }
            None => match self.occurrence(account_id, id).await? {
                Some(occurrence) => (occurrence, Some(RepeatScope::This)),
                None => return Err(SyncError::Backend(BackendError::NotFound)),
            },
        };
        made_here(&occurrence.event)?;
        let Some(edit) = edit else {
            return Ok((EventChange::Remove(occurrence), scope));
        };
        let before = model::Event {
            start: occurrence.start,
            end: occurrence.end,
            ..model::Event::clone(&occurrence.event)
        };
        let mut edited = before.clone();
        edit.apply(&mut edited);
        let how = seen_by_guests(&before, &edited);
        Ok((EventChange::Edit { occurrence, edited, how }, scope))
    }

    /// Whether the account mails the guests of every change, as Graph
    /// does.
    fn always_mails(&self, account_id: AccountId) -> Result<bool, SyncError> {
        let services = self.accounts.services(account_id).ok_or(SyncError::UnknownAccount(account_id))?;
        Ok(!services.offers().quiet_changes)
    }

    /// Occurrences over `from` to `to` on the calendars `scope` names,
    /// from the copy.
    async fn occurrences(
        &self,
        account_id: AccountId,
        from: EpochMillis,
        to: EpochMillis,
        scope: store::CalendarScope,
    ) -> Result<Permitted<Vec<model::Occurrence>>, SyncError> {
        if let Permitted::NeedsPermission = self.ready(account_id).await? {
            return Ok(Permitted::NeedsPermission);
        }
        let occurrences = self.db.read(move |c| store::occurrences(c, &[account_id], from, to, scope)).await?;
        Ok(Permitted::Done(occurrences))
    }

    /// Waits for the copy's first read of the account when it has none
    /// yet. An account whose provider has no calendar is
    /// `BackendError::Unsupported`.
    async fn ready(&self, account_id: AccountId) -> Result<Permitted<()>, SyncError> {
        self.calendar(account_id)?;
        self.copy.ready(account_id, crate::now_millis()).await
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

    /// The neutral event a fresh `create` writes: on `calendar`, or the
    /// primary calendar when it names none, in that calendar's zone,
    /// under a new id, always busy, since an event the assistant makes is
    /// never a placeholder.
    async fn new_event(
        &self,
        account_id: AccountId,
        calendar: Option<&str>,
        edit: &EventEdit,
    ) -> Result<model::Event, SyncError> {
        let target = self.target(account_id, calendar).await?;
        let mut event = model::Event {
            calendar: target.id,
            id: new_event_id(),
            zone: target.zone,
            busy: true,
            ..model::Event::default()
        };
        edit.apply(&mut event);
        Ok(event)
    }

    /// The calendar a new event goes on: the one `calendar` names by id,
    /// or, when it names none, the one the provider lists as primary (a
    /// real Google account's primary calendar is named by its address, not
    /// `primary`). Either must take events from the account; a calendar
    /// the account cannot write to, or an id naming none, is
    /// `SyncError::NoCalendar`. The no-primary fallback serves a copy that
    /// could not read the calendar list, whose provider still takes
    /// `primary` as the primary calendar's name.
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

    fn calendar(&self, account_id: AccountId) -> Result<AnyCalendar, SyncError> {
        self.accounts
            .services(account_id)
            .ok_or(SyncError::UnknownAccount(account_id))?
            .calendar
            .ok_or(SyncError::Backend(BackendError::Unsupported))
    }
}

/// What an assistant's edit from `before` to `after` changes as the
/// guests see it. Every field a tool sets is one the guests see, so a
/// field counts only when its value differs.
fn seen_by_guests(before: &model::Event, after: &model::Event) -> Edit {
    let moves = (before.start, before.end, before.all_day) != (after.start, after.end, after.all_day);
    let guests = event_change::adds_guests(&before.guests, &after.guests)
        || event_change::adds_guests(&after.guests, &before.guests);
    let seen = moves
        || guests
        || before.title != after.title
        || before.place != after.place
        || before.description != after.description;
    Edit { moves, seen, ..Edit::default() }
}

/// Refuses a change to a birthday or a working location, which only
/// Google's own apps make and change, before it reaches the queue. Google
/// would turn most such changes down, and a birthday's Undo would then
/// have nothing to undo.
fn made_here(event: &model::Event) -> Result<(), SyncError> {
    if event.kind.made_elsewhere() {
        return Err(SyncError::MadeInGoogle(event.title.clone()));
    }
    Ok(())
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
