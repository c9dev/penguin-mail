//! Threads for mail from a server that keeps none, found the way
//! Thunderbird finds them: a copy of a stored message, or a reply that
//! arrived first, shares its thread; then the nearest message the
//! References and In-Reply-To headers name; then, for a subject with a
//! reply prefix, the nearest message with the same base subject within 30
//! days. A message that finds none starts a thread named by its own id.
//! Gmail supplies its own threads, and nothing sends its mail here.

use mailrs_domain::{AccountId, EpochMillis, MessageMeta, subject};
use rusqlite::{Connection, OptionalExtension, params};

use crate::Result;

/// The headers that link a message to the ones it answers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Links {
    pub in_reply_to: Option<String>,
    /// Oldest first, as the header lists them.
    pub references: Vec<String>,
}

const WINDOW: EpochMillis = 30 * 24 * 60 * 60 * 1000;

/// The Message-ID inside `id`, without its angle brackets or spaces, or
/// `None` for an empty or bracket-only header value such as `<>`, which
/// never names a real message and must never link two messages that both
/// happen to lack one.
fn bare(id: &str) -> Option<&str> {
    let bare = id.trim().trim_start_matches('<').trim_end_matches('>');
    (!bare.is_empty()).then_some(bare)
}

/// The thread `meta` joins, or its own id.
pub(crate) fn thread_for(
    conn: &Connection,
    account_id: AccountId,
    meta: &MessageMeta,
    links: &Links,
) -> Result<String> {
    if let Some(own) = meta.rfc822_msgid.as_deref().and_then(bare) {
        if let Some(thread) = by_msgid(conn, account_id, own)? {
            return Ok(thread);
        }
        if let Some(thread) = naming(conn, account_id, own)? {
            return Ok(thread);
        }
    }
    // The newest entry in References is the message this one answers
    // directly; In-Reply-To is only a fallback for a sender that sent
    // no References at all.
    let named = links.references.iter().rev().chain(links.in_reply_to.iter());
    for id in named.filter_map(|id| bare(id)) {
        if let Some(thread) = by_msgid(conn, account_id, id)? {
            return Ok(thread);
        }
    }
    let base = subject::base(&meta.subject);
    if subject::replies(&meta.subject)
        && !base.is_empty()
        && let Some(thread) = by_subject(conn, account_id, &base, meta.date)?
    {
        return Ok(thread);
    }
    Ok(meta.id.clone())
}

/// Records what `meta` links to and its base subject, so mail that comes
/// later can find it. Only messages threaded here carry a base subject.
pub(crate) fn remember(
    conn: &Connection,
    account_id: AccountId,
    meta: &MessageMeta,
    links: &Links,
) -> Result<()> {
    conn.prepare_cached("UPDATE messages SET base_subject = ?3 WHERE account_id = ?1 AND id = ?2")?
        .execute(params![account_id, meta.id, subject::base(&meta.subject)])?;
    conn.prepare_cached("DELETE FROM message_links WHERE account_id = ?1 AND message_id = ?2")?
        .execute(params![account_id, meta.id])?;
    let mut insert = conn.prepare_cached(
        "INSERT OR IGNORE INTO message_links (account_id, message_id, msgid) VALUES (?1, ?2, ?3)",
    )?;
    for id in links.references.iter().chain(links.in_reply_to.iter()).filter_map(|id| bare(id)) {
        insert.execute(params![account_id, meta.id, id])?;
    }
    Ok(())
}

/// `by_msgid`'s statement. `INDEXED BY` pins the plan to the partial
/// index built for this lookup: without it, a store with no `ANALYZE`
/// statistics can read the `base_subject IS NOT NULL` term as a plain
/// filter and scan every local message instead. A test below prepares
/// this text on its own and reads back `EXPLAIN QUERY PLAN` for it, so a
/// future migration that drops or renames the index fails loudly rather
/// than turning this lookup quadratic again.
const BY_MSGID_SQL: &str = "SELECT thread_id FROM messages INDEXED BY messages_local_by_msgid \
     WHERE account_id = ?1 AND base_subject IS NOT NULL AND rfc822_msgid IN (?2, ?3) LIMIT 1";

