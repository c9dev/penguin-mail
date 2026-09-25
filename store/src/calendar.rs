//! Each account's calendars and events on this computer, and the queue
//! of changes made here that the provider has not taken yet.
//!
//! A series is stored once, with its rules and the end of its last
//! occurrence, and [`occurrences`] expands it for the range asked for.
//! An occurrence someone changed is a row of its own that names its
//! series and the start it replaces.

use std::collections::HashSet;
use std::sync::Arc;

use mailrs_domain::calendar::{self as model, Access, Calendar, Event, Guest, Occurrence, Status};
use mailrs_domain::{AccountId, EpochMillis};
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::Result;

/// A day, in milliseconds.
const DAY: EpochMillis = 24 * 60 * 60 * 1000;

/// The longest range [`occurrences`] answers whole. Memory item 1: the
/// query clones a whole event into every occurrence it returns, so a
/// range with no bound could hold a year of a daily series' guests and
/// descriptions for however far ahead the caller asked.
const MAX_RANGE: EpochMillis = 366 * DAY;

/// The most occurrences [`occurrences`] returns, the live path's own
/// `MOST_EVENTS` (`gmail/src/calendar.rs`).
const MOST_EVENTS: usize = 500;

/// Which of an account's calendars an [`occurrences`] query reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CalendarScope {
    /// Calendars the person has not hidden.
    Shown,
    /// Every calendar, hidden or not.
    All,
    /// Calendars the account can write to, for the clash line and free
    /// time (ruling R3), whether or not the person hid them.
    Owned,
}

/// Replaces the account's calendar list. A calendar that stays keeps its
/// shown flag and sync token; one that went takes its events with it.
pub fn save_calendars(conn: &Connection, account_id: AccountId, list: &[Calendar]) -> Result<()> {
    let keep: HashSet<&str> = list.iter().map(|c| c.id.as_str()).collect();
    let held: Vec<String> = {
        let mut stmt = conn.prepare("SELECT id FROM calendars WHERE account_id = ?1")?;
        stmt.query_map(params![account_id], |row| row.get(0))?.collect::<rusqlite::Result<_>>()?
    };
    for gone in held.iter().filter(|id| !keep.contains(id.as_str())) {
        conn.execute("DELETE FROM calendars WHERE account_id = ?1 AND id = ?2", params![account_id, gone])?;
    }
    for calendar in list {
        conn.execute(
            "INSERT INTO calendars (account_id, id, name, color, access, zone, is_primary, shown, reminders) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) \
             ON CONFLICT (account_id, id) DO UPDATE SET name = excluded.name, color = excluded.color, \
             access = excluded.access, zone = excluded.zone, is_primary = excluded.is_primary, \
             reminders = excluded.reminders",
            params![
                account_id,
                calendar.id,
                calendar.name,
                calendar.color,
                calendar.access.as_str(),
                calendar.zone,
                calendar.primary,
                calendar.shown,
                json(&calendar.reminders),
            ],
        )?;
    }
    Ok(())
}

