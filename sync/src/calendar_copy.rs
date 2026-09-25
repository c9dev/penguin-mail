//! The calendar's local copy: every calendar of every account whose
//! provider offers one, read into the store and kept fresh, so the
//! calendar view, the assistant and the invitation card read events
//! without asking the provider.
//!
//! The app calls [`CalendarCopy::refresh_due`] on a timer. Each account is
//! read every minute while the window is open and every five while only
//! the tray runs; its calendar list every half hour, and a calendar the
//! person hid at the slow, five-minute rate even while the window is
//! open. A calendar is read whole once, from a year back, and after that
//! only what changed since the provider's sync token. An expired token
//! reads it whole again, which replaces what the store held for it.
//!
//! An account that granted `calendar.events` but not the list scope
//! (ruling R2) still gets its primary calendar, addressed by the
//! account's own address, which needs no list permission; stage 2 asks
//! for the list scope so a shared or subscribed calendar joins it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use mailrs_domain::calendar::{Access, Calendar, Event};
use mailrs_domain::{AccountId, EpochMillis};
use mailrs_store::Db;
use mailrs_store::calendar as store;

use crate::settings::Permitted;
use crate::{Accounts, AnyCalendar, BackendError, CalendarService, SyncError};

pub const READ_EVERY_OPEN: EpochMillis = 60_000;
pub const READ_EVERY_TRAY: EpochMillis = 5 * 60_000;
pub const LIST_EVERY: EpochMillis = 30 * 60_000;
pub const FIRST_READ_BACK: EpochMillis = 365 * 24 * 60 * 60_000;

/// Pages one calendar read walks before it stops. A calendar past this
/// keeps what was read and no token, so the next read walks it again.
const PAGE_LIMIT: usize = 200;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Refreshed {
    /// Events stored or removed.
    pub events: usize,
    /// Accounts whose calendars the provider would not hand over until
    /// the person grants the permission.
    pub needs_permission: Vec<AccountId>,
    /// Changes the provider turned down while the queue went out ahead of
    /// this read (`send`, called once per account before it is read).
    pub turned_down: Vec<TurnedDown>,
}

/// A change made here that the provider turned down. The copy now holds
/// the provider's version, so the window can say what happened. Named
/// apart from the glossary's Clash, which avoids "conflict" (ruling R6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnedDown {
    pub account_id: AccountId,
    pub calendar: String,
    pub event: String,
    pub title: String,
    /// `None` when the event changed elsewhere first; otherwise the
    /// provider's reason, such as the calendar turning read-only.
    pub reason: Option<String>,
}

/// An id for an event made on this computer. Google takes a client's own
/// id when it is 5 to 1024 characters of base 32 hex, which lets the copy
/// store the event under the id it will keep.
pub fn new_event_id() -> String {
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuv";
    let mut id = String::from("pm");
    for _ in 0..30 {
        id.push(DIGITS[rand::random_range(0..DIGITS.len())] as char);
    }
    id
}

pub struct CalendarCopy<A: Accounts> {
    accounts: Arc<A>,
    db: Db,
    last_list: Mutex<HashMap<AccountId, EpochMillis>>,
    last_read: Mutex<HashMap<AccountId, EpochMillis>>,
    /// Held for the length of one `refresh_due` pass, so a tick that is
    /// still reading a large calendar is never joined by a second one
    /// reading and, once Task 6 lands, sending the same change twice
    /// (reconcile.md Task 5 item 8). `send` waits on it instead of
    /// walking past it, since a person's own edit should still go out.
    running: tokio::sync::Mutex<()>,
}

impl<A: Accounts> CalendarCopy<A> {
    pub fn new(accounts: Arc<A>, db: Db) -> Self {
        CalendarCopy {
            accounts,
            db,
            last_list: Mutex::new(HashMap::new()),
            last_read: Mutex::new(HashMap::new()),
            running: tokio::sync::Mutex::new(()),
        }
    }

