//! Message rows, what they are in and carry, and the derived thread rows.
//!
//! Every write goes through [`apply`], which refreshes each thread it
//! touched inside the caller's transaction, so no caller has to remember
//! to.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use mailrs_domain::mailbox::keyword;
use mailrs_domain::{
    AccountId, Address, Applied, MailSet, Membership, Memberships, MessageMeta, Role,
};
use rusqlite::{Connection, OptionalExtension, params};

use crate::threading::{self, Links};
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
    /// Stores a message from a server that keeps no threads, in the thread
    /// local threading finds for it. A message already stored keeps its
    /// thread, so a second fetch never moves it.
    UpsertLocal {
        meta: Box<MessageMeta>,
        generation: i64,
        links: Links,
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
            Change::UpsertLocal {
                meta,
                generation,
                links,
            } => {
                let mut meta = (**meta).clone();
                meta.thread_id = match thread_id_of(conn, account_id, &meta.id)? {
                    Some(kept) => kept,
                    None => threading::thread_for(conn, account_id, &meta, links)?,
                };
                upsert_message(conn, &meta, *generation)?;
                threading::remember(conn, account_id, &meta, links)?;
                touched.threads.insert(meta.thread_id);
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
    let target = (account_id, message_id);
    let changed = match membership {
        Membership::Mailbox(id) if on => {
            let key = mailbox_key(conn, account_id, id)?;
            conn.prepare_cached(
                "INSERT OR IGNORE INTO message_mailboxes (account_id, message_id, mailbox) VALUES (?1, ?2, ?3)",
            )?
            .execute(params![target.0, target.1, key])?
        }
        Membership::Mailbox(id) => conn
            .prepare_cached(
                "DELETE FROM message_mailboxes WHERE account_id = ?1 AND message_id = ?2 \
                 AND mailbox = (SELECT key FROM mailboxes WHERE account_id = ?1 AND id = ?3)",
            )?
            .execute(params![target.0, target.1, id])?,
        Membership::Keyword(k) if on => conn
            .prepare_cached(
                "INSERT OR IGNORE INTO message_keywords (account_id, message_id, keyword) VALUES (?1, ?2, ?3)",
            )?
            .execute(params![target.0, target.1, k])?,
        Membership::Keyword(k) => conn
            .prepare_cached(
                "DELETE FROM message_keywords WHERE account_id = ?1 AND message_id = ?2 AND keyword = ?3",
            )?
            .execute(params![target.0, target.1, k])?,
        Membership::Category(c) if on => conn
            .prepare_cached(
                "INSERT OR IGNORE INTO message_categories (account_id, message_id, category) VALUES (?1, ?2, ?3)",
            )?
            .execute(params![target.0, target.1, c])?,
        Membership::Category(c) => conn
            .prepare_cached(
                "DELETE FROM message_categories WHERE account_id = ?1 AND message_id = ?2 AND category = ?3",
            )?
            .execute(params![target.0, target.1, c])?,
    } > 0;
    if changed && matches!(membership, Membership::Keyword(k) if k == keyword::SEEN) {
        conn.prepare_cached("UPDATE messages SET seen = ?3 WHERE account_id = ?1 AND id = ?2")?
            .execute(params![account_id, message_id, on])?;
    }
    Ok(Some((thread_id, changed)))
}

fn upsert_message(conn: &Connection, m: &MessageMeta, sync_gen: i64) -> Result<()> {
    let to = serde_json::to_string(&m.to).unwrap_or_else(|_| "[]".into());
    let cc = serde_json::to_string(&m.cc).unwrap_or_else(|_| "[]".into());
    let seen = !m.is_unread();
    conn.execute(
        "INSERT INTO messages (account_id, id, thread_id, rfc822_msgid, from_name, from_addr, to_addrs, \
         cc_addrs, subject, date, snippet, size, has_attachments, list_unsubscribe, one_click, sync_gen, seen) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17) \
         ON CONFLICT (account_id, id) DO UPDATE SET thread_id = excluded.thread_id, \
         rfc822_msgid = excluded.rfc822_msgid, from_name = excluded.from_name, \
         from_addr = excluded.from_addr, to_addrs = excluded.to_addrs, cc_addrs = excluded.cc_addrs, \
         subject = excluded.subject, date = excluded.date, snippet = excluded.snippet, \
         size = excluded.size, has_attachments = excluded.has_attachments, \
         list_unsubscribe = excluded.list_unsubscribe, one_click = excluded.one_click, \
         sync_gen = excluded.sync_gen, seen = excluded.seen",
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
            seen,
        ],
    )?;
    replace_memberships(conn, m.account_id, &m.id, &m.held)?;
    // For Gmail the server's id is the store's own.
    conn.prepare_cached(
        "INSERT OR IGNORE INTO remote_refs (account_id, message_id, remote) VALUES (?1, ?2, ?2)",
    )?
    .execute(params![m.account_id, m.id])?;
    Ok(())
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

/// Marks `keyword` on each of `ids` as kept on this computer: the server
/// cannot store it, so no sync sends it back or takes it away.
pub fn mark_local(
    conn: &Connection,
    account_id: AccountId,
    ids: &[String],
    keyword: &str,
) -> Result<()> {
    let mut stmt = conn.prepare_cached(
        "UPDATE message_keywords SET local = 1 \
         WHERE account_id = ?1 AND message_id = ?2 AND keyword = ?3",
    )?;
    for id in ids {
        stmt.execute(params![account_id, id, keyword])?;
    }
    Ok(())
}

/// Replaces what a stored message is in and carries. A keyword marked
/// local stays, since the server never sent it and would not send it back.
fn replace_memberships(
    conn: &Connection,
    account_id: AccountId,
    message_id: &str,
    held: &Memberships,
) -> Result<()> {
    for table in ["message_mailboxes", "message_categories"] {
        conn.prepare_cached(&format!(
            "DELETE FROM {table} WHERE account_id = ?1 AND message_id = ?2"
        ))?
        .execute(params![account_id, message_id])?;
    }
    conn.prepare_cached(
        "DELETE FROM message_keywords WHERE account_id = ?1 AND message_id = ?2 AND local = 0",
    )?
    .execute(params![account_id, message_id])?;
    let mut mailbox = conn.prepare_cached(
        "INSERT OR IGNORE INTO message_mailboxes (account_id, message_id, mailbox) VALUES (?1, ?2, ?3)",
    )?;
    for id in &held.mailboxes {
        mailbox.execute(params![
            account_id,
            message_id,
            mailbox_key(conn, account_id, id)?
        ])?;
    }
    let mut keyword = conn.prepare_cached(
        "INSERT OR IGNORE INTO message_keywords (account_id, message_id, keyword) VALUES (?1, ?2, ?3)",
    )?;
    for k in &held.keywords {
        keyword.execute(params![account_id, message_id, k])?;
    }
    let mut category = conn.prepare_cached(
        "INSERT OR IGNORE INTO message_categories (account_id, message_id, category) VALUES (?1, ?2, ?3)",
    )?;
    for c in &held.categories {
        category.execute(params![account_id, message_id, c])?;
    }
    Ok(())
}

/// The key of the account's mailbox `id`. A mailbox the store meets on a
/// message before any listing named it is made unlisted, named after its
/// id. The server's listing names the mailbox's role and kind; until it
/// does, the mailbox holds mail and lists under its id alone.
pub(crate) fn mailbox_key(conn: &Connection, account_id: AccountId, id: &str) -> Result<i64> {
    let known: Option<i64> = conn
        .prepare_cached("SELECT key FROM mailboxes WHERE account_id = ?1 AND id = ?2")?
        .query_row(params![account_id, id], |row| row.get(0))
        .optional()?;
    if let Some(key) = known {
        return Ok(key);
    }
    Ok(conn
        .prepare_cached(
            "INSERT INTO mailboxes (account_id, id, name, role, kind, named) \
             VALUES (?1, ?2, ?2, NULL, 'label', 0) RETURNING key",
        )?
        .query_row(params![account_id, id], |row| row.get(0))?)
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

/// The size the server reported for a stored message, in bytes. Gmail's
/// is `sizeEstimate`; 0 means it gave none.
pub fn size_of(conn: &Connection, account_id: AccountId, message_id: &str) -> Result<Option<i64>> {
    Ok(conn
        .prepare_cached("SELECT size FROM messages WHERE account_id = ?1 AND id = ?2")?
        .query_row(params![account_id, message_id], |row| row.get(0))
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

/// The ids of the stored messages in `set`.
pub fn held_by(conn: &Connection, account_id: AccountId, set: &MailSet) -> Result<HashSet<String>> {
    match set {
        MailSet::Unseen => ids(
            conn.prepare_cached("SELECT id FROM messages WHERE account_id = ?1 AND seen = 0")?,
            &[&account_id],
        ),
        MailSet::Keyword(k) => ids(
            conn.prepare_cached(
                "SELECT message_id FROM message_keywords WHERE keyword = ?2 AND account_id = ?1",
            )?,
            &[&account_id, k],
        ),
        MailSet::Category(c) => ids(
            conn.prepare_cached(
                "SELECT message_id FROM message_categories WHERE category = ?2 AND account_id = ?1",
            )?,
            &[&account_id, c],
        ),
        MailSet::Role(role) => ids(
            conn.prepare_cached(
                "SELECT l.message_id FROM message_mailboxes l CROSS JOIN mailboxes b ON b.key = l.mailbox \
                 WHERE b.account_id = ?1 AND b.role = ?2",
            )?,
            &[&account_id, &role.as_str()],
        ),
        MailSet::Mailbox(id) => ids(
            conn.prepare_cached(
                "SELECT l.message_id FROM message_mailboxes l CROSS JOIN mailboxes b ON b.key = l.mailbox \
                 WHERE b.account_id = ?1 AND b.id = ?2",
            )?,
            &[&account_id, id],
        ),
    }
}

/// The ids a statement of one text column answers.
fn ids(
    mut stmt: rusqlite::CachedStatement<'_>,
    params: &[&dyn rusqlite::ToSql],
) -> Result<HashSet<String>> {
    Ok(stmt
        .query_map(params, |row| row.get(0))?
        .collect::<rusqlite::Result<HashSet<String>>>()?)
}

/// The subset of `ids` that is stored, from one statement however many
/// ids there are.
pub fn existing_ids(
    conn: &Connection,
    account_id: AccountId,
    ids: &[String],
) -> Result<HashSet<String>> {
    existing_ids_json(conn, account_id, &json_list(ids))
}

fn existing_ids_json(
    conn: &Connection,
    account_id: AccountId,
    ids: &str,
) -> Result<HashSet<String>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id FROM messages WHERE account_id = ?1 AND id IN (SELECT value FROM json_each(?2))",
    )?;
    let rows = stmt.query_map(params![account_id, ids], |row| row.get(0))?;
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
    let held = memberships_by_message(
        conn.prepare_cached(THREAD_MEMBERSHIPS)?
            .query_map(params![account_id, thread_id], membership_row)?,
    )?;
    let roles = roles_by_id(conn, account_id)?;
    rows.into_iter()
        .map(|r| message_meta(account_id, r, &held, &roles))
        .collect()
}

/// Each message's memberships, from `(message id, kind, value)` rows,
/// where kind is `m` for a mailbox, `k` for a keyword and `c` for a
/// category.
fn memberships_by_message(
    rows: impl Iterator<Item = rusqlite::Result<(String, String, String)>>,
) -> Result<HashMap<String, Memberships>> {
    let mut held: HashMap<String, Memberships> = HashMap::new();
    for row in rows {
        let (message_id, kind, value) = row?;
        let entry = held.entry(message_id).or_default();
        match kind.as_str() {
            "m" => entry.mailboxes.push(value),
            "k" => entry.keywords.push(value),
            _ => entry.categories.push(value),
        }
    }
    Ok(held)
}

fn membership_row(row: &rusqlite::Row) -> rusqlite::Result<(String, String, String)> {
    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
}

/// The memberships of a thread's messages. `?1` is the account and `?2`
/// the thread. Each part starts from the thread's few messages.
const THREAD_MEMBERSHIPS: &str = "SELECT m.id, 'm', b.id FROM messages m \
     CROSS JOIN message_mailboxes l ON l.account_id = m.account_id AND l.message_id = m.id \
     CROSS JOIN mailboxes b ON b.key = l.mailbox WHERE m.account_id = ?1 AND m.thread_id = ?2 \
     UNION ALL SELECT m.id, 'k', k.keyword FROM messages m \
     CROSS JOIN message_keywords k ON k.account_id = m.account_id AND k.message_id = m.id \
     WHERE m.account_id = ?1 AND m.thread_id = ?2 \
     UNION ALL SELECT m.id, 'c', c.category FROM messages m \
     CROSS JOIN message_categories c ON c.account_id = m.account_id AND c.message_id = m.id \
     WHERE m.account_id = ?1 AND m.thread_id = ?2";

/// The memberships of the messages named in `?2`, a JSON array of ids,
/// in account `?1`.
const LISTED_MEMBERSHIPS: &str = "SELECT l.message_id, 'm', b.id FROM message_mailboxes l \
     CROSS JOIN mailboxes b ON b.key = l.mailbox \
     WHERE l.account_id = ?1 AND l.message_id IN (SELECT value FROM json_each(?2)) \
     UNION ALL SELECT message_id, 'k', keyword FROM message_keywords \
     WHERE account_id = ?1 AND message_id IN (SELECT value FROM json_each(?2)) \
     UNION ALL SELECT message_id, 'c', category FROM message_categories \
     WHERE account_id = ?1 AND message_id IN (SELECT value FROM json_each(?2))";

/// What each of `ids` is in and carries. A stored message with nothing
/// at all is unread and in no mailbox; an id the store lacks is missing.
pub fn memberships_of(
    conn: &Connection,
    account_id: AccountId,
    ids: &[String],
) -> Result<HashMap<String, Memberships>> {
    let ids = json_list(ids);
    let mut held = memberships_by_message(
        conn.prepare_cached(LISTED_MEMBERSHIPS)?
            .query_map(params![account_id, ids], membership_row)?,
    )?;
    for id in existing_ids_json(conn, account_id, &ids)? {
        held.entry(id).or_default();
    }
    Ok(held)
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

/// The role of each of the account's mailboxes that has one, by id.
fn roles_by_id(conn: &Connection, account_id: AccountId) -> Result<HashMap<String, Role>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, role FROM mailboxes WHERE account_id = ?1 AND role IS NOT NULL",
    )?;
    let rows = stmt.query_map(params![account_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut roles = HashMap::new();
    for row in rows {
        let (id, role) = row?;
        let role = role.parse::<Role>().map_err(|_| StoreError::Corrupt {
            column: "mailboxes.role",
            value: role.clone(),
        })?;
        roles.insert(id, role);
    }
    Ok(roles)
}

/// A message row with what it holds, sorted, and the roles of the
/// mailboxes it sits in.
fn message_meta(
    account_id: AccountId,
    r: MessageRow,
    held: &HashMap<String, Memberships>,
    roles: &HashMap<String, Role>,
) -> Result<MessageMeta> {
    let mut held = held.get(&r.id).cloned().unwrap_or_default();
    held.sort();
    let mut roles: Vec<Role> = held
        .mailboxes
        .iter()
        .filter_map(|m| roles.get(m).copied())
        .collect();
    roles.sort();
    roles.dedup();
    Ok(MessageMeta {
        account_id,
        to: parse_addresses("messages.to_addrs", &r.to)?,
        cc: parse_addresses("messages.cc_addrs", &r.cc)?,
        from: r.from_addr.map(|email| Address {
            name: r.from_name,
            email,
        }),
        held,
        roles,
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
    let held = memberships_by_message(
        conn.prepare_cached(LISTED_MEMBERSHIPS)?
            .query_map(params![account_id, ids], membership_row)?,
    )?;
    let roles = roles_by_id(conn, account_id)?;
    rows.into_iter()
        .map(|r| message_meta(account_id, r, &held, &roles))
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
    // What the thread's messages carry, and whether one of them sits
    // outside the Trash and Spam, which is what keeps a thread in lists
    // of any mail.
    let (unread, starred, muted, listed): (bool, bool, bool, bool) = conn
        .prepare_cached(
            "SELECT MAX(m.seen = 0), \
             MAX(EXISTS (SELECT 1 FROM message_keywords k WHERE k.account_id = m.account_id \
                 AND k.message_id = m.id AND k.keyword = '$flagged')), \
             MAX(EXISTS (SELECT 1 FROM message_keywords k WHERE k.account_id = m.account_id \
                 AND k.message_id = m.id AND k.keyword = '$muted')), \
             MAX(NOT EXISTS (SELECT 1 FROM message_mailboxes h WHERE h.account_id = m.account_id \
                 AND h.message_id = m.id AND h.mailbox IN (SELECT key FROM mailboxes \
                 WHERE account_id = ?1 AND role IN ('trash', 'junk')))) \
             FROM messages m WHERE m.account_id = ?1 AND m.thread_id = ?2",
        )?
        .query_row(params![account_id, thread_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?;
    let flag_color: Option<String> = conn
        .prepare_cached(NEWEST_FLAG_COLOR)?
        .query_row(params![account_id, thread_id], |row| row.get(0))
        .optional()?;
    conn.prepare_cached(
        "INSERT INTO threads (account_id, id, last_message_at, subject, snippet, from_display, message_count, \
         unread, starred, has_attachments, flag_color, from_email, muted, listed) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14) \
         ON CONFLICT (account_id, id) DO UPDATE SET last_message_at = excluded.last_message_at, \
         subject = excluded.subject, snippet = excluded.snippet, from_display = excluded.from_display, \
         message_count = excluded.message_count, unread = excluded.unread, starred = excluded.starred, \
         has_attachments = excluded.has_attachments, flag_color = excluded.flag_color, \
         from_email = excluded.from_email, muted = excluded.muted, listed = excluded.listed",
    )?
    .execute(params![
        account_id,
        thread_id,
        last.unwrap_or(0),
        subject,
        snippet,
        from,
        count,
        unread,
        starred,
        has_attachments.unwrap_or(false),
        flag_color,
        from_email,
        muted,
        listed,
    ])?;
    // One row per mailbox a message of the thread sits in. `listed` is
    // Gmail's rule for that mailbox's list: the thread shows while one of
    // its messages there sits outside the Trash and Spam, not counting the
    // mailbox itself, so the Trash still lists what is in the Trash.
    for table in ["thread_mailboxes", "thread_categories"] {
        conn.prepare_cached(&format!(
            "DELETE FROM {table} WHERE account_id = ?1 AND thread_id = ?2"
        ))?
        .execute(params![account_id, thread_id])?;
    }
    conn.prepare_cached(
        "INSERT INTO thread_mailboxes (account_id, thread_id, mailbox, listed, unread) \
         SELECT ?1, ?2, l.mailbox, \
         MAX(NOT EXISTS (SELECT 1 FROM message_mailboxes h WHERE h.account_id = l.account_id \
             AND h.message_id = l.message_id AND h.mailbox <> l.mailbox \
             AND h.mailbox IN (SELECT key FROM mailboxes WHERE account_id = ?1 AND role IN ('trash', 'junk')))), \
         ?3 \
         FROM messages m CROSS JOIN message_mailboxes l ON l.account_id = m.account_id AND l.message_id = m.id \
         WHERE m.account_id = ?1 AND m.thread_id = ?2 GROUP BY l.mailbox",
    )?
    .execute(params![account_id, thread_id, unread])?;
    conn.prepare_cached(
        "INSERT INTO thread_categories (account_id, thread_id, category, listed, unread) \
         SELECT ?1, ?2, c.category, \
         MAX(NOT EXISTS (SELECT 1 FROM message_mailboxes h WHERE h.account_id = c.account_id \
             AND h.message_id = c.message_id \
             AND h.mailbox IN (SELECT key FROM mailboxes WHERE account_id = ?1 AND role IN ('trash', 'junk')))), \
         ?3 \
         FROM messages m CROSS JOIN message_categories c ON c.account_id = m.account_id AND c.message_id = m.id \
         WHERE m.account_id = ?1 AND m.thread_id = ?2 GROUP BY c.category",
    )?
    .execute(params![account_id, thread_id, unread])?;
    Ok(())
}
