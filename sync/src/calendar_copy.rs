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

use mailrs_domain::calendar::{Access, Calendar};
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
