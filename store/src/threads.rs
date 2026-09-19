//! Thread queries. `messages::refresh_thread` maintains the rows.

use mailrs_domain::{AccountId, FlagColor, ThreadSummary};
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::Result;

/// Which threads a list shows: one label, across all accounts or one,
/// optionally narrowed to a flag colour or to some senders.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ThreadFilter {
    pub account_id: Option<AccountId>,
    /// Empty means any mail outside Trash and Spam.
    pub label_id: String,
    /// Only mail starred with this colour. Starred mail without a colour
    /// counts as red.
    pub flag: Option<FlagColor>,
    /// Only threads with a message from one of these addresses.
    pub senders: Vec<String>,
}

impl ThreadFilter {
    /// Every account. Use system labels such as `INBOX`; user label ids differ per account.
    pub fn unified(label_id: impl Into<String>) -> Self {
        ThreadFilter {
            label_id: label_id.into(),
            ..ThreadFilter::default()
        }
    }

    pub fn account(account_id: AccountId, label_id: impl Into<String>) -> Self {
        ThreadFilter {
            account_id: Some(account_id),
            label_id: label_id.into(),
            ..ThreadFilter::default()
        }
    }

    pub fn with_flag(mut self, flag: FlagColor) -> Self {
        self.flag = Some(flag);
        self
    }

    pub fn from_senders(mut self, senders: Vec<String>) -> Self {
        self.senders = senders;
        self
    }

    /// The `FROM … WHERE …` part of a query over messages aliased `alias`,
    /// grouped by `thread` (the thread id column in scope). `?1` is the
    /// label and `?2` the account.
    fn clause(&self, from: &str, account: &str, thread: &str, message: Option<&str>) -> String {
        let scope = |inner: &str| match message {
            Some(message) => format!("{inner} AND x.id = {message}"),
            None => format!("{inner} AND x.thread_id = {thread}"),
        };
        let mut sql = format!("{from} WHERE (?2 IS NULL OR {account} = ?2)");
        let labelled = |label: &str| match message {
            Some(message) => format!(
                "EXISTS (SELECT 1 FROM message_labels l WHERE l.account_id = {account} \
                     AND l.message_id = {message} AND l.label_id = {label})"
            ),
            None => format!(
                "EXISTS (SELECT 1 FROM thread_labels l WHERE l.account_id = {account} \
                     AND l.thread_id = {thread} AND l.label_id = {label})"
            ),
        };
        if self.label_id.is_empty() {
            sql.push_str(&format!(
                " AND ?1 = '' AND NOT {} AND NOT {}",
                labelled("'TRASH'"),
                labelled("'SPAM'")
            ));
        } else {
            sql.push_str(&format!(" AND {}", labelled("?1")));
        }
        if let Some(flag) = self.flag {
            // The colour comes from a fixed list, so it is safe in the text.
            let color = flag.as_str();
            let default = if flag == FlagColor::Red {
                " OR f.color IS NULL"
            } else {
                ""
            };
            sql.push_str(&format!(
                " AND EXISTS (SELECT 1 FROM messages x JOIN message_labels s \
                 ON s.account_id = x.account_id AND s.message_id = x.id AND s.label_id = 'STARRED' \
                 LEFT JOIN flags f ON f.account_id = x.account_id AND f.message_id = x.id \
                 WHERE {} AND (f.color = '{color}'{default}))",
                scope(&format!("x.account_id = {account}"))
            ));
        }
        if !self.senders.is_empty() {
            let list = self
                .senders
                .iter()
                .map(|s| format!("'{}'", s.to_lowercase().replace('\'', "''")))
                .collect::<Vec<_>>()
                .join(", ");
            sql.push_str(&format!(
                " AND EXISTS (SELECT 1 FROM messages x WHERE {} AND lower(x.from_addr) IN ({list}))",
                scope(&format!("x.account_id = {account}"))
            ));
        }
        sql
    }

    fn threads(&self) -> String {
        self.clause("FROM threads t", "t.account_id", "t.id", None)
    }

    fn messages(&self) -> String {
        self.clause(
            "FROM messages m",
            "m.account_id",
            "m.thread_id",
            Some("m.id"),
        )
    }
}

const COLUMNS: &str = "t.account_id, t.id, t.last_message_at, t.subject, t.snippet, t.from_display, \
                       t.message_count, t.unread, t.starred, t.has_attachments, \
                       (SELECT f.color FROM flags f JOIN messages fm \
                        ON fm.account_id = f.account_id AND fm.id = f.message_id \
                        WHERE fm.account_id = t.account_id AND fm.thread_id = t.id \
                        ORDER BY fm.date DESC LIMIT 1)";