    /// Refreshes each account whose last read is older than the cadence
    /// for the window being open or not. Skips the whole pass, rather
    /// than any one account, while a pass from an earlier tick is still
    /// running.
    pub async fn refresh_due(
        &self,
        accounts: &[AccountId],
        now: EpochMillis,
        window_open: bool,
    ) -> Result<Refreshed, SyncError> {
        let Ok(_run) = self.running.try_lock() else {
            return Ok(Refreshed::default());
        };
        let every = if window_open { READ_EVERY_OPEN } else { READ_EVERY_TRAY };
        let mut total = Refreshed::default();
        for &account_id in accounts {
            // Queued changes go out before the account is read, so an
            // edit made here shows up in what comes back rather than
            // waiting for the read after it (Task 6 Interfaces). Calls
            // the version that assumes the run lock is already held,
            // since `send`'s own lock is not reentrant.
            match self.send_locked(account_id).await {
                Ok(turned_down) => total.turned_down.extend(turned_down),
                Err(err) => {
                    tracing::warn!(account = account_id, %err, "could not send the calendar queue");
                }
            }
            let last = self.last_read.lock().expect("copy poisoned").get(&account_id).copied();
            if last.is_some_and(|last| now - last < every) {
                continue;
            }
            match self.refresh(account_id, now).await {
                Ok(Permitted::Done(one)) => total.events += one.events,
                Ok(Permitted::NeedsPermission) => total.needs_permission.push(account_id),
                // One account's trouble, such as one not yet running, one
                // signed out or one just removed, must not stop the
                // accounts after it from being read (reconcile.md Task 5
                // item 3).
                Err(err) => {
                    tracing::warn!(account = account_id, %err, "could not refresh the calendar copy");
                }
            }
        }
        Ok(total)
    }

    /// Reads the account's calendar list when it is due, then the changes
    /// to each of its calendars. An account whose provider offers no
    /// calendar answers `Done` with nothing read, quietly: IMAP today, and
    /// any other provider without one tomorrow (the provider-neutrality
    /// rule).
    pub async fn refresh(&self, account_id: AccountId, now: EpochMillis) -> Result<Permitted<Refreshed>, SyncError> {
        let Some(calendar) = self.calendar(account_id)? else {
            return Ok(Permitted::Done(Refreshed::default()));
        };
        // Nobody waits on a refresh; it runs behind the person's own
        // calls (reconcile.md Task 5 item 7).
        crate::background(self.refresh_calendars(&calendar, account_id, now)).await
    }

    async fn refresh_calendars(
        &self,
        calendar: &AnyCalendar,
        account_id: AccountId,
        now: EpochMillis,
    ) -> Result<Permitted<Refreshed>, SyncError> {
        let list_due = self
            .last_list
            .lock()
            .expect("copy poisoned")
            .get(&account_id)
            .is_none_or(|last| now - last >= LIST_EVERY);
        if list_due {
            // Recorded before the call, so a refusal still waits
            // LIST_EVERY instead of asking again on the very next tick
            // (reconcile.md Task 5 item 4).
            self.last_list.lock().expect("copy poisoned").insert(account_id, now);
            match calendar.calendars().await {
                Ok(list) => {
                    self.db.write(move |c| store::save_calendars(c, account_id, &list)).await?;
                }
                Err(BackendError::NeedsPermission) => {
                    let address = self.address(account_id).await?;
                    let fallback = vec![primary_fallback(&address)];
                    self.db.write(move |c| store::save_calendars(c, account_id, &fallback)).await?;
                }
                Err(err) => return Err(err.into()),
            }
        }
        self.last_read.lock().expect("copy poisoned").insert(account_id, now);
        let calendars: Vec<Calendar> = self.db.read(move |c| store::calendars(c, account_id)).await?;
        let mut refreshed = Refreshed::default();
        for entry in calendars {
            if !entry.shown {
                let id = entry.id.clone();
                let synced_at = self.db.read(move |c| store::synced_at(c, account_id, &id)).await?;
                let stale = synced_at.is_none_or(|synced_at| now - synced_at >= READ_EVERY_TRAY);
                if !stale {
                    continue;
                }
            }
            match self.read_calendar(calendar, account_id, &entry.id, now).await? {
                Permitted::Done(count) => refreshed.events += count,
                Permitted::NeedsPermission => return Ok(Permitted::NeedsPermission),
            }
        }
        Ok(Permitted::Done(refreshed))
    }

