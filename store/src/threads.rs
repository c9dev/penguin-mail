//! Thread queries. `messages::refresh_thread` maintains the rows.

use std::collections::HashMap;

use mailrs_domain::system_label::{SPAM, STARRED, TRASH};
use mailrs_domain::{AccountId, Category, FlagColor, ThreadSummary};
use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, Row, params, params_from_iter};

use crate::Result;

/// The labels a list hides. Gmail shows trashed and spam mail only in the
/// Trash and Spam lists, so every other list leaves it out, whether it asks
/// for one label or for any mail.
const HIDDEN: [&str; 2] = [TRASH, SPAM];

/// The hidden labels that a list of `label_id` still leaves out. Listing
/// the Trash keeps trashed mail; it only drops what is also spam.
fn hidden_from(label_id: &str) -> Vec<&'static str> {
    HIDDEN.into_iter().filter(|l| *l != label_id).collect()
}

/// Which threads a list shows: one label, across all accounts or one,
/// optionally narrowed to a flag colour or to some senders.
///
/// Whichever label it names, the list leaves out mail in the Trash and
/// Spam, as Gmail's own Sent and label views do.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ThreadFilter {
    pub account_id: Option<AccountId>,
    /// Empty means any mail with a label or without one.
    pub label_id: String,
    /// Only mail starred with this colour. Starred mail without a colour
    /// counts as red.
    pub flag: Option<FlagColor>,
    /// Only threads with a message from one of these addresses.
    pub senders: Vec<String>,
    /// Only threads with at least one of these labels, such as Gmail's
    /// `CATEGORY_SOCIAL` and `CATEGORY_FORUMS`. Empty means no condition.
    pub any_labels: Vec<String>,
    /// Only threads with none of these labels.
    pub no_labels: Vec<String>,
    /// Only these threads, by id. Empty means every thread.
    pub thread_ids: Vec<String>,
}

/// SQL text with anonymous `?` placeholders, and their values in order.
#[derive(Default)]
struct Sql {
    text: String,
    params: Vec<Value>,
}

impl Sql {
    fn push(&mut self, text: &str) -> &mut Self {
        self.text.push_str(text);
        self
    }

    fn bind(&mut self, value: impl Into<Value>) -> &mut Self {
        self.text.push('?');
        self.params.push(value.into());
        self
    }

    /// `?, ?, …` for each of `values`.
    fn bind_list<S: AsRef<str>>(&mut self, values: &[S]) -> &mut Self {
        for (i, value) in values.iter().enumerate() {
            if i > 0 {
                self.text.push_str(", ");
            }
            self.bind(value.as_ref().to_string());
        }
        self
    }
}

/// What a query lists: thread rows, or single messages.
#[derive(Clone, Copy)]
enum Rows {
    Threads,
    Messages,
}

