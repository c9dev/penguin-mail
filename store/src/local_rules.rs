//! Rules that run on this computer, for an account whose server runs
//! none, with what keeps each new Inbox message running through them
//! once: the newest INTERNALDATE they looked at, and the messages they
//! ran on within [`OVERLAP`] of it.

use mailrs_domain::{AccountId, EpochMillis, Filter, MessageMeta};
use rusqlite::{Connection, OptionalExtension, params};

use crate::{Result, StoreError, messages};

/// How far behind the watermark a message still counts as new. A server
/// stamps INTERNALDATE in whole seconds and several messages share one;
/// an hour also covers a server clock that runs a little behind.
pub const OVERLAP: EpochMillis = 60 * 60 * 1000;

/// The account's rules, in the order the person made them.
pub fn list(conn: &Connection, account_id: AccountId) -> Result<Vec<Filter>> {
    let mut stmt = conn.prepare(
        "SELECT id, filter FROM local_rules WHERE account_id = ?1 ORDER BY position, id",
    )?;
    let rows = stmt.query_map([account_id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?;
    let mut filters = Vec::new();
    for row in rows {
        let (id, text) = row?;
        let mut filter: Filter = serde_json::from_str(&text)
            .map_err(|_| StoreError::Corrupt { column: "local_rules.filter", value: text.clone() })?;
        filter.id = Some(id);
        filters.push(filter);
    }
    Ok(filters)
}

/// Adds `filter`, which carries its id, after the account's other rules.
pub fn add(conn: &Connection, account_id: AccountId, filter: &Filter) -> Result<()> {
    let id = filter
        .id
        .clone()
        .ok_or(StoreError::Corrupt { column: "local_rules.id", value: String::new() })?;
    let text = serde_json::to_string(filter)
        .map_err(|err| StoreError::Corrupt { column: "local_rules.filter", value: err.to_string() })?;
    conn.execute(
        "INSERT OR REPLACE INTO local_rules (account_id, id, position, filter) VALUES (?1, ?2, \
         (SELECT COALESCE(MAX(position), -1) + 1 FROM local_rules WHERE account_id = ?1), ?3)",
        params![account_id, id, text],
    )?;
    Ok(())
}

/// Deletes rule `id`; `false` when the account had no such rule.
pub fn remove(conn: &Connection, account_id: AccountId, id: &str) -> Result<bool> {
    Ok(conn.execute("DELETE FROM local_rules WHERE account_id = ?1 AND id = ?2", params![account_id, id])? > 0)
}

/// Puts `filter`, which carries its id, where rule `old_id` was, so the
/// edited rule keeps its turn; `false` when the account had no such rule.
pub fn replace(conn: &Connection, account_id: AccountId, old_id: &str, filter: &Filter) -> Result<bool> {
    let id = filter
        .id
        .clone()
        .ok_or(StoreError::Corrupt { column: "local_rules.id", value: String::new() })?;
    let text = serde_json::to_string(filter)
        .map_err(|err| StoreError::Corrupt { column: "local_rules.filter", value: err.to_string() })?;
    Ok(conn.execute(
        "UPDATE local_rules SET id = ?3, filter = ?4 WHERE account_id = ?1 AND id = ?2",
        params![account_id, old_id, id, text],
    )? > 0)
}

/// Sets the watermark to `now` unless the rules have run before, so the
/// first rule an account gets runs on mail from now on and not on the
/// Inbox it already holds.
pub fn start_running(conn: &Connection, account_id: AccountId, now: EpochMillis) -> Result<()> {
    conn.execute(
        "UPDATE accounts SET rules_ran_until = ?2 WHERE id = ?1 AND rules_ran_until IS NULL",
        params![account_id, now],
    )?;
    Ok(())
}

/// The newest INTERNALDATE the rules have looked at; `None` before the
/// account had a rule.
pub fn ran_until(conn: &Connection, account_id: AccountId) -> Result<Option<EpochMillis>> {
    Ok(conn
        .query_row("SELECT rules_ran_until FROM accounts WHERE id = ?1", [account_id], |row| row.get(0))
        .optional()?
        .flatten())
}

/// At most `limit` Inbox messages the rules have not run on, received no
/// more than [`OVERLAP`] before the watermark, oldest first. None before
/// the account has a watermark.
pub fn candidates(conn: &Connection, account_id: AccountId, limit: usize) -> Result<Vec<MessageMeta>> {
    let Some(since) = ran_until(conn, account_id)? else {
        return Ok(Vec::new());
    };
    let mut stmt = conn.prepare(
        "SELECT m.id FROM messages m \
         JOIN message_mailboxes mm ON mm.account_id = m.account_id AND mm.message_id = m.id \
         JOIN mailboxes b ON b.key = mm.mailbox AND b.role = 'inbox' \
         WHERE m.account_id = ?1 AND m.date >= ?2 \
         AND NOT EXISTS (SELECT 1 FROM local_rules_ran r WHERE r.account_id = m.account_id AND r.message_id = m.id) \
         ORDER BY m.date, m.id LIMIT ?3",
    )?;
    let ids: Vec<String> = stmt
        .query_map(params![account_id, since - OVERLAP, i64::try_from(limit).unwrap_or(i64::MAX)], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut found = messages::by_ids(conn, account_id, &ids)?;
    found.sort_by(|a, b| a.date.cmp(&b.date).then_with(|| a.id.cmp(&b.id)));
    Ok(found)
}

/// Records that the rules ran on `ran`, each with its INTERNALDATE, moves
/// the watermark to the newest, and forgets what fell more than
/// [`OVERLAP`] below it.
pub fn mark_ran(conn: &Connection, account_id: AccountId, ran: &[(String, EpochMillis)]) -> Result<()> {
    for (message_id, date) in ran {
        conn.execute(
            "INSERT OR IGNORE INTO local_rules_ran (account_id, message_id, date) VALUES (?1, ?2, ?3)",
            params![account_id, message_id, date],
        )?;
    }
    if let Some(newest) = ran.iter().map(|(_, date)| *date).max() {
        conn.execute(
            "UPDATE accounts SET rules_ran_until = MAX(COALESCE(rules_ran_until, 0), ?2) WHERE id = ?1",
            params![account_id, newest],
        )?;
    }
    conn.execute(
        "DELETE FROM local_rules_ran WHERE account_id = ?1 AND date < \
         (SELECT COALESCE(rules_ran_until, 0) FROM accounts WHERE id = ?1) - ?2",
        params![account_id, OVERLAP],
    )?;
    Ok(())
}
