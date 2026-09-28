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
//! still gets its primary calendar, addressed by the
//! account's own address, which needs no list permission; sign-in asks
//! for the list scope too, so a shared or subscribed calendar joins it. An
//! account without `calendar.events` costs one list call and one read,
//! then nothing until half an hour passes or the person signs it in again.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use mailrs_domain::calendar::series::{self, Picked, RepeatScope, Step};
use mailrs_domain::calendar::{Access, Calendar, Event, Notify, Occurrence};
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
/// apart from the glossary's Clash, which avoids "conflict".
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
    /// When each account was last found to lack `calendar.events` itself.
    refused: Mutex<HashMap<AccountId, EpochMillis>>,
    /// Held for the length of one `refresh_due` pass, so a tick that is
    /// still reading a large calendar is never joined by a second one
    /// reading it again and sending the same change twice. `send` waits on it instead of
    /// walking past it, since a person's own edit should still go out.
    running: tokio::sync::Mutex<()>,
    /// The rows of held changes, which a read leaves alone as it leaves
    /// queued ones. One entry per step, so two changes touching one row
    /// each release only their own.
    held: Mutex<Vec<HeldKey>>,
    /// The one held change whose Undo toast is up. Holding another commits
    /// it first. Locked for the whole of `hold`, `commit` and `revert`, so
    /// two of them never interleave their writes.
    waiting: tokio::sync::Mutex<Option<Held>>,
    /// Numbers each held change, so a commit or revert of one that was
    /// already committed can tell.
    serial: AtomicU64,
}

/// A change written to the copy and not queued yet, while its Undo toast
/// is up. Undo reverts it; the toast closing commits it. Only one waits at
/// a time: holding another commits it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Held {
    pub account_id: AccountId,
    pub steps: Vec<Step>,
    /// Each row the steps touched, as it was, with a removed series'
    /// changed occurrences.
    before: Vec<Event>,
    /// Whether the provider mails the guests about every step.
    notify: Notify,
    serial: u64,
}

impl Held {
    fn keys(&self) -> Vec<HeldKey> {
        held_keys(self.account_id, &self.steps)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HeldKey {
    account_id: AccountId,
    calendar: String,
    id: String,
    /// The step takes the event off, so a read keeps its changed
    /// occurrences out too.
    removal: bool,
}

fn held_keys(account_id: AccountId, steps: &[Step]) -> Vec<HeldKey> {
    steps
        .iter()
        .map(|step| {
            let (calendar, id) = step.key();
            HeldKey { account_id, calendar, id, removal: matches!(step, Step::Remove { .. }) }
        })
        .collect()
}

/// Releases a held change's keys when dropped, whether its write worked,
/// failed, or its caller went away part way.
struct Release<'a> {
    held: &'a Mutex<Vec<HeldKey>>,
    keys: Vec<HeldKey>,
}

impl Release<'_> {
    fn new(held: &Mutex<Vec<HeldKey>>, keys: Vec<HeldKey>) -> Release<'_> {
        held.lock().expect("copy poisoned").extend(keys.iter().cloned());
        Release { held, keys }
    }

    /// Keeps the keys held after this guard goes, for a change that stays
    /// waiting on its toast.
    fn keep(mut self) {
        self.keys.clear();
    }
}

impl Drop for Release<'_> {
    fn drop(&mut self) {
        let mut held = self.held.lock().expect("copy poisoned");
        for key in &self.keys {
            if let Some(at) = held.iter().position(|k| k == key) {
                held.swap_remove(at);
            }
        }
    }
}

impl<A: Accounts> CalendarCopy<A> {
    pub fn new(accounts: Arc<A>, db: Db) -> Self {
        CalendarCopy {
            accounts,
            db,
            last_list: Mutex::new(HashMap::new()),
            last_read: Mutex::new(HashMap::new()),
            refused: Mutex::new(HashMap::new()),
            running: tokio::sync::Mutex::new(()),
            held: Mutex::new(Vec::new()),
            waiting: tokio::sync::Mutex::new(None),
            serial: AtomicU64::new(0),
        }
    }

    /// Forgets what the account was refused and when it was last read,
    /// so the next tick reads it at once. The app calls this once the
    /// person signs the account in again, which is how a permission is
    /// granted.
    pub fn permission_changed(&self, account_id: AccountId) {
        self.refused.lock().expect("copy poisoned").remove(&account_id);
        self.last_list.lock().expect("copy poisoned").remove(&account_id);
        self.last_read.lock().expect("copy poisoned").remove(&account_id);
    }