pub fn calendars(conn: &Connection, account_id: AccountId) -> Result<Vec<Calendar>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, color, access, zone, is_primary, shown, reminders FROM calendars \
         WHERE account_id = ?1 ORDER BY is_primary DESC, name",
    )?;
    let rows = stmt.query_map(params![account_id], |row| {
        Ok(Calendar {
            id: row.get(0)?,
            name: row.get(1)?,
            color: row.get(2)?,
            access: Access::parse(&row.get::<_, String>(3)?),
            zone: row.get(4)?,
            primary: row.get(5)?,
            shown: row.get(6)?,
            reminders: parse(&row.get::<_, String>(7)?),
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn set_shown(conn: &Connection, account_id: AccountId, calendar: &str, shown: bool) -> Result<()> {
    conn.execute(
        "UPDATE calendars SET shown = ?3 WHERE account_id = ?1 AND id = ?2",
        params![account_id, calendar, shown],
    )?;
    Ok(())
}

pub fn token(conn: &Connection, account_id: AccountId, calendar: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT sync_token FROM calendars WHERE account_id = ?1 AND id = ?2",
            params![account_id, calendar],
            |row| row.get(0),
        )
        .optional()?
        .flatten())
}

pub fn set_token(
    conn: &Connection,
    account_id: AccountId,
    calendar: &str,
    token: Option<&str>,
    at: EpochMillis,
) -> Result<()> {
    conn.execute(
        "UPDATE calendars SET sync_token = ?3, synced_at = ?4 WHERE account_id = ?1 AND id = ?2",
        params![account_id, calendar, token, at],
    )?;
    Ok(())
}

/// When a calendar was last read whole or for its changes, `None` before
/// its first read. `CalendarCopy` reads this for a hidden calendar, which
/// it reads at the slow cadence rather than every tick.
pub fn synced_at(conn: &Connection, account_id: AccountId, calendar: &str) -> Result<Option<EpochMillis>> {
    Ok(conn
        .query_row(
            "SELECT synced_at FROM calendars WHERE account_id = ?1 AND id = ?2",
            params![account_id, calendar],
            |row| row.get(0),
        )
        .optional()?
        .flatten())
}

/// Whether the copy is worth reading instead of asking the provider: the
/// primary calendar has been read whole at least once.
pub fn synced(conn: &Connection, account_id: AccountId) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM calendars WHERE account_id = ?1 AND is_primary = 1 AND sync_token IS NOT NULL",
            params![account_id],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// Stores events, replacing what each had before, guests included, and
/// marks every row `seen_at`, the page of a whole read that wrote it (or
/// the moment of a single save), so [`sweep`] can tell a row no later
/// page repeated from one still current.
pub fn save_events(conn: &Connection, account_id: AccountId, events: &[Event], seen_at: EpochMillis) -> Result<()> {
    for event in events {
        conn.execute(
            "INSERT OR REPLACE INTO events (account_id, calendar, id, uid, etag, starts_at, ends_at, zone, \
             all_day, title, place, description, color, busy, status, private, organizer, my_answer, \
             reminders, conference, rules, series_end, series, original_start, pending, seen_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, \
             ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26)",
            params![
                account_id,
                event.calendar,
                event.id,
                event.uid,
                event.etag,
                event.start,
                event.end,
                event.zone,
                event.all_day,
                event.title,
                event.place,
                event.description,
                event.color,
                event.busy,
                event.status.as_str(),
                event.private,
                event.organizer,
                event.my_answer.map(|a| a.as_str()),
                event.reminders.as_ref().map(json),
                event.conference,
                event.rules.join("\n"),
                model::series_end(event),
                event.series,
                event.original_start,
                event.pending,
                seen_at,
            ],
        )?;
        conn.execute(
            "DELETE FROM event_guests WHERE account_id = ?1 AND calendar = ?2 AND event = ?3",
            params![account_id, event.calendar, event.id],
        )?;
        for guest in &event.guests {
            conn.execute(
                "INSERT OR REPLACE INTO event_guests (account_id, calendar, event, email, name, answer, organizer, me) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    account_id,
                    event.calendar,
                    event.id,
                    guest.email,
                    guest.name,
                    guest.answer.map(|a| a.as_str()),
                    guest.organizer,
                    guest.me,
                ],
            )?;
        }
    }
    Ok(())
}

pub fn remove_events(conn: &Connection, account_id: AccountId, calendar: &str, ids: &[String]) -> Result<()> {
    for id in ids {
        conn.execute(
            "DELETE FROM events WHERE account_id = ?1 AND calendar = ?2 AND (id = ?3 OR series = ?3)",
            params![account_id, calendar, id],
        )?;
    }
    Ok(())
}

/// Drops a calendar's rows a whole read did not repeat: every row still
/// marked `seen_at` before `before`, save for one a queued change still
/// owns, which the read must not drop out from under an unsent edit.
pub fn sweep(conn: &Connection, account_id: AccountId, calendar: &str, before: EpochMillis) -> Result<()> {
    conn.execute(
        "DELETE FROM events WHERE account_id = ?1 AND calendar = ?2 AND pending = 0 AND seen_at < ?3",
        params![account_id, calendar, before],
    )?;
    Ok(())
}

const COLUMNS: &str = "e.account_id, e.calendar, e.id, e.uid, e.etag, e.starts_at, e.ends_at, e.zone, \
    e.all_day, e.title, e.place, e.description, e.color, e.busy, e.status, e.private, e.organizer, \
    e.my_answer, e.reminders, e.conference, e.rules, e.series, e.original_start, e.pending";

pub fn event(conn: &Connection, account_id: AccountId, calendar: &str, id: &str) -> Result<Option<Event>> {
    let found = conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM events e WHERE e.account_id = ?1 AND e.calendar = ?2 AND e.id = ?3"),
            params![account_id, calendar, id],
            read_event,
        )
        .optional()?;
    match found {
        Some(mut event) => {
            event.guests = guests(conn, account_id, calendar, id)?;
            Ok(Some(event))
        }
        None => Ok(None),
    }
}

/// Finds an event by its id alone, when which calendar holds it is not
/// known: the primary calendar first, then a calendar the account owns,
/// then any other. The same id can show on more than one calendar at
/// once (an invitation's event sits on the guest's primary calendar and
/// on the organizer's shared one under the same id), so the order picks
/// the copy of it a write should land on.
pub fn find_event(conn: &Connection, account_id: AccountId, id: &str) -> Result<Option<Event>> {
    let found: Option<String> = conn
        .query_row(
            "SELECT e.calendar FROM events e \
             JOIN calendars c ON c.account_id = e.account_id AND c.id = e.calendar \
             WHERE e.account_id = ?1 AND e.id = ?2 \
             ORDER BY c.is_primary DESC, c.access = 'owner' DESC LIMIT 1",
            params![account_id, id],
            |row| row.get(0),
        )
        .optional()?;
    match found {
        Some(calendar) => event(conn, account_id, &calendar, id),
        None => Ok(None),
    }
}

