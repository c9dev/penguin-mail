//! Each account's calendars and events on this computer, and the queue
//! of changes made here that the provider has not taken yet.
//!
//! A series is stored once, with its rules and the end of its last
//! occurrence, and [`occurrences`] expands it for the range asked for.
//! An occurrence someone changed is a row of its own that names its
//! series and the start it replaces.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use mailrs_domain::calendar::series::Step;
use mailrs_domain::calendar::{self as model, Access, Calendar, Event, Guest, Kind, Notify, Occurrence, Status};
use mailrs_domain::invitation::Answer;
use mailrs_domain::{AccountId, EpochMillis};
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::Result;

/// A day, in milliseconds.
const DAY: EpochMillis = 24 * 60 * 60 * 1000;

/// The longest range [`occurrences`] answers whole. The query clones a whole event into every occurrence it returns, so a
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
    /// time, whether or not the person hid them. A calendar the account
    /// only reads is someone else's time.
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
        "SELECT id, name, COALESCE(own_color, color), access, zone, is_primary, shown, reminders, \
         COALESCE(provider_hidden, 0) \
         FROM calendars WHERE account_id = ?1 ORDER BY is_primary DESC, name",
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
            hidden: row.get(8)?,
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

/// Takes a calendar off the sidebar's list, or puts it back. A calendar
/// off the list is off the grid too, so this sets its shown flag with
/// it, and one put back shows again.
pub fn set_listed(conn: &Connection, account_id: AccountId, calendar: &str, listed: bool) -> Result<()> {
    conn.execute(
        "UPDATE calendars SET listed = ?3, shown = ?3 WHERE account_id = ?1 AND id = ?2",
        params![account_id, calendar, listed],
    )?;
    Ok(())
}