    /// Whether the account turned out to lack the calendar permission less
    /// than [`LIST_EVERY`] ago, so asking again would only spend a call on
    /// a refusal.
    fn refused_lately(&self, account_id: AccountId, now: EpochMillis) -> bool {
        self.refused
            .lock()
            .expect("copy poisoned")
            .get(&account_id)
            .is_some_and(|at| now - at < LIST_EVERY)
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
            // An account still starting, signed out or just removed has
            // nothing to read with, and the app ticks every 15 seconds.
            if self.accounts.services(account_id).is_none() {
                continue;
            }
            // Queued changes go out before the account is read, so an
            // edit made here shows up in what comes back rather than
            // waiting for the read after it. Calls
            // the version that assumes the run lock is already held,
            // since `send`'s own lock is not reentrant.
            match self.send_locked(account_id).await {
                Ok(turned_down) => total.turned_down.extend(turned_down),
                Err(err) => {
                    tracing::warn!(account = account_id, %err, "could not send the calendar queue");
                }
            }
            let last = self.last_read.lock().expect("copy poisoned").get(&account_id).copied();
            if last.is_some_and(|last| now - last < every) || self.refused_lately(account_id, now) {
                continue;
            }
            match self.refresh(account_id, now).await {
                Ok(Permitted::Done(one)) => total.events += one.events,
                Ok(Permitted::NeedsPermission) => total.needs_permission.push(account_id),
                // One account's trouble, such as one not yet running, one
                // signed out or one just removed, must not stop the
                // accounts after it from being read.
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
        // calls.
        crate::background(self.refresh_calendars(&calendar, account_id, now)).await
    }

    async fn refresh_calendars(
        &self,
        calendar: &AnyCalendar,
        account_id: AccountId,
        now: EpochMillis,
    ) -> Result<Permitted<Refreshed>, SyncError> {
        if self.refused_lately(account_id, now) {
            return Ok(Permitted::NeedsPermission);
        }
        let withheld = self
            .accounts
            .services(account_id)
            .ok_or(SyncError::UnknownAccount(account_id))?
            .withheld();
        // The person unticked the calendar scope itself: nothing here is
        // reachable, so this costs no call and touches no stored row.
        // Recording the refusal makes the next tick wait LIST_EVERY too,
        // the same cadence a refusal Google itself answered gets below.
        if withheld.calendar {
            self.refused.lock().expect("copy poisoned").insert(account_id, now);
            return Ok(Permitted::NeedsPermission);
        }
        let list_due = self
            .last_list
            .lock()
            .expect("copy poisoned")
            .get(&account_id)
            .is_none_or(|last| now - last >= LIST_EVERY);
        if list_due {
            // Recorded before the call, so a refusal still waits
            // LIST_EVERY instead of asking again on the very next tick.
            self.last_list.lock().expect("copy poisoned").insert(account_id, now);
            if withheld.calendar_list {
                // The list scope alone is missing: the primary calendar
                // needs no list call, so skip straight to it.
                let address = self.address(account_id).await?;
                let fallback = vec![primary_fallback(&address)];
                self.db.write(move |c| store::save_calendars(c, account_id, &fallback)).await?;
            } else {
                match calendar.calendars().await {
                    Ok(list) => {
                        self.db.write(move |c| store::save_calendars(c, account_id, &list)).await?;
                    }
                    Err(BackendError::NeedsPermission) => {
                        let address = self.address(account_id).await?;
                        let fallback = vec![primary_fallback(&address)];
                        self.db
                            .write(move |c| store::save_calendars(c, account_id, &fallback))
                            .await?;
                    }
                    Err(err) => return Err(err.into()),
                }
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
            match self.read_calendar(calendar, account_id, &entry.id, now).await {
                Ok(Permitted::Done(count)) => refreshed.events += count,
                // One calendar's trouble must not leave the calendars after
                // it unread. A calendar removed elsewhere answers 404 until
                // the list is read again, so that is read on the next tick.
                Err(SyncError::Backend(BackendError::NotFound)) => {
                    tracing::info!(account = account_id, calendar = entry.id, "a calendar is gone; reading the list again");
                    self.last_list.lock().expect("copy poisoned").remove(&account_id);
                }
                // The network or the rate limit would stop every other
                // calendar the same way.
                Err(SyncError::Backend(err)) if err.is_transient() => return Err(err.into()),
                Err(err) if !matches!(err, SyncError::Store(_)) => {
                    tracing::warn!(account = account_id, calendar = entry.id, %err, "could not read a calendar");
                }
                Err(err) => return Err(err),
                Ok(Permitted::NeedsPermission) => {
                    // Google cannot say which scope a refusal is for, so
                    // the list call alone could not tell a missing list
                    // scope from a missing calendar.events. This read
                    // can: the account has no calendar to read, and the
                    // fallback calendar would only ask again every minute.
                    self.refused.lock().expect("copy poisoned").insert(account_id, now);
                    self.db.write(move |c| store::save_calendars(c, account_id, &[])).await?;
                    return Ok(Permitted::NeedsPermission);
                }
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
                    // lost its place; elsewhere a 404 means an event is
                    // gone.
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
                // Taken before the write, since the lock must not be held
                // across an await.
                let (held, held_removals) = self.held_in(account_id, id);
                self.db
                    .write(move |c| {
                        // An event with a change of ours still queued or
                        // held keeps our version until the queue sends it.
                        let mut pending = store::pending_ids(c, account_id, &calendar_id)?;
                        pending.extend(held);
                        // A series on its way out takes its changed
                        // occurrences with it; a read must not bring them
                        // back to stand alone.
                        let mut removing = store::removing_ids(c, account_id, &calendar_id)?;
                        removing.extend(held_removals);
                        events.retain(|e| {
                            !pending.contains(&e.id) && !e.series.as_ref().is_some_and(|s| removing.contains(s))
                        });
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
    /// `Create` only when it carries no stored etag, is no occurrence of a
    /// series, and nothing is already queued for it, so a brand-new event edited twice before a
    /// send is created once and changed the second time, never created
    /// twice. An empty etag alone would not say that: an event already
    /// queued as a create has none either.
    pub async fn save(&self, account_id: AccountId, mut event: Event) -> Result<(), SyncError> {
        event.pending = true;
        let now = crate::now_millis();
        self.db
            .write(move |c| {
                let already_queued = store::pending_ids(c, account_id, &event.calendar)?.contains(&event.id);
                // An occurrence has no etag before its first change, and
                // its id, which holds `_`, is not one a create may use.
                let kind = if event.etag.is_empty() && event.series.is_none() && !already_queued {
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

    /// Sends the account's queue in order, waiting behind a refresh
    /// already under way, since a person's own edit should still go out.
    /// A change the provider turns down leaves the queue and comes back as
    /// a `TurnedDown`, with the provider's version in the copy. A failure
    /// that may pass, such as the network going, or one about the whole
    /// account stops the send and leaves that change and the rest queued
    /// for next time.
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
        let mut turned_down = Vec::new();
        let mut after = 0;
        while let Some(change) = self.db.read(move |c| store::next_change(c, account_id, after)).await? {
            let seq = change.seq;
            after = seq;
            let create = change.kind == store::ChangeKind::Create;
            let answer = match change.kind {
                store::ChangeKind::Remove => calendar
                    .remove_event(&change.calendar, &change.event, change.etag.as_deref(), change.notify)
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
                    match calendar.put_event(&body, change.etag.as_deref(), create, change.notify).await {
                        // The id is one this computer made, so a 409 means an
                        // earlier send of this create reached Google and its
                        // answer was lost. An edit made since sits in the
                        // body, so it goes out as a change.
                        Err(BackendError::Changed) if create => {
                            calendar.put_event(&body, None, false, change.notify).await
                        }
                        other => other,
                    }
                    .map(Some)
                }
            };
            let turned_before = turned_down.len();
            match (change.kind, answer) {
                (_, Ok(Some(mut sent))) => {
                    sent.pending = false;
                    let attempted = change.body.clone().expect("a create or save always has a body");
                    let new_etag = sent.etag.clone();
                    let waiting = self
                        .db
                        .write(move |c| {
                            // An edit or a delete made while this change
                            // was in flight wins over the answer.
                            if store::finish_change(c, account_id, seq, &attempted, &new_etag)? {
                                store::save_events(c, account_id, &[sent], crate::now_millis())?;
                            }
                            store::first_waiting_on(c, seq)
                        })
                        .await?;
                    // A change waiting on this one that the send already
                    // walked past, because it sat earlier in the queue,
                    // goes out now.
                    if let Some(first) = waiting {
                        after = after.min(first - 1);
                    }
                }
                // Gone already or not, the removal is done.
                (_, Ok(None)) | (store::ChangeKind::Remove, Err(BackendError::NotFound)) => {
                    let waiting = self
                        .db
                        .write(move |c| {
                            store::finish_removal(c, account_id, seq)?;
                            store::first_waiting_on(c, seq)
                        })
                        .await?;
                    if let Some(first) = waiting {
                        after = after.min(first - 1);
                    }
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
            // A change turned down can put back an earlier edit that a
            // step waiting on it had folded into, in a row this send has
            // already walked past, or queue the old series' rules. Walk
            // the queue again so they go out now. A row behind `after`
            // has left, still waits, or holds an edit made while it was in
            // flight, which may go out again now.
            if turned_down.len() > turned_before {
                after = 0;
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
                drop_waiting(c, account_id, seq)?;
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
        let restores = change.restores.clone();
        self.db
            .write(move |c| {
                store::dequeue(c, seq)?;
                for calendar in drop_waiting(c, account_id, seq)? {
                    store::set_token(c, account_id, &calendar, None, 0)?;
                }
                store::remove_events(c, account_id, &cal, std::slice::from_ref(&id))?;
                // Google took the cut and turned the new series down, so
                // the old series ends where the new one should have
                // begun. Its rules go back, against the version the cut
                // left, and the dates after the cut come back.
                if let Some(old) = restores {
                    let now = store::event(c, account_id, &old.calendar, &old.id)?.unwrap_or_else(|| old.clone());
                    let whole = Event { rules: old.rules, pending: true, ..now };
                    store::save_events(c, account_id, std::slice::from_ref(&whole), crate::now_millis())?;
                    store::enqueue(c, account_id, store::ChangeKind::Save, &whole)?;
                }
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

    /// Writes `steps` to the copy, marked waiting, and queues nothing. A
    /// change held before and still waiting is committed first, since only
    /// one Undo toast shows at a time. An account with no calendar answers
    /// `Unsupported`, and one whose calendar permission is withheld
    /// `NeedsPermission`, so the queue never takes a change it cannot send.
    /// The guests hear of it; [`Self::hold_with`] lets the person say no.
    pub async fn hold(&self, account_id: AccountId, steps: Vec<Step>) -> Result<Permitted<Held>, SyncError> {
        self.hold_with(account_id, steps, Notify::Guests).await
    }

    /// [`Self::hold`], with the person's choice of whether the provider
    /// mails the guests. The choice stays with the change through Undo, a
    /// restart and the queue.
    pub async fn hold_with(
        &self,
        account_id: AccountId,
        steps: Vec<Step>,
        notify: Notify,
    ) -> Result<Permitted<Held>, SyncError> {
        if self.calendar(account_id)?.is_none() {
            return Err(SyncError::Backend(BackendError::Unsupported));
        }
        if self.withheld(account_id)? {
            return Ok(Permitted::NeedsPermission);
        }
        let mut waiting = self.waiting.lock().await;
        if let Some(prior) = waiting.take() {
            self.queue_held(prior).await?;
        }
        let keys = held_keys(account_id, &steps);
        let release = Release::new(&self.held, keys);
        let mut touched: Vec<(String, String)> = Vec::new();
        for step in &steps {
            let key = step.key();
            if !touched.contains(&key) {
                touched.push(key);
            }
        }
        let removals: HashSet<(String, String)> = steps
            .iter()
            .filter(|s| matches!(s, Step::Remove { .. }))
            .map(Step::key)
            .collect();
        let (keys, writes) = (touched, steps.clone());
        let now = crate::now_millis();
        let before = self
            .db
            .write(move |c| {
                let mut before = Vec::new();
                for key in &keys {
                    let (calendar, id) = key;
                    before.extend(store::event(c, account_id, calendar, id)?);
                    // Removing a series takes its changed occurrences off
                    // the copy too, so Undo must have them to put back.
                    if removals.contains(key) {
                        before.extend(store::changed_occurrences(c, account_id, calendar, id)?);
                    }
                }
                for step in &writes {
                    match step {
                        Step::Save(event) | Step::Cancel(event) => {
                            let event = Event { pending: true, ..event.clone() };
                            store::save_events(c, account_id, std::slice::from_ref(&event), now)?;
                        }
                        Step::Remove { calendar, id } => {
                            store::remove_events(c, account_id, calendar, std::slice::from_ref(id))?;
                        }
                    }
                }
                // Persisted in the same transaction as the rows above, so
                // a crash or a quit before the Undo toast closes still
                // has this change to queue at the next start.
                store::save_holding(c, account_id, &writes, &before, notify)?;
                Ok(before)
            })
            .await?;
        release.keep();
        let serial = self.serial.fetch_add(1, Ordering::Relaxed);
        let held = Held { account_id, steps, before, notify, serial };
        *waiting = Some(held.clone());
        Ok(Permitted::Done(held))
    }

    /// Queues what `held` wrote, for the next send. A change already
    /// committed, because another was held after it, stays as it is.
    pub async fn commit(&self, held: Held) -> Result<(), SyncError> {
        let mut waiting = self.waiting.lock().await;
        let Some(held) = waiting.take_if(|w| w.serial == held.serial) else {
            return Ok(());
        };
        self.queue_held(held).await
    }

    /// Puts back every row `held` touched, as it was, and queues nothing.
    /// A change already committed, because another was held after it,
    /// stays committed.
    pub async fn revert(&self, held: Held) -> Result<(), SyncError> {
        let mut waiting = self.waiting.lock().await;
        let Some(held) = waiting.take_if(|w| w.serial == held.serial) else {
            return Ok(());
        };
        let _release = Release { held: &self.held, keys: held.keys() };
        let Held { account_id, steps, before, .. } = held;
        let now = crate::now_millis();
        self.db
            .write(move |c| {
                // A row the change made, such as a new series or a first
                // changed occurrence, has no earlier version to put back.
                // `remove_events` would also take a series' changed
                // occurrences, which a save leaves alone, so only a row
                // missing from `before` goes this way.
                for step in &steps {
                    let (calendar, id) = step.key();
                    if !before.iter().any(|e| e.calendar == calendar && e.id == id) {
                        store::remove_events(c, account_id, &calendar, std::slice::from_ref(&id))?;
                    }
                }
                store::save_events(c, account_id, &before, now)?;
                store::clear_holding(c, account_id)?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    /// Whether `held` is still the one change waiting on its Undo toast.
    /// A hold, commit or revert already under way answers `true`, so as
    /// not to say a change already gone before its own write lands. The
    /// view polls this while a toast is up: an assistant edit made
    /// through [`apply`](Self::apply) commits the change waiting before
    /// it, same as holding a new one does, and the view then closes its
    /// toast instead of leaving an Undo that would do nothing.
    pub fn still_waiting(&self, held: &Held) -> bool {
        self.waiting.try_lock().map(|w| w.as_ref().is_some_and(|w| w.serial == held.serial)).unwrap_or(true)
    }

    /// Writes `steps` and queues them at once, for a change with no Undo
    /// toast, such as one the assistant makes. Like `hold`, it commits a
    /// change still waiting on its toast first.
    pub async fn apply(&self, account_id: AccountId, steps: Vec<Step>) -> Result<Permitted<()>, SyncError> {
        self.apply_with(account_id, steps, Notify::Guests).await
    }

    /// [`Self::apply`], with the person's choice of whether the provider
    /// mails the guests.
    pub async fn apply_with(
        &self,
        account_id: AccountId,
        steps: Vec<Step>,
        notify: Notify,
    ) -> Result<Permitted<()>, SyncError> {
        match self.hold_with(account_id, steps, notify).await? {
            Permitted::Done(held) => self.commit(held).await.map(Permitted::Done),
            Permitted::NeedsPermission => Ok(Permitted::NeedsPermission),
        }
    }

    /// Commits the change still waiting on its Undo toast, if any: for the
    /// window closing or the app quitting.
    pub async fn commit_waiting(&self) -> Result<(), SyncError> {
        let mut waiting = self.waiting.lock().await;
        match waiting.take() {
            Some(held) => self.queue_held(held).await,
            None => Ok(()),
        }
    }

    /// Queues every change a hold left waiting when this run last
    /// stopped: no Undo toast survived a crash or a quit to close over
    /// it, so there is nothing left to revert, only to send. Call once at
    /// start, before the first read or send touches an account.
    pub async fn recover_holds(&self) -> Result<(), SyncError> {
        let found = self.db.read(store::holdings).await?;
        for (account_id, steps, before, notify) in found {
            if steps.is_empty() {
                continue;
            }
            let serial = self.serial.fetch_add(1, Ordering::Relaxed);
            self.queue_held(Held { account_id, steps, before, notify, serial }).await?;
        }
        Ok(())
    }

    /// Queues a held change's steps and releases its rows to reads. A new
    /// event goes out as a create; an occurrence, which has no etag before
    /// its first change, as a change of that occurrence. Each step waits
    /// on the one before it, so a split's new series goes out only once
    /// Google took the cut, and its removals only once Google took the new
    /// series; a step turned down drops the steps after it. The new series
    /// carries the old one as it was, to put back if Google takes the cut
    /// and turns the new series down.
    async fn queue_held(&self, held: Held) -> Result<(), SyncError> {
        let _release = Release { held: &self.held, keys: held.keys() };
        let Held { account_id, steps, before, notify, .. } = held;
        let cut_from = cut_series(&steps, &before).cloned();
        self.db
            .write(move |c| {
                let mut lead = None;
                for step in &steps {
                    let seq = match step {
                        Step::Save(event) => {
                            let queued = store::pending_ids(c, account_id, &event.calendar)?.contains(&event.id);
                            let kind = if event.etag.is_empty() && event.series.is_none() && !queued {
                                store::ChangeKind::Create
                            } else {
                                store::ChangeKind::Save
                            };
                            let event = Event { pending: true, ..event.clone() };
                            let restores = cut_from.as_ref().filter(|_| kind == store::ChangeKind::Create);
                            store::enqueue_after(c, account_id, kind, &event, lead, restores, notify)?
                        }
                        // The cancelled row stays in the copy; the provider
                        // gets a delete of that occurrence.
                        Step::Cancel(event) => {
                            store::enqueue_after(c, account_id, store::ChangeKind::Remove, event, lead, None, notify)?
                        }
                        Step::Remove { calendar, id } => {
                            let prior = before
                                .iter()
                                .find(|e| &e.calendar == calendar && &e.id == id)
                                .cloned()
                                .unwrap_or_else(|| Event {
                                    calendar: calendar.clone(),
                                    id: id.clone(),
                                    ..Event::default()
                                });
                            store::enqueue_after(c, account_id, store::ChangeKind::Remove, &prior, lead, None, notify)?
                        }
                    };
                    lead = seq.or(lead);
                }
                store::clear_holding(c, account_id)?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    /// The writes that make `edited` true of `occurrence`, and of the
    /// others in its series that `scope` covers. A one-off event, or no
    /// scope, saves the event as it is.
    pub async fn change_steps(
        &self,
        account_id: AccountId,
        occurrence: &Occurrence,
        edited: Event,
        scope: Option<RepeatScope>,
    ) -> Result<Vec<Step>, SyncError> {
        let Some(scope) = scope.filter(|_| series::in_series(&occurrence.event)) else {
            return Ok(vec![Step::Save(edited)]);
        };
        let Some((whole, changed)) = self.series_of(account_id, &occurrence.event).await? else {
            return Ok(vec![Step::Save(edited)]);
        };
        Ok(series::change(&whole, &changed, picked(occurrence), edited, scope, &new_event_id()))
    }

    /// The writes that delete `occurrence`, and the others `scope` covers.
    pub async fn delete_steps(
        &self,
        account_id: AccountId,
        occurrence: &Occurrence,
        scope: Option<RepeatScope>,
    ) -> Result<Vec<Step>, SyncError> {
        let alone = || {
            vec![Step::Remove { calendar: occurrence.event.calendar.clone(), id: occurrence.event.id.clone() }]
        };
        let Some(scope) = scope.filter(|_| series::in_series(&occurrence.event)) else {
            return Ok(alone());
        };
        let Some((whole, changed)) = self.series_of(account_id, &occurrence.event).await? else {
            return Ok(alone());
        };
        Ok(series::delete(&whole, &changed, picked(occurrence), scope))
    }

    /// The series `event` belongs to, and its changed occurrences.
    async fn series_of(&self, account_id: AccountId, event: &Event) -> Result<Option<(Event, Vec<Event>)>, SyncError> {
        let calendar = event.calendar.clone();
        let id = event.series.clone().unwrap_or_else(|| event.id.clone());
        Ok(self
            .db
            .read(move |c| {
                Ok(match store::event(c, account_id, &calendar, &id)? {
                    Some(whole) => Some((whole, store::changed_occurrences(c, account_id, &calendar, &id)?)),
                    None => None,
                })
            })
            .await?)
    }

    /// The ids on one calendar that held changes write, and those they
    /// remove.
    fn held_in(&self, account_id: AccountId, calendar: &str) -> (Vec<String>, Vec<String>) {
        let held = self.held.lock().expect("copy poisoned");
        let mine = held.iter().filter(|k| k.account_id == account_id && k.calendar == calendar);
        let ids = mine.clone().map(|k| k.id.clone()).collect();
        let removals = mine.filter(|k| k.removal).map(|k| k.id.clone()).collect();
        (ids, removals)
    }

    fn withheld(&self, account_id: AccountId) -> Result<bool, SyncError> {
        Ok(self
            .accounts
            .services(account_id)
            .ok_or(SyncError::UnknownAccount(account_id))?
            .withheld()
            .calendar)
    }

    fn calendar(&self, account_id: AccountId) -> Result<Option<AnyCalendar>, SyncError> {
        Ok(self
            .accounts
            .services(account_id)
            .ok_or(SyncError::UnknownAccount(account_id))?
            .calendar)
    }

    /// The account's own address, which names its primary calendar when
    /// the list scope is missing.
    async fn address(&self, account_id: AccountId) -> Result<String, SyncError> {
        Ok(self
            .db
            .read(move |c| mailrs_store::accounts::account(c, account_id))
            .await?
            .ok_or(SyncError::UnknownAccount(account_id))?
            .email)
    }
}

/// The occurrence a person picked: a changed occurrence knows its place
/// in the series, and a plain one starts where the series put it.
fn picked(occurrence: &Occurrence) -> Picked {
    Picked { original_start: occurrence.event.original_start.unwrap_or(occurrence.start), start: occurrence.start }
}

/// Drops the changes waiting on the row `seq`, which the provider turned
/// down, and takes their events off the copy: a split's new series never
/// reached the provider, and the next whole read of each calendar
/// answered brings back what the provider still holds. An earlier edit a
/// dropped step had folded into stays queued and shows again. Answers
/// the calendars touched.
fn drop_waiting(c: &rusqlite::Connection, account_id: AccountId, seq: i64) -> mailrs_store::Result<HashSet<String>> {
    let mut calendars = HashSet::new();
    for dropped in store::drop_waiting_on(c, seq)? {
        match dropped {
            store::Dropped::Gone { calendar, event } => {
                store::remove_events(c, account_id, &calendar, std::slice::from_ref(&event))?;
                calendars.insert(calendar);
            }
            store::Dropped::Kept { body: Some(body), .. } => {
                let body = Event { pending: true, ..*body };
                store::save_events(c, account_id, std::slice::from_ref(&body), crate::now_millis())?;
            }
            store::Dropped::Kept { body: None, .. } => {}
        }
    }
    Ok(calendars)
}

/// The series as it was before `steps` cut it short, when they split it:
/// the first step saves an event `before` holds with other rules.
fn cut_series<'a>(steps: &[Step], before: &'a [Event]) -> Option<&'a Event> {
    let Some(Step::Save(cut)) = steps.first() else {
        return None;
    };
    before
        .iter()
        .find(|e| e.calendar == cut.calendar && e.id == cut.id)
        .filter(|old| !old.rules.is_empty() && old.rules != cut.rules)
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
/// the account's own address, and that address needs no list permission.
/// Only the list can say the calendar's zone, so it stays empty, and a new
/// event on it names no zone and takes the calendar's own on Google.
fn primary_fallback(address: &str) -> Calendar {
    Calendar {
        id: address.to_string(),
        name: address.to_string(),
        color: String::new(),
        access: Access::Owner,
        zone: String::new(),
        primary: true,
        shown: true,
        reminders: Vec::new(),
    }
}
