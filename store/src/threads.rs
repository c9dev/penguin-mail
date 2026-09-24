//! Thread queries. `messages::apply` keeps the rows they read.
//!
//! Filters name mail by mail set: a role, a server mailbox, a keyword,
//! unread mail or a category. Each query resolves the set to what it
//! names in the tables, then chooses how to reach the rows.

use std::collections::{HashMap, HashSet};

use mailrs_domain::mailbox::keyword::{FLAGGED, MUTED};
use mailrs_domain::{AccountId, Category, FlagColor, MailSet, Role, ThreadSummary};
use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, Row, params, params_from_iter};

use crate::Result;

/// What a mail set names in the tables.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Named {
    /// Server mailboxes by key: one for each account in question that has
    /// a mailbox with that id.
    Mailboxes(Vec<i64>),
    Keyword(String),
    /// Unseen mail: no `$seen`.
    Unread,
    Category(String),
}

impl Named {
    fn of(conn: &Connection, account: Option<AccountId>, set: &MailSet) -> Result<Named> {
        Ok(match set {
            MailSet::Role(role) => Named::Mailboxes(keys(
                conn,
                account,
                "SELECT key FROM mailboxes WHERE role = ?1",
                role.as_str(),
            )?),
            MailSet::Mailbox(id) => Named::Mailboxes(keys(
                conn,
                account,
                "SELECT key FROM mailboxes WHERE id = ?1",
                id,
            )?),
            MailSet::Keyword(k) => Named::Keyword(k.clone()),
            MailSet::Unseen => Named::Unread,
            MailSet::Category(c) => Named::Category(c.clone()),
        })
    }
}