/// The ids of the account's calendars the person took off the list.
pub fn unlisted(conn: &Connection, account_id: AccountId) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT id FROM calendars WHERE account_id = ?1 AND NOT listed ORDER BY name")?;
    let rows = stmt.query_map(params![account_id], |row| row.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// A colour of the person's own for a calendar, `#rrggbb`, which
/// [`calendars`] gives in place of the provider's. `None` goes back to
/// the provider's colour.
pub fn set_own_color(conn: &Connection, account_id: AccountId, calendar: &str, color: Option<&str>) -> Result<()> {
    conn.execute(
        "UPDATE calendars SET own_color = ?3 WHERE account_id = ?1 AND id = ?2",
        params![account_id, calendar, color],
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

/// How far back the copy of `calendar` reaches: events that end before
/// this instant may be missing. `None` before its first whole read, and
/// for a calendar that does not exist.
pub fn reach(conn: &Connection, account_id: AccountId, calendar: &str) -> Result<Option<EpochMillis>> {
    Ok(conn
        .query_row(
            "SELECT reaches_back FROM calendars WHERE account_id = ?1 AND id = ?2",
            params![account_id, calendar],
            |row| row.get(0),
        )
        .optional()?
        .flatten())
}

/// Records that the copy of `calendar` now holds every event that ends
/// after `from`. It leaves the sync token alone, since a read of an older
/// range is not a read of changes.
pub fn set_reach(conn: &Connection, account_id: AccountId, calendar: &str, from: EpochMillis) -> Result<()> {
    conn.execute(
        "UPDATE calendars SET reaches_back = ?3 WHERE account_id = ?1 AND id = ?2",
        params![account_id, calendar, from],
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
        let attachments = kept_attachments(conn, account_id, event)?;
        conn.execute(
            "INSERT OR REPLACE INTO events (account_id, calendar, id, uid, etag, starts_at, ends_at, zone, \
             all_day, title, place, description, color, busy, status, private, organizer, my_answer, \
             reminders, conference, rules, series_end, series, original_start, pending, seen_at, sequence, kind, \
             attachments) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, \
             ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29)",
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
                event.sequence,
                (event.kind != Kind::Event).then(|| json(&event.kind)),
                attachments.as_ref().map(json),
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

/// Drops the changed occurrences of `series` on `calendar` whose ids are
/// not in `keep`, except one a queued change still owns.
pub fn keep_occurrences(
    conn: &Connection,
    account_id: AccountId,
    calendar: &str,
    series: &str,
    keep: &[String],
) -> Result<()> {
    let keep = serde_json::to_string(keep).unwrap_or_else(|_| "[]".to_string());
    conn.execute(
        "DELETE FROM events WHERE account_id = ?1 AND calendar = ?2 AND series = ?3 AND pending = 0 \
         AND id NOT IN (SELECT value FROM json_each(?4))",
        params![account_id, calendar, series, keep],
    )?;
    Ok(())
}

/// The rows `set_my_answer` changes: event `id`, and every other row of
/// the account that shares its uid, such as a moved occurrence stored
/// apart from its series. An empty uid matches nothing but the event.
const SAME_EVENT: &str = "account_id = ?1 AND ((calendar = ?2 AND id = ?3) OR (uid <> '' AND uid = \
     (SELECT uid FROM events WHERE account_id = ?1 AND calendar = ?2 AND id = ?3)))";

/// Records the account's own answer to event `id`, for an answer given
/// from the calendar view: `my_answer` on the event, and the answer of
/// the guest row marked `me`. The provider answers a whole series by its
/// uid, so every row of the account with that uid takes the answer too;
/// a changed occurrence would otherwise keep its old answer, and its
/// dashed outline, until the next read.
pub fn set_my_answer(
    conn: &Connection,
    account_id: AccountId,
    calendar: &str,
    id: &str,
    answer: Answer,
) -> Result<()> {
    conn.execute(
        &format!("UPDATE event_guests SET answer = ?4 WHERE account_id = ?1 AND me = 1 AND (calendar, event) IN \
             (SELECT calendar, id FROM events WHERE {SAME_EVENT})"),
        params![account_id, calendar, id, answer.as_str()],
    )?;
    conn.execute(
        &format!("UPDATE events SET my_answer = ?4 WHERE {SAME_EVENT}"),
        params![account_id, calendar, id, answer.as_str()],
    )?;
    Ok(())
}

/// Records the account's own answer on every row of the account whose
/// iCalendar UID is `uid`, for an answer the invitation card gave, which
/// knows the event by its UID alone. With `occurrence`, only the changed
/// occurrence stored for that original start takes it: the provider
/// answered that one occurrence, and the rest of the series still waits.
pub fn set_my_answer_for_uid(
    conn: &Connection,
    account_id: AccountId,
    uid: &str,
    occurrence: Option<EpochMillis>,
    answer: Answer,
) -> Result<()> {
    if uid.trim().is_empty() {
        return Ok(());
    }
    let rows = "SELECT calendar, id FROM events WHERE account_id = ?1 AND lower(uid) = lower(?2) \
         AND (?3 IS NULL OR original_start = ?3)";
    conn.execute(
        &format!("UPDATE event_guests SET answer = ?4 WHERE account_id = ?1 AND me = 1 AND (calendar, event) IN ({rows})"),
        params![account_id, uid, occurrence, answer.as_str()],
    )?;
    conn.execute(
        "UPDATE events SET my_answer = ?4 WHERE account_id = ?1 AND lower(uid) = lower(?2) \
         AND (?3 IS NULL OR original_start = ?3)",
        params![account_id, uid, occurrence, answer.as_str()],
    )?;
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
    e.my_answer, e.reminders, e.conference, e.rules, e.series, e.original_start, e.pending, e.sequence, e.kind, \
    e.attachments";

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

/// A series' changed and cancelled occurrences with their guests, earliest
/// original start first.
pub fn changed_occurrences(
    conn: &Connection,
    account_id: AccountId,
    calendar: &str,
    series: &str,
) -> Result<Vec<Event>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM events e WHERE e.account_id = ?1 AND e.calendar = ?2 AND e.series = ?3 \
         ORDER BY e.original_start"
    ))?;
    let mut found: Vec<Event> =
        stmt.query_map(params![account_id, calendar, series], read_event)?.collect::<rusqlite::Result<_>>()?;
    for event in &mut found {
        event.guests = guests(conn, account_id, calendar, &event.id)?;
    }
    Ok(found)
}

/// The attachments to store for `event`. A change made here (`pending`)
/// takes its list as it is. A list from the provider keeps what only this
/// computer knows: the files still waiting to upload, and which uploaded
/// files are shared with whom (`calendar::keep_local`).
fn kept_attachments(conn: &Connection, account_id: AccountId, event: &Event) -> Result<Option<Vec<model::Attachment>>> {
    let Some(fresh) = &event.attachments else {
        return Ok(None);
    };
    let mut fresh = fresh.clone();
    if !event.pending {
        let held: Option<Option<String>> = conn
            .query_row(
                "SELECT attachments FROM events WHERE account_id = ?1 AND calendar = ?2 AND id = ?3",
                params![account_id, event.calendar, event.id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(Some(text)) = held {
            model::keep_local(&mut fresh, &parse::<Vec<model::Attachment>>(&text));
        }
    }
    Ok(Some(fresh))
}

/// Writes `list` as the event's attachments in queued change `seq`'s body
/// and in the copy's row: the queue's uploads and shares, recorded before
/// the write goes out, so a write that fails after them does not repeat
/// them.
pub fn replace_attachments(
    conn: &Connection,
    account_id: AccountId,
    seq: i64,
    calendar: &str,
    id: &str,
    list: &[model::Attachment],
) -> Result<()> {
    let body: Option<Option<String>> = conn
        .query_row("SELECT body FROM calendar_changes WHERE seq = ?1", params![seq], |row| row.get(0))
        .optional()?;
    if let Some(body) = body.flatten() {
        let mut event: Event = parse(&body);
        if event.attachments.is_some() {
            event.attachments = Some(list.to_vec());
            conn.execute("UPDATE calendar_changes SET body = ?2 WHERE seq = ?1", params![seq, json(&event)])?;
        }
    }
    conn.execute(
        "UPDATE events SET attachments = ?4 WHERE account_id = ?1 AND calendar = ?2 AND id = ?3 \
         AND attachments IS NOT NULL",
        params![account_id, calendar, id, json(list)],
    )?;
    Ok(())
}

/// The events, by calendar and id, holding a file that waits for the
/// account to grant Drive.
pub fn waiting_for_access(conn: &Connection, account_id: AccountId) -> Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT calendar, id FROM events WHERE account_id = ?1 AND attachments LIKE '%\"problem\":\"NeedsAccess\"%' \
         ORDER BY calendar, id",
    )?;
    let rows = stmt.query_map(params![account_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Marks an event as matching the provider again once its queued change
/// went out. A cancelled occurrence needs this: its row stays in the copy
/// after its removal went out, and would otherwise show as waiting.
pub fn settle(conn: &Connection, account_id: AccountId, calendar: &str, id: &str) -> Result<()> {
    conn.execute(
        "UPDATE events SET pending = 0 WHERE account_id = ?1 AND calendar = ?2 AND id = ?3",
        params![account_id, calendar, id],
    )?;
    Ok(())
}

/// Moves event `from` to the id `to`, for a provider that answers a
/// create with an id of its own: the row, its guests, the changed
/// occurrences that name it as their series, the reminders already
/// shown, every queued change for it (whose bodies carry the id too) and
/// the change held for Undo. A later change then goes to the event the
/// provider knows. Runs inside the caller's write transaction.
pub fn rename_event(conn: &Connection, account_id: AccountId, calendar: &str, from: &str, to: &str) -> Result<()> {
    if from == to {
        return Ok(());
    }
    // Guests point at their event with no cascade on a change of id.
    // Checking the keys at the end of the transaction lets every table
    // move before any is checked.
    conn.pragma_update(None, "defer_foreign_keys", true)?;
    let at = params![account_id, calendar, from, to];
    for table in [
        "events SET id",
        "events SET series",
        "event_guests SET event",
        "event_reminders_shown SET event",
        "calendar_changes SET event",
    ] {
        let column = table.rsplit(' ').next().unwrap_or("id");
        conn.execute(&format!("UPDATE {table} = ?4 WHERE account_id = ?1 AND calendar = ?2 AND {column} = ?3"), at)?;
    }
    // A queued event carries its id, and a changed occurrence its series,
    // in its body too. A move's body names its destination in `calendar`.
    for column in ["body", "prior_body", "restores"] {
        for field in ["id", "series"] {
            conn.execute(
                &format!(
                    "UPDATE calendar_changes SET {column} = json_set({column}, '$.{field}', ?4) \
                     WHERE account_id = ?1 AND {column} IS NOT NULL \
                     AND json_extract({column}, '$.calendar') = ?2 AND json_extract({column}, '$.{field}') = ?3"
                ),
                at,
            )?;
        }
    }
    let held: Option<(String, Option<String>)> = conn
        .query_row("SELECT steps, before FROM calendar_holds WHERE account_id = ?1", params![account_id], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .optional()?;
    if let Some((steps, before)) = held {
        let rename = |event: &mut Event| {
            if event.calendar == calendar && event.id == from {
                event.id = to.to_string();
            }
            if event.calendar == calendar && event.series.as_deref() == Some(from) {
                event.series = Some(to.to_string());
            }
        };
        let mut steps: Vec<Step> = parse(&steps);
        for step in &mut steps {
            match step {
                Step::Save(event) | Step::Cancel(event) => rename(event),
                Step::Remove { calendar: on, id } | Step::Move { to: on, id, .. } => {
                    if on == calendar && id == from {
                        *id = to.to_string();
                    }
                }
            }
        }
        let mut before: Vec<Event> = before.as_deref().map(parse).unwrap_or_default();
        before.iter_mut().for_each(rename);
        conn.execute(
            "UPDATE calendar_holds SET steps = ?2, before = ?3 WHERE account_id = ?1",
            params![account_id, json(&steps), json(&before)],
        )?;
    }
    Ok(())
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
        // occurrence does not load on every call.
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
        found.extend(expand_rows(conn, account_id, events, from, to)?);
    }
    found.sort_by(|a, b| (a.start, &a.event.title).cmp(&(b.start, &b.event.title)));
    found.truncate(MOST_EVENTS);
    Ok(found)
}

/// Turns one account's rows into their occurrences between `from` and
/// `to`: a series gives one per repeat, a changed or cancelled occurrence
/// takes the place of the one it names, and a cancelled row gives nothing.
fn expand_rows(
    conn: &Connection,
    account_id: AccountId,
    events: Vec<Event>,
    from: EpochMillis,
    to: EpochMillis,
) -> Result<Vec<Occurrence>> {
    // What each changed occurrence replaces: (calendar, series, original start).
    let replaced: HashSet<(String, String, EpochMillis)> = events
        .iter()
        .filter_map(|e| Some((e.calendar.clone(), e.series.clone()?, e.original_start?)))
        .collect();
    let mut found = Vec::new();
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
    Ok(found)
}

/// The occurrences between `from` and `to` of the events whose iCalendar
/// UID is `uid`, on the account's shown calendars, earliest first. A
/// series and the occurrences someone changed share one UID, so the
/// answer holds the series with its changes in place. Show in Calendar
/// uses this to find the event an invitation names.
pub fn with_uid(
    conn: &Connection,
    account_id: AccountId,
    uid: &str,
    from: EpochMillis,
    to: EpochMillis,
) -> Result<Vec<Occurrence>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM events e JOIN calendars c ON c.account_id = e.account_id AND c.id = e.calendar \
         WHERE e.account_id = ?1 AND c.shown = 1 AND lower(e.uid) = lower(?2)"
    ))?;
    let events: Vec<Event> =
        stmt.query_map(params![account_id, uid], read_event)?.collect::<rusqlite::Result<_>>()?;
    let mut found = expand_rows(conn, account_id, events, from, to)?;
    found.sort_by(|a, b| (a.start, &a.event.title).cmp(&(b.start, &b.event.title)));
    Ok(found)
}

/// The invitations on the accounts' shown calendars that still want the
/// account's own answer, each at its next occurrence that has not ended
/// by `now`, nearest first, at most `limit` of them. An invitation is a
/// row with a guest marked `me`, no answer of the account's, not
/// cancelled, and not over. The query filters in SQL before it caps, so
/// a calendar full of other events cannot push an invitation out, and it
/// expands only the rows it keeps, never a year of every series.
///
/// A series and the occurrences someone changed share a uid, so one
/// invitation keeps one row: the one whose occurrence comes first.
pub fn waiting(
    conn: &Connection,
    accounts: &[AccountId],
    now: EpochMillis,
    limit: usize,
) -> Result<Vec<Occurrence>> {
    // A row per invitation is the common case; the spare half covers a
    // series and its changed occurrences, which collapse to one below.
    let rows = limit.saturating_mul(2);
    let mut nearest: HashMap<(AccountId, String, String), Occurrence> = HashMap::new();
    for &account_id in accounts {
        let mut stmt = conn.prepare(&format!(
            "SELECT {COLUMNS} FROM events e \
             JOIN calendars c ON c.account_id = e.account_id AND c.id = e.calendar \
             WHERE e.account_id = ?1 AND c.shown = 1 AND e.my_answer IS NULL AND e.status <> 'cancelled' \
               AND EXISTS (SELECT 1 FROM event_guests g WHERE g.account_id = e.account_id \
                   AND g.calendar = e.calendar AND g.event = e.id AND g.me = 1) \
               AND ((e.rules = '' AND (e.ends_at > ?2 OR (e.ends_at = e.starts_at AND e.starts_at >= ?2))) \
                 OR (e.rules <> '' AND (e.series_end IS NULL OR e.series_end > ?2))) \
             ORDER BY MAX(e.starts_at, ?2), e.title LIMIT ?3"
        ))?;
        let events: Vec<Event> = stmt
            .query_map(params![account_id, now, rows as i64], read_event)?
            .collect::<rusqlite::Result<_>>()?;
        for event in events {
            let Some((start, end)) = next_after(conn, account_id, &event, now)? else {
                continue;
            };
            let same = if event.uid.is_empty() { event.id.clone() } else { event.uid.to_lowercase() };
            let key = (account_id, event.calendar.clone(), same);
            if nearest.get(&key).is_some_and(|kept| kept.start <= start) {
                continue;
            }
            let event = Arc::new(event);
            nearest.insert(key, Occurrence { account_id, event, start, end });
        }
    }
    let mut found: Vec<Occurrence> = nearest.into_values().collect();
    found.sort_by(|a, b| (a.start, &a.event.title).cmp(&(b.start, &b.event.title)));
    found.truncate(limit);
    for occurrence in &mut found {
        let event = Arc::make_mut(&mut occurrence.event);
        event.guests = guests(conn, occurrence.account_id, &event.calendar, &event.id)?;
    }
    Ok(found)
}

/// The first occurrence of `event` that has not ended by `now`, within a
/// year, skipping the ones a changed or cancelled occurrence replaces.
fn next_after(
    conn: &Connection,
    account_id: AccountId,
    event: &Event,
    now: EpochMillis,
) -> Result<Option<(EpochMillis, EpochMillis)>> {
    if event.rules.is_empty() {
        return Ok(model::expand(event, now, now + MAX_RANGE).first().copied());
    }
    let mut stmt = conn.prepare(
        "SELECT original_start FROM events WHERE account_id = ?1 AND calendar = ?2 AND series = ?3 \
         AND original_start IS NOT NULL",
    )?;
    let replaced: HashSet<EpochMillis> = stmt
        .query_map(params![account_id, event.calendar, event.id], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(model::expand(event, now, now + MAX_RANGE)
        .into_iter()
        .find(|(start, _)| !replaced.contains(start)))
}

/// The SQL function that reads an event description the way the editor's
/// Notes field does, turning Google's HTML into the words it shows
/// (`mailrs_mime::notes::text`), so a search matches "bread" split by a
/// `<b>` and never matches a `<br>` tag as the word "br".
pub const NOTES_TEXT: &str = "penguin_notes_text";

/// Events whose title, place, description or a guest's name or address
/// holds `text`, matched with full Unicode folding through
/// [`crate::query::FOLD`] rather than SQLite's ASCII-only `lower`. Each
/// match becomes its next occurrence at or after `from`, within a year
/// ahead; or its last occurrence within a year before `from` when none
/// is ahead; or the event's own span. A series and one of its changed
/// occurrences share a uid and can both match a search on their shared
/// title; only the earlier coming one of the pair stays. Coming events
/// sort earliest first, then past ones latest first.
///
/// Reading stops at [`MOST_EVENTS`] rows per account, so a word every
/// event shares cannot make a search load a whole large calendar; the
/// SQL keeps the rows the cap should not drop, ordered ahead of the
/// rest: a still-running series (its stored span may be its first
/// occurrence, long past, so any series counts as coming) or an event
/// whose own span has not ended, soonest first, then past events, most
/// recent first. The final ranking above still re-sorts what the cap
/// let through by each match's true next occurrence.
pub fn search(
    conn: &Connection,
    accounts: &[AccountId],
    text: &str,
    from: EpochMillis,
    reach: CalendarScope,
    limit: usize,
) -> Result<Vec<Occurrence>> {
    let calendar_filter = match reach {
        CalendarScope::Shown => "AND c.shown = 1",
        CalendarScope::All => "",
        CalendarScope::Owned => "AND c.access = 'owner'",
    };
    let fold = crate::query::FOLD;
    let notes_text = NOTES_TEXT;
    let needle = text.to_lowercase();
    // The best-ranked occurrence for one (account, calendar, uid) group,
    // so a series and its own changed occurrence collapse to one result.
    // `past` and `rank` are the sort key: coming events (`past` false)
    // before past ones, earliest coming or latest past first.
    type Ranked = (bool, EpochMillis, AccountId, Event, EpochMillis, EpochMillis);
    let mut ranked: HashMap<(AccountId, String, String), Ranked> = HashMap::new();
    for &account_id in accounts {
        let mut stmt = conn.prepare(&format!(
            "SELECT DISTINCT {COLUMNS} FROM events e \
             JOIN calendars c ON c.account_id = e.account_id AND c.id = e.calendar \
             LEFT JOIN event_guests g ON g.account_id = e.account_id AND g.calendar = e.calendar AND g.event = e.id \
             WHERE e.account_id = ?1 {calendar_filter} AND e.status <> 'cancelled' AND ( \
               instr({fold}(e.title), ?2) > 0 OR instr({fold}(e.place), ?2) > 0 \
               OR instr({fold}({notes_text}(e.description)), ?2) > 0 OR instr({fold}(g.email), ?2) > 0 \
               OR instr({fold}(coalesce(g.name, '')), ?2) > 0) \
             ORDER BY \
               CASE WHEN e.rules <> '' OR e.ends_at >= ?3 THEN 0 ELSE 1 END, \
               CASE WHEN e.rules <> '' OR e.ends_at >= ?3 THEN e.starts_at ELSE -e.starts_at END \
             LIMIT {MOST_EVENTS}"
        ))?;
        let events: Vec<Event> =
            stmt.query_map(params![account_id, needle, from], read_event)?.collect::<rusqlite::Result<_>>()?;
        for event in events {
            let (start, end) = next_showing(&event, from);
            let past = start < from;
            let rank = if past { -start } else { start };
            let key = (account_id, event.calendar.clone(), event.uid.clone());
            let keep = ranked.get(&key).is_none_or(|(p, r, ..)| (past, rank) < (*p, *r));
            if keep {
                ranked.insert(key, (past, rank, account_id, event, start, end));
            }
        }
    }
    let mut found: Vec<Ranked> = ranked.into_values().collect();
    found.sort_by_key(|(past, rank, ..)| (*past, *rank));
    found.truncate(limit);
    let mut occurrences = Vec::with_capacity(found.len());
    for (_, _, account_id, mut event, start, end) in found {
        event.guests = guests(conn, account_id, &event.calendar, &event.id)?;
        occurrences.push(Occurrence { account_id, event: Arc::new(event), start, end });
    }
    Ok(occurrences)
}

/// The event's next occurrence at or after `from`, within a year ahead;
/// else its last occurrence within a year before `from`; else the
/// event's own span. `MAX_RANGE` keeps each expansion to a year, so a
/// long-running daily series cannot make a search walk its whole run.
fn next_showing(event: &Event, from: EpochMillis) -> (EpochMillis, EpochMillis) {
    if let Some(&showing) = model::expand(event, from, from + MAX_RANGE).first() {
        return showing;
    }
    if let Some(&showing) = model::expand(event, from.saturating_sub(MAX_RANGE), from).last() {
        return showing;
    }
    (event.start, event.end)
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
        sequence: row.get(24)?,
        guests: Vec::new(),
        // A Meet request lives only in a queued write, never in a row
        // the store reads back.
        meet_request: None,
        kind: row.get::<_, Option<String>>(25)?.map(|k| parse(&k)).unwrap_or_default(),
        attachments: row.get::<_, Option<String>>(26)?.map(|a| parse(&a)),
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
    /// Move the event from the row's calendar to the one its body names.
    Move,
    /// The account's answer to an invitation, as a guest, on the row's
    /// event: a series' own id answers every occurrence, an occurrence's
    /// id that one alone. It carries a [`QueuedAnswer`] and no body, and
    /// goes out apart from any edit of the same event, since an edit
    /// never writes the guest list of an event the account only attends.
    Answer,
}

impl ChangeKind {
    fn as_str(self) -> &'static str {
        match self {
            ChangeKind::Create => "create",
            ChangeKind::Save => "save",
            ChangeKind::Remove => "remove",
            ChangeKind::Move => "move",
            ChangeKind::Answer => "answer",
        }
    }

    fn parse(word: &str) -> ChangeKind {
        match word {
            "create" => ChangeKind::Create,
            "remove" => ChangeKind::Remove,
            "move" => ChangeKind::Move,
            "answer" => ChangeKind::Answer,
            _ => ChangeKind::Save,
        }
    }
}

/// One change waiting for the provider. Named apart from the glossary's
/// "Queued message" (`mailrs_store::outbox::Queued`), which is something
/// else.
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
    /// The event to write. For a move, the event as it was, with the
    /// calendar it moves to; `calendar` above is the one it leaves.
    pub body: Option<Event>,
    /// The `seq` of the change this one goes out after, while that one is
    /// still queued. See [`enqueue_after`].
    pub waits_on: Option<i64>,
    /// The old series as it was before a split, on the new series' row,
    /// to put back if the provider turns this row down.
    pub restores: Option<Event>,
    /// Whether the provider mails the guests about this change.
    pub notify: Notify,
    /// What an [`ChangeKind::Answer`] row says; `None` on any other.
    pub answer: Option<QueuedAnswer>,
}

/// A guest's answer waiting for the provider.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct QueuedAnswer {
    /// The account's address, which finds its own entry on the guest list.
    pub me: String,
    pub answer: Answer,
    /// The note the organizer reads with the answer. `None` leaves any
    /// earlier note as the provider holds it.
    pub note: Option<String>,
    /// The event's title, for saying which answer the provider turned down.
    pub title: String,
}

/// A queued change dropped unsent because a change it waited on was
/// turned down.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dropped {
    /// The row is gone; the event's copy row should go too.
    Gone { calendar: String, event: String },
    /// A waiting step had folded into an earlier change of this event,
    /// which is queued again as it was: `body` is that change's event, or
    /// `None` for a removal.
    Kept { calendar: String, event: String, body: Option<Box<Event>> },
}

/// Queues a change for the provider. One row holds the latest change for
/// an event, so an event edited several times offline queues one body
/// rather than one row per edit:
/// - a Save meeting an unsent Create or Save replaces that row's body;
/// - a Remove meeting an unsent Create drops the row: the provider never
///   heard of the event, so there is nothing left to tell it;
/// - a Remove meeting an unsent Save turns that row into the Remove.
/// - a Remove meeting an unsent Remove changes nothing;
/// - a Save meeting an unsent Remove turns that row into the Save.
pub fn enqueue(conn: &Connection, account_id: AccountId, kind: ChangeKind, event: &Event) -> Result<()> {
    enqueue_after(conn, account_id, kind, event, None, None, Notify::Guests).map(drop)
}

/// [`enqueue`], with the change held back until the row `waits_on` names
/// has left the queue, and dropped unsent when that row is turned down
/// (see [`drop_waiting_on`]). `restores` is kept on the row for the
/// sender to put back if the provider turns it down. `notify` says
/// whether the provider mails the guests; see [`folded_notify`] for a
/// change that folds into one already queued. Answers the `seq` of the
/// row that now holds the change, or `None` when the change cancelled an
/// unsent create.
pub fn enqueue_after(
    conn: &Connection,
    account_id: AccountId,
    kind: ChangeKind,
    event: &Event,
    waits_on: Option<i64>,
    restores: Option<&Event>,
    notify: Notify,
) -> Result<Option<i64>> {
    let existing: Option<(i64, String, Option<String>)> = conn
        .query_row(
            "SELECT seq, kind, notify FROM calendar_changes \
             WHERE account_id = ?1 AND calendar = ?2 AND event = ?3 AND kind NOT IN ('move', 'answer')",
            params![account_id, event.calendar, event.id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    if let Some((seq, queued_kind, queued)) = &existing {
        let folded = folded_notify(ChangeKind::parse(queued_kind), Notify::from_stored(queued.as_deref()), kind, notify);
        conn.execute("UPDATE calendar_changes SET notify = ?2 WHERE seq = ?1", params![seq, folded.stored()])?;
    }
    let existing = existing.map(|(seq, kind, _)| (seq, kind));
    // A waiting step that folds into an earlier change keeps that change,
    // so dropping the step can put it back.
    if let (Some((seq, _)), Some(_)) = (&existing, waits_on) {
        conn.execute(
            "UPDATE calendar_changes SET prior_kind = kind, prior_body = body \
             WHERE seq = ?1 AND prior_kind IS NULL",
            params![seq],
        )?;
    }
    let restores = restores.map(json);
    let waited = |seq: i64| -> Result<Option<i64>> {
        conn.execute(
            "UPDATE calendar_changes SET waits_on = COALESCE(?2, waits_on), restores = COALESCE(?3, restores) \
             WHERE seq = ?1",
            params![seq, waits_on, restores],
        )?;
        Ok(Some(seq))
    };

    if let Some((seq, existing_kind)) = existing {
        match (kind, existing_kind.as_str()) {
            (ChangeKind::Remove, "create") => {
                conn.execute("DELETE FROM calendar_changes WHERE seq = ?1", params![seq])?;
                return Ok(None);
            }
            (ChangeKind::Remove, "save") => {
                conn.execute(
                    "UPDATE calendar_changes SET kind = 'remove', body = NULL WHERE seq = ?1",
                    params![seq],
                )?;
                return waited(seq);
            }
            (ChangeKind::Save, "create") | (ChangeKind::Save, "save") => {
                conn.execute("UPDATE calendar_changes SET body = ?2 WHERE seq = ?1", params![seq, json(event)])?;
                return waited(seq);
            }
            // The removal already covers the event.
            (ChangeKind::Remove, "remove") => return waited(seq),
            // The provider still holds the event, since its removal never
            // went out, so the edit changes it against the same version.
            (ChangeKind::Save, "remove") => {
                conn.execute(
                    "UPDATE calendar_changes SET kind = 'save', body = ?2 WHERE seq = ?1",
                    params![seq, json(event)],
                )?;
                return waited(seq);
            }
            _ => {}
        }
    }

    let etag = (!event.etag.is_empty()).then_some(event.etag.as_str());
    let body = match kind {
        ChangeKind::Create | ChangeKind::Save | ChangeKind::Move => Some(json(event)),
        ChangeKind::Remove | ChangeKind::Answer => None,
    };
    conn.execute(
        "INSERT INTO calendar_changes (account_id, calendar, event, kind, etag, body, waits_on, restores, notify) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![account_id, event.calendar, event.id, kind.as_str(), etag, body, waits_on, restores, notify.stored()],
    )?;
    Ok(Some(conn.last_insert_rowid()))
}

/// Queues the account's answer to event `id` on `calendar`. An answer
/// still unsent for the same event takes the new one in its place, so
/// the organizer hears the last word once; its note stays unless the new
/// answer brings one.
pub fn enqueue_answer(
    conn: &Connection,
    account_id: AccountId,
    calendar: &str,
    id: &str,
    answer: &QueuedAnswer,
) -> Result<()> {
    let existing: Option<(i64, Option<String>)> = conn
        .query_row(
            "SELECT seq, body FROM calendar_changes \
             WHERE account_id = ?1 AND calendar = ?2 AND event = ?3 AND kind = 'answer'",
            params![account_id, calendar, id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    match existing {
        Some((seq, body)) => {
            let earlier = body.and_then(|b| serde_json::from_str::<QueuedAnswer>(&b).ok());
            let merged = QueuedAnswer {
                note: answer.note.clone().or_else(|| earlier.and_then(|e| e.note)),
                ..answer.clone()
            };
            conn.execute("UPDATE calendar_changes SET body = ?2 WHERE seq = ?1", params![seq, json(&merged)])?;
        }
        None => {
            conn.execute(
                "INSERT INTO calendar_changes (account_id, calendar, event, kind, body) \
                 VALUES (?1, ?2, ?3, 'answer', ?4)",
                params![account_id, calendar, id, json(answer)],
            )?;
        }
    }
    Ok(())
}

/// Settles an answer the provider took: the row comes off, the event's
/// copy takes `new_etag`, and an edit of the event queued behind the
/// answer moves to that version too. The answer is the only change
/// between the two versions, and an edit never writes the guest list, so
/// the edit still says what the person meant. The event stops showing as
/// waiting once nothing else is queued for it.
pub fn finish_answer(
    conn: &Connection,
    account_id: AccountId,
    seq: i64,
    calendar: &str,
    id: &str,
    new_etag: &str,
) -> Result<()> {
    conn.execute("DELETE FROM calendar_changes WHERE seq = ?1", params![seq])?;
    conn.execute(
        "UPDATE calendar_changes SET etag = ?4 \
         WHERE account_id = ?1 AND calendar = ?2 AND event = ?3 AND kind = 'save'",
        params![account_id, calendar, id, new_etag],
    )?;
    conn.execute(
        "UPDATE events SET etag = ?4 WHERE account_id = ?1 AND calendar = ?2 AND id = ?3",
        params![account_id, calendar, id, new_etag],
    )?;
    let still_queued: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM calendar_changes WHERE account_id = ?1 AND calendar = ?2 AND event = ?3)",
        params![account_id, calendar, id],
        |row| row.get(0),
    )?;
    if !still_queued {
        settle(conn, account_id, calendar, id)?;
    }
    Ok(())
}

/// Queues moving `event`, as it stands on `from`, to the calendar it
/// names, after the row `waits_on`. The row sits under `from`, so a read
/// of that calendar leaves the event and a series' changed occurrences
/// alone until the move goes out. An unsent create of the event on `from`
/// needs no move, since the provider never heard of it: the row goes, and
/// the save that follows the move creates the event where it now lives.
/// Answers the `seq` of the move's row, or `None` for that create.
pub fn enqueue_move(
    conn: &Connection,
    account_id: AccountId,
    from: &str,
    event: &Event,
    waits_on: Option<i64>,
    notify: Notify,
) -> Result<Option<i64>> {
    let dropped = conn.execute(
        "DELETE FROM calendar_changes WHERE account_id = ?1 AND calendar = ?2 AND event = ?3 AND kind = 'create'",
        params![account_id, from, event.id],
    )?;
    if dropped > 0 {
        return Ok(None);
    }
    let etag = (!event.etag.is_empty()).then_some(event.etag.as_str());
    conn.execute(
        "INSERT INTO calendar_changes (account_id, calendar, event, kind, etag, body, waits_on, notify) \
         VALUES (?1, ?2, ?3, 'move', ?4, ?5, ?6, ?7)",
        params![account_id, from, event.id, etag, json(event), waits_on, notify.stored()],
    )?;
    Ok(Some(conn.last_insert_rowid()))
}

/// Settles a move that went out: the row comes off the queue, and the
/// changes of the event waiting on it go out against `new_etag`, the
/// version the move left. Answers whether one waits, in which case its
/// own answer, not the move's, belongs in the copy.
pub fn finish_move(conn: &Connection, seq: i64, id: &str, new_etag: &str) -> Result<bool> {
    conn.execute("DELETE FROM calendar_changes WHERE seq = ?1", params![seq])?;
    let waiting = conn.execute(
        "UPDATE calendar_changes SET etag = ?3 WHERE waits_on = ?1 AND event = ?2",
        params![seq, id, new_etag],
    )?;
    Ok(waiting > 0)
}

/// Whether the guests hear of a change that folds into one already
/// queued for the same event. A delete replacing an unsent edit, or an
/// edit replacing an unsent delete, goes out as the newer change asked,
/// since the guests never heard of the one it replaces. Two edits, or an
/// edit onto an unsent create, tell the guests if either did: the
/// earlier one may be what invites them.
fn folded_notify(queued_kind: ChangeKind, queued: Notify, kind: ChangeKind, notify: Notify) -> Notify {
    if (queued_kind == ChangeKind::Remove) == (kind == ChangeKind::Remove) {
        queued.and(notify)
    } else {
        notify
    }
}

/// A row's `prior_kind` and `prior_body`.
type Prior = (Option<String>, Option<String>);

/// Drops, unsent, the changes waiting on the row `seq`, whose own change
/// the provider turned down, and the ones waiting on those in turn. A
/// row a waiting step folded into goes back to the change it held
/// before, and stays queued.
pub fn drop_waiting_on(conn: &Connection, seq: i64) -> Result<Vec<Dropped>> {
    let mut dropped = Vec::new();
    let mut leads = vec![seq];
    while let Some(lead) = leads.pop() {
        let waiting: Vec<(i64, String, String, Prior)> = {
            let mut stmt = conn.prepare(
                "SELECT seq, calendar, event, prior_kind, prior_body FROM calendar_changes WHERE waits_on = ?1",
            )?;
            let rows = stmt.query_map(params![lead], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, (row.get(3)?, row.get(4)?)))
            })?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        for (seq, calendar, event, (prior_kind, prior_body)) in waiting {
            leads.push(seq);
            match prior_kind {
                Some(kind) => {
                    conn.execute(
                        "UPDATE calendar_changes SET kind = ?2, body = ?3, waits_on = NULL, restores = NULL, \
                         prior_kind = NULL, prior_body = NULL WHERE seq = ?1",
                        params![seq, kind, prior_body],
                    )?;
                    let body = prior_body.and_then(|b| serde_json::from_str(&b).ok());
                    dropped.push(Dropped::Kept { calendar, event, body });
                }
                None => {
                    conn.execute("DELETE FROM calendar_changes WHERE seq = ?1", params![seq])?;
                    dropped.push(Dropped::Gone { calendar, event });
                }
            }
        }
    }
    Ok(dropped)
}

/// The first change still waiting on the row `seq`, once that row has
/// gone out, so a send that already walked past it can go back.
pub fn first_waiting_on(conn: &Connection, seq: i64) -> Result<Option<i64>> {
    Ok(conn.query_row(
        "SELECT MIN(seq) FROM calendar_changes WHERE waits_on = ?1",
        params![seq],
        |row| row.get(0),
    )?)
}

pub fn queued(conn: &Connection, account_id: AccountId) -> Result<Vec<QueuedChange>> {
    let mut stmt = conn.prepare(
        "SELECT seq, calendar, event, kind, etag, body, waits_on, restores, notify FROM calendar_changes \
         WHERE account_id = ?1 ORDER BY seq",
    )?;
    let rows = stmt.query_map(params![account_id], |row| read_change(row, account_id))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The account's first queued change after `after`, as it stands now. A
/// send reads each change just before it goes out, so a change deleted
/// or edited since the send began goes out as it is now or not at all,
/// and one queued during the send still goes out in the same send. A
/// change waiting on a row still queued is passed over.
pub fn next_change(conn: &Connection, account_id: AccountId, after: i64) -> Result<Option<QueuedChange>> {
    Ok(conn
        .query_row(
            "SELECT seq, calendar, event, kind, etag, body, waits_on, restores, notify FROM calendar_changes c \
             WHERE account_id = ?1 AND seq > ?2 \
             AND (waits_on IS NULL OR NOT EXISTS (SELECT 1 FROM calendar_changes w WHERE w.seq = c.waits_on)) \
             ORDER BY seq LIMIT 1",
            params![account_id, after],
            |row| read_change(row, account_id),
        )
        .optional()?)
}

fn read_change(row: &Row, account_id: AccountId) -> rusqlite::Result<QueuedChange> {
    let kind = ChangeKind::parse(&row.get::<_, String>(3)?);
    let body = row.get::<_, Option<String>>(5)?;
    // An answer's body is the answer, which must not be read as an event.
    let (body, answer) = match kind {
        ChangeKind::Answer => (None, body.and_then(|b| serde_json::from_str(&b).ok())),
        _ => (body.and_then(|b| serde_json::from_str(&b).ok()), None),
    };
    Ok(QueuedChange {
        seq: row.get(0)?,
        account_id,
        calendar: row.get(1)?,
        event: row.get(2)?,
        kind,
        etag: row.get(4)?,
        body,
        answer,
        waits_on: row.get(6)?,
        restores: row.get::<_, Option<String>>(7)?.and_then(|b| serde_json::from_str(&b).ok()),
        notify: Notify::from_stored(row.get::<_, Option<String>>(8)?.as_deref()),
    })
}

/// The ids of a calendar's events with an unsent change, without loading
/// every queued body.
pub fn pending_ids(conn: &Connection, account_id: AccountId, calendar: &str) -> Result<HashSet<String>> {
    let mut stmt =
        conn.prepare("SELECT event FROM calendar_changes WHERE account_id = ?1 AND calendar = ?2")?;
    let rows = stmt.query_map(params![account_id, calendar], |row| row.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The ids of a calendar's events with an unsent removal or move away. A
/// read keeps a series' changed occurrences out while the series waits
/// to leave.
pub fn removing_ids(conn: &Connection, account_id: AccountId, calendar: &str) -> Result<HashSet<String>> {
    let mut stmt = conn.prepare(
        "SELECT event FROM calendar_changes WHERE account_id = ?1 AND calendar = ?2 AND kind IN ('remove', 'move')",
    )?;
    let rows = stmt.query_map(params![account_id, calendar], |row| row.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Settles a removal that went out: the row comes off the queue and the
/// event, a cancelled occurrence still in the copy, stops waiting. An
/// edit made while the removal was in flight turned the row into a save,
/// which stays queued, and the event keeps waiting for it.
pub fn finish_removal(conn: &Connection, account_id: AccountId, seq: i64) -> Result<()> {
    let removed: Option<(String, String)> = conn
        .query_row(
            "DELETE FROM calendar_changes WHERE seq = ?1 AND kind = 'remove' RETURNING calendar, event",
            params![seq],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((calendar, id)) = removed {
        settle(conn, account_id, &calendar, &id)?;
    }
    Ok(())
}

pub fn dequeue(conn: &Connection, seq: i64) -> Result<()> {
    conn.execute("DELETE FROM calendar_changes WHERE seq = ?1", params![seq])?;
    Ok(())
}

/// Persists the steps of a change waiting on its Undo toast, replacing
/// any row already there: only one change is held at a time. A crash or
/// a quit before the toast closes leaves this row for the next start to
/// queue, since no toast survives to close over it.
pub fn save_holding(
    conn: &Connection,
    account_id: AccountId,
    steps: &[Step],
    before: &[Event],
    notify: Notify,
) -> Result<()> {
    conn.execute(
        "INSERT INTO calendar_holds (account_id, steps, before, notify) VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT (account_id) DO UPDATE SET steps = excluded.steps, before = excluded.before, \
         notify = excluded.notify",
        params![account_id, json(steps), json(before), notify.stored()],
    )?;
    Ok(())
}

/// Drops the account's persisted held change, once it is queued or taken
/// back.
pub fn clear_holding(conn: &Connection, account_id: AccountId) -> Result<()> {
    conn.execute("DELETE FROM calendar_holds WHERE account_id = ?1", params![account_id])?;
    Ok(())
}

/// A held change persisted for the next start: its account, its steps,
/// the rows they replaced, and whether the guests hear of it.
pub type Holding = (AccountId, Vec<Step>, Vec<Event>, Notify);

/// Every held change still persisted from a run that stopped before its
/// Undo toast closed, read once at start so it can be queued.
pub fn holdings(conn: &Connection) -> Result<Vec<Holding>> {
    let mut stmt = conn.prepare("SELECT account_id, steps, before, notify FROM calendar_holds")?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, AccountId>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
        ))
    })?;
    let mut found = Vec::new();
    for row in rows {
        let (account_id, steps, before, notify) = row?;
        found.push((
            account_id,
            parse(&steps),
            before.as_deref().map(parse).unwrap_or_default(),
            Notify::from_stored(notify.as_deref()),
        ));
    }
    Ok(found)
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

fn json<T: serde::Serialize + ?Sized>(value: &T) -> String {
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
    use mailrs_domain::calendar::{Access, Calendar, Event, Guest, Notify, Status};

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
            hidden: false,
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

    #[test]
    fn a_renamed_event_takes_its_guests_and_its_queued_change_along() {
        let (conn, id) = store();
        let mut series = event("primary", "pmlocal1", MONDAY, 1);
        series.guests = vec![Guest { email: "ann@example.com".into(), ..Guest::default() }];
        let mut changed = event("primary", "pmlocal1_x", MONDAY + DAY, 1);
        changed.series = Some("pmlocal1".into());
        save_events(&conn, id, &[series.clone(), changed], 1).unwrap();
        enqueue(&conn, id, ChangeKind::Save, &Event { title: "Later".into(), ..series.clone() }).unwrap();
        let held = [Step::Save(series.clone()), Step::Remove { calendar: "primary".into(), id: "pmlocal1".into() }];
        save_holding(&conn, id, &held, std::slice::from_ref(&series), Notify::Guests).unwrap();
        conn.execute("UPDATE calendar_changes SET prior_body = body, restores = body", []).unwrap();

        // `Db::write` runs every write in a transaction, which the
        // deferred key check needs.
        let tx = conn.unchecked_transaction().unwrap();
        rename_event(&tx, id, "primary", "pmlocal1", "AAMkAGraph=").unwrap();
        tx.commit().unwrap();

        assert!(super::event(&conn, id, "primary", "pmlocal1").unwrap().is_none());
        let moved = super::event(&conn, id, "primary", "AAMkAGraph=").unwrap().unwrap();
        assert_eq!(moved.guests.len(), 1);
        let occurrence = super::event(&conn, id, "primary", "pmlocal1_x").unwrap().unwrap();
        assert_eq!(occurrence.series.as_deref(), Some("AAMkAGraph="));
        let queued = next_change(&conn, id, 0).unwrap().unwrap();
        assert_eq!(queued.event, "AAMkAGraph=");
        assert_eq!(queued.body.unwrap().id, "AAMkAGraph=");
        let (prior, restores): (String, String) = conn
            .query_row("SELECT prior_body, restores FROM calendar_changes", [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert!(prior.contains("AAMkAGraph=") && restores.contains("AAMkAGraph="));
        let (_, steps, before, _) = holdings(&conn).unwrap().remove(0);
        assert_eq!(steps[0].key().1, "AAMkAGraph=");
        assert_eq!(steps[1].key().1, "AAMkAGraph=");
        assert_eq!(before[0].id, "AAMkAGraph=");
    }

    fn starts(found: &[Occurrence]) -> Vec<(String, EpochMillis)> {
        found.iter().map(|o| (o.event.id.clone(), o.start)).collect()
    }

    #[test]
    fn the_copy_keeps_what_sort_of_entry_an_event_is() {
        use mailrs_domain::calendar::{Decline, Declines, Kind, Workplace};
        let (conn, id) = store();
        let away = Kind::OutOfOffice(Decline { meetings: Declines::All, message: "Back Monday".into() });
        let events = [
            Event { kind: away.clone(), ..event("primary", "away", MONDAY, 8) },
            Event { kind: Kind::WorkingLocation(Workplace::Office("Lisbon HQ".into())), ..event("primary", "where", MONDAY, 24) },
            event("primary", "plain", MONDAY, 1),
        ];
        save_events(&conn, id, &events, 0).unwrap();
        let read = |event: &str| super::event(&conn, id, "primary", event).unwrap().unwrap().kind;
        assert_eq!(read("away"), away);
        assert_eq!(read("where"), Kind::WorkingLocation(Workplace::Office("Lisbon HQ".into())));
        assert_eq!(read("plain"), Kind::Event);
    }

    #[test]
    fn the_copy_keeps_an_events_attachments() {
        let (conn, account) = store();
        let file = mailrs_domain::calendar::Attachment {
            title: "Agenda.pdf".into(),
            file_url: "https://drive.google.com/file/d/1abc/view".into(),
            mime_type: "application/pdf".into(),
            icon_link: "https://drive-thirdparty.googleusercontent.com/16/type/application/pdf".into(),
            file_id: "1abc".into(),
            ..Default::default()
        };
        let waiting = mailrs_domain::calendar::Attachment {
            title: "Notes.txt".into(),
            mime_type: "text/plain".into(),
            waiting: Some("/home/me/Notes.txt".into()),
            ..Default::default()
        };
        let with = Event { attachments: Some(vec![file, waiting]), ..event("primary", "a", 0, 1) };
        let without = Event { attachments: Some(Vec::new()), ..event("primary", "b", 0, 1) };
        save_events(&conn, account, &[with.clone(), without], 0).unwrap();
        assert_eq!(super::event(&conn, account, "primary", "a").unwrap().unwrap().attachments, with.attachments);
        assert_eq!(super::event(&conn, account, "primary", "b").unwrap().unwrap().attachments, Some(Vec::new()));
    }

    fn uploaded(id: &str, share: bool) -> mailrs_domain::calendar::Attachment {
        mailrs_domain::calendar::Attachment {
            title: id.into(),
            file_url: format!("https://drive.google.com/file/d/{id}/view"),
            file_id: id.into(),
            share: Some(share),
            shared_with: vec!["ana@example.com".into()],
            ..Default::default()
        }
    }

    fn waiting_file(problem: Option<mailrs_domain::calendar::UploadProblem>) -> mailrs_domain::calendar::Attachment {
        mailrs_domain::calendar::Attachment {
            title: "Notes.txt".into(),
            waiting: Some("/home/me/Notes.txt".into()),
            share: Some(true),
            problem,
            ..Default::default()
        }
    }

    #[test]
    fn a_read_from_the_provider_keeps_waiting_files_and_sharing() {
        let (conn, account) = store();
        let mine = Event {
            pending: true,
            attachments: Some(vec![uploaded("1abc", false), waiting_file(None)]),
            ..event("primary", "a", 0, 1)
        };
        save_events(&conn, account, &[mine], 0).unwrap();
        // Google's copy knows the Drive file and nothing of the rest.
        let google = mailrs_domain::calendar::Attachment { share: None, shared_with: Vec::new(), ..uploaded("1abc", true) };
        let theirs = Event { attachments: Some(vec![google]), ..event("primary", "a", 0, 1) };
        save_events(&conn, account, &[theirs], 1).unwrap();
        let kept = super::event(&conn, account, "primary", "a").unwrap().unwrap().attachments.unwrap();
        assert_eq!(kept, vec![uploaded("1abc", false), waiting_file(None)]);
    }

    #[test]
    fn a_change_made_here_takes_the_list_as_it_is() {
        let (conn, account) = store();
        let with = Event { pending: true, attachments: Some(vec![waiting_file(None)]), ..event("primary", "a", 0, 1) };
        save_events(&conn, account, &[with], 0).unwrap();
        let removed = Event { pending: true, attachments: Some(Vec::new()), ..event("primary", "a", 0, 1) };
        save_events(&conn, account, &[removed], 1).unwrap();
        assert_eq!(super::event(&conn, account, "primary", "a").unwrap().unwrap().attachments, Some(Vec::new()));
    }

    #[test]
    fn new_attachments_go_into_the_queued_change_and_the_copy() {
        let (conn, account) = store();
        let with = Event { pending: true, attachments: Some(vec![waiting_file(None)]), ..event("primary", "a", 0, 1) };
        save_events(&conn, account, std::slice::from_ref(&with), 0).unwrap();
        enqueue(&conn, account, ChangeKind::Create, &with).unwrap();
        let seq = queued(&conn, account).unwrap()[0].seq;
        let list = vec![uploaded("1abc", true)];
        replace_attachments(&conn, account, seq, "primary", "a", &list).unwrap();
        assert_eq!(queued(&conn, account).unwrap()[0].body.as_ref().unwrap().attachments, Some(list.clone()));
        assert_eq!(super::event(&conn, account, "primary", "a").unwrap().unwrap().attachments, Some(list));
    }

    #[test]
    fn files_waiting_for_access_name_their_events() {
        let (conn, account) = store();
        let stuck = Event {
            attachments: Some(vec![waiting_file(Some(mailrs_domain::calendar::UploadProblem::NeedsAccess))]),
            ..event("primary", "a", 0, 1)
        };
        let gone = Event {
            attachments: Some(vec![waiting_file(Some(mailrs_domain::calendar::UploadProblem::NotFound))]),
            ..event("primary", "b", 0, 1)
        };
        save_events(&conn, account, &[stuck, gone, event("primary", "c", 0, 1)], 0).unwrap();
        assert_eq!(waiting_for_access(&conn, account).unwrap(), [("primary".to_string(), "a".to_string())]);
    }

    #[test]
    fn the_copy_keeps_the_organizers_version_of_an_event() {
        let (conn, id) = store();
        let review = Event { sequence: 3, ..event("primary", "review", MONDAY, 1) };
        save_events(&conn, id, &[review], 0).unwrap();
        assert_eq!(super::event(&conn, id, "primary", "review").unwrap().unwrap().sequence, 3);
    }

    #[test]
    fn set_my_answer_marks_the_event_and_the_guest_who_is_me() {
        let (conn, id) = store();
        let mut standup = event("primary", "standup", MONDAY + 9 * HOUR, 1);
        standup.guests = vec![
            Guest { email: "me@example.com".into(), me: true, ..Guest::default() },
            Guest { email: "priya@example.com".into(), organizer: true, ..Guest::default() },
        ];
        save_events(&conn, id, &[standup], 0).unwrap();

        set_my_answer(&conn, id, "primary", "standup", Answer::Yes).unwrap();

        let found = super::event(&conn, id, "primary", "standup").unwrap().unwrap();
        assert_eq!(found.my_answer, Some(Answer::Yes));
        assert_eq!(found.guests.iter().find(|g| g.me).and_then(|g| g.answer), Some(Answer::Yes));
        assert_eq!(
            found.guests.iter().find(|g| g.organizer).and_then(|g| g.answer),
            None,
            "only the guest marked me changes"
        );
    }

    #[test]
    fn set_my_answer_marks_every_row_of_the_series() {
        let (conn, id) = store();
        let me = || vec![Guest { email: "me@example.com".into(), me: true, ..Guest::default() }];
        let mut series = event("primary", "planning", MONDAY + 10 * HOUR, 1);
        series.guests = me();
        let mut moved = event("primary", "planning_moved", MONDAY + 11 * HOUR, 1);
        moved.uid = series.uid.clone();
        moved.series = Some("planning".into());
        moved.guests = me();
        let mut other = event("primary", "lunch", MONDAY + 12 * HOUR, 1);
        other.guests = me();
        save_events(&conn, id, &[series, moved, other], 0).unwrap();

        set_my_answer(&conn, id, "primary", "planning", Answer::Maybe).unwrap();

        let moved = super::event(&conn, id, "primary", "planning_moved").unwrap().unwrap();
        assert_eq!(moved.my_answer, Some(Answer::Maybe));
        assert_eq!(moved.guests[0].answer, Some(Answer::Maybe));
        let other = super::event(&conn, id, "primary", "lunch").unwrap().unwrap();
        assert_eq!(other.my_answer, None, "another event keeps its own answer");
    }

    #[test]
    fn set_my_answer_leaves_events_with_no_uid_alone() {
        let (conn, id) = store();
        let mut first = event("primary", "first", MONDAY + 10 * HOUR, 1);
        first.uid = String::new();
        let mut second = event("primary", "second", MONDAY + 12 * HOUR, 1);
        second.uid = String::new();
        save_events(&conn, id, &[first, second], 0).unwrap();

        set_my_answer(&conn, id, "primary", "first", Answer::Yes).unwrap();

        let second = super::event(&conn, id, "primary", "second").unwrap().unwrap();
        assert_eq!(second.my_answer, None);
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

    /// The query's `WHERE` once put `e.starts_at < ?3` on every branch, so
    /// a changed occurrence moved past the range end never entered the
    /// result, never masked the series' own date for it, and the original
    /// showed as if nothing had moved it.
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

    #[test]
    fn a_calendar_taken_off_the_list_leaves_the_range_too() {
        let (conn, id) = store();
        save_events(&conn, id, &[event("team", "retro", MONDAY + 9 * HOUR, 1)], 0).unwrap();
        set_listed(&conn, id, "team", false).unwrap();
        assert_eq!(unlisted(&conn, id).unwrap(), vec!["team".to_string()]);
        assert!(occurrences(&conn, &[id], MONDAY, MONDAY + DAY, CalendarScope::Shown).unwrap().is_empty());
    }

    #[test]
    fn a_calendar_put_back_on_the_list_shows_again() {
        let (conn, id) = store();
        set_listed(&conn, id, "team", false).unwrap();
        set_listed(&conn, id, "team", true).unwrap();
        assert!(unlisted(&conn, id).unwrap().is_empty());
        let team = calendars(&conn, id).unwrap().into_iter().find(|c| c.id == "team").unwrap();
        assert!(team.shown);
    }

    #[test]
    fn a_colour_chosen_here_outlasts_the_next_read_of_the_calendar_list() {
        let (conn, id) = store();
        set_own_color(&conn, id, "team", Some("#16a766")).unwrap();
        save_calendars(&conn, id, &[calendar("primary", true), calendar("team", false)]).unwrap();
        let team = calendars(&conn, id).unwrap().into_iter().find(|c| c.id == "team").unwrap();
        assert_eq!(team.color, "#16a766");
    }

    #[test]
    fn the_original_colour_comes_back_when_the_own_one_goes() {
        let (conn, id) = store();
        set_own_color(&conn, id, "team", Some("#16a766")).unwrap();
        set_own_color(&conn, id, "team", None).unwrap();
        let team = calendars(&conn, id).unwrap().into_iter().find(|c| c.id == "team").unwrap();
        assert_eq!(team.color, "#3584e4");
    }

    /// `CalendarScope::Owned` is what the clash line and free time need, so a calendar the account can only
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

    /// `occurrences` clones a whole event into every row it
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

    /// The live path caps a listing at `MOST_EVENTS` (500);
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
    fn search_finds_by_title_place_and_guest() {
        let (conn, id) = store();
        let mut lunch = event("primary", "lunch", MONDAY + 12 * HOUR, 1);
        lunch.place = "Café Império".into();
        let mut review = event("team", "review", MONDAY + DAY, 1);
        review.guests = vec![Guest { email: "rita@c9dev.pt".into(), name: Some("Rita Lopes".into()), ..Guest::default() }];
        let mut standup = event("team", "standup", MONDAY + 2 * DAY, 1);
        standup.guests = vec![Guest { email: "elia@c9dev.pt".into(), name: Some("Élia Constante".into()), ..Guest::default() }];
        save_events(&conn, id, &[lunch, review, standup], 0).unwrap();
        let ids = |text: &str| {
            search(&conn, &[id], text, MONDAY, CalendarScope::Shown, 10)
                .unwrap()
                .into_iter()
                .map(|o| o.event.id.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids("império"), vec!["lunch"]);
        assert_eq!(ids("RITA"), vec!["review"]);
        assert_eq!(ids("ÉLIA"), vec!["standup"], "penguin_fold folds beyond ASCII, so a search for the upper case still finds Élia");
        assert!(ids("nothing").is_empty());
    }

    /// Google keeps a description as HTML once someone has edited it in
    /// its own editor, and an inline tag such as `<b>` splits a word
    /// across two runs of text with nothing between them. Reading the
    /// description as words, the way the editor's Notes field does
    /// (`mailrs_mime::notes::text`), joins the runs back into "bread".
    #[test]
    fn search_finds_a_word_an_inline_tag_splits() {
        let (conn, id) = store();
        let mut planning = event("primary", "planning", MONDAY + 3 * DAY, 1);
        planning.description = "Br<b>ead</b> for the team".into();
        save_events(&conn, id, &[planning], 0).unwrap();
        let found = search(&conn, &[id], "bread", MONDAY, CalendarScope::Shown, 10).unwrap();
        assert_eq!(found.len(), 1, "a word an inline tag splits must still match once its tags are read as text");
    }

    /// A `<br>` line break must not itself read as the word "br": the
    /// tag never reaches the reader as text, only the line break it
    /// stands for.
    #[test]
    fn search_does_not_match_a_br_tag_as_the_word_br() {
        let (conn, id) = store();
        let mut sync = event("primary", "sync", MONDAY + 4 * DAY, 1);
        sync.description = "Notes<br>more notes".into();
        save_events(&conn, id, &[sync], 0).unwrap();
        let found = search(&conn, &[id], "br", MONDAY, CalendarScope::Shown, 10).unwrap();
        assert!(found.is_empty(), "the <br> tag's own name must not match a search for it");
    }

    #[test]
    fn a_search_stops_at_the_requested_limit() {
        let (conn, id) = store();
        let events: Vec<Event> =
            (0..5).map(|n| event("primary", &format!("standup{n}"), MONDAY + n * HOUR, 1)).collect();
        save_events(&conn, id, &events, 0).unwrap();
        let found = search(&conn, &[id], "standup", MONDAY, CalendarScope::Shown, 3).unwrap();
        assert_eq!(found.len(), 3);
    }

    /// The live path caps a search at `MOST_EVENTS` (500) event rows per
    /// account before ranking, so a word every event shares cannot make a
    /// search load a whole large calendar.
    #[test]
    fn a_search_reads_at_most_500_rows_per_account() {
        let (conn, id) = store();
        let events: Vec<Event> =
            (0..510).map(|n| event("primary", &format!("standup{n}"), MONDAY + n * HOUR, 1)).collect();
        save_events(&conn, id, &events, 0).unwrap();
        let found = search(&conn, &[id], "standup", MONDAY, CalendarScope::Shown, 1000).unwrap();
        assert_eq!(found.len(), 500);
    }

    /// A word every event shares still surfaces what is coming up: the
    /// row cap must keep the events that matter rather than whichever
    /// 500 the table scan reaches first. 500 old matches are saved
    /// before 5 upcoming ones, so an unordered `LIMIT` would fill the
    /// cap from the old rows alone and never reach the new ones.
    #[test]
    fn a_search_keeps_upcoming_events_when_500_past_ones_already_matched() {
        let (conn, id) = store();
        let past: Vec<Event> =
            (0..500).map(|n| event("primary", &format!("standup-old{n}"), MONDAY - (500 - n) * DAY, 1)).collect();
        save_events(&conn, id, &past, 0).unwrap();
        let soon: Vec<Event> =
            (0..5).map(|n| event("primary", &format!("standup-soon{n}"), MONDAY + (n + 1) * DAY, 1)).collect();
        save_events(&conn, id, &soon, 0).unwrap();
        let found = search(&conn, &[id], "standup", MONDAY, CalendarScope::Shown, 505).unwrap();
        let kept_soon = found.iter().filter(|o| o.event.id.starts_with("standup-soon")).count();
        assert_eq!(kept_soon, 5, "every upcoming match must survive the cap, not just whichever 500 rows came first");
    }

    #[test]
    fn a_series_and_its_changed_occurrence_count_as_one_result() {
        let (conn, id) = store();
        let mut standup = event("primary", "standup", MONDAY + 9 * HOUR, 1);
        standup.rules = vec!["RRULE:FREQ=DAILY;COUNT=5".into()];
        let mut moved = event("primary", "standup_tue", MONDAY + DAY + 11 * HOUR, 1);
        moved.uid = standup.uid.clone();
        moved.series = Some("standup".into());
        moved.original_start = Some(MONDAY + DAY + 9 * HOUR);
        save_events(&conn, id, &[standup, moved], 0).unwrap();
        let found = search(&conn, &[id], "standup", MONDAY, CalendarScope::Shown, 10).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].start, MONDAY + 9 * HOUR, "the earliest coming occurrence wins");
    }

    #[test]
    fn a_search_reads_shown_calendars_only() {
        let (conn, id) = store();
        save_events(&conn, id, &[event("team", "hidden-lunch", MONDAY + HOUR, 1)], 0).unwrap();
        set_shown(&conn, id, "team", false).unwrap();
        assert!(search(&conn, &[id], "hidden-lunch", MONDAY, CalendarScope::Shown, 10).unwrap().is_empty());
        assert_eq!(search(&conn, &[id], "hidden-lunch", MONDAY, CalendarScope::All, 10).unwrap().len(), 1);
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

    /// An event edited twice before a send
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

    /// Removing an event this computer made
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

    /// Removing an event with an unsent edit
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

    fn going(answer: Answer, note: Option<&str>) -> QueuedAnswer {
        QueuedAnswer {
            me: "me@example.com".into(),
            answer,
            note: note.map(str::to_string),
            title: "Stand-up".into(),
        }
    }

    /// Answering twice before a send queues the latest answer once, and
    /// a note given with the first stays unless the second brings one.
    #[test]
    fn two_answers_before_a_send_queue_the_latest_one() {
        let (conn, id) = store();
        enqueue_answer(&conn, id, "primary", "standup_20261020T090000Z", &going(Answer::Yes, Some("Late"))).unwrap();
        enqueue_answer(&conn, id, "primary", "standup_20261020T090000Z", &going(Answer::Maybe, None)).unwrap();
        let held = queued(&conn, id).unwrap();
        assert_eq!(held.len(), 1);
        assert_eq!(held[0].kind, ChangeKind::Answer);
        assert_eq!(held[0].answer, Some(going(Answer::Maybe, Some("Late"))));
        assert!(held[0].body.is_none());
    }

    /// An answer and an edit of the same event go out as two writes: the
    /// edit's body carries no answer, and the answer patches no edit.
    #[test]
    fn an_edit_after_an_answer_queues_apart_from_it() {
        let (conn, id) = store();
        let standup = event("primary", "standup", MONDAY, 1);
        enqueue_answer(&conn, id, "primary", "standup", &going(Answer::No, None)).unwrap();
        enqueue(&conn, id, ChangeKind::Save, &standup).unwrap();
        let kinds: Vec<ChangeKind> = queued(&conn, id).unwrap().iter().map(|q| q.kind).collect();
        assert_eq!(kinds, vec![ChangeKind::Answer, ChangeKind::Save]);
    }

    /// Google gives the event a new version for the answer, so an edit
    /// queued behind it goes out against that version, not a stale one.
    #[test]
    fn a_sent_answer_moves_the_edit_behind_it_to_the_new_version() {
        let (conn, id) = store();
        let standup = Event { pending: true, ..event("primary", "standup", MONDAY, 1) };
        save_events(&conn, id, std::slice::from_ref(&standup), 0).unwrap();
        enqueue_answer(&conn, id, "primary", "standup", &going(Answer::No, None)).unwrap();
        let answer_seq = queued(&conn, id).unwrap()[0].seq;
        finish_answer(&conn, id, answer_seq, "primary", "standup", "\"2\"").unwrap();
        assert!(queued(&conn, id).unwrap().is_empty());
        let row = super::event(&conn, id, "primary", "standup").unwrap().unwrap();
        assert_eq!((row.etag.as_str(), row.pending), ("\"2\"", false));

        enqueue_answer(&conn, id, "primary", "standup", &going(Answer::Yes, None)).unwrap();
        enqueue(&conn, id, ChangeKind::Save, &row).unwrap();
        let answer_seq = queued(&conn, id).unwrap()[0].seq;
        finish_answer(&conn, id, answer_seq, "primary", "standup", "\"3\"").unwrap();
        let held = queued(&conn, id).unwrap();
        assert_eq!(held.len(), 1);
        assert_eq!(held[0].etag.as_deref(), Some("\"3\""));
    }

    /// A caller that only needs to know which
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

    /// A series handed back whole keeps the changed occurrences it still
    /// has; one the organizer took back goes, and one a queued change owns
    /// stays.
    #[test]
    fn keep_occurrences_drops_the_changed_occurrences_a_page_no_longer_gives() {
        let (conn, id) = store();
        let occurrence = |name: &str, pending: bool| Event {
            series: Some("standup".into()),
            pending,
            ..event("primary", name, MONDAY, 1)
        };
        let rows = [
            event("primary", "standup", MONDAY, 1),
            occurrence("kept", false),
            occurrence("taken-back", false),
            occurrence("queued", true),
        ];
        save_events(&conn, id, &rows, 1).unwrap();
        keep_occurrences(&conn, id, "primary", "standup", &["kept".to_string()]).unwrap();
        let held = |name: &str| super::event(&conn, id, "primary", name).unwrap().is_some();
        assert_eq!([held("standup"), held("kept"), held("taken-back"), held("queued")], [true, true, false, true]);
    }

    /// A page-at-a-time read marks each row
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

    /// How far back a calendar's copy reaches is kept per calendar, and a
    /// refresh of the list leaves it alone.
    #[test]
    fn how_far_back_a_calendar_reaches_is_remembered_and_survives_a_list_refresh() {
        let (conn, id) = store();
        assert_eq!(reach(&conn, id, "primary").unwrap(), None, "unknown before the first read");
        set_reach(&conn, id, "primary", 1_000).unwrap();
        assert_eq!(reach(&conn, id, "primary").unwrap(), Some(1_000));
        assert_eq!(reach(&conn, id, "team").unwrap(), None, "another calendar is unaffected");
        save_calendars(&conn, id, &[calendar("primary", true), calendar("team", false)]).unwrap();
        assert_eq!(reach(&conn, id, "primary").unwrap(), Some(1_000));
    }

    /// A range fetched for an older week writes rows and moves the reach,
    /// and neither touches the sync token the next change read starts from.
    #[test]
    fn setting_the_reach_leaves_the_sync_token_alone() {
        let (conn, id) = store();
        set_token(&conn, id, "primary", Some("t"), 5).unwrap();
        set_reach(&conn, id, "primary", 1_000).unwrap();
        assert_eq!(token(&conn, id, "primary").unwrap().as_deref(), Some("t"));
        assert_eq!(synced_at(&conn, id, "primary").unwrap(), Some(5));
    }

    #[test]
    fn the_account_counts_as_synced_once_its_primary_calendar_has_a_token() {
        let (conn, id) = store();
        assert!(!synced(&conn, id).unwrap());
        set_token(&conn, id, "primary", Some("t"), 1).unwrap();
        assert!(synced(&conn, id).unwrap());
    }

    /// A change whose row nobody touched while
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

    #[test]
    fn a_series_lists_its_changed_occurrences() {
        let (conn, id) = store();
        let mut standup = event("primary", "standup", MONDAY + 9 * HOUR, 1);
        standup.rules = vec!["RRULE:FREQ=DAILY;COUNT=3".into()];
        let mut moved = event("primary", "standup_tue", MONDAY + DAY + 11 * HOUR, 1);
        moved.series = Some("standup".into());
        moved.original_start = Some(MONDAY + DAY + 9 * HOUR);
        moved.pending = true;
        moved.guests = vec![Guest { email: "ann@example.com".into(), ..Guest::default() }];
        save_events(&conn, id, &[standup, moved], 1).unwrap();
        let found = changed_occurrences(&conn, id, "primary", "standup").unwrap();
        assert_eq!(found.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), vec!["standup_tue"]);
        assert_eq!(found[0].guests.len(), 1, "with its guests");
        settle(&conn, id, "primary", "standup_tue").unwrap();
        assert!(!super::event(&conn, id, "primary", "standup_tue").unwrap().unwrap().pending);
    }

    /// A cancelled occurrence can be removed twice before a send, once by
    /// the window and once by the assistant, and each row goes out once.
    #[test]
    fn a_second_removal_of_one_event_queues_nothing_more() {
        let (conn, id) = store();
        let lunch = event("primary", "lunch", MONDAY, 1);
        enqueue(&conn, id, ChangeKind::Remove, &lunch).unwrap();
        enqueue(&conn, id, ChangeKind::Remove, &lunch).unwrap();
        assert_eq!(queued(&conn, id).unwrap().len(), 1);
    }

    /// The provider still holds an event whose removal has not gone out,
    /// so an edit after it changes the event instead of queueing a second
    /// row behind the removal.
    #[test]
    fn an_edit_after_an_unsent_removal_takes_its_place() {
        let (conn, id) = store();
        let lunch = event("primary", "lunch", MONDAY, 1);
        enqueue(&conn, id, ChangeKind::Remove, &lunch).unwrap();
        let edited = Event { title: "Late lunch".into(), ..lunch };
        enqueue(&conn, id, ChangeKind::Save, &edited).unwrap();
        let held = queued(&conn, id).unwrap();
        assert_eq!(held.len(), 1);
        assert_eq!(held[0].kind, ChangeKind::Save);
        assert_eq!(held[0].etag.as_deref(), Some("\"1\""));
        assert_eq!(held[0].body.as_ref().map(|e| e.title.as_str()), Some("Late lunch"));
    }

    /// A held change persists so a crash or a quit before its Undo toast
    /// closes still has it to queue at the next start.
    #[test]
    fn a_held_change_survives_to_the_next_start() {
        let (conn, id) = store();
        let dentist = event("primary", "dentist", MONDAY, 1);
        let moved = Event { title: "Moved".into(), ..dentist.clone() };
        save_holding(&conn, id, &[Step::Save(moved.clone())], std::slice::from_ref(&dentist), Notify::Guests).unwrap();

        assert_eq!(holdings(&conn).unwrap(), vec![(id, vec![Step::Save(moved)], vec![dentist], Notify::Guests)]);
    }

    /// A move the person chose to keep from the guests stays quiet when a
    /// restart queues it.
    #[test]
    fn a_held_change_keeps_the_choice_not_to_tell_the_guests() {
        let (conn, id) = store();
        let remove = vec![Step::Remove { calendar: "primary".into(), id: "dentist".into() }];
        save_holding(&conn, id, &remove, &[], Notify::Nobody).unwrap();

        assert_eq!(holdings(&conn).unwrap()[0].3, Notify::Nobody);
    }

    #[test]
    fn a_queued_change_keeps_the_choice_not_to_tell_the_guests() {
        let (conn, id) = store();
        let lunch = event("primary", "lunch", MONDAY, 1);
        enqueue_after(&conn, id, ChangeKind::Save, &lunch, None, None, Notify::Nobody).unwrap();
        enqueue(&conn, id, ChangeKind::Remove, &event("primary", "dinner", MONDAY, 1)).unwrap();

        let notices: Vec<Notify> = queued(&conn, id).unwrap().iter().map(|q| q.notify).collect();
        assert_eq!(notices, [Notify::Nobody, Notify::Guests]);
    }

    /// The guests of an event made here hear of it through the create; a
    /// quiet move folded onto that unsent create must not swallow their
    /// invitation.
    #[test]
    fn a_quiet_move_folded_onto_an_unsent_create_still_invites_the_guests() {
        let (conn, id) = store();
        let lunch = event("primary", "lunch", MONDAY, 1);
        enqueue(&conn, id, ChangeKind::Create, &lunch).unwrap();
        enqueue_after(&conn, id, ChangeKind::Save, &lunch, None, None, Notify::Nobody).unwrap();

        assert_eq!(queued(&conn, id).unwrap()[0].notify, Notify::Guests);
    }

    /// The guests never heard of an unsent edit, so the delete that
    /// replaces it goes out as the person chose.
    #[test]
    fn a_quiet_delete_of_an_unsent_edit_stays_quiet() {
        let (conn, id) = store();
        let lunch = event("primary", "lunch", MONDAY, 1);
        enqueue(&conn, id, ChangeKind::Save, &lunch).unwrap();
        enqueue_after(&conn, id, ChangeKind::Remove, &lunch, None, None, Notify::Nobody).unwrap();

        let held = queued(&conn, id).unwrap();
        assert_eq!((held[0].kind, held[0].notify), (ChangeKind::Remove, Notify::Nobody));
    }

    /// Only one change is held at a time, so holding another replaces the
    /// persisted row rather than adding a second one.
    #[test]
    fn holding_another_change_replaces_the_persisted_one() {
        let (conn, id) = store();
        let remove = |event: &str| vec![Step::Remove { calendar: "primary".into(), id: event.into() }];
        save_holding(&conn, id, &remove("a"), &[], Notify::Guests).unwrap();
        save_holding(&conn, id, &remove("b"), &[], Notify::Guests).unwrap();

        assert_eq!(holdings(&conn).unwrap(), vec![(id, remove("b"), Vec::new(), Notify::Guests)]);
    }

    /// Queuing or reverting a held change clears its persisted row, so a
    /// later start does not queue it a second time.
    #[test]
    fn clearing_a_held_change_drops_its_persisted_row() {
        let (conn, id) = store();
        save_holding(&conn, id, &[Step::Remove { calendar: "primary".into(), id: "a".into() }], &[], Notify::Guests)
            .unwrap();

        clear_holding(&conn, id).unwrap();

        assert!(holdings(&conn).unwrap().is_empty());
    }

    #[test]
    fn a_finished_removal_settles_its_cancelled_occurrence() {
        let (conn, id) = store();
        let mut cancelled = event("primary", "standup_tue", MONDAY, 1);
        cancelled.pending = true;
        save_events(&conn, id, std::slice::from_ref(&cancelled), 1).unwrap();
        enqueue(&conn, id, ChangeKind::Remove, &cancelled).unwrap();
        assert_eq!(removing_ids(&conn, id, "primary").unwrap(), HashSet::from(["standup_tue".to_string()]));
        let seq = queued(&conn, id).unwrap()[0].seq;
        finish_removal(&conn, id, seq).unwrap();
        assert!(queued(&conn, id).unwrap().is_empty());
        assert!(!super::event(&conn, id, "primary", "standup_tue").unwrap().unwrap().pending);
    }

    /// The edit landed on the removal's row while the removal was on its
    /// way, so finishing the removal must leave the edit queued.
    #[test]
    fn finishing_a_removal_an_edit_replaced_keeps_the_edit() {
        let (conn, id) = store();
        let lunch = event("primary", "lunch", MONDAY, 1);
        enqueue(&conn, id, ChangeKind::Remove, &lunch).unwrap();
        let seq = queued(&conn, id).unwrap()[0].seq;
        enqueue(&conn, id, ChangeKind::Save, &lunch).unwrap();
        finish_removal(&conn, id, seq).unwrap();
        assert_eq!(queued(&conn, id).unwrap()[0].kind, ChangeKind::Save);
    }

    #[test]
    fn the_events_of_one_uid_come_back_as_occurrences() {
        let (conn, id) = store();
        let mut standup = event("primary", "standup", MONDAY + 9 * HOUR, 1);
        standup.rules = vec!["RRULE:FREQ=DAILY;COUNT=3".into()];
        // Google gives a changed occurrence the UID of its series.
        let mut moved = event("primary", "standup_tue", MONDAY + DAY + 11 * HOUR, 1);
        moved.uid = standup.uid.clone();
        moved.series = Some("standup".into());
        moved.original_start = Some(MONDAY + DAY + 9 * HOUR);
        let lunch = event("primary", "lunch", MONDAY + 12 * HOUR, 1);
        save_events(&conn, id, &[standup, moved, lunch], 0).unwrap();
        let found = with_uid(&conn, id, "STANDUP@example.com", MONDAY, MONDAY + 7 * DAY).unwrap();
        assert_eq!(
            starts(&found),
            vec![
                ("standup".into(), MONDAY + 9 * HOUR),
                ("standup_tue".into(), MONDAY + DAY + 11 * HOUR),
                ("standup".into(), MONDAY + 2 * DAY + 9 * HOUR),
            ]
        );
    }

    #[test]
    fn a_uid_on_a_hidden_calendar_finds_nothing() {
        let (conn, id) = store();
        save_events(&conn, id, &[event("team", "retro", MONDAY + 9 * HOUR, 1)], 0).unwrap();
        set_shown(&conn, id, "team", false).unwrap();
        assert!(with_uid(&conn, id, "retro@example.com", MONDAY, MONDAY + DAY).unwrap().is_empty());
    }
}
