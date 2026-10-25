//! Switching an IMAP account to JMAP in place. The account keeps its row,
//! so its address, and with it every setting the app keys by address,
//! stays. Every message the JMAP server holds takes its JMAP id and
//! thread; what the store kept beside a message or a thread follows the
//! new ids; a message the server no longer holds goes; the mailboxes take
//! their JMAP ids and the local rules with them. One transaction, so a
//! failure leaves the IMAP account as it was.

use std::collections::{BTreeSet, HashMap};

use mailrs_domain::{AccountId, MailSet};
use rusqlite::{Connection, OptionalExtension, params};

use crate::jmap::{self, JmapSaved};
use crate::{Result, StoreError, local_rules};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredMessage {
    pub id: String,
    pub message_id: Option<String>,
    /// When it arrived, or 0 when the server did not say.
    pub date: i64,
    pub size: i64,
    pub subject: String,
    pub from: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredMailbox {
    pub id: String,
    pub name: String,
    pub role: Option<String>,
}

/// A stored message's id now, its JMAP id and its JMAP thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Moved {
    pub from: String,
    pub to: String,
    pub thread: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Adoption {
    pub messages: Vec<Moved>,
    /// An IMAP mailbox's id to its JMAP id.
    pub mailboxes: Vec<(String, String)>,
    pub saved: JmapSaved,
    /// The session serves contacts, so the CardDAV copy goes and JMAP
    /// fills the address book again.
    pub contacts_from_jmap: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Adopted {
    pub kept: usize,
    pub dropped: usize,
    /// Local rules turned off because they name a mailbox the server lacks.
    pub rules_off: usize,
}

/// Every table that names a message by `(account_id, message_id)`.
const MESSAGE_TABLES: [&str; 12] = [
    "attachments",
    "bodies",
    "drafts",
    "flags",
    "invitations",
    "local_messages",
    "local_rules_ran",
    "message_categories",
    "message_keywords",
    "message_links",
    "message_mailboxes",
    "receipts",
];

/// The tables in `MESSAGE_TABLES` whose rows no foreign key removes with
/// their message, so the swap removes a dropped message's rows itself.
const LOOSE_MESSAGE_TABLES: [&str; 4] = ["drafts", "invitations", "local_rules_ran", "receipts"];

/// Every table that names a thread and holds something the person did.
const THREAD_TABLES: [&str; 2] = ["reminders", "follow_up_dismissals"];

pub fn stored_messages(conn: &Connection, account_id: AccountId) -> Result<Vec<StoredMessage>> {
    let mut stmt = conn.prepare("SELECT id, rfc822_msgid, date, size, subject, from_addr FROM messages WHERE account_id = ?1")?;
    let rows = stmt.query_map(params![account_id], |row| {
        Ok(StoredMessage {
            id: row.get(0)?,
            message_id: row.get(1)?,
            date: row.get(2)?,
            size: row.get(3)?,
            subject: row.get(4)?,
            from: row.get(5)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn stored_mailboxes(conn: &Connection, account_id: AccountId) -> Result<Vec<StoredMailbox>> {
    let mut stmt =
        conn.prepare("SELECT id, name, role FROM mailboxes WHERE account_id = ?1 AND named = 1")?;
    let rows = stmt.query_map(params![account_id], |row| {
        Ok(StoredMailbox {
            id: row.get(0)?,
            name: row.get(1)?,
            role: row.get(2)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Rewrites the account as a JMAP account. Runs inside the caller's
/// transaction; an error leaves the transaction to roll back. None, with
/// nothing changed, when the stored account is no longer IMAP.
pub fn adopt_jmap(
    conn: &Connection,
    account_id: AccountId,
    adoption: &Adoption,
) -> Result<Option<Adopted>> {
    // A second click, or a retry after the first switch went through,
    // would find none of its IMAP ids and drop every message.
    let provider: Option<String> = conn
        .query_row(
            "SELECT provider FROM accounts WHERE id = ?1",
            params![account_id],
            |row| row.get(0),
        )
        .optional()?;
    if provider.as_deref() != Some("imap") {
        return Ok(None);
    }
    let targets: BTreeSet<&str> = adoption.messages.iter().map(|m| m.to.as_str()).collect();
    if targets.len() != adoption.messages.len() {
        return Err(StoreError::Corrupt {
            column: "switch.messages",
            value: "two messages take one JMAP id".into(),
        });
    }
    conn.execute_batch(
        "PRAGMA defer_foreign_keys = ON;
         CREATE TEMP TABLE IF NOT EXISTS switch_messages (old TEXT PRIMARY KEY, new TEXT NOT NULL, thread TEXT NOT NULL);
         CREATE TEMP TABLE IF NOT EXISTS switch_threads (old TEXT PRIMARY KEY, new TEXT NOT NULL);
         CREATE TEMP TABLE IF NOT EXISTS switch_mailboxes (old TEXT PRIMARY KEY, new TEXT NOT NULL);
         CREATE TEMP TABLE IF NOT EXISTS switch_dropped (id TEXT PRIMARY KEY);
         CREATE TEMP TABLE IF NOT EXISTS switch_reminders (thread_id TEXT PRIMARY KEY, subject TEXT NOT NULL, remind_at INTEGER NOT NULL);
         DELETE FROM switch_messages; DELETE FROM switch_threads; DELETE FROM switch_mailboxes;
         DELETE FROM switch_dropped; DELETE FROM switch_reminders;",
    )?;
    {
        let mut insert =
            conn.prepare("INSERT INTO switch_messages (old, new, thread) VALUES (?1, ?2, ?3)")?;
        for moved in &adoption.messages {
            insert.execute(params![moved.from, moved.to, moved.thread])?;
        }
        let mut insert = conn.prepare("INSERT INTO switch_mailboxes (old, new) VALUES (?1, ?2)")?;
        for (old, new) in &adoption.mailboxes {
            insert.execute(params![old, new])?;
        }
    }
    // Each old thread follows the newest of its messages the server kept;
    // rows come oldest first, so the newest writes last.
    conn.execute(
        "INSERT INTO switch_threads (old, new) \
         SELECT m.thread_id, s.thread FROM messages m JOIN switch_messages s ON s.old = m.id \
         WHERE m.account_id = ?1 ORDER BY m.date ASC \
         ON CONFLICT (old) DO UPDATE SET new = excluded.new",
        params![account_id],
    )?;
    conn.execute(
        "INSERT INTO switch_dropped (id) SELECT id FROM messages WHERE account_id = ?1 AND id NOT IN (SELECT old FROM switch_messages)",
        params![account_id],
    )?;
    let dropped = conn.execute(
        "DELETE FROM messages WHERE account_id = ?1 AND id IN (SELECT id FROM switch_dropped)",
        params![account_id],
    )?;
    for table in LOOSE_MESSAGE_TABLES {
        conn.execute(
            &format!("DELETE FROM {table} WHERE account_id = ?1 AND message_id IN (SELECT id FROM switch_dropped)"),
            params![account_id],
        )?;
    }
    // A queued send keeps its bytes and still goes; it lets go of a draft
    // the server no longer holds.
    conn.execute(
        "UPDATE outbox SET draft_id = NULL WHERE account_id = ?1 AND draft_id IN (SELECT id FROM switch_dropped)",
        params![account_id],
    )?;
    for table in MESSAGE_TABLES {
        conn.execute(
            &format!(
                "UPDATE {table} SET message_id = (SELECT new FROM switch_messages WHERE old = {table}.message_id) \
                 WHERE account_id = ?1 AND message_id IN (SELECT old FROM switch_messages)"
            ),
            params![account_id],
        )?;
    }
    conn.execute(
        "UPDATE outbox SET message_id = (SELECT new FROM switch_messages WHERE old = outbox.message_id) \
         WHERE account_id = ?1 AND message_id IN (SELECT old FROM switch_messages)",
        params![account_id],
    )?;
    // A queued send keeps pointing at its draft, now by the Email id.
    conn.execute(
        "UPDATE outbox SET draft_id = (SELECT new FROM switch_messages WHERE old = outbox.draft_id) \
         WHERE account_id = ?1 AND draft_id IN (SELECT old FROM switch_messages)",
        params![account_id],
    )?;
    conn.execute(
        "UPDATE outbox SET thread_id = (SELECT new FROM switch_threads WHERE old = outbox.thread_id) \
         WHERE account_id = ?1 AND thread_id IN (SELECT old FROM switch_threads)",
        params![account_id],
    )?;
    let kept = conn.execute(
        "UPDATE messages SET id = (SELECT new FROM switch_messages WHERE old = messages.id), \
         thread_id = (SELECT thread FROM switch_messages WHERE old = messages.id) \
         WHERE account_id = ?1 AND id IN (SELECT old FROM switch_messages)",
        params![account_id],
    )?;
    // A JMAP draft goes by its Email id.
    conn.execute(
        "UPDATE drafts SET draft_id = message_id WHERE account_id = ?1",
        params![account_id],
    )?;
    conn.execute(
        "DELETE FROM remote_refs WHERE account_id = ?1",
        params![account_id],
    )?;
    // Two old threads may become one JMAP thread. When both had a
    // reminder, the earlier one stays, so the person is not reminded late.
    // SQLite takes the bare column `subject` from the row MIN picked.
    conn.execute(
        "INSERT INTO switch_reminders (thread_id, subject, remind_at) \
         SELECT s.new, r.subject, MIN(r.remind_at) FROM reminders r JOIN switch_threads s ON s.old = r.thread_id \
         WHERE r.account_id = ?1 GROUP BY s.new",
        params![account_id],
    )?;
    conn.execute(
        "DELETE FROM reminders WHERE account_id = ?1 AND thread_id IN (SELECT old FROM switch_threads)",
        params![account_id],
    )?;
    conn.execute(
        "INSERT INTO reminders (account_id, thread_id, subject, remind_at) \
         SELECT ?1, thread_id, subject, remind_at FROM switch_reminders WHERE true \
         ON CONFLICT (account_id, thread_id) DO UPDATE SET remind_at = MIN(remind_at, excluded.remind_at)",
        params![account_id],
    )?;
    for table in THREAD_TABLES {
        conn.execute(
            &format!(
                "UPDATE OR IGNORE {table} SET thread_id = (SELECT new FROM switch_threads WHERE old = {table}.thread_id) \
                 WHERE account_id = ?1 AND thread_id IN (SELECT old FROM switch_threads)"
            ),
            params![account_id],
        )?;
        // What stayed under an old id lost every message to the switch, or
        // collided with a thread that already had one.
        conn.execute(
            &format!("DELETE FROM {table} WHERE account_id = ?1 AND thread_id NOT IN (SELECT thread_id FROM messages WHERE account_id = ?1)"),
            params![account_id],
        )?;
    }
    // Mailboxes: the parent names follow before the ids change.
    conn.execute(
        "UPDATE mailboxes SET parent = (SELECT new FROM switch_mailboxes WHERE old = mailboxes.parent) \
         WHERE account_id = ?1 AND parent IN (SELECT old FROM switch_mailboxes)",
        params![account_id],
    )?;
    conn.execute(
        "UPDATE mailboxes SET id = (SELECT new FROM switch_mailboxes WHERE old = mailboxes.id) \
         WHERE account_id = ?1 AND id IN (SELECT old FROM switch_mailboxes)",
        params![account_id],
    )?;
    conn.execute(
        "DELETE FROM mailboxes WHERE account_id = ?1 AND id NOT IN (SELECT new FROM switch_mailboxes)",
        params![account_id],
    )?;
    let renamed: HashMap<&str, &str> = adoption
        .mailboxes
        .iter()
        .map(|(o, n)| (o.as_str(), n.as_str()))
        .collect();
    let mut rules_off = 0;
    for mut rule in local_rules::list(conn, account_id)? {
        // A rule naming a mailbox the server lacks would fail on every
        // message, or take mail out of the Inbox and file it nowhere. It
        // stays as the person wrote it, turned off.
        let lacks = |set: &MailSet| matches!(set, MailSet::Mailbox(id) if !renamed.contains_key(id.as_str()));
        if rule.action.add.iter().chain(&rule.action.remove).any(lacks) {
            if !rule.off
                && let Some(id) = rule.id.clone()
            {
                rule.off = true;
                rules_off += 1;
                local_rules::replace(conn, account_id, &id, &rule)?;
            }
            continue;
        }
        let mut changed = false;
        for set in rule
            .action
            .add
            .iter_mut()
            .chain(rule.action.remove.iter_mut())
        {
            if let MailSet::Mailbox(id) = set
                && let Some(new) = renamed.get(id.as_str())
            {
                *id = new.to_string();
                changed = true;
            }
        }
        if changed && let Some(id) = rule.id.clone() {
            local_rules::replace(conn, account_id, &id, &rule)?;
        }
    }
    for table in ["thread_mailboxes", "thread_categories", "threads"] {
        conn.execute(
            &format!("DELETE FROM {table} WHERE account_id = ?1"),
            params![account_id],
        )?;
    }
    let threads: Vec<String> = conn
        .prepare("SELECT DISTINCT thread_id FROM messages WHERE account_id = ?1")?
        .query_map(params![account_id], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for thread in &threads {
        crate::messages::refresh_thread(conn, account_id, thread)?;
    }
    if adoption.contacts_from_jmap {
        for table in ["contact_addresses", "contacts", "contact_books"] {
            conn.execute(
                &format!("DELETE FROM {table} WHERE account_id = ?1"),
                params![account_id],
            )?;
        }
        conn.execute(
            "DELETE FROM account_services WHERE account_id = ?1 AND service = 'carddav'",
            params![account_id],
        )?;
    }
    conn.execute(
        "UPDATE accounts SET provider = 'jmap', sync_state = NULL, backfill_cursor = NULL, backfill_done = 0, \
         backfill_extends = 0 WHERE id = ?1",
        params![account_id],
    )?;
    conn.execute(
        "DELETE FROM account_servers WHERE account_id = ?1",
        params![account_id],
    )?;
    jmap::save(conn, account_id, &adoption.saved)?;
    conn.execute_batch(
        "DELETE FROM switch_messages; DELETE FROM switch_threads; DELETE FROM switch_mailboxes; \
         DELETE FROM switch_dropped; DELETE FROM switch_reminders;",
    )?;
    Ok(Some(Adopted {
        kept,
        dropped,
        rules_off,
    }))
}
