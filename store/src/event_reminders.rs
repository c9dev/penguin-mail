//! Event reminders already put up on this computer, and the ones the
//! person snoozed. The reminder scheduler reads the whole log on each
//! check and records what it posted, so a reminder goes up once per
//! occurrence even though the app restarts itself a minute after its
//! window closes.

use std::collections::HashMap;

use mailrs_domain::{AccountId, EpochMillis};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

use crate::Result;

/// One reminder of one occurrence: which event, the start of that
/// showing of it, and how many minutes before the start it goes up.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Key {
    pub account_id: AccountId,
    pub calendar: String,
    pub event: String,
    pub start: EpochMillis,
    pub minutes: u32,
}

/// What the log holds for one reminder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Logged {
    pub shown_at: EpochMillis,
    /// When a snoozed reminder goes up again.
    pub snoozed_until: Option<EpochMillis>,
}

/// How long a row stays after its event ends. An event that runs late
/// or moves by a few minutes keeps its row, and the table stays a few
/// days of reminders long.
pub const KEEP_AFTER_END: EpochMillis = 24 * 60 * 60 * 1000;

pub fn log(conn: &Connection) -> Result<HashMap<Key, Logged>> {
    let mut stmt = conn.prepare(
        "SELECT account_id, calendar, event, starts_at, minutes, shown_at, snoozed_until \
         FROM event_reminders_shown",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            Key {
                account_id: row.get(0)?,
                calendar: row.get(1)?,
                event: row.get(2)?,
                start: row.get(3)?,
                minutes: row.get(4)?,
            },
            Logged { shown_at: row.get(5)?, snoozed_until: row.get(6)? },
        ))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Records each reminder as shown at `now`, with its occurrence's end,
/// and clears any snooze on it. Then drops the rows of events that ended
/// more than [`KEEP_AFTER_END`] ago.
pub fn mark_shown(conn: &Connection, shown: &[(Key, EpochMillis)], now: EpochMillis) -> Result<()> {
    for (key, ends_at) in shown {
        conn.execute(
            "INSERT INTO event_reminders_shown \
             (account_id, calendar, event, starts_at, minutes, ends_at, shown_at, snoozed_until) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL) \
             ON CONFLICT (account_id, calendar, event, starts_at, minutes) DO UPDATE SET \
             ends_at = excluded.ends_at, shown_at = excluded.shown_at, snoozed_until = NULL",
            params![key.account_id, key.calendar, key.event, key.start, key.minutes, ends_at, now],
        )?;
    }
    conn.execute(
        "DELETE FROM event_reminders_shown WHERE ends_at < ?1",
        params![now - KEEP_AFTER_END],
    )?;
    Ok(())
}

/// Brings a reminder back at `until`. A reminder always has its row by
/// the time someone snoozes it; the insert covers a row pruned in
/// between, with the start standing in for the end so it still goes.
pub fn snooze(conn: &Connection, key: &Key, now: EpochMillis, until: EpochMillis) -> Result<()> {
    conn.execute(
        "INSERT INTO event_reminders_shown \
         (account_id, calendar, event, starts_at, minutes, ends_at, shown_at, snoozed_until) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?4, ?6, ?7) \
         ON CONFLICT (account_id, calendar, event, starts_at, minutes) DO UPDATE SET \
         snoozed_until = excluded.snoozed_until",
        params![key.account_id, key.calendar, key.event, key.start, key.minutes, now, until],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts;

    const MIN: EpochMillis = 60_000;
    const HOUR: EpochMillis = 60 * MIN;
    // Wednesday 23 September 2026, 14:00 in Lisbon.
    const TWO_PM: EpochMillis = 1_790_168_400_000;

    fn store() -> (Connection, AccountId) {
        let conn = crate::open_in_memory().unwrap();
        let id = accounts::insert_account(&conn, "me@example.com", 0).unwrap();
        (conn, id)
    }

    fn key(account_id: AccountId, minutes: u32) -> Key {
        Key {
            account_id,
            calendar: "me@example.com".into(),
            event: "standup".into(),
            start: TWO_PM,
            minutes,
        }
    }

    #[test]
    fn what_was_shown_and_snoozed_reads_back() {
        let (conn, id) = store();
        let ten = key(id, 10);
        mark_shown(&conn, &[(ten.clone(), TWO_PM + HOUR)], TWO_PM - 10 * MIN).unwrap();
        assert_eq!(
            log(&conn).unwrap().get(&ten),
            Some(&Logged { shown_at: TWO_PM - 10 * MIN, snoozed_until: None })
        );
        snooze(&conn, &ten, TWO_PM - 9 * MIN, TWO_PM - 4 * MIN).unwrap();
        assert_eq!(log(&conn).unwrap()[&ten].snoozed_until, Some(TWO_PM - 4 * MIN));
        // Coming back from the snooze clears it.
        mark_shown(&conn, &[(ten.clone(), TWO_PM + HOUR)], TWO_PM - 4 * MIN).unwrap();
        assert_eq!(
            log(&conn).unwrap()[&ten],
            Logged { shown_at: TWO_PM - 4 * MIN, snoozed_until: None }
        );
    }

    #[test]
    fn each_reminder_of_an_occurrence_is_its_own_row() {
        let (conn, id) = store();
        mark_shown(&conn, &[(key(id, 30), TWO_PM + HOUR), (key(id, 10), TWO_PM + HOUR)], TWO_PM).unwrap();
        assert_eq!(log(&conn).unwrap().len(), 2);
    }

    #[test]
    fn a_row_goes_a_day_after_its_event_ends() {
        let (conn, id) = store();
        mark_shown(&conn, &[(key(id, 10), TWO_PM + HOUR)], TWO_PM).unwrap();
        mark_shown(&conn, &[], TWO_PM + HOUR + KEEP_AFTER_END).unwrap();
        assert_eq!(log(&conn).unwrap().len(), 1, "a day to the millisecond still keeps it");
        mark_shown(&conn, &[], TWO_PM + HOUR + KEEP_AFTER_END + 1).unwrap();
        assert!(log(&conn).unwrap().is_empty());
    }

    #[test]
    fn a_snooze_without_a_row_still_comes_back() {
        let (conn, id) = store();
        snooze(&conn, &key(id, 10), TWO_PM - 9 * MIN, TWO_PM - 4 * MIN).unwrap();
        assert_eq!(log(&conn).unwrap()[&key(id, 10)].snoozed_until, Some(TWO_PM - 4 * MIN));
    }

    #[test]
    fn removing_an_account_takes_its_rows() {
        let (conn, id) = store();
        mark_shown(&conn, &[(key(id, 10), TWO_PM + HOUR)], TWO_PM).unwrap();
        accounts::delete_account(&conn, id).unwrap();
        assert!(log(&conn).unwrap().is_empty());
    }
}
