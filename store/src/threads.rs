//! Thread queries. `messages::refresh_thread` maintains the rows.

use std::collections::{HashMap, HashSet};

use mailrs_domain::system_label::{SPAM, STARRED, TRASH, UNREAD};
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

/// How a query reaches its rows. Every walk finds the same rows; they
/// differ in how many they read on the way.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Walk {
    /// Every row in table order. A count with nothing to start from reads
    /// this way.
    Scan,
    /// Through the rows newest first, stopping at a full page: reads about
    /// as many rows as the page needs, divided by the share that matches.
    Date,
    /// Through the label index, reading the rows that carry any of these
    /// labels, then sorted.
    Labels(Vec<String>),
    /// Through `messages_by_sender`, reading the mail from the filter's
    /// senders, then sorted.
    Senders,
    /// Through the primary key, reading the filter's own threads.
    Threads,
}

/// Whether a page that ends `wanted` rows in should walk by date rather
/// than start from a set of `start_rows` among `all_rows`. Walking by date
/// reads about `wanted * all_rows / start_rows` rows and starting from the
/// set reads `start_rows`, so the date wins once `start_rows²` passes
/// `wanted * all_rows`. An inbox that holds most of the mail walks by date;
/// a label of a few hundred threads among thousands walks its own rows.
fn date_wins(start_rows: i64, all_rows: i64, wanted: i64) -> bool {
    if start_rows <= 0 {
        return false;
    }
    let (start_rows, all_rows, wanted) = (start_rows as i128, all_rows as i128, wanted as i128);
    wanted * all_rows < start_rows * start_rows
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
    /// The row table.
    fn table(self) -> &'static str {
        match self {
            Rows::Threads => "threads",
            Rows::Messages => "messages",
        }
    }

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

    /// Appends the condition that the row is not hidden by one of
    /// `hidden`. A message row is hidden when it carries one. A thread row
    /// is hidden only when every message in it that carries `label` (any
    /// message, for an empty label) carries one too: Gmail keeps a
    /// conversation in the inbox while one of its messages outside the
    /// Trash is there, so trashing the start of a thread leaves the reply.
    /// The thread's own labels answer the common case, a thread with no
    /// hidden label at all, without looking at its messages.
    fn not_hidden(self, sql: &mut Sql, label: &str, hidden: &[&str]) {
        sql.push("(NOT ");
        self.has_any(sql, hidden);
        if let Rows::Threads = self {
            sql.push(
                " OR EXISTS (SELECT 1 FROM messages x \
                 WHERE x.account_id = t.account_id AND x.thread_id = t.id",
            );
            if !label.is_empty() {
                sql.push(
                    " AND EXISTS (SELECT 1 FROM message_labels k \
                     WHERE k.account_id = x.account_id AND k.message_id = x.id \
                     AND k.label_id = ",
                )
                .bind(label.to_string())
                .push(")");
            }
            sql.push(
                " AND NOT EXISTS (SELECT 1 FROM message_labels h \
                 WHERE h.account_id = x.account_id AND h.message_id = x.id \
                 AND h.label_id IN (",
            )
            .bind_list(hidden)
            .push(")))");
        }
        sql.push(")");
    }

    /// The index that holds the rows in the order a list shows them.
    fn order_index(self) -> &'static str {
        match self {
            Rows::Threads => "threads_by_order",
            Rows::Messages => "messages_by_order",
        }
    }

    /// Appends the order a list shows, newest first with ties broken on
    /// account and id so pages never overlap, and the page's bounds.
    fn order(self, sql: &mut Sql, offset: i64, limit: i64) {
        let order = match self {
            Rows::Threads => " ORDER BY t.last_message_at DESC, t.account_id, t.id LIMIT ",
            Rows::Messages => " ORDER BY m.date DESC, m.account_id, m.id LIMIT ",
        };
        sql.push(order).bind(limit).push(" OFFSET ").bind(offset);
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

    /// Appends the `FROM … WHERE …` part of a query over `rows`, reaching
    /// them through `walk`.
    ///
    /// A walk that starts from an index joins the rows it names, so it
    /// reads only those. `CROSS JOIN` fixes that order, since SQLite would
    /// otherwise walk every row newest first and probe the index for each.
    /// Walking by date does exactly that, on purpose, through the order
    /// index; see [`date_wins`] for when it pays. Whatever the walk, every
    /// condition of the filter follows, less the one the walk already met.
    fn rows_matching(&self, rows: Rows, sql: &mut Sql, walk: &Walk) {
        let row = rows.alias();
        let (labels, key) = rows.labels();
        let table = rows.table();
        match walk {
            Walk::Scan => {
                sql.push(&format!("FROM {table} {row} WHERE 1"));
            }
            Walk::Date => {
                sql.push(&format!(
                    "FROM {table} {row} INDEXED BY {} WHERE 1",
                    rows.order_index()
                ));
            }
            Walk::Labels(start) if start.len() == 1 => {
                sql.push(&format!(
                    "FROM {labels} d CROSS JOIN {table} {row} \
                     ON {row}.account_id = d.account_id AND {row}.id = d.{key} WHERE d.label_id = "
                ))
                .bind(start[0].clone());
                if let Some(account) = self.account_id {
                    sql.push(" AND d.account_id = ").bind(account);
                }
            }
            Walk::Labels(start) => {
                // A row can carry two of the labels, so the set is made
                // distinct before it names rows.
                sql.push(&format!(
                    "FROM (SELECT DISTINCT account_id, {key} AS id FROM {labels} \
                     WHERE label_id IN ("
                ))
                .bind_list(start)
                .push(")");
                if let Some(account) = self.account_id {
                    sql.push(" AND account_id = ").bind(account);
                }
                sql.push(&format!(
                    ") d CROSS JOIN {table} {row} \
                     ON {row}.account_id = d.account_id AND {row}.id = d.id WHERE 1"
                ));
            }
            Walk::Senders => match rows {
                Rows::Threads => {
                    sql.push(
                        "FROM (SELECT DISTINCT account_id, thread_id AS id \
                         FROM messages INDEXED BY messages_by_sender WHERE lower(from_addr) IN (",
                    )
                    .bind_list(&self.lowercase_senders())
                    .push(")");
                    if let Some(account) = self.account_id {
                        sql.push(" AND account_id = ").bind(account);
                    }
                    sql.push(
                        ") d CROSS JOIN threads t \
                         ON t.account_id = d.account_id AND t.id = d.id WHERE 1",
                    );
                }
                Rows::Messages => {
                    sql.push(
                        "FROM messages m INDEXED BY messages_by_sender \
                         WHERE lower(m.from_addr) IN (",
                    )
                    .bind_list(&self.lowercase_senders())
                    .push(")");
                }
            },
            Walk::Threads => {
                sql.push(&format!(
                    "FROM {table} {row} WHERE {} IN (",
                    rows.thread_key()
                ))
                .bind_list(&self.thread_ids)
                .push(")");
                // The primary key and `messages_by_thread` lead with the
                // account, so naming every account lets a thread id reach
                // its rows through either.
                if self.account_id.is_none() {
                    sql.push(&format!(
                        " AND {row}.account_id IN (SELECT id FROM accounts)"
                    ));
                }
            }
        }
        if !self.label_id.is_empty() && *walk != Walk::Labels(vec![self.label_id.clone()]) {
            sql.push(&format!(
                " AND EXISTS (SELECT 1 FROM {labels} l WHERE l.account_id = {row}.account_id \
                 AND l.{key} = {row}.id AND l.label_id = "
            ))
            .bind(self.label_id.clone())
            .push(")");
        }
        if let Some(account) = self.account_id {
            sql.push(&format!(" AND {row}.account_id = ")).bind(account);
        }
        let hidden = match self.label_id.is_empty() {
            true => HIDDEN.to_vec(),
            false => hidden_from(&self.label_id),
        };
        if !hidden.is_empty() {
            sql.push(" AND ");
            rows.not_hidden(sql, &self.label_id, &hidden);
        }
        if !self.thread_ids.is_empty() && *walk != Walk::Threads {
            sql.push(&format!(" AND {} IN (", rows.thread_key()))
                .bind_list(&self.thread_ids)
                .push(")");
        }
        if !self.any_labels.is_empty() && *walk != Walk::Labels(self.any_labels.clone()) {
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
        if !self.senders.is_empty() && *walk != Walk::Senders {
            // A thread holds a message or two, so reading them beats a
            // probe of `messages_by_sender` for each sender.
            let probe = match rows {
                Rows::Threads => "messages x INDEXED BY messages_by_thread",
                Rows::Messages => "messages x",
            };
            sql.push(&format!(
                " AND EXISTS (SELECT 1 FROM {probe} WHERE {} AND lower(x.from_addr) IN (",
                rows.scope()
            ))
            .bind_list(&self.lowercase_senders())
            .push("))");
        }
    }

    fn lowercase_senders(&self) -> Vec<String> {
        self.senders.iter().map(|s| s.to_lowercase()).collect()
    }

    fn query_walking(&self, rows: Rows, select: &str, walk: &Walk) -> Sql {
        let mut sql = Sql::default();
        sql.push(select).push(" ");
        self.rows_matching(rows, &mut sql, walk);
        sql
    }

    /// A query over `rows` that counts or sums rather than pages, through
    /// the smallest set it can start from. `unread` says the caller keeps
    /// only unread rows, which makes the unread mail one more such set.
    fn counting(&self, conn: &Connection, rows: Rows, select: &str, unread: bool) -> Result<Sql> {
        let walk = self.count_walk(conn, rows, unread)?;
        Ok(self.query_walking(rows, select, &walk))
    }

    /// The walk for a query that reads every row it keeps: from the
    /// smallest set it can start from, or through the whole table.
    fn count_walk(&self, conn: &Connection, rows: Rows, unread: bool) -> Result<Walk> {
        if let Some(walk) = self.fixed_walk() {
            return Ok(walk);
        }
        Ok(self
            .rarest(conn, rows, unread)?
            .map_or(Walk::Scan, |(walk, _)| walk))
    }

    /// The walk that needs no counting: the filter names its threads.
    fn fixed_walk(&self) -> Option<Walk> {
        (!self.thread_ids.is_empty()).then_some(Walk::Threads)
    }

    /// The sets of rows an index can hand over, each of which holds every
    /// row the filter keeps, and so each a place a walk can start. The
    /// label comes last because it is the one most likely to hold most of
    /// the mail, as the inbox does.
    fn starts(&self, unread: bool) -> Vec<Walk> {
        let mut starts = Vec::new();
        if unread {
            starts.push(Walk::Labels(vec![UNREAD.to_string()]));
        }
        // A flag needs a starred message, and so does its thread.
        if self.flag.is_some() {
            starts.push(Walk::Labels(vec![STARRED.to_string()]));
        }
        if !self.senders.is_empty() {
            starts.push(Walk::Senders);
        }
        if !self.any_labels.is_empty() {
            starts.push(Walk::Labels(self.any_labels.clone()));
        }
        if !self.label_id.is_empty() {
            starts.push(Walk::Labels(vec![self.label_id.clone()]));
        }
        starts
    }

    /// The smallest of the sets a walk can start from, and how many rows
    /// it holds. Each count reads one index range and stops once it passes
    /// the smallest set so far, so an inbox of thousands costs no more to
    /// rule out than the few hundred rows that beat it.
    fn rarest(&self, conn: &Connection, rows: Rows, unread: bool) -> Result<Option<(Walk, i64)>> {
        let (labels, _) = rows.labels();
        let mut rarest: Option<(Walk, i64)> = None;
        for walk in self.starts(unread) {
            let least = rarest.as_ref().map(|(_, least)| *least);
            let mut sql = Sql::default();
            sql.push(match least {
                Some(_) => "SELECT count(*) FROM (SELECT 1 FROM ",
                None => "SELECT count(*) FROM ",
            });
            match &walk {
                Walk::Labels(start) => {
                    sql.push(&format!("{labels} WHERE label_id IN ("))
                        .bind_list(start)
                        .push(")");
                }
                _ => {
                    sql.push("messages INDEXED BY messages_by_sender WHERE lower(from_addr) IN (")
                        .bind_list(&self.lowercase_senders())
                        .push(")");
                }
            }
            if let Some(account) = self.account_id {
                sql.push(" AND account_id = ").bind(account);
            }
            if let Some(least) = least {
                sql.push(" LIMIT ").bind(least).push(")");
            }
            let size = count(conn, &sql)?;
            if least.is_none_or(|least| size < least) {
                rarest = Some((walk, size));
            }
        }
        Ok(rarest)
    }

    /// The cheapest walk for a page of `rows` that ends `wanted` rows in.
    fn walk(&self, conn: &Connection, rows: Rows, wanted: i64) -> Result<Walk> {
        if let Some(walk) = self.fixed_walk() {
            return Ok(walk);
        }
        let Some((start, size)) = self.rarest(conn, rows, false)? else {
            return Ok(Walk::Date);
        };
        let mut all_rows = Sql::default();
        all_rows.push(&format!("SELECT count(*) FROM {}", rows.table()));
        if let Some(account) = self.account_id {
            all_rows.push(" WHERE account_id = ").bind(account);
        }
        Ok(match date_wins(size, count(conn, &all_rows)?, wanted) {
            true => Walk::Date,
            false => start,
        })
    }

    /// Every walk this filter can take, for tests that check they agree.
    #[cfg(test)]
    fn every_walk(&self, unread: bool) -> Vec<Walk> {
        let mut walks = vec![Walk::Scan, Walk::Date];
        walks.extend(self.starts(unread));
        walks.extend(self.fixed_walk());
        walks
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
    let walk = filter.walk(conn, Rows::Threads, offset + limit)?;
    threads_walking(conn, filter, offset, limit, walk)
}

fn threads_walking(
    conn: &Connection,
    filter: &ThreadFilter,
    offset: i64,
    limit: i64,
    walk: Walk,
) -> Result<Vec<ThreadSummary>> {
    let mut sql = filter.query_walking(Rows::Threads, &format!("SELECT {COLUMNS}"), &walk);
    Rows::Threads.order(&mut sql, offset, limit);
    let mut stmt = conn.prepare_cached(&sql.text)?;
    let rows = stmt.query_map(params_from_iter(&sql.params), to_summary)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn count_threads(conn: &Connection, filter: &ThreadFilter) -> Result<i64> {
    count(
        conn,
        &filter.counting(conn, Rows::Threads, "SELECT COUNT(*)", false)?,
    )
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
    let walk = filter.walk(conn, Rows::Messages, offset + limit)?;
    messages_walking(conn, filter, offset, limit, walk)
}

fn messages_walking(
    conn: &Connection,
    filter: &ThreadFilter,
    offset: i64,
    limit: i64,
    walk: Walk,
) -> Result<Vec<ThreadSummary>> {
    let mut sql = filter.query_walking(Rows::Messages, &format!("SELECT {MESSAGE_COLUMNS}"), &walk);
    Rows::Messages.order(&mut sql, offset, limit);
    let mut stmt = conn.prepare_cached(&sql.text)?;
    let rows = stmt.query_map(params_from_iter(&sql.params), to_message_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn unread_messages(conn: &Connection, filter: &ThreadFilter) -> Result<i64> {
    let mut sql = filter.counting(conn, Rows::Messages, "SELECT COUNT(*)", true)?;
    sql.push(MESSAGE_UNREAD);
    count(conn, &sql)
}

pub fn unread_threads(conn: &Connection, filter: &ThreadFilter) -> Result<i64> {
    let mut sql = filter.counting(conn, Rows::Threads, "SELECT COUNT(*)", true)?;
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
         WHERE (NOT EXISTS (SELECT 1 FROM thread_labels h WHERE h.account_id = d.account_id \
             AND h.thread_id = d.thread_id AND h.label_id IN ('{TRASH}', '{SPAM}') \
             AND h.label_id <> d.label_id) \
           OR EXISTS (SELECT 1 FROM messages x JOIN message_labels k \
             ON k.account_id = x.account_id AND k.message_id = x.id AND k.label_id = d.label_id \
             WHERE x.account_id = d.account_id AND x.thread_id = d.thread_id \
             AND NOT EXISTS (SELECT 1 FROM message_labels h WHERE h.account_id = x.account_id \
               AND h.message_id = x.id AND h.label_id IN ('{TRASH}', '{SPAM}') \
               AND h.label_id <> d.label_id))) \
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

/// Which unread threads have mail from which senders, for the VIP counts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SenderCounts {
    /// Each unread thread, by account and id, with the lowercased senders
    /// of its messages among the ones asked about.
    threads: HashMap<(AccountId, String), HashSet<String>>,
}

impl SenderCounts {
    /// `unread_threads` for `ThreadFilter::unified("").from_senders(senders)`.
    /// A thread with mail from two of `senders` counts once.
    pub fn unread(&self, senders: &[String]) -> i64 {
        let wanted: HashSet<String> = senders.iter().map(|s| s.to_lowercase()).collect();
        self.threads
            .values()
            .filter(|from| !from.is_disjoint(&wanted))
            .count() as i64
    }
}

/// The unread threads with mail from any of `senders`, and who sent it, in
/// one query. The sidebar shows a VIP row for everyone and one per person,
/// and a query per row walked every thread once for each.
pub fn sender_counts(conn: &Connection, senders: &[String]) -> Result<SenderCounts> {
    let mut senders: Vec<String> = senders.iter().map(|s| s.to_lowercase()).collect();
    senders.sort();
    senders.dedup();
    if senders.is_empty() {
        return Ok(SenderCounts::default());
    }
    // The inner query is the one `unread_threads` runs for every sender at
    // once, so a thread counts here exactly when it counts there. The outer
    // one reads who wrote in each from `messages_by_sender` alone.
    let inner = ThreadFilter::unified("")
        .from_senders(senders.clone())
        .counting(conn, Rows::Threads, "SELECT t.account_id, t.id", true)?;
    let mut sql = Sql::default();
    sql.push("SELECT DISTINCT x.account_id, x.thread_id, lower(x.from_addr) FROM (")
        .push(&inner.text)
        .push(
            " AND t.unread = 1) v CROSS JOIN messages x INDEXED BY messages_by_sender \
               ON x.account_id = v.account_id AND x.thread_id = v.id \
               WHERE lower(x.from_addr) IN (",
        );
    sql.params.extend(inner.params);
    sql.bind_list(&senders).push(")");
    let mut stmt = conn.prepare_cached(&sql.text)?;
    let mut rows = stmt.query(params_from_iter(&sql.params))?;
    let mut counts = SenderCounts::default();
    while let Some(row) = rows.next()? {
        counts
            .threads
            .entry((row.get(0)?, row.get(1)?))
            .or_default()
            .insert(row.get(2)?);
    }
    Ok(counts)
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
    let base = filter.clone().with_labels(&[], &[]);
    let walk = base.count_walk(conn, rows, true)?;
    base.rows_matching(rows, &mut sql, &walk);
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

#[cfg(test)]
mod walk_tests {
    use mailrs_domain::{Address, MessageMeta};

    use super::*;
    use crate::{accounts, messages, open_in_memory};

    fn message(
        account_id: AccountId,
        id: &str,
        thread: &str,
        date: i64,
        labels: &[&str],
    ) -> MessageMeta {
        MessageMeta {
            account_id,
            id: id.into(),
            thread_id: thread.into(),
            rfc822_msgid: Some(format!("<{id}@example.com>")),
            from: Some(Address {
                name: None,
                email: format!("sender{}@example.com", date % 3),
            }),
            to: vec![],
            cc: vec![],
            subject: format!("Subject {id}"),
            date,
            snippet: String::new(),
            size: 100,
            has_attachments: false,
            label_ids: labels.iter().map(|l| l.to_string()).collect(),
            list_unsubscribe: None,
            one_click: false,
        }
    }

    /// Sixty threads over two accounts: most in the inbox, some trashed or
    /// spam, some in a category, a few under a sparse user label, some
    /// sharing a date so the tie-breakers matter, and threads whose first
    /// message is trashed while the reply stays in the inbox.
    fn mailbox() -> (Connection, AccountId, AccountId) {
        let conn = open_in_memory().unwrap();
        let a = accounts::insert_account(&conn, "a@example.com", 0).unwrap();
        let b = accounts::insert_account(&conn, "b@example.com", 0).unwrap();
        let mut all = Vec::new();
        for i in 0..60i64 {
            let account = if i % 2 == 0 { a } else { b };
            let thread = format!("t{i}");
            let date = 1000 + (i / 3) * 10;
            let mut labels = vec![];
            if i % 10 != 9 {
                labels.push("INBOX");
            }
            match i % 7 {
                1 => labels.push("CATEGORY_UPDATES"),
                2 => labels.push("CATEGORY_PROMOTIONS"),
                3 => labels.push("CATEGORY_SOCIAL"),
                _ => {}
            }
            if i % 11 == 0 {
                labels.push("TRASH");
            }
            if i % 13 == 0 {
                labels.push("SPAM");
            }
            if i % 8 == 0 {
                labels.push("Label_1");
            }
            if i % 5 == 0 {
                labels.push("UNREAD");
                labels.push("STARRED");
            }
            all.push(message(account, &format!("m{i}"), &thread, date, &labels));
            if i % 6 == 0 {
                all.push(message(
                    account,
                    &format!("r{i}"),
                    &thread,
                    date + 1,
                    &["INBOX", "TRASH"],
                ));
            }
        }
        for m in &all {
            messages::upsert_message(&conn, m, 1).unwrap();
        }
        for m in &all {
            messages::refresh_thread(&conn, m.account_id, &m.thread_id).unwrap();
        }
        (conn, a, b)
    }

    fn filters(a: AccountId) -> Vec<ThreadFilter> {
        let (_, not_primary) = Category::Primary.labels();
        let (social, _) = Category::Social.labels();
        let sender = || vec!["Sender1@example.com".to_string()];
        vec![
            ThreadFilter::unified("INBOX"),
            ThreadFilter::account(a, "INBOX"),
            ThreadFilter::unified("INBOX").with_labels(&[], not_primary),
            ThreadFilter::unified("INBOX").with_labels(&["CATEGORY_SOCIAL"], &[]),
            ThreadFilter::account(a, "INBOX").with_labels(social, &[]),
            ThreadFilter::unified("Label_1"),
            ThreadFilter::unified("TRASH"),
            ThreadFilter::unified("INBOX").with_flag(FlagColor::Red),
            ThreadFilter::unified("").with_flag(FlagColor::Red),
            ThreadFilter::unified("INBOX").from_senders(sender()),
            ThreadFilter::unified("").from_senders(sender()),
            ThreadFilter::account(a, "").from_senders(sender()),
            ThreadFilter::unified("INBOX").with_threads(vec![
                "t3".into(),
                "t10".into(),
                "t11".into(),
            ]),
            ThreadFilter::account(a, "INBOX").with_threads(vec!["t0".into(), "t6".into()]),
            ThreadFilter::unified(""),
        ]
    }

    fn keys(rows: Vec<ThreadSummary>) -> Vec<(AccountId, String, Option<String>)> {
        rows.into_iter()
            .map(|r| (r.account_id, r.id, r.message_id))
            .collect()
    }

    #[test]
    fn every_walk_lists_the_same_rows() {
        let (conn, a, _) = mailbox();
        for filter in filters(a) {
            for (offset, limit) in [(0, 7), (7, 7), (0, 100)] {
                let threads =
                    keys(threads_walking(&conn, &filter, offset, limit, Walk::Scan).unwrap());
                let messages =
                    keys(messages_walking(&conn, &filter, offset, limit, Walk::Scan).unwrap());
                for walk in filter.every_walk(false) {
                    let by_walk = threads_walking(&conn, &filter, offset, limit, walk.clone());
                    assert_eq!(
                        keys(by_walk.unwrap()),
                        threads,
                        "threads, {filter:?} {walk:?}"
                    );
                    let by_walk = messages_walking(&conn, &filter, offset, limit, walk.clone());
                    assert_eq!(
                        keys(by_walk.unwrap()),
                        messages,
                        "messages, {filter:?} {walk:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn every_walk_counts_the_same_unread_rows() {
        let (conn, a, _) = mailbox();
        for filter in filters(a) {
            for (rows, unread) in [
                (Rows::Threads, " AND t.unread = 1"),
                (Rows::Messages, MESSAGE_UNREAD),
            ] {
                let unread_by = |walk: &Walk| {
                    let mut sql = filter.query_walking(rows, "SELECT COUNT(*)", walk);
                    sql.push(unread);
                    count(&conn, &sql).unwrap()
                };
                let scanned = unread_by(&Walk::Scan);
                for walk in filter.every_walk(true) {
                    assert_eq!(unread_by(&walk), scanned, "{filter:?} {walk:?}");
                }
            }
        }
    }

    #[test]
    fn a_small_category_of_a_big_inbox_is_walked_from_the_category() {
        let (conn, _, _) = mailbox();
        let (social, _) = Category::Social.labels();
        let filter = ThreadFilter::unified("INBOX").with_labels(social, &[]);
        assert_eq!(
            filter.walk(&conn, Rows::Threads, 51).unwrap(),
            Walk::Labels(social.iter().map(|l| l.to_string()).collect())
        );
    }

    #[test]
    fn unread_mail_is_counted_from_the_unread_label_when_that_is_smaller() {
        let (conn, _, _) = mailbox();
        let filter = ThreadFilter::unified("INBOX");
        for rows in [Rows::Threads, Rows::Messages] {
            assert_eq!(
                filter.count_walk(&conn, rows, true).unwrap(),
                Walk::Labels(vec![UNREAD.to_string()])
            );
        }
    }

    #[test]
    fn named_threads_are_read_through_the_primary_key() {
        let (conn, a, _) = mailbox();
        for filter in [
            ThreadFilter::account(a, "INBOX").with_threads(vec!["t0".into()]),
            ThreadFilter::unified("INBOX").with_threads(vec!["t0".into()]),
        ] {
            assert_eq!(filter.walk(&conn, Rows::Threads, 1).unwrap(), Walk::Threads);
            let sql = filter.query_walking(Rows::Threads, "SELECT t.id", &Walk::Threads);
            let plan: Vec<String> = conn
                .prepare(&format!("EXPLAIN QUERY PLAN {}", sql.text))
                .unwrap()
                .query_map(params_from_iter(&sql.params), |r| r.get::<_, String>(3))
                .unwrap()
                .map(|r| r.unwrap())
                .collect();
            assert!(
                plan.iter()
                    .any(|p| p.contains("sqlite_autoindex_threads_1 (account_id=? AND id=?)")),
                "{plan:?}"
            );
        }
    }

    #[test]
    fn a_label_holding_most_mail_is_walked_by_date() {
        assert!(date_wins(14_800, 15_300, 101));
    }

    #[test]
    fn a_small_label_is_walked_through_its_own_rows() {
        assert!(!date_wins(500, 15_300, 101));
    }

    #[test]
    fn an_empty_label_is_walked_through_its_own_rows() {
        assert!(!date_wins(0, 15_300, 101));
    }

    #[test]
    fn the_date_walk_reads_the_order_index_and_sorts_nothing() {
        let (conn, _, _) = mailbox();
        let filter = ThreadFilter::unified("INBOX");
        for (rows, select) in [
            (Rows::Threads, format!("SELECT {COLUMNS}")),
            (Rows::Messages, format!("SELECT {MESSAGE_COLUMNS}")),
        ] {
            let mut sql = filter.query_walking(rows, &select, &Walk::Date);
            rows.order(&mut sql, 0, 10);
            let plan: Vec<String> = conn
                .prepare(&format!("EXPLAIN QUERY PLAN {}", sql.text))
                .unwrap()
                .query_map(params_from_iter(&sql.params), |r| r.get::<_, String>(3))
                .unwrap()
                .map(|r| r.unwrap())
                .collect();
            assert!(plan.iter().any(|p| p.contains("_by_order")), "{plan:?}");
            assert!(!plan.iter().any(|p| p.contains("TEMP B-TREE")), "{plan:?}");
        }
    }
}
