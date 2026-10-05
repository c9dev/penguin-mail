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

/// Whether the account's first check finished: every message the server
/// listed then was downloaded or recorded as failed. Until it has, a check
/// takes each message's own date and announces nothing.
pub fn first_check_finished(conn: &Connection, account_id: AccountId) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT pop3_first_check_done FROM accounts WHERE id = ?1",
        [account_id],
        |row| row.get(0),
    )?)
}

/// Records that the account's first check finished.
pub fn finish_first_check(conn: &Connection, account_id: AccountId) -> Result<()> {
    conn.execute(
        "UPDATE accounts SET pop3_first_check_done = 1 WHERE id = ?1",
        [account_id],
    )?;
    Ok(())
}

/// The store id for a download of `uidl`: `pop3/<uidl>`, or while a
/// message holds that id, `pop3/<uidl>/<n>` with the first free `n` from 2.
/// A server may give a UIDL to a new message once the old one has left it
/// (RFC 1939 section 7), and the old message here may be the only copy.
pub fn download_id(conn: &Connection, account_id: AccountId, uidl: &str) -> Result<String> {
    let mut stmt = conn.prepare_cached(
        "SELECT EXISTS (SELECT 1 FROM messages WHERE account_id = ?1 AND id = ?2) \
         OR EXISTS (SELECT 1 FROM local_messages WHERE account_id = ?1 AND message_id = ?2)",
    )?;
    let mut id = format!("pop3/{uidl}");
    let mut n = 1;
    while stmt.query_row(params![account_id, id], |row| row.get::<_, bool>(0))? {
        n += 1;
        id = format!("pop3/{uidl}/{n}");
    }
    Ok(id)
}