/// `naming`'s statement. `INDEXED BY` keeps the same guarantee
/// `BY_MSGID_SQL` needs: a store with no statistics still starts from
/// the few rows naming `msgid`, not from `message_links` as a whole.
const NAMING_SQL: &str = "SELECT m.thread_id FROM message_links l INDEXED BY message_links_by_msgid \
     CROSS JOIN messages m ON m.account_id = l.account_id AND m.id = l.message_id \
     WHERE l.account_id = ?1 AND l.msgid = ?2 LIMIT 1";

/// The thread of a message threaded here whose Message-ID is `msgid`,
/// stored with or without its angle brackets.
fn by_msgid(conn: &Connection, account_id: AccountId, msgid: &str) -> Result<Option<String>> {
    Ok(conn
        .prepare_cached(BY_MSGID_SQL)?
        .query_row(params![account_id, msgid, format!("<{msgid}>")], |row| row.get(0))
        .optional()?)
}

/// The thread of a stored message that names `msgid` among its links: a
/// reply that arrived before the message it answers.
fn naming(conn: &Connection, account_id: AccountId, msgid: &str) -> Result<Option<String>> {
    Ok(conn
        .prepare_cached(NAMING_SQL)?
        .query_row(params![account_id, msgid], |row| row.get(0))
        .optional()?)
}

/// The thread of the message threaded here nearest in time to `date`
/// whose base subject is `base`, within 30 days either side. `thread_id`
/// breaks a tie between two candidates equally far from `date`, so the
/// choice does not depend on the order SQLite happens to visit rows in.
fn by_subject(
    conn: &Connection,
    account_id: AccountId,
    base: &str,
    date: EpochMillis,
) -> Result<Option<String>> {
    Ok(conn
        .prepare_cached(
            "SELECT thread_id FROM messages WHERE account_id = ?1 AND base_subject = ?2 \
             AND date BETWEEN ?3 AND ?4 ORDER BY abs(date - ?5), thread_id LIMIT 1",
        )?
        .query_row(
            params![account_id, base, date - WINDOW, date + WINDOW, date],
            |row| row.get(0),
        )
        .optional()?)
}

#[cfg(test)]
mod tests {
    use super::{BY_MSGID_SQL, NAMING_SQL};

    /// `INDEXED BY` fails at prepare, not merely at query time, when the
    /// named index does not exist or cannot serve the query. A store
    /// freshly opened at migration 30 exercises this before either
    /// statement ever runs against real rows.
    #[test]
    fn by_msgid_and_naming_prepare_against_a_fresh_store() {
        let conn = crate::open_in_memory().unwrap();
        conn.prepare(BY_MSGID_SQL).unwrap();
        conn.prepare(NAMING_SQL).unwrap();
    }

    /// The plan SQLite picks for each statement names the index its
    /// `INDEXED BY` demands, not a table scan, even before `ANALYZE` has
    /// run.
    #[test]
    fn by_msgid_and_naming_read_through_the_index_named_in_indexed_by() {
        let conn = crate::open_in_memory().unwrap();
        let by_msgid = plan(&conn, BY_MSGID_SQL, 3);
        assert!(by_msgid.contains("messages_local_by_msgid"), "{by_msgid}");
        let naming = plan(&conn, NAMING_SQL, 2);
        assert!(naming.contains("message_links_by_msgid"), "{naming}");
    }

    /// `EXPLAIN QUERY PLAN` for `sql`, bound to `params` placeholder
    /// values of one account each, its `detail` column from every step
    /// joined into one string. Planning never reads the values, only
    /// their count and positions, so a placeholder of `1` fits any
    /// parameter these statements take.
    fn plan(conn: &rusqlite::Connection, sql: &str, params: usize) -> String {
        conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
            .unwrap()
            .query_map(rusqlite::params_from_iter(vec![1; params]), |row| {
                row.get::<_, String>(3)
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
            .join(" ")
    }
}