fn flag_color(row: &Row<'_>, index: usize) -> rusqlite::Result<Option<FlagColor>> {
    Ok(row
        .get::<_, Option<String>>(index)?
        .and_then(|c| c.parse().ok()))
}

fn to_summary(row: &Row<'_>) -> rusqlite::Result<ThreadSummary> {
    Ok(ThreadSummary {
        account_id: row.get(0)?,
        id: row.get(1)?,
        message_id: None,
        last_message_at: row.get(2)?,
        subject: row.get(3)?,
        snippet: row.get(4)?,
        from: row.get(5)?,
        message_count: row.get(6)?,
        unread: row.get(7)?,
        starred: row.get(8)?,
        has_attachments: row.get(9)?,
        flag_color: flag_color(row, 10)?,
    })
}

pub fn get_thread(
    conn: &Connection,
    account_id: AccountId,
    thread_id: &str,
) -> Result<Option<ThreadSummary>> {
    let sql = format!("SELECT {COLUMNS} FROM threads t WHERE t.account_id = ?1 AND t.id = ?2");
    Ok(conn
        .query_row(&sql, params![account_id, thread_id], to_summary)
        .optional()?)
}

/// Newest first. Ties break on account and thread id so pages never overlap.
pub fn list_threads(
    conn: &Connection,
    filter: &ThreadFilter,
    offset: i64,
    limit: i64,
) -> Result<Vec<ThreadSummary>> {
    let sql = format!(
        "SELECT {COLUMNS} {} ORDER BY t.last_message_at DESC, t.account_id, t.id LIMIT ?3 OFFSET ?4",
        filter.threads()
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map(
        params![filter.label_id, filter.account_id, limit, offset],
        to_summary,
    )?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn count_threads(conn: &Connection, filter: &ThreadFilter) -> Result<i64> {
    let sql = format!("SELECT COUNT(*) {}", filter.threads());
    Ok(
        conn.query_row(&sql, params![filter.label_id, filter.account_id], |row| {
            row.get(0)
        })?,
    )
}

/// Columns for one message shown as a list row.
const MESSAGE_COLUMNS: &str = "m.account_id, m.thread_id, m.id, m.date, m.subject, m.snippet, \
     COALESCE(m.from_name, m.from_addr, ''), m.has_attachments, \
     EXISTS (SELECT 1 FROM message_labels u WHERE u.account_id = m.account_id AND u.message_id = m.id \
             AND u.label_id = 'UNREAD'), \
     EXISTS (SELECT 1 FROM message_labels s WHERE s.account_id = m.account_id AND s.message_id = m.id \
             AND s.label_id = 'STARRED'), \
     (SELECT f.color FROM flags f WHERE f.account_id = m.account_id AND f.message_id = m.id)";

fn to_message_row(row: &Row<'_>) -> rusqlite::Result<ThreadSummary> {
    Ok(ThreadSummary {
        account_id: row.get(0)?,
        id: row.get(1)?,
        message_id: Some(row.get(2)?),
        last_message_at: row.get(3)?,
        subject: row.get(4)?,
        snippet: row.get(5)?,
        from: row.get(6)?,
        message_count: 1,
        has_attachments: row.get(7)?,
        unread: row.get(8)?,
        starred: row.get(9)?,
        flag_color: flag_color(row, 10)?,
    })
}

/// Messages rather than threads, newest first, for when conversation
/// grouping is off.
pub fn list_messages(
    conn: &Connection,
    filter: &ThreadFilter,
    offset: i64,
    limit: i64,
) -> Result<Vec<ThreadSummary>> {
    let sql = format!(
        "SELECT {MESSAGE_COLUMNS} {} ORDER BY m.date DESC, m.account_id, m.id LIMIT ?3 OFFSET ?4",
        filter.messages()
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map(
        params![filter.label_id, filter.account_id, limit, offset],
        to_message_row,
    )?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn unread_messages(conn: &Connection, filter: &ThreadFilter) -> Result<i64> {
    let sql = format!(
        "SELECT COUNT(*) {} AND EXISTS (SELECT 1 FROM message_labels u \
         WHERE u.account_id = m.account_id AND u.message_id = m.id AND u.label_id = 'UNREAD')",
        filter.messages()
    );
    Ok(
        conn.query_row(&sql, params![filter.label_id, filter.account_id], |row| {
            row.get(0)
        })?,
    )
}

pub fn unread_threads(conn: &Connection, filter: &ThreadFilter) -> Result<i64> {
    let sql = format!("SELECT COUNT(*) {} AND t.unread = 1", filter.threads());
    Ok(
        conn.query_row(&sql, params![filter.label_id, filter.account_id], |row| {
            row.get(0)
        })?,
    )
}
