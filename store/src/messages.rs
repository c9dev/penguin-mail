//! Message rows, what they are in and carry, and the derived thread rows.
//!
//! Every write goes through [`apply`], which refreshes each thread it
//! touched inside the caller's transaction, so no caller has to remember
//! to.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use mailrs_domain::gmail;
use mailrs_domain::{AccountId, Address, Applied, Membership, MessageMeta, system_label};
use rusqlite::{Connection, OptionalExtension, params};

use crate::{Result, StoreError};

/// One change to stored mail. [`apply`] takes a list of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// Stores a message as the server described it, replacing whatever
    /// the store held of it, under sync generation `generation`.
    Upsert {
        meta: Box<MessageMeta>,
        generation: i64,
    },
    /// Keeps a stored message as it is under a new sync generation, so the
    /// sweep that ends a relisting keeps it.
    Keep {
        message_id: String,
        generation: i64,
    },
    AddToMailbox {
        message_id: String,
        mailbox: String,
    },
    RemoveFromMailbox {
        message_id: String,
        mailbox: String,
    },
    SetKeyword {
        message_id: String,
        keyword: String,
        on: bool,
    },
    SetCategory {
        message_id: String,
        category: String,
        on: bool,
    },
    Delete {
        message_id: String,
    },
    DeleteThread {
        thread_id: String,
    },
    /// Notes that the store now holds every message of the thread.
    MarkWhole {
        thread_id: String,
    },
}

impl Change {
    /// The change that gives message `message_id` the `membership`, or
    /// with `on` false takes it away.
    pub fn of(message_id: &str, membership: Membership, on: bool) -> Change {
        let message_id = message_id.to_string();
        match membership {
            Membership::Mailbox(mailbox) if on => Change::AddToMailbox {
                message_id,
                mailbox,
            },
            Membership::Mailbox(mailbox) => Change::RemoveFromMailbox {
                message_id,
                mailbox,
            },
            Membership::Keyword(keyword) => Change::SetKeyword {
                message_id,
                keyword,
                on,
            },
            Membership::Category(category) => Change::SetCategory {
                message_id,
                category,
                on,
            },
        }
    }

    /// The change that puts Gmail's `label` on the message (`carried`) or
    /// takes it off. Gmail's `UNREAD` going on takes `$seen` away.
    pub fn label(message_id: &str, label: &str, carried: bool) -> Change {
        let (membership, held) = gmail::membership_of(label);
        Change::of(message_id, membership, carried == held)
    }

    /// The message, membership and direction of a membership change.
    fn membership(&self) -> Option<(&str, Membership, bool)> {
        match self {
            Change::AddToMailbox {
                message_id,
                mailbox,
            } => Some((message_id, Membership::Mailbox(mailbox.clone()), true)),
            Change::RemoveFromMailbox {
                message_id,
                mailbox,
            } => Some((message_id, Membership::Mailbox(mailbox.clone()), false)),
            Change::SetKeyword {
                message_id,
                keyword,
                on,
            } => Some((message_id, Membership::Keyword(keyword.clone()), *on)),
            Change::SetCategory {
                message_id,
                category,
                on,
            } => Some((message_id, Membership::Category(category.clone()), *on)),
            _ => None,
        }
    }
}

/// What [`apply`] did: the threads whose rows it wrote, which the caller
/// announces once the transaction commits, and what each membership
/// change did to each message, in the order the changes came.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Touched {
    pub threads: BTreeSet<String>,
    pub applied: Vec<Applied>,
}