/// Records `uidl` as downloaded at `at` into message `message_id`, and
/// drops its failure count.
pub fn mark_downloaded(
    conn: &Connection,
    account_id: AccountId,
    uidl: &str,
    message_id: &str,
    at: EpochMillis,
) -> Result<()> {
    conn.execute(
        "INSERT INTO pop3_seen (account_id, uidl, downloaded_at, message_id) VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT (account_id, uidl) DO NOTHING",
        params![account_id, uidl, at, message_id],
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

/// Asks for a DELE of the downloads that brought `message_ids`, for Delete
/// Forever on an account that removes mail from the server. A message
/// made here, or one whose UIDL the server has since given to another
/// message, has no row and is left alone.
pub fn want_removed_of(
    conn: &Connection,
    account_id: AccountId,
    message_ids: &[String],
) -> Result<()> {
    conn.execute(
        "UPDATE pop3_seen SET remove_wanted = 1 WHERE account_id = ?1 AND removed = 0 \
         AND message_id IN (SELECT value FROM json_each(?2))",
        params![account_id, json(message_ids)],
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

/// Drops the failure count of every UIDL `listed`, the server's whole list,
/// lacks, a page at a time. Such a message will never download, so it
/// leaves the account's menu. True when one that the menu showed went.
///
/// A message the server lists again later starts its count from nothing
/// and reaches the menu again at its third failure. To this account it is
/// a new arrival: the server may have given its UIDL to another message.
pub fn forget_gone_failures(
    conn: &Connection,
    account_id: AccountId,
    listed: &HashSet<String>,
) -> Result<bool> {
    let mut after = String::new();
    let mut shown_went = false;
    loop {
        let page: Vec<(String, i64)> = conn
            .prepare_cached(
                "SELECT uidl, failures FROM pop3_failures WHERE account_id = ?1 AND uidl > ?2 \
                 ORDER BY uidl LIMIT ?3",
            )?
            .query_map(params![account_id, after, PAGE as i64], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        let Some((last, _)) = page.last() else {
            return Ok(shown_went);
        };
        after = last.clone();
        let gone: Vec<String> = page
            .into_iter()
            .filter(|(uidl, _)| !listed.contains(uidl))
            .map(|(uidl, failures)| {
                shown_went |= failures >= SHOWN_AFTER;
                uidl
            })
            .collect();
        conn.execute(
            "DELETE FROM pop3_failures WHERE account_id = ?1 AND uidl IN (SELECT value FROM json_each(?2))",
            params![account_id, json(&gone)],
        )?;
    }
}

/// Why a message did not download. Stored as a code, so the window shows
/// it in the language it runs in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FailReason {
    /// The server answered `-ERR`, in the words kept beside it.
    Refused,
    /// `LIST` or the answer itself was past what Penguin Mail reads.
    TooLarge,
    /// The server's answer was not POP3.
    Unreadable,
    /// The connection dropped while the message came down.
    Dropped,
}

impl FailReason {
    fn code(self) -> &'static str {
        match self {
            FailReason::Refused => "refused",
            FailReason::TooLarge => "too_large",
            FailReason::Unreadable => "unreadable",
            FailReason::Dropped => "dropped",
        }
    }

    fn from_code(code: &str) -> FailReason {
        match code {
            "too_large" => FailReason::TooLarge,
            "unreadable" => FailReason::Unreadable,
            "dropped" => FailReason::Dropped,
            _ => FailReason::Refused,
        }
    }
}

/// A message that failed [`SHOWN_AFTER`] times or more.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failing {
    pub uidl: String,
    /// Why it failed the last time.
    pub reason: FailReason,
    /// The server's words for a refusal; empty for the other reasons.
    pub words: String,
    /// Who sent it and its subject, once a `TOP` has read its headers.
    pub sender: Option<String>,
    pub subject: Option<String>,
}

/// Counts one failed RETR of `uidl`, keeping why and, for a refusal, the
/// server's words. Answers how many there have been.
pub fn record_failure(
    conn: &Connection,
    account_id: AccountId,
    uidl: &str,
    reason: FailReason,
    words: &str,
) -> Result<i64> {
    Ok(conn.query_row(
        "INSERT INTO pop3_failures (account_id, uidl, failures, last_error, reason) VALUES (?1, ?2, 1, ?3, ?4) \
         ON CONFLICT (account_id, uidl) DO UPDATE SET failures = failures + 1, \
         last_error = excluded.last_error, reason = excluded.reason \
         RETURNING failures",
        params![account_id, uidl, words, reason.code()],
        |row| row.get(0),
    )?)
}

/// Keeps who sent the failing message `uidl` and its subject, read from
/// its headers, so the account's menu can name it.
pub fn name_failure(
    conn: &Connection,
    account_id: AccountId,
    uidl: &str,
    sender: Option<&str>,
    subject: Option<&str>,
) -> Result<()> {
    conn.execute(
        "UPDATE pop3_failures SET sender = ?3, subject = ?4 WHERE account_id = ?1 AND uidl = ?2",
        params![account_id, uidl, sender, subject],
    )?;
    Ok(())
}

/// The UIDLs among `uidls` that have failed before, each with why it
/// failed the last time, in UIDL order.
pub fn failure_reasons(
    conn: &Connection,
    account_id: AccountId,
    uidls: &[String],
) -> Result<Vec<(String, FailReason)>> {
    let mut stmt = conn.prepare_cached(
        "SELECT uidl, reason FROM pop3_failures \
         WHERE account_id = ?1 AND uidl IN (SELECT value FROM json_each(?2)) ORDER BY uidl",
    )?;
    let rows = stmt.query_map(params![account_id, json(uidls)], |row| {
        Ok((
            row.get(0)?,
            FailReason::from_code(&row.get::<_, String>(1)?),
        ))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The messages that failed [`SHOWN_AFTER`] times or more, for the
/// account's menu.
pub fn failing(conn: &Connection, account_id: AccountId) -> Result<Vec<Failing>> {
    let mut stmt = conn.prepare_cached(
        "SELECT uidl, reason, last_error, sender, subject FROM pop3_failures \
         WHERE account_id = ?1 AND failures >= ?2 ORDER BY uidl",
    )?;
    let rows = stmt.query_map(params![account_id, SHOWN_AFTER], |row| {
        Ok(Failing {
            uidl: row.get(0)?,
            reason: FailReason::from_code(&row.get::<_, String>(1)?),
            words: row.get(2)?,
            sender: row.get(3)?,
            subject: row.get(4)?,
        })
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
