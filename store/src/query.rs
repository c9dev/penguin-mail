//! Runs a query tree over the store's copy of one account's mail, for a
//! provider whose server reads no Gmail syntax. Text matches ignore case
//! in every script: SQLite's own `lower` folds ASCII letters alone, so the
//! store folds text with [`FOLD`], which every connection registers.

use chrono::{DateTime, NaiveDate, TimeZone};
use mailrs_domain::query::{self, Query, Term};
use mailrs_domain::{AccountId, EpochMillis, MailSet, Role};
use rusqlite::{Connection, params_from_iter};

use crate::Result;
use crate::threads::Sql;

/// The SQL function that folds text to lower case in every script, and
/// reads a missing value as empty text so a comparison never yields NULL.
pub const FOLD: &str = "penguin_fold";

const DAY: i64 = 24 * 60 * 60 * 1000;

/// One stored message a query matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Matched {
    pub message_id: String,
    pub thread_id: String,
}

/// At most `limit` of the account's stored messages that `query` matches,
/// newest first. `NewerThan` counts back from `now`, and `Since` and
/// `Before` start their day at midnight in `now`'s zone. As in Gmail's
/// search, mail in the Junk or the Trash stays out unless the query asks
/// for that mailbox.
pub fn matching<Tz: TimeZone>(
    conn: &Connection,
    account_id: AccountId,
    query: &Query,
    now: &DateTime<Tz>,
    limit: usize,
) -> Result<Vec<Matched>> {
    let scope = Scope {
        now,
        mailboxes: spellings(conn, account_id)?,
    };
    let mut sql = Sql::default();
    sql.push("SELECT m.id, m.thread_id FROM messages m WHERE m.account_id = ")
        .bind(account_id)
        .push(" AND ");
    // A tree with nothing left in it, like an empty search, holds for
    // every message.
    append(
        &mut sql,
        condition(query, &scope).unwrap_or_else(|| text("1")),
    );
    for role in [Role::Junk, Role::Trash] {
        let set = MailSet::Role(role);
        if !query.asks_for(&set) {
            sql.push(" AND NOT ");
            append(&mut sql, in_set(&set));
        }
    }
    sql.push(" ORDER BY m.date DESC, m.id LIMIT ")
        .bind(i64::try_from(limit).unwrap_or(i64::MAX));
    let mut stmt = conn.prepare(&sql.text)?;
    let rows = stmt
        .query_map(params_from_iter(sql.params.iter()), |row| {
            Ok(Matched {
                message_id: row.get(0)?,
                thread_id: row.get(1)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<Matched>>>()?;
    Ok(rows)
}

/// What a condition needs beyond the tree: the clock and the account's
/// mailboxes.
struct Scope<'a, Tz: TimeZone> {
    now: &'a DateTime<Tz>,
    /// Each mailbox's key with its name in Gmail's label spelling.
    mailboxes: Vec<(i64, String)>,
}

/// The account's mailboxes, each key with its name in Gmail's label
/// spelling. SQL cannot spell a name that way, so the store matches a
/// typed mailbox name to keys here and binds the keys.
fn spellings(conn: &Connection, account_id: AccountId) -> Result<Vec<(i64, String)>> {
    let mut stmt = conn.prepare("SELECT key, name FROM mailboxes WHERE account_id = ?1")?;
    let rows = stmt
        .query_map([account_id], |row| {
            Ok((
                row.get(0)?,
                query::label_spelling(&row.get::<_, String>(1)?),
            ))
        })?
        .collect::<rusqlite::Result<Vec<(i64, String)>>>()?;
    Ok(rows)
}

/// The condition that message `m` matches `query`, or `None` when nothing
/// is left of it. A text term with nothing to search for drops out, with
/// the `Not` around it, and an `And` or `Or` of such terms drops out too,
/// as they do from the Gmail printer's text.
///
/// This recurses once per level of the tree. Typed search caps its depth
/// at [`query::MAX_DEPTH`], and folders and smart mailboxes build trees a
/// few levels deep, so the stack holds for any tree the product builds.
fn condition<Tz: TimeZone>(query: &Query, scope: &Scope<'_, Tz>) -> Option<Sql> {
    match query {
        Query::Term(term) => term_condition(term, scope),
        Query::And(items) => joined(items, " AND ", scope),
        Query::Or(items) => joined(items, " OR ", scope),
        Query::Not(inner) => {
            let inner = condition(inner, scope)?;
            let mut sql = text("NOT (");
            append(&mut sql, inner);
            sql.push(")");
            Some(sql)
        }
    }
}

/// `items` joined by `operator`, leaving out the ones that dropped out.
fn joined<Tz: TimeZone>(items: &[Query], operator: &str, scope: &Scope<'_, Tz>) -> Option<Sql> {
    let parts: Vec<Sql> = items
        .iter()
        .filter_map(|item| condition(item, scope))
        .collect();
    if parts.is_empty() {
        return None;
    }
    let mut sql = text("(");
    for (i, part) in parts.into_iter().enumerate() {
        if i > 0 {
            sql.push(operator);
        }
        append(&mut sql, part);
    }
    sql.push(")");
    Some(sql)
}

fn term_condition<Tz: TimeZone>(term: &Term, scope: &Scope<'_, Tz>) -> Option<Sql> {
    let now = scope.now;
    let mut sql = Sql::default();
    match term {
        Term::From(words) => return contains(&["m.from_name", "m.from_addr"], &[], words),
        Term::To(words) => return contains(&[], &["m.to_addrs"], words),
        Term::Subject(words) => return contains(&["m.subject"], &[], words),
        Term::Words(words) => return anywhere(words),
        Term::Since(day) => {
            sql.push("m.date >= ").bind(start_of(*day, now));
        }
        Term::Before(day) => {
            sql.push("m.date < ").bind(start_of(*day, now));
        }
        Term::NewerThan(days) => {
            sql.push("m.date >= ")
                .bind(now.timestamp_millis() - i64::from(*days) * DAY);
        }
        Term::HasAttachment => {
            sql.push("m.has_attachments = 1");
        }
        Term::Unread => {
            sql.push("m.seen = 0");
        }
        Term::Flagged => return Some(in_set(&MailSet::flagged())),
        Term::In(set) => return Some(in_set(set)),
        Term::MailboxNamed(name) => return Some(in_mailbox_named(name, &scope.mailboxes)),
        Term::Larger(bytes) => {
            sql.push("m.size > ").bind(*bytes);
        }
    }
    Some(sql)
}

/// The condition that message `m` is in a mailbox whose name Gmail's
/// search spells as it spells `name`, so `receipts-2025` finds the
/// mailbox `Receipts (2025)`. A name no mailbox spells matches nothing.
fn in_mailbox_named(name: &str, mailboxes: &[(i64, String)]) -> Sql {
    let wanted = query::label_spelling(name);
    let keys: Vec<i64> = mailboxes
        .iter()
        .filter(|(_, spelling)| *spelling == wanted)
        .map(|(key, _)| *key)
        .collect();
    let mut sql = text(
        "EXISTS (SELECT 1 FROM message_mailboxes l WHERE l.account_id = m.account_id \
         AND l.message_id = m.id AND l.mailbox IN (",
    );
    sql.bind_keys(&keys).push("))");
    sql
}

/// The condition that one of `columns`, or a name or an address in one
/// of the address lists `lists`, holds `words`, whatever the case. `None`
/// when nothing is left to search for. An address list is JSON, so the
/// search reads each address's name and email out of it: matching the
/// raw text would find the JSON's own keys, such as `email` or `null`.
fn contains(columns: &[&str], lists: &[&str], words: &str) -> Option<Sql> {
    let needle = query::plain(words).to_lowercase();
    if needle.is_empty() {
        return None;
    }
    let mut parts = Vec::new();
    for column in columns {
        parts.push(holds(column, &needle));
    }
    for list in lists {
        let mut sql = text(&format!("EXISTS (SELECT 1 FROM json_each({list}) r WHERE "));
        append(&mut sql, holds("json_extract(r.value, '$.name')", &needle));
        sql.push(" OR ");
        append(&mut sql, holds("json_extract(r.value, '$.email')", &needle));
        sql.push(")");
        parts.push(sql);
    }
    let mut sql = text("(");
    for (i, part) in parts.into_iter().enumerate() {
        if i > 0 {
            sql.push(" OR ");
        }
        append(&mut sql, part);
    }
    sql.push(")");
    Some(sql)
}

/// The condition that the text `expr` gives holds `needle`, which is
/// already folded.
fn holds(expr: &str, needle: &str) -> Sql {
    let mut sql = text(&format!("instr({FOLD}({expr}), "));
    sql.bind(needle.to_string()).push(") > 0");
    sql
}

/// The condition that the message's headers, snippet or stored body hold
/// `words`, or `None` when nothing is left to search for.
fn anywhere(words: &str) -> Option<Sql> {
    let headers = contains(
        &["m.subject", "m.snippet", "m.from_name", "m.from_addr"],
        &["m.to_addrs", "m.cc_addrs"],
        words,
    )?;
    let mut sql = text("(");
    append(&mut sql, headers);
    sql.push(&format!(
        " OR EXISTS (SELECT 1 FROM bodies y WHERE y.account_id = m.account_id \
         AND y.message_id = m.id AND instr({FOLD}(y.text), "
    ))
    .bind(query::plain(words).to_lowercase())
    .push(") > 0))");
    Some(sql)
}

fn text(sql: &str) -> Sql {
    let mut out = Sql::default();
    out.push(sql);
    out
}

/// Appends `part`, its text and its values.
fn append(sql: &mut Sql, part: Sql) {
    sql.text.push_str(&part.text);
    sql.params.extend(part.params);
}

/// The condition that message `m` is in `set`.
fn in_set(set: &MailSet) -> Sql {
    let mut sql = Sql::default();
    match set {
        MailSet::Role(role) => {
            sql.push(
                "EXISTS (SELECT 1 FROM message_mailboxes l JOIN mailboxes b ON b.key = l.mailbox \
                 WHERE l.account_id = m.account_id AND l.message_id = m.id AND b.role = ",
            )
            .bind(role.as_str().to_string())
            .push(")");
        }
        MailSet::Mailbox(id) => {
            sql.push(
                "EXISTS (SELECT 1 FROM message_mailboxes l JOIN mailboxes b ON b.key = l.mailbox \
                 WHERE l.account_id = m.account_id AND l.message_id = m.id AND b.id = ",
            )
            .bind(id.clone())
            .push(")");
        }
        MailSet::Keyword(k) => {
            sql.push(
                "EXISTS (SELECT 1 FROM message_keywords l WHERE l.account_id = m.account_id \
                 AND l.message_id = m.id AND l.keyword = ",
            )
            .bind(k.clone())
            .push(")");
        }
        MailSet::Unseen => {
            sql.push("m.seen = 0");
        }
        MailSet::Category(c) => {
            sql.push(
                "EXISTS (SELECT 1 FROM message_categories l WHERE l.account_id = m.account_id \
                 AND l.message_id = m.id AND l.category = ",
            )
            .bind(c.clone())
            .push(")");
        }
    }
    sql
}

/// When `day` starts in `now`'s zone, in milliseconds. A day whose
/// midnight the clocks skip starts at its first whole hour.
fn start_of<Tz: TimeZone>(day: NaiveDate, now: &DateTime<Tz>) -> EpochMillis {
    let zone = now.timezone();
    (0..24)
        .filter_map(|hour| day.and_hms_opt(hour, 0, 0))
        .find_map(|local| zone.from_local_datetime(&local).earliest())
        .map_or_else(
            || {
                day.and_hms_opt(0, 0, 0)
                    .map_or(0, |t| t.and_utc().timestamp_millis())
            },
            |start| start.timestamp_millis(),
        )
}