/// Writes `changes`, all of them for `account_id`, in order, then
/// refreshes the row of every thread they touched, and marks threads
/// whole last so a thread the batch created has a row to mark. A
/// membership change on a message the store lacks changes nothing and
/// touches no thread.
pub fn apply(conn: &Connection, account_id: AccountId, changes: &[Change]) -> Result<Touched> {
    let mut touched = Touched::default();
    let mut applied: BTreeMap<String, Applied> = BTreeMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut whole: Vec<&str> = Vec::new();
    for change in changes {
        match change {
            Change::Upsert { meta, generation } => {
                upsert_message(conn, meta, *generation)?;
                touched.threads.insert(meta.thread_id.clone());
            }
            Change::Keep {
                message_id,
                generation,
            } => {
                conn.prepare_cached(
                    "UPDATE messages SET sync_gen = ?3 WHERE account_id = ?1 AND id = ?2",
                )?
                .execute(params![account_id, message_id, generation])?;
            }
            Change::Delete { message_id } => {
                touched
                    .threads
                    .extend(delete_message(conn, account_id, message_id)?);
            }
            Change::DeleteThread { thread_id } => {
                delete_thread(conn, account_id, thread_id)?;
                touched.threads.insert(thread_id.clone());
            }
            Change::MarkWhole { thread_id } => whole.push(thread_id),
            other => {
                let Some((message_id, membership, on)) = other.membership() else {
                    continue;
                };
                let Some((thread_id, changed)) =
                    set_membership(conn, account_id, message_id, &membership, on)?
                else {
                    continue;
                };
                touched.threads.insert(thread_id.clone());
                if !changed {
                    continue;
                }
                let entry = applied.entry(message_id.to_string()).or_insert_with(|| {
                    order.push(message_id.to_string());
                    Applied {
                        thread_id,
                        message_id: message_id.to_string(),
                        gained: Vec::new(),
                        lost: Vec::new(),
                    }
                });
                match on {
                    true => entry.gained.push(membership),
                    false => entry.lost.push(membership),
                }
            }
        }
    }
    for thread_id in &touched.threads {
        refresh_thread(conn, account_id, thread_id)?;
    }
    for thread_id in whole {
        mark_whole(conn, account_id, thread_id)?;
    }
    touched.applied = order
        .into_iter()
        .filter_map(|id| applied.remove(&id))
        .collect();
    Ok(touched)
}

/// Gives a stored message `membership`, or takes it away. Returns the
/// message's thread and whether anything changed, or `None` when the
/// message is not stored.
fn set_membership(
    conn: &Connection,
    account_id: AccountId,
    message_id: &str,
    membership: &Membership,
    on: bool,
) -> Result<Option<(String, bool)>> {
    let Some(thread_id) = thread_id_of(conn, account_id, message_id)? else {
        return Ok(None);
    };
    // Today's tables hold Gmail label ids; a keyword Gmail has no label
    // for has nowhere to go and changes nothing.
    let Some((label, carried)) = gmail::label_of(membership, on) else {
        return Ok(Some((thread_id, false)));
    };
    let sql = match carried {
        true => {
            "INSERT OR IGNORE INTO message_labels (account_id, message_id, label_id) VALUES (?1, ?2, ?3)"
        }
        false => {
            "DELETE FROM message_labels WHERE account_id = ?1 AND message_id = ?2 AND label_id = ?3"
        }
    };
    let changed = conn
        .prepare_cached(sql)?
        .execute(params![account_id, message_id, label])?
        > 0;
    Ok(Some((thread_id, changed)))
}