/// The keys `sql` selects with `?1` bound to `value`, narrowed to one
/// account when `account` names one.
fn keys(conn: &Connection, account: Option<AccountId>, sql: &str, value: &str) -> Result<Vec<i64>> {
    let rows = match account {
        Some(account) => conn
            .prepare_cached(&format!("{sql} AND account_id = ?2"))?
            .query_map(params![value, account], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<i64>>>()?,
        None => conn
            .prepare_cached(sql)?
            .query_map([value], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<i64>>>()?,
    };
    Ok(rows)
}

/// The keys of the Trash and Spam of the accounts in question. Gmail
/// shows trashed and spam mail only in those two lists, so every other
/// list leaves it out. The keys are few and bound into each query as
/// constants, which lets SQLite plan against them.
fn hidden_keys(conn: &Connection, account: Option<AccountId>) -> Result<Vec<i64>> {
    let by_role = "SELECT key FROM mailboxes WHERE role = ?1";
    let mut hidden = keys(conn, account, by_role, Role::Trash.as_str())?;
    hidden.extend(keys(conn, account, by_role, Role::Junk.as_str())?);
    Ok(hidden)
}

/// A filter's mail set, resolved.
struct Resolved {
    label: Option<Named>,
    any: Vec<Named>,
    none: Vec<Named>,
    /// The Trash and Spam keys a list leaves out: all of them, less the
    /// set's own, since listing the Trash keeps trashed mail.
    hidden: Vec<i64>,
}

/// How a query reaches its rows. Every walk finds the same rows; they
/// differ in how many they read on the way.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Walk {
    /// Every row in table order.
    Scan,
    /// Through the rows newest first, stopping at a full page.
    Date,
    /// Through the listed thread rows of these mailboxes, or the messages
    /// filed in them.
    Mailboxes(Vec<i64>),
    /// Through the messages carrying this keyword, or for flagged
    /// threads, the index of starred threads.
    Keyword(String),
    /// Through the partial index of unread threads or unseen messages.
    Unread,
    /// Through the rows in any of these categories.
    Categories(Vec<String>),
    /// Through `messages_by_sender`, reading the mail from the filter's
    /// senders.
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

/// Which threads a list shows: one mail set, across all accounts or one,
/// optionally narrowed to a flag colour or to some senders.
///
/// Whichever set it names, the list leaves out mail in the Trash and
/// Spam, as Gmail's own Sent and label views do.
///
/// A filter starts from [`ThreadFilter::unified`] or
/// [`ThreadFilter::account`] and narrows through the `with_` methods. The
/// fields stay private so the store can choose how to reach the rows they
/// describe without callers building filters it has not planned for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ThreadFilter {
    account_id: Option<AccountId>,
    /// `None` means any mail.
    set: Option<MailSet>,
    /// Only mail starred with this colour. Starred mail without a colour
    /// counts as red.
    flag: Option<FlagColor>,
    /// Only threads with a message from one of these addresses.
    senders: Vec<String>,
    /// Only threads with at least one of these categories, such as
    /// Gmail's `CATEGORY_SOCIAL` and `CATEGORY_FORUMS`. Empty means no
    /// condition.
    any_categories: Vec<String>,
    /// Only threads with none of these categories.
    no_categories: Vec<String>,
    /// Only these threads, by id. Empty means every thread.
    thread_ids: Vec<String>,
}

/// SQL text with anonymous `?` placeholders, and their values in order.
#[derive(Default)]
pub(crate) struct Sql {
    pub(crate) text: String,
    pub(crate) params: Vec<Value>,
}

impl Sql {
    pub(crate) fn push(&mut self, text: &str) -> &mut Self {
        self.text.push_str(text);
        self
    }

    pub(crate) fn bind(&mut self, value: impl Into<Value>) -> &mut Self {
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

    /// `?, ?, …` for each key. An empty list leaves `IN ()`, which SQLite
    /// reads as false.
    fn bind_keys(&mut self, keys: &[i64]) -> &mut Self {
        for (i, key) in keys.iter().enumerate() {
            if i > 0 {
                self.text.push_str(", ");
            }
            self.bind(*key);
        }
        self
    }
}

/// Appends the condition that message `x` holds `named`.
fn message_holds(sql: &mut Sql, x: &str, named: &Named) {
    match named {
        Named::Mailboxes(keys) => {
            sql.push(&format!(
                "EXISTS (SELECT 1 FROM message_mailboxes l WHERE l.account_id = {x}.account_id \
                 AND l.message_id = {x}.id AND l.mailbox IN ("
            ))
            .bind_keys(keys)
            .push("))");
        }
        Named::Keyword(k) => {
            sql.push(&format!(
                "EXISTS (SELECT 1 FROM message_keywords l WHERE l.account_id = {x}.account_id \
                 AND l.message_id = {x}.id AND l.keyword = "
            ))
            .bind(k.clone())
            .push(")");
        }
        Named::Unread => {
            sql.push(&format!("{x}.seen = 0"));
        }
        Named::Category(c) => {
            sql.push(&format!(
                "EXISTS (SELECT 1 FROM message_categories l WHERE l.account_id = {x}.account_id \
                 AND l.message_id = {x}.id AND l.category = "
            ))
            .bind(c.clone())
            .push(")");
        }
    }
}

/// Appends the condition that message `x` sits in none of `hidden`.
fn message_shown(sql: &mut Sql, x: &str, hidden: &[i64]) {
    sql.push(&format!(
        "NOT EXISTS (SELECT 1 FROM message_mailboxes h WHERE h.account_id = {x}.account_id \
         AND h.message_id = {x}.id AND h.mailbox IN ("
    ))
    .bind_keys(hidden)
    .push("))");
}

/// Which rows of a list a page holds.
#[derive(Clone, Copy)]
enum Page<'a> {
    /// `limit` rows after the first `offset`.
    Offset(i64, i64),
    /// `limit` rows after this row of the previous page, or from the top.
    After(Option<&'a ThreadSummary>, i64),
}

impl Page<'_> {
    /// How far into the list the page ends, counted from where the
    /// query starts reading.
    fn end(self) -> i64 {
        match self {
            Page::Offset(offset, limit) => offset + limit,
            Page::After(_, limit) => limit,
        }
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

    /// Appends the condition that the row holds `named`: a message that
    /// does, or a thread with a message that does, in the Trash or out.
    fn holds(self, sql: &mut Sql, named: &Named) {
        match (self, named) {
            (Rows::Messages, _) => message_holds(sql, "m", named),
            (Rows::Threads, Named::Mailboxes(keys)) => {
                sql.push(
                    "EXISTS (SELECT 1 FROM thread_mailboxes l WHERE l.account_id = t.account_id \
                     AND l.thread_id = t.id AND l.mailbox IN (",
                )
                .bind_keys(keys)
                .push("))");
            }
            (Rows::Threads, Named::Category(c)) => {
                sql.push(
                    "EXISTS (SELECT 1 FROM thread_categories l WHERE l.account_id = t.account_id \
                     AND l.thread_id = t.id AND l.category = ",
                )
                .bind(c.clone())
                .push(")");
            }
            (Rows::Threads, Named::Unread) => {
                sql.push("t.unread = 1");
            }
            (Rows::Threads, Named::Keyword(k)) if k == FLAGGED => {
                sql.push("t.starred = 1");
            }
            (Rows::Threads, Named::Keyword(k)) if k == MUTED => {
                sql.push("t.muted = 1");
            }
            (Rows::Threads, Named::Keyword(_)) => {
                sql.push(
                    "EXISTS (SELECT 1 FROM messages x WHERE x.account_id = t.account_id \
                     AND x.thread_id = t.id AND ",
                );
                message_holds(sql, "x", named);
                sql.push(")");
            }
        }
    }

    /// Appends `(… OR …)`: the row holds one of `named`.
    fn holds_any(self, sql: &mut Sql, named: &[Named]) {
        sql.push("(");
        for (i, one) in named.iter().enumerate() {
            if i > 0 {
                sql.push(" OR ");
            }
            self.holds(sql, one);
        }
        sql.push(")");
    }

    /// Appends ` AND …` conditions that a list of the filter's mail set
    /// shows the row: it holds the set, and the Trash and Spam do not hide
    /// it. A message is hidden when it sits in one of `resolved.hidden`. A
    /// thread is hidden only when every message of it holding the set
    /// (any message, with no set) is: Gmail keeps a conversation in the
    /// inbox while one of its messages outside the Trash is there, so
    /// trashing the start of a thread leaves the reply. For a mailbox or a
    /// category the derived `listed` column already says this; for a
    /// keyword it is worked out from the thread's messages. `holds` false
    /// leaves out a message's first half, for a walk that started from
    /// the set's own rows.
    fn shown(self, sql: &mut Sql, resolved: &Resolved, holds: bool) {
        match (self, &resolved.label) {
            (Rows::Threads, None) => {
                sql.push(" AND t.listed = 1");
            }
            (Rows::Threads, Some(Named::Mailboxes(keys))) => {
                sql.push(
                    " AND EXISTS (SELECT 1 FROM thread_mailboxes l WHERE l.account_id = t.account_id \
                     AND l.thread_id = t.id AND l.listed = 1 AND l.mailbox IN (",
                )
                .bind_keys(keys)
                .push("))");
            }
            (Rows::Threads, Some(Named::Category(c))) => {
                sql.push(
                    " AND EXISTS (SELECT 1 FROM thread_categories l WHERE l.account_id = t.account_id \
                     AND l.thread_id = t.id AND l.listed = 1 AND l.category = ",
                )
                .bind(c.clone())
                .push(")");
            }
            (Rows::Threads, Some(named)) => {
                sql.push(
                    " AND EXISTS (SELECT 1 FROM messages x WHERE x.account_id = t.account_id \
                     AND x.thread_id = t.id AND ",
                );
                message_holds(sql, "x", named);
                if !resolved.hidden.is_empty() {
                    sql.push(" AND ");
                    message_shown(sql, "x", &resolved.hidden);
                }
                sql.push(")");
            }
            (Rows::Messages, label) => {
                if let (Some(named), true) = (label, holds) {
                    sql.push(" AND ");
                    message_holds(sql, "m", named);
                }
                if !resolved.hidden.is_empty() {
                    sql.push(" AND ");
                    message_shown(sql, "m", &resolved.hidden);
                }
            }
        }
    }

    /// The per-row table of a category, and its column naming the row.
    fn categories(self) -> (&'static str, &'static str) {
        match self {
            Rows::Threads => ("thread_categories", "thread_id"),
            Rows::Messages => ("message_categories", "message_id"),
        }
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
    fn order(self, sql: &mut Sql, page: Page) {
        let (date, row) = match self {
            Rows::Threads => ("t.last_message_at", "t"),
            Rows::Messages => ("m.date", "m"),
        };
        let (offset, limit) = match page {
            Page::Offset(offset, limit) => (offset, limit),
            Page::After(None, limit) => (0, limit),
            Page::After(Some(last), limit) => {
                let id = match self {
                    Rows::Threads => &last.id,
                    Rows::Messages => last.message_id.as_ref().unwrap_or(&last.id),
                };
                // The first bound on its own lets a walk by date seek
                // straight to the page instead of reading its way there.
                sql.push(&format!(" AND {date} <= "))
                    .bind(last.last_message_at)
                    .push(&format!(" AND ({date} < "))
                    .bind(last.last_message_at)
                    .push(&format!(" OR {row}.account_id > "))
                    .bind(last.account_id)
                    .push(&format!(" OR ({row}.account_id = "))
                    .bind(last.account_id)
                    .push(&format!(" AND {row}.id > "))
                    .bind(id.clone())
                    .push("))");
                (0, limit)
            }
        };
        sql.push(&format!(
            " ORDER BY {date} DESC, {row}.account_id, {row}.id LIMIT "
        ))
        .bind(limit)
        .push(" OFFSET ")
        .bind(offset);
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
    /// The mail in `set` across every account.
    pub fn unified(set: MailSet) -> Self {
        ThreadFilter {
            set: Some(set),
            ..ThreadFilter::default()
        }
    }

    pub fn account(account_id: AccountId, set: MailSet) -> Self {
        ThreadFilter {
            account_id: Some(account_id),
            set: Some(set),
            ..ThreadFilter::default()
        }
    }

    /// Every stored thread outside the Trash and Spam, in every account.
    pub fn everything() -> Self {
        ThreadFilter::default()
    }

    /// Narrows the list to one account's mail.
    pub fn in_account(mut self, account_id: AccountId) -> Self {
        self.account_id = Some(account_id);
        self
    }

    pub fn with_flag(mut self, flag: FlagColor) -> Self {
        self.flag = Some(flag);
        self
    }

    pub fn from_senders(mut self, senders: Vec<String>) -> Self {
        self.senders = senders;
        self
    }

    /// Narrows the list to threads in one of `any` categories and none of
    /// `none`.
    pub fn with_categories(mut self, any: &[&str], none: &[&str]) -> Self {
        self.any_categories = any.iter().map(|c| c.to_string()).collect();
        self.no_categories = none.iter().map(|c| c.to_string()).collect();
        self
    }

    /// Narrows the list to these threads, so a caller can re-read the rows
    /// a change event named instead of listing the mailbox again.
    pub fn with_threads(mut self, thread_ids: Vec<String>) -> Self {
        self.thread_ids = thread_ids;
        self
    }

    /// The filter's mail set resolved to what it names, in its account or
    /// in all.
    fn resolve(&self, conn: &Connection) -> Result<Resolved> {
        let label = self
            .set
            .as_ref()
            .map(|s| Named::of(conn, self.account_id, s))
            .transpose()?;
        let mut hidden = hidden_keys(conn, self.account_id)?;
        if let Some(Named::Mailboxes(own)) = &label {
            hidden.retain(|key| !own.contains(key));
        }
        Ok(Resolved {
            label,
            any: self
                .any_categories
                .iter()
                .map(|c| Named::Category(c.clone()))
                .collect(),
            none: self
                .no_categories
                .iter()
                .map(|c| Named::Category(c.clone()))
                .collect(),
            hidden,
        })
    }

    /// The walk that starts from the mail set's own rows. A mailbox no
    /// account in question has is no start: it names no rows,
    /// and SQLite finds no plan that reads the listed index over an empty
    /// set of keys. The other walks find nothing for it instead.
    fn label_start(&self, resolved: &Resolved) -> Option<Walk> {
        Some(match resolved.label.as_ref()? {
            Named::Mailboxes(keys) if keys.is_empty() => return None,
            Named::Mailboxes(keys) => Walk::Mailboxes(keys.clone()),
            Named::Keyword(k) => Walk::Keyword(k.clone()),
            Named::Unread => Walk::Unread,
            Named::Category(c) => Walk::Categories(vec![c.clone()]),
        })
    }

    /// The walk that starts from the rows holding one of `any_categories`,
    /// when every one of them is a category.
    fn any_start(&self, resolved: &Resolved) -> Option<Walk> {
        let categories: Option<Vec<String>> = resolved
            .any
            .iter()
            .map(|named| match named {
                Named::Category(c) => Some(c.clone()),
                _ => None,
            })
            .collect();
        categories.filter(|c| !c.is_empty()).map(Walk::Categories)
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
    fn rows_matching(&self, resolved: &Resolved, rows: Rows, sql: &mut Sql, walk: &Walk) {
        let row = rows.alias();
        let table = rows.table();
        match (walk, rows) {
            (Walk::Scan, _) => {
                sql.push(&format!("FROM {table} {row} WHERE 1"));
            }
            (Walk::Date, _) => {
                sql.push(&format!(
                    "FROM {table} {row} INDEXED BY {} WHERE 1",
                    rows.order_index()
                ));
            }
            (Walk::Mailboxes(keys), Rows::Threads) => {
                sql.push(
                    "FROM thread_mailboxes d INDEXED BY thread_mailboxes_listed CROSS JOIN threads t \
                     ON t.account_id = d.account_id AND t.id = d.thread_id \
                     WHERE d.listed = 1 AND d.mailbox IN (",
                )
                .bind_keys(keys)
                .push(")");
            }
            (Walk::Mailboxes(keys), Rows::Messages) => {
                sql.push(
                    "FROM message_mailboxes d INDEXED BY message_mailboxes_by_mailbox \
                     CROSS JOIN messages m ON m.account_id = d.account_id AND m.id = d.message_id \
                     WHERE d.mailbox IN (",
                )
                .bind_keys(keys)
                .push(")");
            }
            (Walk::Keyword(k), Rows::Threads) if k == FLAGGED => {
                sql.push("FROM threads t INDEXED BY threads_starred WHERE t.starred = 1");
            }
            (Walk::Keyword(k), Rows::Threads) => {
                sql.push(
                    "FROM (SELECT DISTINCT x.account_id, x.thread_id AS id FROM message_keywords d \
                     CROSS JOIN messages x ON x.account_id = d.account_id AND x.id = d.message_id \
                     WHERE d.keyword = ",
                )
                .bind(k.clone());
                if let Some(account) = self.account_id {
                    sql.push(" AND d.account_id = ").bind(account);
                }
                sql.push(") d CROSS JOIN threads t ON t.account_id = d.account_id AND t.id = d.id WHERE 1");
            }
            (Walk::Keyword(k), Rows::Messages) => {
                sql.push(
                    "FROM message_keywords d CROSS JOIN messages m \
                     ON m.account_id = d.account_id AND m.id = d.message_id WHERE d.keyword = ",
                )
                .bind(k.clone());
                if let Some(account) = self.account_id {
                    sql.push(" AND d.account_id = ").bind(account);
                }
            }
            (Walk::Unread, Rows::Threads) => {
                sql.push("FROM threads t INDEXED BY threads_unread WHERE t.unread = 1");
            }
            (Walk::Unread, Rows::Messages) => {
                sql.push("FROM messages m INDEXED BY messages_unseen WHERE m.seen = 0");
            }
            (Walk::Categories(categories), _) if categories.len() == 1 => {
                let (per_row, key) = rows.categories();
                sql.push(&format!(
                    "FROM {per_row} d CROSS JOIN {table} {row} \
                     ON {row}.account_id = d.account_id AND {row}.id = d.{key} WHERE d.category = "
                ))
                .bind(categories[0].clone());
                if let Some(account) = self.account_id {
                    sql.push(" AND d.account_id = ").bind(account);
                }
            }
            (Walk::Categories(categories), _) => {
                // A row can be in two of the categories, so the set is made
                // distinct before it names rows.
                let (per_row, key) = rows.categories();
                sql.push(&format!(
                    "FROM (SELECT DISTINCT account_id, {key} AS id FROM {per_row} WHERE category IN ("
                ))
                .bind_list(categories)
                .push(")");
                if let Some(account) = self.account_id {
                    sql.push(" AND account_id = ").bind(account);
                }
                sql.push(&format!(
                    ") d CROSS JOIN {table} {row} ON {row}.account_id = d.account_id AND {row}.id = d.id WHERE 1"
                ));
            }
            (Walk::Senders, Rows::Threads) => {
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
            (Walk::Senders, Rows::Messages) => {
                sql.push(
                    "FROM messages m INDEXED BY messages_by_sender \
                     WHERE lower(m.from_addr) IN (",
                )
                .bind_list(&self.lowercase_senders())
                .push(")");
            }
            (Walk::Threads, _) => {
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
        let from_label = self.label_start(resolved).as_ref() == Some(walk);
        // A walk over a mailbox's listed thread rows has met the whole rule.
        if !(from_label && matches!((walk, rows), (Walk::Mailboxes(_), Rows::Threads))) {
            rows.shown(sql, resolved, !from_label);
        }
        if let Some(account) = self.account_id {
            sql.push(&format!(" AND {row}.account_id = ")).bind(account);
        }
        if !self.thread_ids.is_empty() && *walk != Walk::Threads {
            sql.push(&format!(" AND {} IN (", rows.thread_key()))
                .bind_list(&self.thread_ids)
                .push(")");
        }
        if !resolved.any.is_empty() && self.any_start(resolved).as_ref() != Some(walk) {
            sql.push(" AND ");
            rows.holds_any(sql, &resolved.any);
        }
        if !resolved.none.is_empty() {
            sql.push(" AND NOT ");
            rows.holds_any(sql, &resolved.none);
        }
        if let Some(flag) = self.flag {
            if let Rows::Threads = rows {
                // Only a thread with a starred message can match, and
                // `threads.starred` says which do without a lookup.
                sql.push(" AND t.starred = 1");
            }
            sql.push(&format!(
                " AND EXISTS (SELECT 1 FROM messages x JOIN message_keywords s \
                 ON s.account_id = x.account_id AND s.message_id = x.id AND s.keyword = '{FLAGGED}' \
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

    fn query_walking(&self, resolved: &Resolved, rows: Rows, select: &str, walk: &Walk) -> Sql {
        let mut sql = Sql::default();
        sql.push(select).push(" ");
        self.rows_matching(resolved, rows, &mut sql, walk);
        sql
    }

    /// A query over `rows` that counts or sums rather than pages, through
    /// the smallest set it can start from. `unread` says the caller keeps
    /// only unread rows, which makes the unread mail one more such set.
    fn counting(&self, conn: &Connection, rows: Rows, select: &str, unread: bool) -> Result<Sql> {
        let resolved = self.resolve(conn)?;
        let walk = self.count_walk(conn, &resolved, rows, unread)?;
        Ok(self.query_walking(&resolved, rows, select, &walk))
    }

    /// The walk for a query that reads every row it keeps: from the
    /// smallest set it can start from, or through the whole table.
    fn count_walk(
        &self,
        conn: &Connection,
        resolved: &Resolved,
        rows: Rows,
        unread: bool,
    ) -> Result<Walk> {
        if let Some(walk) = self.fixed_walk() {
            return Ok(walk);
        }
        Ok(self
            .rarest(conn, resolved, rows, unread)?
            .map_or(Walk::Scan, |(walk, _)| walk))
    }

    /// The walk that needs no counting: the filter names its threads.
    fn fixed_walk(&self) -> Option<Walk> {
        (!self.thread_ids.is_empty()).then_some(Walk::Threads)
    }

    /// The sets of rows an index can hand over, each of which holds every
    /// row the filter keeps, and so each a place a walk can start. The
    /// filter's own mail set comes last because it is the one most likely
    /// to hold most of the mail, as the inbox does.
    fn starts(&self, resolved: &Resolved, unread: bool) -> Vec<Walk> {
        let mut starts = Vec::new();
        if unread {
            starts.push(Walk::Unread);
        }
        // A flag needs a starred message, and so does its thread.
        if self.flag.is_some() {
            starts.push(Walk::Keyword(FLAGGED.into()));
        }
        if !self.senders.is_empty() {
            starts.push(Walk::Senders);
        }
        starts.extend(self.any_start(resolved));
        starts.extend(self.label_start(resolved));
        starts
    }

    /// Appends the table and condition that hold a start's rows, for
    /// counting them.
    fn start_rows(&self, rows: Rows, walk: &Walk, sql: &mut Sql) {
        match (walk, rows) {
            (Walk::Mailboxes(keys), Rows::Threads) => {
                // Mailbox keys belong to one account each, so the keys
                // already narrow the count to the filter's account.
                sql.push(
                    "thread_mailboxes INDEXED BY thread_mailboxes_listed \
                     WHERE listed = 1 AND mailbox IN (",
                )
                .bind_keys(keys)
                .push(")");
                return;
            }
            (Walk::Mailboxes(keys), Rows::Messages) => {
                sql.push("message_mailboxes WHERE mailbox IN (")
                    .bind_keys(keys)
                    .push(")");
                return;
            }
            (Walk::Unread, Rows::Threads) => {
                sql.push("threads WHERE unread = 1");
            }
            (Walk::Unread, Rows::Messages) => {
                sql.push("messages WHERE seen = 0");
            }
            (Walk::Keyword(k), Rows::Threads) if k == FLAGGED => {
                sql.push("threads WHERE starred = 1");
            }
            (Walk::Keyword(k), _) => {
                sql.push("message_keywords WHERE keyword = ")
                    .bind(k.clone());
            }
            (Walk::Categories(categories), _) => {
                sql.push(&format!("{} WHERE category IN (", rows.categories().0))
                    .bind_list(categories)
                    .push(")");
            }
            (Walk::Senders, _) => {
                sql.push("messages INDEXED BY messages_by_sender WHERE lower(from_addr) IN (")
                    .bind_list(&self.lowercase_senders())
                    .push(")");
            }
            (Walk::Scan | Walk::Date | Walk::Threads, _) => {
                sql.push(&format!("{} WHERE 1", rows.table()));
            }
        }
        if let Some(account) = self.account_id {
            sql.push(" AND account_id = ").bind(account);
        }
    }

    /// The smallest of the sets a walk can start from, and how many rows
    /// it holds. Each count reads one index range and stops once it passes
    /// the smallest set so far, so an inbox of thousands costs no more to
    /// rule out than the few hundred rows that beat it.
    fn rarest(
        &self,
        conn: &Connection,
        resolved: &Resolved,
        rows: Rows,
        unread: bool,
    ) -> Result<Option<(Walk, i64)>> {
        let mut rarest: Option<(Walk, i64)> = None;
        for walk in self.starts(resolved, unread) {
            let least = rarest.as_ref().map(|(_, least)| *least);
            let mut sql = Sql::default();
            sql.push(match least {
                Some(_) => "SELECT count(*) FROM (SELECT 1 FROM ",
                None => "SELECT count(*) FROM ",
            });
            self.start_rows(rows, &walk, &mut sql);
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
    fn walk(
        &self,
        conn: &Connection,
        resolved: &Resolved,
        rows: Rows,
        wanted: i64,
    ) -> Result<Walk> {
        if let Some(walk) = self.fixed_walk() {
            return Ok(walk);
        }
        let Some((start, size)) = self.rarest(conn, resolved, rows, false)? else {
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
    fn every_walk(&self, resolved: &Resolved, unread: bool) -> Vec<Walk> {
        let mut walks = vec![Walk::Scan, Walk::Date];
        walks.extend(self.starts(resolved, unread));
        walks.extend(self.fixed_walk());
        walks
    }
}

/// Appends the condition that a message row `m` is unread.
const MESSAGE_UNREAD: &str = " AND m.seen = 0";

fn count(conn: &Connection, sql: &Sql) -> Result<i64> {
    Ok(conn
        .prepare_cached(&sql.text)?
        .query_row(params_from_iter(&sql.params), |row| row.get(0))?)
}

const COLUMNS: &str = "t.account_id, t.id, t.last_message_at, t.subject, t.snippet, t.from_display, \
                       t.message_count, t.unread, t.starred, t.has_attachments, t.flag_color, \
                       t.from_email, t.muted";

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

/// The account holding the thread `thread_id`, for a caller that knows
/// only Gmail's id. Two accounts can in principle share one; the first
/// added wins.
pub fn account_of(conn: &Connection, thread_id: &str) -> Result<Option<AccountId>> {
    Ok(conn
        .prepare_cached("SELECT account_id FROM threads WHERE id = ?1 ORDER BY account_id LIMIT 1")?
        .query_row([thread_id], |row| row.get(0))
        .optional()?)
}

/// Newest first. Ties break on account and thread id so pages never overlap.
/// A page deep in the list reads every row before it; `list_threads_after`
/// does not.
pub fn list_threads(
    conn: &Connection,
    filter: &ThreadFilter,
    offset: i64,
    limit: i64,
) -> Result<Vec<ThreadSummary>> {
    let resolved = filter.resolve(conn)?;
    let page = Page::Offset(offset, limit);
    let walk = filter.walk(conn, &resolved, Rows::Threads, page.end())?;
    threads_walking(conn, filter, &resolved, page, walk)
}

fn threads_walking(
    conn: &Connection,
    filter: &ThreadFilter,
    resolved: &Resolved,
    page: Page,
    walk: Walk,
) -> Result<Vec<ThreadSummary>> {
    let mut sql =
        filter.query_walking(resolved, Rows::Threads, &format!("SELECT {COLUMNS}"), &walk);
    Rows::Threads.order(&mut sql, page);
    let mut stmt = conn.prepare_cached(&sql.text)?;
    let rows = stmt.query_map(params_from_iter(&sql.params), to_summary)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// The `limit` threads that follow `last`, the final row of the page
/// before, in the order `list_threads` gives; the first page when `last`
/// is `None`. The page starts where `last` sits in the order, so it costs
/// the same however deep it is.
pub fn list_threads_after(
    conn: &Connection,
    filter: &ThreadFilter,
    last: Option<&ThreadSummary>,
    limit: i64,
) -> Result<Vec<ThreadSummary>> {
    let resolved = filter.resolve(conn)?;
    let page = Page::After(last, limit);
    let walk = filter.walk(conn, &resolved, Rows::Threads, page.end())?;
    threads_walking(conn, filter, &resolved, page, walk)
}

pub fn count_threads(conn: &Connection, filter: &ThreadFilter) -> Result<i64> {
    count(
        conn,
        &filter.counting(conn, Rows::Threads, "SELECT COUNT(*)", false)?,
    )
}

/// Columns for one message shown as a list row.
const MESSAGE_COLUMNS: &str = "m.account_id, m.thread_id, m.id, m.date, m.subject, m.snippet, \
     COALESCE(m.from_name, m.from_addr, ''), m.has_attachments, m.seen = 0, \
     EXISTS (SELECT 1 FROM message_keywords s WHERE s.account_id = m.account_id AND s.message_id = m.id \
             AND s.keyword = '$flagged'), \
     (SELECT f.color FROM flags f WHERE f.account_id = m.account_id AND f.message_id = m.id), \
     COALESCE(m.from_addr, ''), \
     EXISTS (SELECT 1 FROM message_keywords z WHERE z.account_id = m.account_id AND z.message_id = m.id \
             AND z.keyword = '$muted')";

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
    let resolved = filter.resolve(conn)?;
    let page = Page::Offset(offset, limit);
    let walk = filter.walk(conn, &resolved, Rows::Messages, page.end())?;
    messages_walking(conn, filter, &resolved, page, walk)
}

/// `list_threads_after` for single messages: the `limit` messages that
/// follow the message row `last`.
pub fn list_messages_after(
    conn: &Connection,
    filter: &ThreadFilter,
    last: Option<&ThreadSummary>,
    limit: i64,
) -> Result<Vec<ThreadSummary>> {
    let resolved = filter.resolve(conn)?;
    let page = Page::After(last, limit);
    let walk = filter.walk(conn, &resolved, Rows::Messages, page.end())?;
    messages_walking(conn, filter, &resolved, page, walk)
}

fn messages_walking(
    conn: &Connection,
    filter: &ThreadFilter,
    resolved: &Resolved,
    page: Page,
    walk: Walk,
) -> Result<Vec<ThreadSummary>> {
    let mut sql = filter.query_walking(
        resolved,
        Rows::Messages,
        &format!("SELECT {MESSAGE_COLUMNS}"),
        &walk,
    );
    Rows::Messages.order(&mut sql, page);
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

/// How many threads are in a mail set, and how many of those are unread.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Count {
    pub threads: i64,
    pub unread: i64,
}

/// Thread counts for every mail set the sidebar shows, from one grouped
/// query per kind of set.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MailCounts {
    counts: HashMap<(AccountId, MailSet), Count>,
}

impl MailCounts {
    /// `count_threads` and `unread_threads` for `ThreadFilter::account(account_id, set)`.
    pub fn account(&self, account_id: AccountId, set: &MailSet) -> Count {
        self.counts
            .get(&(account_id, set.clone()))
            .copied()
            .unwrap_or_default()
    }

    /// `count_threads` and `unread_threads` for `ThreadFilter::unified(set)`.
    pub fn unified(&self, set: &MailSet) -> Count {
        self.counts
            .iter()
            .filter(|((_, s), _)| s == set)
            .fold(Count::default(), |sum, (_, c)| Count {
                threads: sum.threads + c.threads,
                unread: sum.unread + c.unread,
            })
    }
}

/// Every mail set's thread and unread counts, for the sidebar. A
/// mailbox's and a category's come from their listed thread rows,
/// grouped, which already leave out the Trash and Spam the way a list
/// does. A mailbox with a role counts under both its id and its role, so
/// the unified inbox sums every inbox whatever each server calls it. The
/// three keyword sets count through the query their list runs, grouped
/// by account.
pub fn mail_counts(conn: &Connection) -> Result<MailCounts> {
    let mut counts: HashMap<(AccountId, MailSet), Count> = HashMap::new();
    let mut note = |account: AccountId, set: MailSet, threads: i64, unread: i64| {
        counts.insert((account, set), Count { threads, unread });
    };
    let mut mailboxes = conn.prepare_cached(
        "SELECT g.account_id, b.id, b.role, g.n, g.u FROM (SELECT mailbox, account_id, COUNT(*) AS n, \
         SUM(unread) AS u FROM thread_mailboxes INDEXED BY thread_mailboxes_listed \
         WHERE listed = 1 GROUP BY mailbox, account_id) g CROSS JOIN mailboxes b ON b.key = g.mailbox",
    )?;
    let mut rows = mailboxes.query([])?;
    while let Some(row) = rows.next()? {
        let (account, id, role): (AccountId, String, Option<String>) =
            (row.get(0)?, row.get(1)?, row.get(2)?);
        let (threads, unread): (i64, i64) = (row.get(3)?, row.get(4)?);
        if let Some(role) = role.as_deref().and_then(|r| r.parse::<Role>().ok()) {
            note(account, MailSet::Role(role), threads, unread);
        }
        note(account, MailSet::Mailbox(id), threads, unread);
    }
    let mut categories = conn.prepare_cached(
        "SELECT account_id, category, COUNT(*), SUM(unread) FROM thread_categories \
         WHERE listed = 1 GROUP BY category, account_id",
    )?;
    let mut rows = categories.query([])?;
    while let Some(row) = rows.next()? {
        let (account, category): (AccountId, String) = (row.get(0)?, row.get(1)?);
        note(
            account,
            MailSet::Category(category),
            row.get(2)?,
            row.get(3)?,
        );
    }
    for set in [MailSet::Unseen, MailSet::flagged(), MailSet::muted()] {
        let mut sql = ThreadFilter::unified(set.clone()).counting(
            conn,
            Rows::Threads,
            "SELECT t.account_id, COUNT(*), SUM(t.unread)",
            false,
        )?;
        sql.push(" GROUP BY t.account_id");
        let mut stmt = conn.prepare_cached(&sql.text)?;
        let mut rows = stmt.query(params_from_iter(&sql.params))?;
        while let Some(row) = rows.next()? {
            note(row.get(0)?, set.clone(), row.get(1)?, row.get(2)?);
        }
    }
    counts.retain(|_, count| count.threads > 0);
    Ok(MailCounts { counts })
}

/// Which unread threads have mail from which senders, for the VIP counts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SenderCounts {
    /// Each unread thread, by account and id, with the lowercased senders
    /// of its messages among the ones asked about.
    threads: HashMap<(AccountId, String), HashSet<String>>,
}

impl SenderCounts {
    /// `unread_threads` for `ThreadFilter::everything().from_senders(senders)`.
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
    let inner = ThreadFilter::everything()
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
/// Each category's mail replaces the filter's own, as `with_categories` does.
pub fn category_unread_threads(
    conn: &Connection,
    filter: &ThreadFilter,
) -> Result<HashMap<Category, i64>> {
    category_unread(conn, filter, Rows::Threads)
}

/// `unread_messages` for `filter` narrowed to each category, in one query.
/// Each category's mail replaces the filter's own, as `with_categories` does.
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
    let base = filter.clone().with_categories(&[], &[]);
    let named = |categories: &[&str]| -> Vec<Named> {
        categories
            .iter()
            .map(|c| Named::Category(c.to_string()))
            .collect()
    };
    let mut sql = Sql::default();
    sql.push("SELECT ");
    for (i, category) in Category::ALL.into_iter().enumerate() {
        if i > 0 {
            sql.push(", ");
        }
        let (any, none) = category.categories();
        sql.push("COALESCE(SUM(1");
        if !any.is_empty() {
            sql.push(" AND ");
            rows.holds_any(&mut sql, &named(any));
        }
        if !none.is_empty() {
            sql.push(" AND NOT ");
            rows.holds_any(&mut sql, &named(none));
        }
        sql.push("), 0)");
    }
    sql.push(" ");
    let resolved = base.resolve(conn)?;
    let walk = base.count_walk(conn, &resolved, rows, true)?;
    base.rows_matching(&resolved, rows, &mut sql, &walk);
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
    use mailrs_gmail::labels::set_label_ids;

    use super::*;
    use crate::testing::list_gmail_roles;
    use crate::{accounts, messages, open_in_memory};

    fn message(
        account_id: AccountId,
        id: &str,
        thread: &str,
        date: i64,
        labels: &[&str],
    ) -> MessageMeta {
        let mut meta = MessageMeta {
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
            held: Default::default(),
            roles: vec![],
            list_unsubscribe: None,
            one_click: false,
        };
        let labels: Vec<String> = labels.iter().map(|l| l.to_string()).collect();
        set_label_ids(&mut meta, &labels);
        meta
    }

    /// Sixty threads over two accounts: most in the inbox, some trashed or
    /// spam, some in a category, a few under a sparse user label, some
    /// sharing a date so the tie-breakers matter, and threads whose first
    /// message is trashed while the reply stays in the inbox.
    fn mailbox() -> (Connection, AccountId, AccountId) {
        let conn = open_in_memory().unwrap();
        let a = accounts::insert_account(&conn, "a@example.com", 0).unwrap();
        let b = accounts::insert_account(&conn, "b@example.com", 0).unwrap();
        list_gmail_roles(&conn, a);
        list_gmail_roles(&conn, b);
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
        for account in [a, b] {
            let changes: Vec<messages::Change> = all
                .iter()
                .filter(|m| m.account_id == account)
                .map(|m| messages::Change::Upsert {
                    meta: Box::new(m.clone()),
                    generation: 1,
                })
                .collect();
            messages::apply(&conn, account, &changes).unwrap();
        }
        (conn, a, b)
    }

    /// The mail set the fixture's Gmail label names. The fixture keeps
    /// Gmail labels; only the filters built from them take mail sets.
    fn set(label: &str) -> MailSet {
        mailrs_gmail::labels::set_of(label)
    }

    fn filters(a: AccountId) -> Vec<ThreadFilter> {
        let (_, not_primary) = Category::Primary.categories();
        let (social, _) = Category::Social.categories();
        let sender = || vec!["Sender1@example.com".to_string()];
        vec![
            ThreadFilter::unified(set("INBOX")),
            ThreadFilter::account(a, set("INBOX")),
            ThreadFilter::unified(set("INBOX")).with_categories(&[], not_primary),
            ThreadFilter::unified(set("INBOX")).with_categories(&["CATEGORY_SOCIAL"], &[]),
            ThreadFilter::account(a, set("INBOX")).with_categories(social, &[]),
            ThreadFilter::unified(set("Label_1")),
            ThreadFilter::unified(set("TRASH")),
            ThreadFilter::unified(set("INBOX")).with_flag(FlagColor::Red),
            ThreadFilter::everything().with_flag(FlagColor::Red),
            ThreadFilter::unified(set("INBOX")).from_senders(sender()),
            ThreadFilter::everything().from_senders(sender()),
            ThreadFilter::everything().in_account(a).from_senders(sender()),
            ThreadFilter::unified(set("INBOX")).with_threads(vec![
                "t3".into(),
                "t10".into(),
                "t11".into(),
            ]),
            ThreadFilter::account(a, set("INBOX")).with_threads(vec!["t0".into(), "t6".into()]),
            ThreadFilter::everything(),
            ThreadFilter::unified(set("STARRED")),
            ThreadFilter::unified(set("UNREAD")),
            ThreadFilter::unified(set("CATEGORY_SOCIAL")),
            ThreadFilter::account(a, set("TRASH")),
            ThreadFilter::unified(set("SPAM")).with_flag(FlagColor::Red),
        ]
    }

    fn keys(rows: Vec<ThreadSummary>) -> Vec<(AccountId, String, Option<String>)> {
        rows.into_iter()
            .map(|r| (r.account_id, r.id, r.message_id))
            .collect()
    }

    fn walking(
        conn: &Connection,
        filter: &ThreadFilter,
        rows: Rows,
        page: Page,
        walk: Walk,
    ) -> Vec<ThreadSummary> {
        let resolved = filter.resolve(conn).unwrap();
        match rows {
            Rows::Threads => threads_walking(conn, filter, &resolved, page, walk),
            Rows::Messages => messages_walking(conn, filter, &resolved, page, walk),
        }
        .unwrap()
    }

    #[test]
    fn a_thread_id_finds_the_account_that_holds_it() {
        let (conn, a, b) = mailbox();
        assert_eq!(account_of(&conn, "t0").unwrap(), Some(a));
        assert_eq!(account_of(&conn, "t1").unwrap(), Some(b));
        assert_eq!(account_of(&conn, "nowhere").unwrap(), None);
    }

    #[test]
    fn every_walk_lists_the_same_rows() {
        let (conn, a, _) = mailbox();
        for filter in filters(a) {
            for (offset, limit) in [(0, 7), (7, 7), (0, 100)] {
                let threads = keys(
                    threads_walking(
                        &conn,
                        &filter,
                        &filter.resolve(&conn).unwrap(),
                        Page::Offset(offset, limit),
                        Walk::Scan,
                    )
                    .unwrap(),
                );
                let messages = keys(
                    messages_walking(
                        &conn,
                        &filter,
                        &filter.resolve(&conn).unwrap(),
                        Page::Offset(offset, limit),
                        Walk::Scan,
                    )
                    .unwrap(),
                );
                for walk in filter.every_walk(&filter.resolve(&conn).unwrap(), false) {
                    let by_walk = threads_walking(
                        &conn,
                        &filter,
                        &filter.resolve(&conn).unwrap(),
                        Page::Offset(offset, limit),
                        walk.clone(),
                    );
                    assert_eq!(
                        keys(by_walk.unwrap()),
                        threads,
                        "threads, {filter:?} {walk:?}"
                    );
                    let by_walk = messages_walking(
                        &conn,
                        &filter,
                        &filter.resolve(&conn).unwrap(),
                        Page::Offset(offset, limit),
                        walk.clone(),
                    );
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
    fn paging_past_the_last_row_lists_what_paging_by_offset_lists() {
        let (conn, a, _) = mailbox();
        for filter in filters(a) {
            for rows in [Rows::Threads, Rows::Messages] {
                let whole = keys(walking(
                    &conn,
                    &filter,
                    rows,
                    Page::Offset(0, 1000),
                    Walk::Scan,
                ));
                for walk in filter.every_walk(&filter.resolve(&conn).unwrap(), false) {
                    let mut paged = Vec::new();
                    let mut last: Option<ThreadSummary> = None;
                    loop {
                        let page = walking(
                            &conn,
                            &filter,
                            rows,
                            Page::After(last.as_ref(), 7),
                            walk.clone(),
                        );
                        let done = page.len() < 7;
                        last = page.last().cloned();
                        paged.extend(keys(page));
                        if done {
                            break;
                        }
                    }
                    assert_eq!(paged, whole, "{filter:?} {walk:?}");
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
                    let mut sql = filter.query_walking(
                        &filter.resolve(&conn).unwrap(),
                        rows,
                        "SELECT COUNT(*)",
                        walk,
                    );
                    sql.push(unread);
                    count(&conn, &sql).unwrap()
                };
                let scanned = unread_by(&Walk::Scan);
                for walk in filter.every_walk(&filter.resolve(&conn).unwrap(), true) {
                    assert_eq!(unread_by(&walk), scanned, "{filter:?} {walk:?}");
                }
            }
        }
    }

    #[test]
    fn a_small_category_of_a_big_inbox_is_walked_from_the_category() {
        let (conn, _, _) = mailbox();
        let (social, _) = Category::Social.categories();
        let filter = ThreadFilter::unified(set("INBOX")).with_categories(social, &[]);
        assert_eq!(
            filter
                .walk(&conn, &filter.resolve(&conn).unwrap(), Rows::Threads, 51)
                .unwrap(),
            Walk::Categories(social.iter().map(|l| l.to_string()).collect())
        );
    }

    #[test]
    fn unread_mail_is_counted_from_the_unread_index_when_that_is_smaller() {
        let (conn, _, _) = mailbox();
        let filter = ThreadFilter::unified(set("INBOX"));
        for rows in [Rows::Threads, Rows::Messages] {
            assert_eq!(
                filter
                    .count_walk(&conn, &filter.resolve(&conn).unwrap(), rows, true)
                    .unwrap(),
                Walk::Unread
            );
        }
    }

    #[test]
    fn named_threads_are_read_through_the_primary_key() {
        let (conn, a, _) = mailbox();
        for filter in [
            ThreadFilter::account(a, set("INBOX")).with_threads(vec!["t0".into()]),
            ThreadFilter::unified(set("INBOX")).with_threads(vec!["t0".into()]),
        ] {
            assert_eq!(
                filter
                    .walk(&conn, &filter.resolve(&conn).unwrap(), Rows::Threads, 1)
                    .unwrap(),
                Walk::Threads
            );
            let sql = filter.query_walking(
                &filter.resolve(&conn).unwrap(),
                Rows::Threads,
                "SELECT t.id",
                &Walk::Threads,
            );
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
        let filter = ThreadFilter::unified(set("INBOX"));
        for (rows, select) in [
            (Rows::Threads, format!("SELECT {COLUMNS}")),
            (Rows::Messages, format!("SELECT {MESSAGE_COLUMNS}")),
        ] {
            let mut sql =
                filter.query_walking(&filter.resolve(&conn).unwrap(), rows, &select, &Walk::Date);
            rows.order(&mut sql, Page::Offset(0, 10));
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