/// Every occurrence on the accounts' calendars that overlaps `from` to
/// `to`, earliest first, at most a year past `from` and at most
/// [`MOST_EVENTS`] of them. Series are expanded; a changed or cancelled
/// occurrence replaces the one it names; cancelled one-offs stay out.
pub fn occurrences(
    conn: &Connection,
    accounts: &[AccountId],
    from: EpochMillis,
    to: EpochMillis,
    reach: CalendarScope,
) -> Result<Vec<Occurrence>> {
    let to = if to - from > MAX_RANGE { from + MAX_RANGE } else { to };
    let calendar_filter = match reach {
        CalendarScope::Shown => "AND c.shown = 1",
        CalendarScope::All => "",
        CalendarScope::Owned => "AND c.access = 'owner'",
    };
    let mut found = Vec::new();
    for &account_id in accounts {
        // The changed-occurrence branch below has no upper bound of its
        // own on how far back a moved or cancelled occurrence's original
        // start may sit; this is its lower bound, so a series' oldest
        // occurrence does not load on every call (reconcile.md Task 2
        // item 4, Memory item 2).
        let longest: EpochMillis = conn.query_row(
            "SELECT COALESCE(MAX(ends_at - starts_at), 0) FROM events WHERE account_id = ?1 AND rules <> ''",
            params![account_id],
            |row| row.get(0),
        )?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {COLUMNS} FROM events e \
             JOIN calendars c ON c.account_id = e.account_id AND c.id = e.calendar \
             WHERE e.account_id = ?1 {calendar_filter} AND ( \
                 (e.starts_at < ?3 AND e.rules = '' \
                      AND (e.ends_at > ?2 OR (e.ends_at = e.starts_at AND e.starts_at >= ?2))) \
              OR (e.starts_at < ?3 AND e.rules <> '' AND (e.series_end IS NULL OR e.series_end > ?2)) \
              OR (e.series IS NOT NULL AND e.original_start < ?3 AND e.original_start >= ?2 - ?4) \
             )"
        ))?;
        let events: Vec<Event> =
            stmt.query_map(params![account_id, from, to, longest], read_event)?.collect::<rusqlite::Result<_>>()?;
        // What each changed occurrence replaces: (calendar, series, original start).
        let replaced: HashSet<(String, String, EpochMillis)> = events
            .iter()
            .filter_map(|e| Some((e.calendar.clone(), e.series.clone()?, e.original_start?)))
            .collect();
        for mut event in events {
            if event.status == Status::Cancelled {
                continue;
            }
            event.guests = guests(conn, account_id, &event.calendar, &event.id)?;
            let event = Arc::new(event);
            for (start, end) in model::expand(&event, from, to) {
                if replaced.contains(&(event.calendar.clone(), event.id.clone(), start)) {
                    continue;
                }
                found.push(Occurrence { account_id, event: Arc::clone(&event), start, end });
            }
        }
    }
    found.sort_by(|a, b| (a.start, &a.event.title).cmp(&(b.start, &b.event.title)));
    found.truncate(MOST_EVENTS);
    Ok(found)
}