impl Rows {
    /// The row table's alias.
    fn alias(self) -> &'static str {
        match self {
            Rows::Threads => "t",
            Rows::Messages => "m",
        }
    }

    /// The label table and its column that holds the row's id.
    fn labels(self) -> (&'static str, &'static str) {
        match self {
            Rows::Threads => ("thread_labels", "thread_id"),
            Rows::Messages => ("message_labels", "message_id"),
        }
    }

    /// Appends `EXISTS (…)`: the row carries one of `labels`.
    fn has_any<S: AsRef<str>>(self, sql: &mut Sql, labels: &[S]) {
        let (table, key) = self.labels();
        let row = self.alias();
        sql.push(&format!(
            "EXISTS (SELECT 1 FROM {table} l WHERE l.account_id = {row}.account_id \
             AND l.{key} = {row}.id AND l.label_id IN ("
        ))
        .bind_list(labels)
        .push("))");
    }

    /// The column that holds the row's thread id.
    fn thread_key(self) -> &'static str {
        match self {
            Rows::Threads => "t.id",
            Rows::Messages => "m.thread_id",
        }
    }

    /// The condition that a message `x` belongs to the row.
    fn scope(self) -> &'static str {
        match self {
            Rows::Threads => "x.account_id = t.account_id AND x.thread_id = t.id",
            Rows::Messages => "x.account_id = m.account_id AND x.id = m.id",
        }
    }
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

    /// Narrows the list to threads carrying one of `any` and none of `none`.
    pub fn with_labels(mut self, any: &[&str], none: &[&str]) -> Self {
        self.any_labels = any.iter().map(|l| l.to_string()).collect();
        self.no_labels = none.iter().map(|l| l.to_string()).collect();
        self
    }

    /// Narrows the list to these threads, so a caller can re-read the rows
    /// a change event named instead of listing the mailbox again.
    pub fn with_threads(mut self, thread_ids: Vec<String>) -> Self {
        self.thread_ids = thread_ids;
        self
    }

    /// Appends the `FROM … WHERE …` part of a query over `rows`.
    ///
    /// A label view starts from the label index and joins the rows it names,
    /// so it reads only that label's rows. `CROSS JOIN` fixes that order,
    /// since SQLite would otherwise walk every row newest first and probe
    /// the label for each. Only "any mail" walks every row.
    fn rows_matching(&self, rows: Rows, sql: &mut Sql) {
        let row = rows.alias();
        let (labels, key) = rows.labels();
        let table = match rows {
            Rows::Threads => "threads",
            Rows::Messages => "messages",
        };
        if self.label_id.is_empty() {
            sql.push(&format!("FROM {table} {row} WHERE "));
            if let Some(account) = self.account_id {
                sql.push(&format!("{row}.account_id = "))
                    .bind(account)
                    .push(" AND ");
            }
            sql.push("NOT ");
            rows.has_any(sql, &HIDDEN);
        } else {
            sql.push(&format!(
                "FROM {labels} d CROSS JOIN {table} {row} \
                 ON {row}.account_id = d.account_id AND {row}.id = d.{key} WHERE d.label_id = "
            ))
            .bind(self.label_id.clone());
            if let Some(account) = self.account_id {
                sql.push(" AND d.account_id = ").bind(account);
            }
            let hidden = hidden_from(&self.label_id);
            if !hidden.is_empty() {
                sql.push(" AND NOT ");
                rows.has_any(sql, &hidden);
            }
        }
        if !self.thread_ids.is_empty() {
            sql.push(&format!(" AND {} IN (", rows.thread_key()))
                .bind_list(&self.thread_ids)
                .push(")");
        }
        if !self.any_labels.is_empty() {
            sql.push(" AND ");
            rows.has_any(sql, &self.any_labels);
        }
        if !self.no_labels.is_empty() {
            sql.push(" AND NOT ");
            rows.has_any(sql, &self.no_labels);
        }
        if let Some(flag) = self.flag {
            if let Rows::Threads = rows {
                // Only a thread with a starred message can match, and
                // `threads.starred` says which do without a lookup.
                sql.push(" AND t.starred = 1");
            }
            sql.push(&format!(
                " AND EXISTS (SELECT 1 FROM messages x JOIN message_labels s \
                 ON s.account_id = x.account_id AND s.message_id = x.id AND s.label_id = '{STARRED}' \
                 LEFT JOIN flags f ON f.account_id = x.account_id AND f.message_id = x.id \
                 WHERE {} AND (f.color = ",
                rows.scope()
            ))
            .bind(flag.as_str().to_string());
            if flag == FlagColor::Red {
                sql.push(" OR f.color IS NULL");
            }
            sql.push("))");
        }
        if !self.senders.is_empty() {
            let senders: Vec<String> = self.senders.iter().map(|s| s.to_lowercase()).collect();
            sql.push(&format!(
                " AND EXISTS (SELECT 1 FROM messages x WHERE {} AND lower(x.from_addr) IN (",
                rows.scope()
            ))
            .bind_list(&senders)
            .push("))");
        }
    }

    fn query(&self, rows: Rows, select: &str) -> Sql {
        let mut sql = Sql::default();
        sql.push(select).push(" ");
        self.rows_matching(rows, &mut sql);
        sql
    }
}

/// Appends the condition that a message row `m` is unread.
const MESSAGE_UNREAD: &str = " AND EXISTS (SELECT 1 FROM message_labels u \
     WHERE u.account_id = m.account_id AND u.message_id = m.id AND u.label_id = 'UNREAD')";

