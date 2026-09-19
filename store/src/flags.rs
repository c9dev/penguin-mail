//! Flag colours for starred mail. Gmail stores only the star.

use std::collections::HashMap;

use mailrs_domain::{AccountId, FlagColor};
use rusqlite::{Connection, params};

use crate::Result;

/// Colours the messages of a thread, or one of its messages. `None` takes
/// the colour off.
pub fn set_color(
    conn: &Connection,
    account_id: AccountId,
    thread_id: &str,
    message_id: Option<&str>,
    color: Option<FlagColor>,
) -> Result<()> {
    let ids: Vec<String> = conn
        .prepare("SELECT id FROM messages WHERE account_id = ?1 AND thread_id = ?2")?
        .query_map(params![account_id, thread_id], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?
        .into_iter()
        .filter(|id| message_id.is_none_or(|only| only == id))
        .collect();
    for id in ids {
        match color {
            Some(color) => conn.execute(
                "INSERT INTO flags (account_id, message_id, color) VALUES (?1, ?2, ?3) \
                 ON CONFLICT (account_id, message_id) DO UPDATE SET color = excluded.color",
                params![account_id, id, color.as_str()],
            )?,
            None => conn.execute(
                "DELETE FROM flags WHERE account_id = ?1 AND message_id = ?2",
                params![account_id, id],
            )?,
        };
    }
    Ok(())
}

/// The colour of each message in a thread, or of its one message, with
/// `None` for messages that have no colour.
pub fn colors(
    conn: &Connection,
    account_id: AccountId,
    thread_id: &str,
    message_id: Option<&str>,
) -> Result<Vec<(String, Option<FlagColor>)>> {
    let rows: Vec<(String, Option<String>)> = conn
        .prepare(
            "SELECT m.id, f.color FROM messages m LEFT JOIN flags f \
             ON f.account_id = m.account_id AND f.message_id = m.id \
             WHERE m.account_id = ?1 AND m.thread_id = ?2 ORDER BY m.id",
        )?
        .query_map(params![account_id, thread_id], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows
        .into_iter()
        .filter(|(id, _)| message_id.is_none_or(|only| only == id))
        .map(|(id, color)| (id, color.and_then(|c| c.parse().ok())))
        .collect())
}

/// How many starred threads carry each colour, across every account.
pub fn counts(conn: &Connection) -> Result<HashMap<FlagColor, i64>> {
    let mut stmt = conn.prepare(
        "SELECT COALESCE((SELECT f.color FROM flags f JOIN messages m \
                ON m.account_id = f.account_id AND m.id = f.message_id \
                WHERE m.account_id = t.account_id AND m.thread_id = t.id \
                ORDER BY m.date DESC LIMIT 1), 'red'), COUNT(*) \
         FROM threads t WHERE t.starred = 1 GROUP BY 1",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    let mut counts = HashMap::new();
    for row in rows {
        let (color, count) = row?;
        if let Ok(color) = color.parse() {
            counts.insert(color, count);
        }
    }
    Ok(counts)
}
