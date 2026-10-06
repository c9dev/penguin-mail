//! What the calendar view, the next-event card and the assistant read
//! from the copy: the occurrences over a range, a search, one event, and
//! each account's calendar list. Every read answers from the store as it
//! stands, with no wait for the account's first sync: the window draws
//! what the copy holds and draws again once a sync changes it. The
//! assistant waits for that first sync itself (`crate::calendar`), then
//! reads through the same calls.

use mailrs_domain::calendar::{Calendar, Event, Occurrence};
use mailrs_domain::{AccountId, EpochMillis};
use mailrs_store::calendar::{self as store, CalendarScope};

use super::CalendarCopy;
use crate::{Accounts, SyncError};

/// One account's calendars as the sidebar lists them.
#[derive(Debug, Clone, PartialEq)]
pub struct Listed {
    pub account_id: AccountId,
    /// Every calendar the copy holds for the account, primary first.
    pub calendars: Vec<Calendar>,
    /// The ids of the calendars the person took off the list.
    pub unlisted: Vec<String>,
}

impl<A: Accounts> CalendarCopy<A> {
    /// Every occurrence on the accounts' calendars that `scope` names and
    /// that overlaps `from` to `to`, earliest first. The store caps one
    /// read at a year and at `store::calendar::MOST_EVENTS` occurrences.
    pub async fn occurrences(
        &self,
        accounts: &[AccountId],
        from: EpochMillis,
        to: EpochMillis,
        scope: CalendarScope,
    ) -> Result<Vec<Occurrence>, SyncError> {
        let accounts = accounts.to_vec();
        Ok(self.db.read(move |c| store::occurrences(c, &accounts, from, to, scope)).await?)
    }

    /// The events that mention `text`, the next occurrence after `from`
    /// first, at most `limit` of them.
    pub async fn search(
        &self,
        accounts: &[AccountId],
        text: &str,
        from: EpochMillis,
        scope: CalendarScope,
        limit: usize,
    ) -> Result<Vec<Occurrence>, SyncError> {
        let (accounts, text) = (accounts.to_vec(), text.to_string());
        Ok(self.db.read(move |c| store::search(c, &accounts, &text, from, scope, limit)).await?)
    }

    /// The event `id` on `calendar`, or `None` when the copy no longer
    /// holds it.
    pub async fn event(&self, account_id: AccountId, calendar: &str, id: &str) -> Result<Option<Event>, SyncError> {
        let (calendar, id) = (calendar.to_string(), id.to_string());
        Ok(self.db.read(move |c| store::event(c, account_id, &calendar, &id)).await?)
    }

    /// Each account's calendars, in the order `accounts` names them.
    pub async fn listed(&self, accounts: &[AccountId]) -> Result<Vec<Listed>, SyncError> {
        let accounts = accounts.to_vec();
        Ok(self
            .db
            .read(move |c| {
                let mut listed = Vec::with_capacity(accounts.len());
                for &account_id in &accounts {
                    listed.push(Listed {
                        account_id,
                        calendars: store::calendars(c, account_id)?,
                        unlisted: store::unlisted(c, account_id)?,
                    });
                }
                Ok(listed)
            })
            .await?)
    }

    /// Shows or hides a calendar's events on this computer. The provider
    /// never hears of it.
    pub async fn show_calendar(&self, account_id: AccountId, calendar: &str, shown: bool) -> Result<(), SyncError> {
        let calendar = calendar.to_string();
        Ok(self.db.write(move |c| store::set_shown(c, account_id, &calendar, shown)).await?)
    }
}