fn count(conn: &Connection, sql: &Sql) -> Result<i64> {
    Ok(conn
        .prepare_cached(&sql.text)?
        .query_row(params_from_iter(&sql.params), |row| row.get(0))?)
}

const COLUMNS: &str = "t.account_id, t.id, t.last_message_at, t.subject, t.snippet, t.from_display, \
                       t.message_count, t.unread, t.starred, t.has_attachments, t.flag_color, \
                       t.from_email, \
                       EXISTS (SELECT 1 FROM thread_labels z WHERE z.account_id = t.account_id \
                               AND z.thread_id = t.id AND z.label_id = 'MUTE')";

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
        from_email: row.get(11)?,
        muted: row.get(12)?,
    })
}

pub fn get_thread(
    conn: &Connection,
    account_id: AccountId,
    thread_id: &str,
) -> Result<Option<ThreadSummary>> {
    let sql = format!("SELECT {COLUMNS} FROM threads t WHERE t.account_id = ?1 AND t.id = ?2");
    Ok(conn
        .prepare_cached(&sql)?
        .query_row(params![account_id, thread_id], to_summary)
        .optional()?)
}

/// Newest first. Ties break on account and thread id so pages never overlap.
pub fn list_threads(
    conn: &Connection,
    filter: &ThreadFilter,
    offset: i64,
    limit: i64,
) -> Result<Vec<ThreadSummary>> {
    let mut sql = filter.query(Rows::Threads, &format!("SELECT {COLUMNS}"));
    sql.push(" ORDER BY t.last_message_at DESC, t.account_id, t.id LIMIT ")
        .bind(limit)
        .push(" OFFSET ")
        .bind(offset);
    let mut stmt = conn.prepare_cached(&sql.text)?;
    let rows = stmt.query_map(params_from_iter(&sql.params), to_summary)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn count_threads(conn: &Connection, filter: &ThreadFilter) -> Result<i64> {
    count(conn, &filter.query(Rows::Threads, "SELECT COUNT(*)"))
}

/// Columns for one message shown as a list row.
const MESSAGE_COLUMNS: &str = "m.account_id, m.thread_id, m.id, m.date, m.subject, m.snippet, \
     COALESCE(m.from_name, m.from_addr, ''), m.has_attachments, \
     EXISTS (SELECT 1 FROM message_labels u WHERE u.account_id = m.account_id AND u.message_id = m.id \
             AND u.label_id = 'UNREAD'), \
     EXISTS (SELECT 1 FROM message_labels s WHERE s.account_id = m.account_id AND s.message_id = m.id \
             AND s.label_id = 'STARRED'), \
     (SELECT f.color FROM flags f WHERE f.account_id = m.account_id AND f.message_id = m.id), \
     COALESCE(m.from_addr, ''), \
     EXISTS (SELECT 1 FROM message_labels z WHERE z.account_id = m.account_id AND z.message_id = m.id \
             AND z.label_id = 'MUTE')";

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
        from_email: row.get(11)?,
        muted: row.get(12)?,
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
    let mut sql = filter.query(Rows::Messages, &format!("SELECT {MESSAGE_COLUMNS}"));
    sql.push(" ORDER BY m.date DESC, m.account_id, m.id LIMIT ")
        .bind(limit)
        .push(" OFFSET ")
        .bind(offset);
    let mut stmt = conn.prepare_cached(&sql.text)?;
    let rows = stmt.query_map(params_from_iter(&sql.params), to_message_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn unread_messages(conn: &Connection, filter: &ThreadFilter) -> Result<i64> {
    let mut sql = filter.query(Rows::Messages, "SELECT COUNT(*)");
    sql.push(MESSAGE_UNREAD);
    count(conn, &sql)
}

pub fn unread_threads(conn: &Connection, filter: &ThreadFilter) -> Result<i64> {
    let mut sql = filter.query(Rows::Threads, "SELECT COUNT(*)");
    sql.push(" AND t.unread = 1");
    count(conn, &sql)
}

/// How many threads carry a label, and how many of those are unread.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Count {
    pub threads: i64,
    pub unread: i64,
}

/// Thread counts for every label of every account, from one grouped query.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LabelCounts {
    counts: HashMap<(AccountId, String), Count>,
}

