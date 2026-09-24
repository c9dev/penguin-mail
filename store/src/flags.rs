//! Flag colours for starred mail. Gmail stores only the star.

use std::collections::HashMap;

use mailrs_domain::mailbox::keyword::FLAGGED;
use mailrs_domain::{AccountId, FlagColor};
use rusqlite::{Connection, params};

use crate::Result;
use crate::messages::NEWEST_FLAG_COLOR;

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
    conn.execute(
        &format!(
            "UPDATE threads SET flag_color = ({NEWEST_FLAG_COLOR}) WHERE account_id = ?1 AND id = ?2"
        ),
        params![account_id, thread_id],
    )?;
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

/// How many starred threads carry each colour, across every account. A
/// thread counts once, under the colour of its newest coloured message.
pub fn counts(conn: &Connection) -> Result<HashMap<FlagColor, i64>> {
    let mut stmt = conn.prepare_cached(
        "SELECT COALESCE(flag_color, 'red'), COUNT(*) FROM threads WHERE starred = 1 GROUP BY 1",
    )?;
    collect(stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?)
}

/// `threads::count_threads` for each Flag mailbox,
/// `ThreadFilter::everything().with_flag(color)`, in one query. A thread
/// counts under every colour one of its starred messages has, so these can
/// add up to more than `counts`. Like the list, it keeps a thread with
/// trashed or spam mail while one of its messages is outside both, which
/// `threads.listed` says.
pub fn mailbox_counts(conn: &Connection) -> Result<HashMap<FlagColor, i64>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT color, COUNT(*) FROM (SELECT DISTINCT x.account_id, x.thread_id, \
             COALESCE(f.color, 'red') AS color FROM message_keywords s \
         CROSS JOIN messages x ON x.account_id = s.account_id AND x.id = s.message_id \
         CROSS JOIN threads t ON t.account_id = x.account_id AND t.id = x.thread_id \
         LEFT JOIN flags f ON f.account_id = x.account_id AND f.message_id = x.id \
         WHERE s.keyword = '{FLAGGED}' AND t.listed = 1) \
         GROUP BY color"
    ))?;
    collect(stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?)
}

fn collect(
    rows: impl Iterator<Item = rusqlite::Result<(String, i64)>>,
) -> Result<HashMap<FlagColor, i64>> {
    let mut counts = HashMap::new();
    for row in rows {
        let (color, count) = row?;
        if let Ok(color) = color.parse() {
            counts.insert(color, count);
        }
    }
    Ok(counts)
}