fn upsert_message(conn: &Connection, m: &MessageMeta, sync_gen: i64) -> Result<()> {
    let to = serde_json::to_string(&m.to).unwrap_or_else(|_| "[]".into());
    let cc = serde_json::to_string(&m.cc).unwrap_or_else(|_| "[]".into());
    conn.execute(
        "INSERT INTO messages (account_id, id, thread_id, rfc822_msgid, from_name, from_addr, to_addrs, \
         cc_addrs, subject, date, snippet, size, has_attachments, list_unsubscribe, one_click, sync_gen) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16) \
         ON CONFLICT (account_id, id) DO UPDATE SET thread_id = excluded.thread_id, \
         rfc822_msgid = excluded.rfc822_msgid, from_name = excluded.from_name, \
         from_addr = excluded.from_addr, to_addrs = excluded.to_addrs, cc_addrs = excluded.cc_addrs, \
         subject = excluded.subject, date = excluded.date, snippet = excluded.snippet, \
         size = excluded.size, has_attachments = excluded.has_attachments, \
         list_unsubscribe = excluded.list_unsubscribe, one_click = excluded.one_click, \
         sync_gen = excluded.sync_gen",
        params![
            m.account_id,
            m.id,
            m.thread_id,
            m.rfc822_msgid,
            m.from.as_ref().and_then(|a| a.name.as_deref()),
            m.from.as_ref().map(|a| a.email.as_str()),
            to,
            cc,
            m.subject,
            m.date,
            m.snippet,
            m.size,
            m.has_attachments,
            m.list_unsubscribe,
            m.one_click,
            sync_gen,
        ],
    )?;
    set_labels(conn, m.account_id, &m.id, &m.label_ids)
}

/// Records what a later metadata fetch found in one message's unsubscribe
/// headers. Mail stored before those headers joined the metadata fetch has
/// nothing in these columns, and this is how it is filled in without
/// rewriting the rest of the row.
pub fn set_unsubscribe(
    conn: &Connection,
    account_id: AccountId,
    message_id: &str,
    header: Option<&str>,
    one_click: bool,
) -> Result<()> {
    conn.execute(
        "UPDATE messages SET list_unsubscribe = ?3, one_click = ?4 \
         WHERE account_id = ?1 AND id = ?2",
        params![account_id, message_id, header, one_click],
    )?;
    Ok(())
}

/// Replaces a stored message's labels.
fn set_labels(
    conn: &Connection,
    account_id: AccountId,
    message_id: &str,
    labels: &[String],
) -> Result<()> {
    conn.execute(
        "DELETE FROM message_labels WHERE account_id = ?1 AND message_id = ?2",
        params![account_id, message_id],
    )?;
    let mut insert = conn.prepare_cached(
        "INSERT OR IGNORE INTO message_labels (account_id, message_id, label_id) VALUES (?1, ?2, ?3)",
    )?;
    for label in labels {
        insert.execute(params![account_id, message_id, label])?;
    }
    Ok(())
}

pub fn thread_id_of(
    conn: &Connection,
    account_id: AccountId,
    message_id: &str,
) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT thread_id FROM messages WHERE account_id = ?1 AND id = ?2",
            params![account_id, message_id],
            |row| row.get(0),
        )
        .optional()?)
}

/// Deletes a message with its labels and body. Returns its thread, or `None`
/// when the message was not stored.
fn delete_message(
    conn: &Connection,
    account_id: AccountId,
    message_id: &str,
) -> Result<Option<String>> {
    let thread_id = thread_id_of(conn, account_id, message_id)?;
    if thread_id.is_some() {
        conn.execute(
            "DELETE FROM messages WHERE account_id = ?1 AND id = ?2",
            params![account_id, message_id],
        )?;
    }
    Ok(thread_id)
}

pub(crate) fn delete_thread(
    conn: &Connection,
    account_id: AccountId,
    thread_id: &str,
) -> Result<()> {
    conn.execute(
        "DELETE FROM messages WHERE account_id = ?1 AND thread_id = ?2",
        params![account_id, thread_id],
    )?;
    conn.execute(
        "DELETE FROM threads WHERE account_id = ?1 AND id = ?2",
        params![account_id, thread_id],
    )?;
    Ok(())
}

/// A stored message's labels, sorted.
pub fn labels_of(
    conn: &Connection,
    account_id: AccountId,
    message_id: &str,
) -> Result<Vec<String>> {
    let mut stmt = conn.prepare_cached(
        "SELECT label_id FROM message_labels WHERE account_id = ?1 AND message_id = ?2 ORDER BY label_id",
    )?;
    let rows = stmt.query_map(params![account_id, message_id], |row| row.get(0))?;
    Ok(rows.collect::<rusqlite::Result<Vec<String>>>()?)
}