impl LabelCounts {
    /// `count_threads` and `unread_threads` for `ThreadFilter::account(account_id, label_id)`.
    pub fn account(&self, account_id: AccountId, label_id: &str) -> Count {
        self.counts
            .get(&(account_id, label_id.to_string()))
            .copied()
            .unwrap_or_default()
    }

    /// `count_threads` and `unread_threads` for `ThreadFilter::unified(label_id)`.
    pub fn unified(&self, label_id: &str) -> Count {
        self.counts
            .iter()
            .filter(|((_, label), _)| label == label_id)
            .fold(Count::default(), |sum, (_, c)| Count {
                threads: sum.threads + c.threads,
                unread: sum.unread + c.unread,
            })
    }
}

/// Every label's thread and unread counts, for the sidebar, in one query
/// instead of two per mailbox. A label leaves out the same trashed and spam
/// mail `ThreadFilter` does, so a count and its list agree.
pub fn label_counts(conn: &Connection) -> Result<LabelCounts> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT d.account_id, d.label_id, COUNT(*), SUM(t.unread) FROM thread_labels d \
         CROSS JOIN threads t ON t.account_id = d.account_id AND t.id = d.thread_id \
         WHERE NOT EXISTS (SELECT 1 FROM thread_labels h WHERE h.account_id = d.account_id \
             AND h.thread_id = d.thread_id AND h.label_id IN ('{TRASH}', '{SPAM}') \
             AND h.label_id <> d.label_id) \
         GROUP BY d.label_id, d.account_id"
    ))?;
    let rows = stmt.query_map([], |row| {
        Ok((
            (row.get::<_, AccountId>(0)?, row.get::<_, String>(1)?),
            Count {
                threads: row.get(2)?,
                unread: row.get(3)?,
            },
        ))
    })?;
    Ok(LabelCounts {
        counts: rows.collect::<rusqlite::Result<_>>()?,
    })
}

/// `unread_threads` for `filter` narrowed to each category, in one query.
/// Each category's labels replace the filter's own, as `with_labels` does.
pub fn category_unread_threads(
    conn: &Connection,
    filter: &ThreadFilter,
) -> Result<HashMap<Category, i64>> {
    category_unread(conn, filter, Rows::Threads)
}

/// `unread_messages` for `filter` narrowed to each category, in one query.
/// Each category's labels replace the filter's own, as `with_labels` does.
pub fn category_unread_messages(
    conn: &Connection,
    filter: &ThreadFilter,
) -> Result<HashMap<Category, i64>> {
    category_unread(conn, filter, Rows::Messages)
}

fn category_unread(
    conn: &Connection,
    filter: &ThreadFilter,
    rows: Rows,
) -> Result<HashMap<Category, i64>> {
    let mut sql = Sql::default();
    sql.push("SELECT ");
    for (i, category) in Category::ALL.into_iter().enumerate() {
        if i > 0 {
            sql.push(", ");
        }
        let (any, none) = category.labels();
        sql.push("COALESCE(SUM(1");
        if !any.is_empty() {
            sql.push(" AND ");
            rows.has_any(&mut sql, any);
        }
        if !none.is_empty() {
            sql.push(" AND NOT ");
            rows.has_any(&mut sql, none);
        }
        sql.push("), 0)");
    }
    sql.push(" ");
    let base = ThreadFilter {
        any_labels: Vec::new(),
        no_labels: Vec::new(),
        ..filter.clone()
    };
    base.rows_matching(rows, &mut sql);
    sql.push(match rows {
        Rows::Threads => " AND t.unread = 1",
        Rows::Messages => MESSAGE_UNREAD,
    });
    let counts =
        conn.prepare_cached(&sql.text)?
            .query_row(params_from_iter(&sql.params), |row| {
                (0..Category::ALL.len())
                    .map(|i| row.get::<_, i64>(i))
                    .collect::<rusqlite::Result<Vec<i64>>>()
            })?;
    Ok(Category::ALL.into_iter().zip(counts).collect())
}
