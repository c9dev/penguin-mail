//! What a POP3 account has downloaded and what it still owes the server.
//! The server's UIDL list is compared a page at a time, so a mailbox of
//! any size never brings the whole table into memory.

use std::collections::HashSet;

use mailrs_domain::{AccountId, EpochMillis};
use rusqlite::{Connection, OptionalExtension, params};

use crate::Result;

/// UIDLs read or compared in one statement.
pub const PAGE: usize = 500;

/// Refused RETRs after which a message shows in the account's menu.
pub const SHOWN_AFTER: i64 = 3;

fn json(list: &[String]) -> String {
    serde_json::to_string(list).unwrap_or_else(|_| "[]".into())
}

/// The UIDLs of `uidls`, a page of the server's list, that this account
/// never downloaded, in the order given.
pub fn unseen(conn: &Connection, account_id: AccountId, uidls: &[String]) -> Result<Vec<String>> {
    let mut stmt = conn.prepare_cached(
        "SELECT j.value FROM json_each(?2) j WHERE NOT EXISTS \
         (SELECT 1 FROM pop3_seen s WHERE s.account_id = ?1 AND s.uidl = j.value) \
         ORDER BY j.key",
    )?;
    let rows = stmt.query_map(params![account_id, json(uidls)], |row| row.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Whether the account has downloaded anything yet. Its first check
/// takes each message's own date and announces nothing.
pub fn has_downloaded(conn: &Connection, account_id: AccountId) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM pop3_seen WHERE account_id = ?1 LIMIT 1",
            [account_id],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// Records `uidl` as downloaded at `at` and drops its failure count.
pub fn mark_downloaded(
    conn: &Connection,
    account_id: AccountId,
    uidl: &str,
    at: EpochMillis,
) -> Result<()> {
    conn.execute(
        "INSERT INTO pop3_seen (account_id, uidl, downloaded_at) VALUES (?1, ?2, ?3) \
         ON CONFLICT (account_id, uidl) DO NOTHING",
        params![account_id, uidl, at],
    )?;
    conn.execute(
        "DELETE FROM pop3_failures WHERE account_id = ?1 AND uidl = ?2",
        params![account_id, uidl],
    )?;
    Ok(())
}

/// Asks for a DELE of each of `uidls` at the next check. A UIDL never
/// downloaded, or already removed, is left alone.
pub fn want_removed(conn: &Connection, account_id: AccountId, uidls: &[String]) -> Result<()> {
    conn.execute(
        "UPDATE pop3_seen SET remove_wanted = 1 WHERE account_id = ?1 AND removed = 0 \
         AND uidl IN (SELECT value FROM json_each(?2))",
        params![account_id, json(uidls)],
    )?;
    Ok(())
}

/// Asks for a DELE of everything downloaded before `cutoff`, for Remove
/// After {n} Days. Answers how many rows it marked.
pub fn want_removed_before(
    conn: &Connection,
    account_id: AccountId,
    cutoff: EpochMillis,
) -> Result<usize> {
    Ok(conn.execute(
        "UPDATE pop3_seen SET remove_wanted = 1 \
         WHERE account_id = ?1 AND removed = 0 AND remove_wanted = 0 AND downloaded_at < ?2",
        params![account_id, cutoff],
    )?)
}

/// At most `limit` UIDLs whose DELE is wanted and not yet confirmed, after
/// `after` in UIDL order. Under Leave on Server there are none.
pub fn pending_removal(
    conn: &Connection,
    account_id: AccountId,
    after: Option<&str>,
    limit: usize,
) -> Result<Vec<String>> {
    let mut stmt = conn.prepare_cached(
        "SELECT uidl FROM pop3_seen WHERE account_id = ?1 AND remove_wanted = 1 AND removed = 0 \
         AND uidl > ?2 ORDER BY uidl LIMIT ?3",
    )?;
    let rows = stmt.query_map(
        params![
            account_id,
            after.unwrap_or(""),
            i64::try_from(limit).unwrap_or(i64::MAX)
        ],
        |row| row.get(0),
    )?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Records that a clean QUIT took each of `uidls` off the server.
pub fn mark_removed(conn: &Connection, account_id: AccountId, uidls: &[String]) -> Result<()> {
    conn.execute(
        "UPDATE pop3_seen SET removed = 1 WHERE account_id = ?1 AND uidl IN (SELECT value FROM json_each(?2))",
        params![account_id, json(uidls)],
    )?;
    Ok(())
}

/// Drops every row whose UIDL `listed`, the server's whole list, lacks,
/// a page of rows at a time. Answers how many went.
pub fn forget_gone(
    conn: &Connection,
    account_id: AccountId,
    listed: &HashSet<String>,
) -> Result<usize> {
    let mut after = String::new();
    let mut forgotten = 0;
    loop {
        let page: Vec<String> = conn
            .prepare_cached(
                "SELECT uidl FROM pop3_seen WHERE account_id = ?1 AND uidl > ?2 ORDER BY uidl LIMIT ?3",
            )?
            .query_map(params![account_id, after, PAGE as i64], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let Some(last) = page.last() else {
            return Ok(forgotten);
        };
        after = last.clone();
        let gone: Vec<String> = page
            .into_iter()
            .filter(|uidl| !listed.contains(uidl))
            .collect();
        forgotten += conn.execute(
            "DELETE FROM pop3_seen WHERE account_id = ?1 AND uidl IN (SELECT value FROM json_each(?2))",
            params![account_id, json(&gone)],
        )?;
    }
}

/// Counts one refused RETR of `uidl` with the server's words, and answers
/// how many there have been.
pub fn record_failure(
    conn: &Connection,
    account_id: AccountId,
    uidl: &str,
    error: &str,
) -> Result<i64> {
    Ok(conn.query_row(
        "INSERT INTO pop3_failures (account_id, uidl, failures, last_error) VALUES (?1, ?2, 1, ?3) \
         ON CONFLICT (account_id, uidl) DO UPDATE SET failures = failures + 1, last_error = excluded.last_error \
         RETURNING failures",
        params![account_id, uidl, error],
        |row| row.get(0),
    )?)
}

/// The UIDLs refused [`SHOWN_AFTER`] times or more, each with the server's
/// last words, for the account's menu.
pub fn failing(conn: &Connection, account_id: AccountId) -> Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare_cached(
        "SELECT uidl, last_error FROM pop3_failures WHERE account_id = ?1 AND failures >= ?2 ORDER BY uidl",
    )?;
    let rows = stmt.query_map(params![account_id, SHOWN_AFTER], |row| {
        Ok((row.get(0)?, row.get(1)?))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The accounts with a message refused [`SHOWN_AFTER`] times or more.
pub fn accounts_failing(conn: &Connection) -> Result<Vec<AccountId>> {
    let mut stmt = conn.prepare_cached(
        "SELECT DISTINCT account_id FROM pop3_failures WHERE failures >= ?1 ORDER BY account_id",
    )?;
    let rows = stmt.query_map([SHOWN_AFTER], |row| row.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The message in the Sent mailbox whose `Message-ID` header is
/// `rfc822_msgid`, angle brackets included, for the outbox to tell a send
/// that went out from one that did not.
pub fn sent_with(
    conn: &Connection,
    account_id: AccountId,
    rfc822_msgid: &str,
) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT m.id FROM messages m \
             JOIN message_mailboxes l ON l.account_id = m.account_id AND l.message_id = m.id \
             JOIN mailboxes b ON b.key = l.mailbox AND b.role = 'sent' \
             WHERE m.account_id = ?1 AND m.rfc822_msgid = ?2 LIMIT 1",
            params![account_id, rfc822_msgid],
            |row| row.get(0),
        )
        .optional()?)
}