/// The ids of the stored messages that carry `label`.
pub fn labelled(conn: &Connection, account_id: AccountId, label: &str) -> Result<HashSet<String>> {
    let mut stmt = conn.prepare_cached(
        "SELECT message_id FROM message_labels WHERE account_id = ?1 AND label_id = ?2",
    )?;
    let rows = stmt.query_map(params![account_id, label], |row| row.get(0))?;
    Ok(rows.collect::<rusqlite::Result<HashSet<String>>>()?)
}

/// The subset of `ids` that is stored, from one statement however many
/// ids there are.
pub fn existing_ids(
    conn: &Connection,
    account_id: AccountId,
    ids: &[String],
) -> Result<HashSet<String>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id FROM messages WHERE account_id = ?1 AND id IN (SELECT value FROM json_each(?2))",
    )?;
    let rows = stmt.query_map(params![account_id, json_list(ids)], |row| row.get(0))?;
    Ok(rows.collect::<rusqlite::Result<HashSet<String>>>()?)
}

/// `ids` as a JSON array, which `json_each` turns back into rows. One
/// bound value holds any number of ids, where a list of placeholders
/// would run into SQLite's limit on them.
fn json_list(ids: &[String]) -> String {
    serde_json::to_string(ids).unwrap_or_else(|_| "[]".into())
}

struct MessageRow {
    id: String,
    thread_id: String,
    rfc822_msgid: Option<String>,
    from_name: Option<String>,
    from_addr: Option<String>,
    to: String,
    cc: String,
    subject: String,
    date: i64,
    snippet: String,
    size: i64,
    has_attachments: bool,
    list_unsubscribe: Option<String>,
    one_click: bool,
}

/// A thread's stored messages, oldest first, with their labels from one
/// more query rather than one per message.
pub fn thread_messages(
    conn: &Connection,
    account_id: AccountId,
    thread_id: &str,
) -> Result<Vec<MessageMeta>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, thread_id, rfc822_msgid, from_name, from_addr, to_addrs, cc_addrs, subject, date, \
         snippet, size, has_attachments, list_unsubscribe, one_click FROM messages \
         WHERE account_id = ?1 AND thread_id = ?2 ORDER BY date ASC, id",
    )?;
    let rows = stmt
        .query_map(params![account_id, thread_id], message_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut labels = conn.prepare_cached(
        "SELECT l.message_id, l.label_id FROM messages m \
         CROSS JOIN message_labels l ON l.account_id = m.account_id AND l.message_id = m.id \
         WHERE m.account_id = ?1 AND m.thread_id = ?2 ORDER BY l.label_id",
    )?;
    let labels = labels_by_message(labels.query_map(params![account_id, thread_id], label_row)?)?;
    rows.into_iter()
        .map(|r| message_meta(account_id, r, &labels))
        .collect()
}

fn label_row(row: &rusqlite::Row) -> rusqlite::Result<(String, String)> {
    Ok((row.get(0)?, row.get(1)?))
}

/// Each message's labels, sorted, from `(message id, label)` rows that
/// arrive in label order.
fn labels_by_message(
    rows: impl Iterator<Item = rusqlite::Result<(String, String)>>,
) -> Result<HashMap<String, Vec<String>>> {
    let mut labels: HashMap<String, Vec<String>> = HashMap::new();
    for row in rows {
        let (message_id, label) = row?;
        labels.entry(message_id).or_default().push(label);
    }
    Ok(labels)
}

fn message_row(row: &rusqlite::Row) -> rusqlite::Result<MessageRow> {
    Ok(MessageRow {
        id: row.get(0)?,
        thread_id: row.get(1)?,
        rfc822_msgid: row.get(2)?,
        from_name: row.get(3)?,
        from_addr: row.get(4)?,
        to: row.get(5)?,
        cc: row.get(6)?,
        subject: row.get(7)?,
        date: row.get(8)?,
        snippet: row.get(9)?,
        size: row.get(10)?,
        has_attachments: row.get(11)?,
        list_unsubscribe: row.get(12)?,
        one_click: row.get(13)?,
    })
}