fn guests(conn: &Connection, account_id: AccountId, calendar: &str, event: &str) -> Result<Vec<Guest>> {
    let mut stmt = conn.prepare(
        "SELECT email, name, answer, organizer, me FROM event_guests \
         WHERE account_id = ?1 AND calendar = ?2 AND event = ?3 ORDER BY organizer DESC, email",
    )?;
    let rows = stmt.query_map(params![account_id, calendar, event], |row| {
        Ok(Guest {
            email: row.get(0)?,
            name: row.get(1)?,
            answer: row.get::<_, Option<String>>(2)?.and_then(|a| a.parse().ok()),
            organizer: row.get(3)?,
            me: row.get(4)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn read_event(row: &Row) -> rusqlite::Result<Event> {
    Ok(Event {
        calendar: row.get(1)?,
        id: row.get(2)?,
        uid: row.get(3)?,
        etag: row.get(4)?,
        start: row.get(5)?,
        end: row.get(6)?,
        zone: row.get(7)?,
        all_day: row.get(8)?,
        title: row.get(9)?,
        place: row.get(10)?,
        description: row.get(11)?,
        color: row.get(12)?,
        busy: row.get(13)?,
        status: Status::parse(&row.get::<_, String>(14)?),
        private: row.get(15)?,
        organizer: row.get(16)?,
        my_answer: row.get::<_, Option<String>>(17)?.and_then(|a| a.parse().ok()),
        reminders: row.get::<_, Option<String>>(18)?.map(|r| parse(&r)),
        conference: row.get(19)?,
        rules: row
            .get::<_, String>(20)?
            .lines()
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect(),
        series: row.get(21)?,
        original_start: row.get(22)?,
        pending: row.get(23)?,
        guests: Vec::new(),
    })
}

/// What a queued change does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    /// An event this computer made, which the provider has never seen.
    Create,
    /// Change an event the provider already knows to match the body.
    Save,
    Remove,
}

impl ChangeKind {
    fn as_str(self) -> &'static str {
        match self {
            ChangeKind::Create => "create",
            ChangeKind::Save => "save",
            ChangeKind::Remove => "remove",
        }
    }

    fn parse(word: &str) -> ChangeKind {
        match word {
            "create" => ChangeKind::Create,
            "remove" => ChangeKind::Remove,
            _ => ChangeKind::Save,
        }
    }
}

/// One change waiting for the provider. Named apart from the glossary's
/// "Queued message" (`mailrs_store::outbox::Queued`), which is something
/// else (reconcile.md Task 2 item 6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedChange {
    pub seq: i64,
    pub account_id: AccountId,
    pub calendar: String,
    pub event: String,
    pub kind: ChangeKind,
    /// The version the change was made against; `None` for an event this
    /// computer made, which the provider has never seen.
    pub etag: Option<String>,
    pub body: Option<Event>,
}

/// Queues a change for the provider. One row holds the latest change for
/// an event, so an event edited several times offline queues one body
/// rather than one row per edit (reconcile.md Task 2 item 8):
/// - a Save meeting an unsent Create or Save replaces that row's body;
/// - a Remove meeting an unsent Create drops the row: the provider never
///   heard of the event, so there is nothing left to tell it;
/// - a Remove meeting an unsent Save turns that row into the Remove.
pub fn enqueue(conn: &Connection, account_id: AccountId, kind: ChangeKind, event: &Event) -> Result<()> {
    let existing: Option<(i64, String)> = conn
        .query_row(
            "SELECT seq, kind FROM calendar_changes WHERE account_id = ?1 AND calendar = ?2 AND event = ?3",
            params![account_id, event.calendar, event.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;

    if let Some((seq, existing_kind)) = existing {
        match (kind, existing_kind.as_str()) {
            (ChangeKind::Remove, "create") => {
                conn.execute("DELETE FROM calendar_changes WHERE seq = ?1", params![seq])?;
                return Ok(());
            }
            (ChangeKind::Remove, "save") => {
                conn.execute(
                    "UPDATE calendar_changes SET kind = 'remove', body = NULL WHERE seq = ?1",
                    params![seq],
                )?;
                return Ok(());
            }
            (ChangeKind::Save, "create") | (ChangeKind::Save, "save") => {
                conn.execute("UPDATE calendar_changes SET body = ?2 WHERE seq = ?1", params![seq, json(event)])?;
                return Ok(());
            }
            _ => {}
        }
    }

    let etag = (!event.etag.is_empty()).then_some(event.etag.as_str());
    let body = match kind {
        ChangeKind::Create | ChangeKind::Save => Some(json(event)),
        ChangeKind::Remove => None,
    };
    conn.execute(
        "INSERT INTO calendar_changes (account_id, calendar, event, kind, etag, body) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![account_id, event.calendar, event.id, kind.as_str(), etag, body],
    )?;
    Ok(())
}

pub fn queued(conn: &Connection, account_id: AccountId) -> Result<Vec<QueuedChange>> {
    let mut stmt = conn.prepare(
        "SELECT seq, calendar, event, kind, etag, body FROM calendar_changes WHERE account_id = ?1 ORDER BY seq",
    )?;
    let rows = stmt.query_map(params![account_id], |row| read_change(row, account_id))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The account's first queued change after `after`, as it stands now. A
/// send reads each change just before it goes out, so a change deleted
/// or edited since the send began goes out as it is now or not at all,
/// and one queued during the send still goes out in the same send.
pub fn next_change(conn: &Connection, account_id: AccountId, after: i64) -> Result<Option<QueuedChange>> {
    Ok(conn
        .query_row(
            "SELECT seq, calendar, event, kind, etag, body FROM calendar_changes \
             WHERE account_id = ?1 AND seq > ?2 ORDER BY seq LIMIT 1",
            params![account_id, after],
            |row| read_change(row, account_id),
        )
        .optional()?)
}

fn read_change(row: &Row, account_id: AccountId) -> rusqlite::Result<QueuedChange> {
    Ok(QueuedChange {
        seq: row.get(0)?,
        account_id,
        calendar: row.get(1)?,
        event: row.get(2)?,
        kind: ChangeKind::parse(&row.get::<_, String>(3)?),
        etag: row.get(4)?,
        body: row.get::<_, Option<String>>(5)?.and_then(|b| serde_json::from_str(&b).ok()),
    })
}

/// The ids of a calendar's events with an unsent change, without loading
/// every queued body (reconcile.md Task 2 item 9).
pub fn pending_ids(conn: &Connection, account_id: AccountId, calendar: &str) -> Result<HashSet<String>> {
    let mut stmt =
        conn.prepare("SELECT event FROM calendar_changes WHERE account_id = ?1 AND calendar = ?2")?;
    let rows = stmt.query_map(params![account_id, calendar], |row| row.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn dequeue(conn: &Connection, seq: i64) -> Result<()> {
    conn.execute("DELETE FROM calendar_changes WHERE seq = ?1", params![seq])?;
    Ok(())
}

/// Settles a change that just went out, and answers whether the
/// provider's answer belongs in the copy.
///
/// Usually the row is done and comes off. But the person can act on the
/// event while the provider is still answering:
/// - An edit collapses onto this very row, since `enqueue` changes an
///   unsent row's body and never its `seq`. The row stays, with its etag
///   moved to `new_etag` and a `Create` turned into a `Save`, so the edit
///   goes out against the version this send just wrote. The copy keeps
///   what the edit wrote.
/// - A delete of a new event drops its unsent `Create` row, since the
///   provider never heard of it. But the create is on its way, so the
///   row is gone and the provider now holds the event: queue its
///   removal against `new_etag`, and keep the answer out of the copy.
pub fn finish_change(
    conn: &Connection,
    account_id: AccountId,
    seq: i64,
    attempted: &Event,
    new_etag: &str,
) -> Result<bool> {
    let current: Option<Option<String>> = conn
        .query_row("SELECT body FROM calendar_changes WHERE seq = ?1", params![seq], |row| row.get(0))
        .optional()?;
    match current {
        None => {
            let gone = Event {
                calendar: attempted.calendar.clone(),
                id: attempted.id.clone(),
                etag: new_etag.to_string(),
                ..Event::default()
            };
            enqueue(conn, account_id, ChangeKind::Remove, &gone)?;
            Ok(false)
        }
        Some(body) if body.as_deref() == Some(json(attempted).as_str()) => {
            conn.execute("DELETE FROM calendar_changes WHERE seq = ?1", params![seq])?;
            Ok(true)
        }
        Some(_) => {
            conn.execute(
                "UPDATE calendar_changes SET etag = ?2, \
                 kind = CASE kind WHEN 'create' THEN 'save' ELSE kind END WHERE seq = ?1",
                params![seq, new_etag],
            )?;
            Ok(false)
        }
    }
}

fn json<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

fn parse<T: serde::de::DeserializeOwned + Default>(text: &str) -> T {
    serde_json::from_str(text).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::accounts;
    use mailrs_domain::calendar::{Access, Calendar, Event, Guest, Status};
    use mailrs_domain::invitation::Answer;

    const HOUR: EpochMillis = 60 * 60 * 1000;
    const DAY: EpochMillis = 24 * HOUR;
    // Monday 19 October 2026, 00:00 UTC.
    const MONDAY: EpochMillis = 1_792_368_000_000;

    fn store() -> (Connection, AccountId) {
        let conn = crate::open_in_memory().unwrap();
        let id = accounts::insert_account(&conn, "me@example.com", 0).unwrap();
        save_calendars(&conn, id, &[calendar("primary", true), calendar("team", false)]).unwrap();
        (conn, id)
    }

    fn calendar(id: &str, primary: bool) -> Calendar {
        Calendar {
            id: id.into(),
            name: id.into(),
            color: "#3584e4".into(),
            access: Access::Owner,
            zone: "UTC".into(),
            primary,
            shown: true,
            reminders: Vec::new(),
        }
    }

    fn event(calendar: &str, id: &str, start: EpochMillis, hours: i64) -> Event {
        Event {
            calendar: calendar.into(),
            id: id.into(),
            uid: format!("{id}@example.com"),
            etag: "\"1\"".into(),
            start,
            end: start + hours * HOUR,
            zone: "UTC".into(),
            title: id.into(),
            busy: true,
            ..Event::default()
        }
    }

    fn starts(found: &[Occurrence]) -> Vec<(String, EpochMillis)> {
        found.iter().map(|o| (o.event.id.clone(), o.start)).collect()
    }

    #[test]
    fn a_saved_event_comes_back_with_its_guests() {
        let (conn, id) = store();
        let mut lunch = event("primary", "lunch", MONDAY + 12 * HOUR, 1);
        lunch.guests = vec![Guest { email: "ana@example.com".into(), answer: Some(Answer::Yes), ..Guest::default() }];
        save_events(&conn, id, &[lunch.clone()], 0).unwrap();
        assert_eq!(super::event(&conn, id, "primary", "lunch").unwrap(), Some(lunch));
    }

    #[test]
    fn find_event_prefers_the_primary_calendar_then_an_owned_one_then_any() {
        let (conn, id) = store();
        let mut reader = calendar("shared", false);
        reader.access = Access::Reader;
        save_calendars(&conn, id, &[calendar("primary", true), calendar("team", false), reader]).unwrap();
        save_events(&conn, id, &[event("shared", "x", MONDAY, 1)], 0).unwrap();
        assert_eq!(find_event(&conn, id, "x").unwrap().map(|e| e.calendar), Some("shared".into()));
        save_events(&conn, id, &[event("team", "x", MONDAY, 1)], 0).unwrap();
        assert_eq!(
            find_event(&conn, id, "x").unwrap().map(|e| e.calendar),
            Some("team".into()),
            "an owned calendar wins over one only read"
        );
        save_events(&conn, id, &[event("primary", "x", MONDAY, 1)], 0).unwrap();
        assert_eq!(
            find_event(&conn, id, "x").unwrap().map(|e| e.calendar),
            Some("primary".into()),
            "the primary calendar wins over any other"
        );
        assert_eq!(find_event(&conn, id, "missing").unwrap(), None);
    }

    #[test]
    fn the_range_holds_one_off_events_that_overlap_it() {
        let (conn, id) = store();
        save_events(
            &conn,
            id,
            &[
                event("primary", "before", MONDAY - 2 * HOUR, 1),
                event("primary", "across", MONDAY - HOUR, 2),
                event("primary", "inside", MONDAY + 9 * HOUR, 1),
                event("primary", "after", MONDAY + DAY, 1),
            ],
            0,
        )
        .unwrap();
        let found = occurrences(&conn, &[id], MONDAY, MONDAY + DAY, CalendarScope::Shown).unwrap();
        assert_eq!(starts(&found), vec![("across".into(), MONDAY - HOUR), ("inside".into(), MONDAY + 9 * HOUR)]);
    }

    #[test]
    fn a_series_shows_each_occurrence_in_range() {
        let (conn, id) = store();
        let mut standup = event("primary", "standup", MONDAY + 9 * HOUR, 1);
        standup.rules = vec!["RRULE:FREQ=DAILY;COUNT=5".into()];
        save_events(&conn, id, &[standup], 0).unwrap();
        let found = occurrences(&conn, &[id], MONDAY + DAY, MONDAY + 3 * DAY, CalendarScope::Shown).unwrap();
        assert_eq!(found.len(), 2);
    }

    #[test]
    fn a_changed_occurrence_takes_the_place_of_the_one_it_replaces() {
        let (conn, id) = store();
        let mut standup = event("primary", "standup", MONDAY + 9 * HOUR, 1);
        standup.rules = vec!["RRULE:FREQ=DAILY;COUNT=3".into()];
        let mut moved = event("primary", "standup_tue", MONDAY + DAY + 11 * HOUR, 1);
        moved.series = Some("standup".into());
        moved.original_start = Some(MONDAY + DAY + 9 * HOUR);
        save_events(&conn, id, &[standup, moved], 0).unwrap();
        let found = occurrences(&conn, &[id], MONDAY, MONDAY + 3 * DAY, CalendarScope::Shown).unwrap();
        assert_eq!(
            starts(&found),
            vec![
                ("standup".into(), MONDAY + 9 * HOUR),
                ("standup_tue".into(), MONDAY + DAY + 11 * HOUR),
                ("standup".into(), MONDAY + 2 * DAY + 9 * HOUR),
            ]
        );
    }

    /// reconcile.md Task 2 item 4: the query's `WHERE` used to `AND` every
    /// branch with `e.starts_at < ?3`, so a changed occurrence moved past
    /// the range end never entered the SQL result, never masked the
    /// series' own date for it, and the original showed as if nothing had
    /// moved it.
    #[test]
    fn a_changed_occurrence_moved_past_the_range_still_hides_its_original() {
        let (conn, id) = store();
        let mut standup = event("primary", "standup", MONDAY + 9 * HOUR, 1);
        standup.rules = vec!["RRULE:FREQ=DAILY;COUNT=3".into()];
        let mut moved = event("primary", "standup_tue", MONDAY + 3 * DAY + 11 * HOUR, 1);
        moved.series = Some("standup".into());
        moved.original_start = Some(MONDAY + DAY + 9 * HOUR);
        save_events(&conn, id, &[standup, moved], 0).unwrap();
        let found = occurrences(&conn, &[id], MONDAY, MONDAY + 2 * DAY, CalendarScope::Shown).unwrap();
        assert_eq!(starts(&found), vec![("standup".into(), MONDAY + 9 * HOUR)]);
    }

    #[test]
    fn a_cancelled_occurrence_leaves_a_gap() {
        let (conn, id) = store();
        let mut standup = event("primary", "standup", MONDAY + 9 * HOUR, 1);
        standup.rules = vec!["RRULE:FREQ=DAILY;COUNT=3".into()];
        let mut cancelled = event("primary", "standup_tue", MONDAY + DAY + 9 * HOUR, 1);
        cancelled.series = Some("standup".into());
        cancelled.original_start = Some(MONDAY + DAY + 9 * HOUR);
        cancelled.status = Status::Cancelled;
        save_events(&conn, id, &[standup, cancelled], 0).unwrap();
        let found = occurrences(&conn, &[id], MONDAY, MONDAY + 3 * DAY, CalendarScope::Shown).unwrap();
        assert_eq!(found.len(), 2);
    }

    #[test]
    fn a_hidden_calendar_leaves_the_range_unless_asked_for() {
        let (conn, id) = store();
        save_events(&conn, id, &[event("team", "retro", MONDAY + 9 * HOUR, 1)], 0).unwrap();
        set_shown(&conn, id, "team", false).unwrap();
        assert!(occurrences(&conn, &[id], MONDAY, MONDAY + DAY, CalendarScope::Shown).unwrap().is_empty());
        assert_eq!(occurrences(&conn, &[id], MONDAY, MONDAY + DAY, CalendarScope::All).unwrap().len(), 1);
    }

    /// reconcile.md Task 2 item 5: `CalendarScope::Owned` is what the clash line
    /// and free time need (ruling R3), so a calendar the account can only
    /// read never counts toward either.
    #[test]
    fn an_owned_reach_leaves_out_a_calendar_the_account_only_reads() {
        let (conn, id) = store();
        let mut team = calendar("team", false);
        team.access = Access::Reader;
        save_calendars(&conn, id, &[calendar("primary", true), team]).unwrap();
        save_events(&conn, id, &[event("team", "retro", MONDAY + 9 * HOUR, 1)], 0).unwrap();
        assert!(occurrences(&conn, &[id], MONDAY, MONDAY + DAY, CalendarScope::Owned).unwrap().is_empty());
        assert_eq!(occurrences(&conn, &[id], MONDAY, MONDAY + DAY, CalendarScope::All).unwrap().len(), 1);
    }

    /// Memory item 1: `occurrences` clones a whole event into every row it
    /// returns, so a range with no bound could hold a year of a daily
    /// series' guests and descriptions. A request for more than a year is
    /// clamped rather than answered whole.
    #[test]
    fn occurrences_clamp_the_range_to_a_year() {
        let (conn, id) = store();
        save_events(
            &conn,
            id,
            &[
                event("primary", "within", MONDAY + 300 * DAY, 1),
                event("primary", "beyond", MONDAY + 370 * DAY, 1),
            ],
            0,
        )
        .unwrap();
        let found = occurrences(&conn, &[id], MONDAY, MONDAY + 400 * DAY, CalendarScope::Shown).unwrap();
        assert_eq!(starts(&found), vec![("within".into(), MONDAY + 300 * DAY)]);
    }

    /// Memory item 1: the live path caps a listing at `MOST_EVENTS` (500);
    /// the local copy's range query does the same, so a very active series
    /// cannot hand back thousands of clones of itself.
    #[test]
    fn occurrences_stop_at_500_results() {
        let (conn, id) = store();
        let mut standup = event("primary", "standup", MONDAY, 1);
        standup.rules = vec!["RRULE:FREQ=HOURLY;COUNT=600".into()];
        save_events(&conn, id, &[standup], 0).unwrap();
        let found = occurrences(&conn, &[id], MONDAY, MONDAY + 30 * DAY, CalendarScope::Shown).unwrap();
        assert_eq!(found.len(), 500);
    }

    #[test]
    fn a_new_calendar_list_keeps_what_the_person_hid_and_drops_what_went() {
        let (conn, id) = store();
        set_shown(&conn, id, "team", false).unwrap();
        set_token(&conn, id, "team", Some("t1"), 5).unwrap();
        save_events(&conn, id, &[event("primary", "lunch", MONDAY, 1)], 0).unwrap();
        save_calendars(&conn, id, &[calendar("team", false)]).unwrap();
        let held = calendars(&conn, id).unwrap();
        assert_eq!(held.len(), 1);
        assert!(!held[0].shown);
        assert_eq!(token(&conn, id, "team").unwrap().as_deref(), Some("t1"));
        assert_eq!(super::event(&conn, id, "primary", "lunch").unwrap(), None);
    }

    #[test]
    fn the_queue_hands_changes_back_in_order() {
        let (conn, id) = store();
        let lunch = event("primary", "lunch", MONDAY, 1);
        let dinner = event("primary", "dinner", MONDAY + 12 * HOUR, 1);
        enqueue(&conn, id, ChangeKind::Save, &lunch).unwrap();
        enqueue(&conn, id, ChangeKind::Remove, &dinner).unwrap();
        let held = queued(&conn, id).unwrap();
        assert_eq!(
            held.iter().map(|q| (q.event.as_str(), q.kind)).collect::<Vec<_>>(),
            vec![("lunch", ChangeKind::Save), ("dinner", ChangeKind::Remove)]
        );
        assert_eq!(held[0].body.as_ref().map(|e| e.id.as_str()), Some("lunch"));
        dequeue(&conn, held[0].seq).unwrap();
        assert_eq!(queued(&conn, id).unwrap().len(), 1);
    }

    /// reconcile.md Task 2 item 8: an event edited twice before a send
    /// queues one change, its latest body, not one row per edit.
    #[test]
    fn two_edits_before_a_send_queue_one_change() {
        let (conn, id) = store();
        let mut lunch = event("primary", "lunch", MONDAY, 1);
        enqueue(&conn, id, ChangeKind::Save, &lunch).unwrap();
        lunch.title = "Lunch with Ana".into();
        enqueue(&conn, id, ChangeKind::Save, &lunch).unwrap();
        let held = queued(&conn, id).unwrap();
        assert_eq!(held.len(), 1);
        assert_eq!(held[0].body.as_ref().map(|e| e.title.as_str()), Some("Lunch with Ana"));
    }

    /// reconcile.md Task 2 item 8: removing an event this computer made
    /// and never sent takes the create off the queue instead of asking
    /// the provider to delete something it never heard of.
    #[test]
    fn removing_an_event_never_sent_queues_nothing() {
        let (conn, id) = store();
        let lunch = event("primary", "lunch", MONDAY, 1);
        enqueue(&conn, id, ChangeKind::Create, &lunch).unwrap();
        enqueue(&conn, id, ChangeKind::Remove, &lunch).unwrap();
        assert!(queued(&conn, id).unwrap().is_empty());
    }

    /// reconcile.md Task 2 item 8: removing an event with an unsent edit
    /// turns that edit into the removal, rather than queueing both.
    #[test]
    fn removing_an_edited_event_turns_the_queued_edit_into_the_removal() {
        let (conn, id) = store();
        let lunch = event("primary", "lunch", MONDAY, 1);
        enqueue(&conn, id, ChangeKind::Save, &lunch).unwrap();
        enqueue(&conn, id, ChangeKind::Remove, &lunch).unwrap();
        let held = queued(&conn, id).unwrap();
        assert_eq!(held.len(), 1);
        assert_eq!(held[0].kind, ChangeKind::Remove);
        assert!(held[0].body.is_none());
    }

    /// reconcile.md Task 2 item 9: a caller that only needs to know which
    /// ids are pending should not have to load every queued body to learn
    /// it.
    #[test]
    fn pending_ids_names_events_with_an_unsent_change() {
        let (conn, id) = store();
        let lunch = event("primary", "lunch", MONDAY, 1);
        enqueue(&conn, id, ChangeKind::Create, &lunch).unwrap();
        let ids = pending_ids(&conn, id, "primary").unwrap();
        assert_eq!(ids, HashSet::from(["lunch".to_string()]));
    }

    /// reconcile.md Task 2 item 10: a page-at-a-time read marks each row
    /// it writes with `seen_at`; sweeping after the last page drops a row
    /// no later page repeated, but never one a queued change still owns.
    #[test]
    fn sweep_drops_a_stale_row_but_keeps_a_pending_one() {
        let (conn, id) = store();
        save_events(&conn, id, &[event("primary", "old", MONDAY, 1)], 1).unwrap();
        let mut kept = event("primary", "kept", MONDAY + HOUR, 1);
        kept.pending = true;
        save_events(&conn, id, &[kept], 1).unwrap();
        sweep(&conn, id, "primary", 2).unwrap();
        assert!(super::event(&conn, id, "primary", "old").unwrap().is_none());
        assert!(super::event(&conn, id, "primary", "kept").unwrap().is_some());
    }

    #[test]
    fn the_account_counts_as_synced_once_its_primary_calendar_has_a_token() {
        let (conn, id) = store();
        assert!(!synced(&conn, id).unwrap());
        set_token(&conn, id, "primary", Some("t"), 1).unwrap();
        assert!(synced(&conn, id).unwrap());
    }

    /// reconcile.md Task 6 item 4: a change whose row nobody touched while
    /// it was in flight comes off the queue once it goes out.
    #[test]
    fn finishing_an_untouched_change_takes_it_off_the_queue() {
        let (conn, id) = store();
        let lunch = event("primary", "lunch", MONDAY, 1);
        enqueue(&conn, id, ChangeKind::Create, &lunch).unwrap();
        let seq = queued(&conn, id).unwrap()[0].seq;
        assert!(finish_change(&conn, id, seq, &lunch, "\"2\"").unwrap());
        assert!(queued(&conn, id).unwrap().is_empty());
    }

    /// A newer edit can collapse onto the row `send` is mid-way through
    /// sending, since `enqueue` only ever touches an unsent row's body,
    /// never its `seq`. Finishing that row must not take the newer edit
    /// down with it: it stays queued, its etag moved to the version the
    /// send just wrote and, for a create, demoted to a save, since the
    /// id now exists.
    #[test]
    fn finishing_a_change_a_newer_edit_collapsed_onto_keeps_the_edit_queued() {
        let (conn, id) = store();
        let lunch = event("primary", "lunch", MONDAY, 1);
        enqueue(&conn, id, ChangeKind::Create, &lunch).unwrap();
        let seq = queued(&conn, id).unwrap()[0].seq;
        let mut renamed = lunch.clone();
        renamed.title = "Lunch with Ana".into();
        enqueue(&conn, id, ChangeKind::Save, &renamed).unwrap();
        assert!(!finish_change(&conn, id, seq, &lunch, "\"2\"").unwrap());
        let held = queued(&conn, id).unwrap();
        assert_eq!(held.len(), 1);
        assert_eq!(held[0].kind, ChangeKind::Save, "a create demotes to a save once the id exists");
        assert_eq!(held[0].etag.as_deref(), Some("\"2\""));
        assert_eq!(held[0].body.as_ref().map(|e| e.title.as_str()), Some("Lunch with Ana"));
    }

    /// A delete made while the event's create is in flight drops the
    /// unsent row, but Google now holds the event. Finishing the create
    /// must queue the delete instead of storing the event again.
    #[test]
    fn finishing_a_create_a_delete_dropped_queues_the_delete() {
        let (conn, id) = store();
        let lunch = event("primary", "lunch", MONDAY, 1);
        enqueue(&conn, id, ChangeKind::Create, &lunch).unwrap();
        let seq = queued(&conn, id).unwrap()[0].seq;
        enqueue(&conn, id, ChangeKind::Remove, &lunch).unwrap();
        assert!(!finish_change(&conn, id, seq, &lunch, "\"2\"").unwrap());
        let held = queued(&conn, id).unwrap();
        assert_eq!(held.len(), 1);
        assert_eq!(held[0].kind, ChangeKind::Remove);
        assert_eq!(held[0].etag.as_deref(), Some("\"2\""));
    }

    #[test]
    fn the_next_change_is_the_first_queued_after_the_one_named() {
        let (conn, id) = store();
        enqueue(&conn, id, ChangeKind::Create, &event("primary", "one", MONDAY, 1)).unwrap();
        enqueue(&conn, id, ChangeKind::Create, &event("primary", "two", MONDAY, 1)).unwrap();
        let first = next_change(&conn, id, 0).unwrap().unwrap();
        assert_eq!(first.event, "one");
        let second = next_change(&conn, id, first.seq).unwrap().unwrap();
        assert_eq!(second.event, "two");
        assert!(next_change(&conn, id, second.seq).unwrap().is_none());
    }
}