    /// Reads one calendar's changes and stores each page as it arrives, so
    /// a read that fails part way has changed only rows the next read
    /// writes again: the sync token moves only once the last page is in.
    async fn read_calendar(
        &self,
        calendar: &AnyCalendar,
        account_id: AccountId,
        id: &str,
        now: EpochMillis,
    ) -> Result<Permitted<usize>, SyncError> {
        let held = {
            let id = id.to_string();
            self.db.read(move |c| store::token(c, account_id, &id)).await?
        };
        let mut token = held;
        let mut retried = false;
        // The moment this read started, marked on every row it writes, so
        // a whole read can tell a row no later page repeated from one
        // still current (`store::sweep`).
        let mark = now;
        'whole: loop {
            let whole = token.is_none();
            let mut page: Option<String> = None;
            let mut count = 0usize;
            for _ in 0..PAGE_LIMIT {
                let answer = calendar
                    .event_changes(id, token.as_deref(), page.as_deref(), now - FIRST_READ_BACK)
                    .await;
                let got = match answer {
                    Ok(got) => got,
                    Err(BackendError::NeedsPermission) => return Ok(Permitted::NeedsPermission),
                    // Only the call that reads changes can say the server
                    // lost its place (ruling R1); elsewhere a 404 means an
                    // event is gone.
                    Err(BackendError::StateLost) if !retried => {
                        retried = true;
                        token = None;
                        continue 'whole;
                    }
                    Err(err) => return Err(err.into()),
                };
                let last_page = got.next_page.is_none();
                count += got.events.len() + got.removed.len();
                let (mut events, mut removed, next_sync) = (got.events, got.removed, got.next_sync);
                let calendar_id = id.to_string();
                self.db
                    .write(move |c| {
                        // An event with a change of ours still queued
                        // keeps our version until the queue sends it.
                        let pending = store::pending_ids(c, account_id, &calendar_id)?;
                        events.retain(|e| !pending.contains(&e.id));
                        removed.retain(|e| !pending.contains(e));
                        store::save_events(c, account_id, &events, mark)?;
                        store::remove_events(c, account_id, &calendar_id, &removed)?;
                        if last_page {
                            if whole {
                                store::sweep(c, account_id, &calendar_id, mark)?;
                            }
                            store::set_token(c, account_id, &calendar_id, next_sync.as_deref(), now)?;
                        }
                        Ok(())
                    })
                    .await?;
                match got.next_page {
                    Some(next) => page = Some(next),
                    None => return Ok(Permitted::Done(count)),
                }
            }
            tracing::warn!(account = account_id, calendar = id, "gave up reading a calendar after {PAGE_LIMIT} pages");
            return Ok(Permitted::Done(count));
        }
    }

    /// Stores `event` as waiting and queues it for the provider. The view
    /// shows it at once; `send` mails it out. `event` queues as a
    /// `Create` only when it carries no stored etag and nothing is
    /// already queued for it, so a brand-new event edited twice before a
    /// send is created once and changed the second time, never created
    /// twice (reconcile.md Task 6 item 2: `send` used to infer a create
    /// from an empty etag instead).
    pub async fn save(&self, account_id: AccountId, mut event: Event) -> Result<(), SyncError> {
        event.pending = true;
        let now = crate::now_millis();
        self.db
            .write(move |c| {
                let already_queued = store::pending_ids(c, account_id, &event.calendar)?.contains(&event.id);
                let kind = if event.etag.is_empty() && !already_queued {
                    store::ChangeKind::Create
                } else {
                    store::ChangeKind::Save
                };
                store::save_events(c, account_id, std::slice::from_ref(&event), now)?;
                store::enqueue(c, account_id, kind, &event)
            })
            .await?;
        Ok(())
    }

    /// Takes the event off the copy and queues its removal.
    pub async fn remove(&self, account_id: AccountId, calendar: &str, id: &str) -> Result<(), SyncError> {
        let (calendar, id) = (calendar.to_string(), id.to_string());
        self.db
            .write(move |c| {
                let held = store::event(c, account_id, &calendar, &id)?.unwrap_or_else(|| Event {
                    calendar: calendar.clone(),
                    id: id.clone(),
                    ..Event::default()
                });
                store::remove_events(c, account_id, &calendar, std::slice::from_ref(&id))?;
                store::enqueue(c, account_id, store::ChangeKind::Remove, &held)
            })
            .await?;
        Ok(())
    }

    /// Changes one occurrence of a series on the provider, then reads its
    /// calendar again so the copy shows the occurrence moved. `event`
    /// carries the occurrence id. The queue holds whole events
    /// only, so this goes to the provider at once and waits for it.
    pub async fn put_occurrence(&self, account_id: AccountId, event: &Event) -> Result<Permitted<Event>, SyncError> {
        let _run = self.running.lock().await;
        let calendar = self.calendar(account_id)?.ok_or(SyncError::Backend(BackendError::Unsupported))?;
        let sent = match calendar.put_event(event, None, false).await {
            Ok(sent) => sent,
            Err(BackendError::NeedsPermission) => return Ok(Permitted::NeedsPermission),
            Err(err) => return Err(err.into()),
        };
        self.read_calendar(&calendar, account_id, &event.calendar, crate::now_millis()).await?;
        Ok(Permitted::Done(sent))
    }

    /// Cancels one occurrence of a series on the provider, then reads its
    /// calendar again so the occurrence leaves the copy. An occurrence the
    /// provider no longer has is already gone, which is what was asked.
    pub async fn remove_occurrence(
        &self,
        account_id: AccountId,
        calendar_id: &str,
        id: &str,
    ) -> Result<Permitted<()>, SyncError> {
        let _run = self.running.lock().await;
        let calendar = self.calendar(account_id)?.ok_or(SyncError::Backend(BackendError::Unsupported))?;
        match calendar.remove_event(calendar_id, id, None).await {
            Ok(()) | Err(BackendError::NotFound) => {}
            Err(BackendError::NeedsPermission) => return Ok(Permitted::NeedsPermission),
            Err(err) => return Err(err.into()),
        }
        self.read_calendar(&calendar, account_id, calendar_id, crate::now_millis()).await?;
        Ok(Permitted::Done(()))
    }

    /// Sends the account's queue in order, waiting behind a refresh
    /// already under way (Task 5 item 8), since a person's own edit
    /// should still go out. A change the provider turns down leaves the
    /// queue and comes back as a `TurnedDown`, with the provider's
    /// version in the copy. A network failure stops the send and leaves
    /// that change and the rest queued for next time.
    pub async fn send(&self, account_id: AccountId) -> Result<Vec<TurnedDown>, SyncError> {
        let _run = self.running.lock().await;
        self.send_locked(account_id).await
    }

    /// `send`'s body, for a caller that already holds `running`:
    /// `refresh_due` takes it once for the whole pass and calls this
    /// directly, since the lock is not reentrant.
    async fn send_locked(&self, account_id: AccountId) -> Result<Vec<TurnedDown>, SyncError> {
        let Some(calendar) = self.calendar(account_id)? else {
            return Ok(Vec::new());
        };
        let queue = self.db.read(move |c| store::queued(c, account_id)).await?;
        let mut turned_down = Vec::new();
        for change in queue {
            let seq = change.seq;
            let create = change.kind == store::ChangeKind::Create;
            let answer = match change.kind {
                store::ChangeKind::Remove => calendar
                    .remove_event(&change.calendar, &change.event, change.etag.as_deref())
                    .await
                    .map(|()| None),
                store::ChangeKind::Create | store::ChangeKind::Save => {
                    let Some(body) = change.body.clone() else {
                        // A row `enqueue` gives a Create or Save always
                        // carries a body; one that does not has nothing
                        // left to send.
                        self.db.write(move |c| store::dequeue(c, seq)).await?;
                        continue;
                    };
                    match calendar.put_event(&body, change.etag.as_deref(), create).await {
                        // The id is one this computer made, so a 409 means an
                        // earlier send of this create reached Google and its
                        // answer was lost. An edit made since sits in the
                        // body, so it goes out as a change.
                        Err(BackendError::Changed) if create => calendar.put_event(&body, None, false).await,
                        other => other,
                    }
                    .map(Some)
                }
            };
            match (change.kind, answer) {
                (_, Ok(Some(mut sent))) => {
                    sent.pending = false;
                    let attempted = change.body.clone().expect("a create or save always has a body");
                    let new_etag = sent.etag.clone();
                    self.db
                        .write(move |c| {
                            // A newer edit that landed while this one was
                            // in flight keeps the row queued, so its body
                            // is not lost; only an untouched row's answer
                            // is worth storing (reconcile.md Task 6 item
                            // 4).
                            if store::finish_change(c, seq, &attempted, &new_etag)? {
                                store::save_events(c, account_id, &[sent], crate::now_millis())?;
                            }
                            Ok(())
                        })
                        .await?;
                }
                (_, Ok(None)) => {
                    self.db.write(move |c| store::dequeue(c, seq)).await?;
                }
                (store::ChangeKind::Remove, Err(BackendError::NotFound)) => {
                    // Gone already; the removal is done.
                    self.db.write(move |c| store::dequeue(c, seq)).await?;
                }
                (store::ChangeKind::Save, Err(BackendError::NotFound)) => {
                    turned_down.push(self.drop_gone(&change, "deleted elsewhere").await?);
                }
                (store::ChangeKind::Create, Err(BackendError::NotFound)) => {
                    // A new event's id cannot be missing, so it is the
                    // calendar that went: its rows in the queue outlive it.
                    turned_down.push(self.drop_gone(&change, "the calendar is gone").await?);
                }
                (_, Err(BackendError::Changed)) => {
                    turned_down.push(self.take_theirs(&calendar, &change, None).await?);
                }
                (_, Err(BackendError::Refused(reason))) => {
                    turned_down.push(self.take_theirs(&calendar, &change, Some(reason)).await?);
                }
                (_, Err(err)) if holds_the_queue(&err) => return Err(err.into()),
                // Any other answer will come again for this change, so it
                // leaves the queue rather than hold every change behind it.
                (_, Err(err)) => {
                    turned_down.push(self.take_theirs(&calendar, &change, Some(err.to_string())).await?);
                }
            }
        }
        Ok(turned_down)
    }

    /// Drops a change whose event, or whose calendar, the provider no
    /// longer has, and takes the event off the copy.
    async fn drop_gone(&self, change: &store::QueuedChange, reason: &str) -> Result<TurnedDown, SyncError> {
        let account_id = change.account_id;
        let (seq, cal, id) = (change.seq, change.calendar.clone(), change.event.clone());
        self.db
            .write(move |c| {
                store::dequeue(c, seq)?;
                store::remove_events(c, account_id, &cal, std::slice::from_ref(&id))
            })
            .await?;
        Ok(TurnedDown {
            account_id,
            calendar: change.calendar.clone(),
            event: change.event.clone(),
            title: change.body.as_ref().map(|b| b.title.clone()).unwrap_or_default(),
            reason: Some(reason.to_string()),
        })
    }

    /// Drops a refused change and puts the provider's version of its
    /// event in the copy, or takes the event out when the provider has
    /// none.
    async fn take_theirs(
        &self,
        calendar: &AnyCalendar,
        change: &store::QueuedChange,
        reason: Option<String>,
    ) -> Result<TurnedDown, SyncError> {
        let account_id = change.account_id;
        let title = change.body.as_ref().map(|b| b.title.clone()).unwrap_or_default();
        // The next read carries Google's version; forget the token so it
        // reads the calendar whole and cannot miss it.
        let (seq, cal, id) = (change.seq, change.calendar.clone(), change.event.clone());
        self.db
            .write(move |c| {
                store::dequeue(c, seq)?;
                store::remove_events(c, account_id, &cal, std::slice::from_ref(&id))?;
                store::set_token(c, account_id, &cal, None, 0)
            })
            .await?;
        let now = crate::now_millis();
        self.read_calendar(calendar, account_id, &change.calendar, now).await?;
        Ok(TurnedDown {
            account_id,
            calendar: change.calendar.clone(),
            event: change.event.clone(),
            title,
            reason,
        })
    }

    fn calendar(&self, account_id: AccountId) -> Result<Option<AnyCalendar>, SyncError> {
        Ok(self
            .accounts
            .services(account_id)
            .ok_or(SyncError::UnknownAccount(account_id))?
            .calendar)
    }

    /// The account's own address, which names its primary calendar when
    /// the list scope is missing (ruling R2).
    async fn address(&self, account_id: AccountId) -> Result<String, SyncError> {
        Ok(self
            .db
            .read(move |c| mailrs_store::accounts::account(c, account_id))
            .await?
            .ok_or(SyncError::UnknownAccount(account_id))?
            .email)
    }
}

/// Whether a failed send should stop and keep the change for the next
/// one: the network or the rate limit may pass, and a sign-in, a
/// permission or an API switched off concerns the account, not the
/// change, so dropping the change would lose it for nothing.
fn holds_the_queue(err: &BackendError) -> bool {
    err.is_transient()
        || matches!(
            err,
            BackendError::NeedsReauth
                | BackendError::NeedsPermission
                | BackendError::ApiDisabled { .. }
                | BackendError::Unsupported
        )
}

/// The lone calendar an account keeps once `calendar.events` is granted
/// but the list scope is not: Google answers the primary calendar under
/// the account's own address, and that address needs no list permission
/// (ruling R2). The zone is a placeholder until the list itself can say;
/// a repeating event on this calendar keeps its own zone regardless.
fn primary_fallback(address: &str) -> Calendar {
    Calendar {
        id: address.to_string(),
        name: address.to_string(),
        color: String::new(),
        access: Access::Owner,
        zone: "UTC".into(),
        primary: true,
        shown: true,
        reminders: Vec::new(),
    }
}