fn message_meta(
    account_id: AccountId,
    r: MessageRow,
    labels: &HashMap<String, Vec<String>>,
) -> Result<MessageMeta> {
    Ok(MessageMeta {
        account_id,
        to: parse_addresses("messages.to_addrs", &r.to)?,
        cc: parse_addresses("messages.cc_addrs", &r.cc)?,
        from: r.from_addr.map(|email| Address {
            name: r.from_name,
            email,
        }),
        label_ids: labels.get(&r.id).cloned().unwrap_or_default(),
        id: r.id,
        thread_id: r.thread_id,
        rfc822_msgid: r.rfc822_msgid,
        subject: r.subject,
        date: r.date,
        snippet: r.snippet,
        size: r.size,
        has_attachments: r.has_attachments,
        list_unsubscribe: r.list_unsubscribe,
        one_click: r.one_click,
    })
}

/// The stored metadata for `ids`, oldest first. Ids the store does not
/// hold are simply missing, so a caller asks Gmail only for those.
pub fn by_ids(
    conn: &Connection,
    account_id: AccountId,
    ids: &[String],
) -> Result<Vec<MessageMeta>> {
    let ids = json_list(ids);
    let mut stmt = conn.prepare_cached(
        "SELECT id, thread_id, rfc822_msgid, from_name, from_addr, to_addrs, cc_addrs, \
         subject, date, snippet, size, has_attachments, list_unsubscribe, one_click \
         FROM messages WHERE account_id = ?1 AND id IN (SELECT value FROM json_each(?2)) \
         ORDER BY date ASC, id",
    )?;
    let rows = stmt
        .query_map(params![account_id, ids], message_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut labels = conn.prepare_cached(
        "SELECT message_id, label_id FROM message_labels \
         WHERE account_id = ?1 AND message_id IN (SELECT value FROM json_each(?2)) \
         ORDER BY label_id",
    )?;
    let labels = labels_by_message(labels.query_map(params![account_id, ids], label_row)?)?;
    rows.into_iter()
        .map(|r| message_meta(account_id, r, &labels))
        .collect()
}

fn parse_addresses(column: &'static str, json: &str) -> Result<Vec<Address>> {
    serde_json::from_str(json).map_err(|_| StoreError::Corrupt {
        column,
        value: json.to_string(),
    })
}

/// The colour of a thread's newest coloured message. `?1` is the account and
/// `?2` the thread.
pub(crate) const NEWEST_FLAG_COLOR: &str = "SELECT f.color FROM messages m \
     CROSS JOIN flags f ON f.account_id = m.account_id AND f.message_id = m.id \
     WHERE m.account_id = ?1 AND m.thread_id = ?2 ORDER BY m.date DESC, m.id DESC LIMIT 1";

/// Notes that the store holds every message Gmail has in the thread, as a
/// fetch of the whole thread leaves it. History keeps it that way: it
/// stores each message added later and drops each one deleted.
fn mark_whole(conn: &Connection, account_id: AccountId, thread_id: &str) -> Result<()> {
    conn.prepare_cached("UPDATE threads SET whole = 1 WHERE account_id = ?1 AND id = ?2")?
        .execute(params![account_id, thread_id])?;
    Ok(())
}

/// Whether the store holds the thread, all of it: false for a thread the
/// store lacks or holds only the window's part of.
pub fn is_whole(conn: &Connection, account_id: AccountId, thread_id: &str) -> Result<bool> {
    Ok(conn
        .prepare_cached("SELECT whole FROM threads WHERE account_id = ?1 AND id = ?2")?
        .query_row(params![account_id, thread_id], |row| row.get(0))
        .optional()?
        .unwrap_or(false))
}

/// Recomputes a thread's summary row and label set from its messages, and
/// deletes the thread when no messages remain.
///
/// Each query starts from the thread's few messages. The `CROSS JOIN`s keep
/// SQLite from starting at a label instead, which walked every message in
/// the account carrying it.
pub(crate) fn refresh_thread(
    conn: &Connection,
    account_id: AccountId,
    thread_id: &str,
) -> Result<()> {
    let (count, last, has_attachments): (i64, Option<i64>, Option<bool>) = conn
        .prepare_cached(
            "SELECT COUNT(*), MAX(date), MAX(has_attachments) FROM messages \
             WHERE account_id = ?1 AND thread_id = ?2",
        )?
        .query_row(params![account_id, thread_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
    if count == 0 {
        conn.execute(
            "DELETE FROM threads WHERE account_id = ?1 AND id = ?2",
            params![account_id, thread_id],
        )?;
        return Ok(());
    }
    let subject: String = conn
        .prepare_cached(
            "SELECT subject FROM messages WHERE account_id = ?1 AND thread_id = ?2 \
             ORDER BY date ASC, id LIMIT 1",
        )?
        .query_row(params![account_id, thread_id], |row| row.get(0))?;
    let (snippet, from, from_email): (String, String, String) = conn
        .prepare_cached(
            "SELECT snippet, COALESCE(from_name, from_addr, ''), COALESCE(from_addr, '') \
             FROM messages WHERE account_id = ?1 AND thread_id = ?2 \
             ORDER BY date DESC, id DESC LIMIT 1",
        )?
        .query_row(params![account_id, thread_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
    let (unread, starred): (Option<bool>, Option<bool>) = conn
        .prepare_cached(
            "SELECT MAX(ml.label_id = ?3), MAX(ml.label_id = ?4) FROM messages m \
             CROSS JOIN message_labels ml ON ml.account_id = m.account_id AND ml.message_id = m.id \
             WHERE m.account_id = ?1 AND m.thread_id = ?2 AND ml.label_id IN (?3, ?4)",
        )?
        .query_row(
            params![
                account_id,
                thread_id,
                system_label::UNREAD,
                system_label::STARRED
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
    let flag_color: Option<String> = conn
        .prepare_cached(NEWEST_FLAG_COLOR)?
        .query_row(params![account_id, thread_id], |row| row.get(0))
        .optional()?;
    conn.prepare_cached(
        "INSERT INTO threads (account_id, id, last_message_at, subject, snippet, from_display, message_count, \
         unread, starred, has_attachments, flag_color, from_email) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12) \
         ON CONFLICT (account_id, id) DO UPDATE SET last_message_at = excluded.last_message_at, \
         subject = excluded.subject, snippet = excluded.snippet, from_display = excluded.from_display, \
         message_count = excluded.message_count, unread = excluded.unread, starred = excluded.starred, \
         has_attachments = excluded.has_attachments, flag_color = excluded.flag_color, \
         from_email = excluded.from_email",
    )?
    .execute(params![
        account_id,
        thread_id,
        last.unwrap_or(0),
        subject,
        snippet,
        from,
        count,
        unread.unwrap_or(false),
        starred.unwrap_or(false),
        has_attachments.unwrap_or(false),
        flag_color,
        from_email,
    ])?;
    conn.prepare_cached("DELETE FROM thread_labels WHERE account_id = ?1 AND thread_id = ?2")?
        .execute(params![account_id, thread_id])?;
    conn.prepare_cached(
        "INSERT INTO thread_labels (account_id, thread_id, label_id) \
         SELECT DISTINCT ?1, ?2, ml.label_id FROM messages m \
         CROSS JOIN message_labels ml ON ml.account_id = m.account_id AND ml.message_id = m.id \
         WHERE m.account_id = ?1 AND m.thread_id = ?2",
    )?
    .execute(params![account_id, thread_id])?;
    Ok(())
}
