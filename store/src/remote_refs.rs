//! Where each stored message sits on its server now. A Gmail message's
//! ref is its own id. An IMAP message's id is the location where the app
//! first met it, and its ref follows it from mailbox to mailbox, so the
//! engine can hand the server the name it knows the message by today and
//! read the server's answers back into store ids.

use std::collections::HashMap;

use mailrs_domain::{AccountId, Location};
use rusqlite::{Connection, params};

use crate::Result;

/// What a name the server used stands for in the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    /// The stored message the server now calls by that name.
    Stored(String),
    /// A stored message's id from before a move gave the message another
    /// name. Nothing on the server goes by it now, so news naming it is
    /// about the place the message left.
    Stale,
}

fn json(names: &[String]) -> String {
    serde_json::to_string(names).unwrap_or_else(|_| "[]".into())
}

/// Records that `message_id` sits at `at` now. A message the store does
/// not hold gets no ref. Another message's ref that still names `at` is
/// dropped, since the server keeps one message at a location.
pub fn locate(
    conn: &Connection,
    account_id: AccountId,
    message_id: &str,
    at: &Location,
) -> Result<()> {
    let remote = at.to_string();
    conn.prepare_cached(
        "DELETE FROM remote_refs WHERE account_id = ?1 AND remote = ?2 AND message_id <> ?3",
    )?
    .execute(params![account_id, remote, message_id])?;
    conn.prepare_cached(
        "INSERT INTO remote_refs (account_id, message_id, remote, mailbox, uidvalidity, uid) \
         SELECT ?1, ?2, ?3, ?4, ?5, ?6 \
         WHERE EXISTS (SELECT 1 FROM messages WHERE account_id = ?1 AND id = ?2) \
         ON CONFLICT (account_id, message_id) DO UPDATE SET remote = excluded.remote, \
         mailbox = excluded.mailbox, uidvalidity = excluded.uidvalidity, uid = excluded.uid",
    )?
    .execute(params![
        account_id,
        message_id,
        remote,
        at.mailbox,
        at.uidvalidity,
        at.uid
    ])?;
    Ok(())
}

/// What each of `names` stands for. A name no ref holds and no stored
/// message goes by is left out of the answer: it names a message the
/// store has not met.
pub fn resolve(
    conn: &Connection,
    account_id: AccountId,
    names: &[String],
) -> Result<HashMap<String, Resolved>> {
    let list = json(names);
    let mut found = HashMap::new();
    let mut by_remote = conn.prepare_cached(
        "SELECT remote, message_id FROM remote_refs \
         WHERE account_id = ?1 AND remote IN (SELECT value FROM json_each(?2))",
    )?;
    let rows = by_remote.query_map(params![account_id, list], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (remote, id) = row?;
        found.insert(remote, Resolved::Stored(id));
    }
    // A stored id no ref names any more belongs to a message that moved.
    let mut by_id = conn.prepare_cached(
        "SELECT message_id FROM remote_refs \
         WHERE account_id = ?1 AND message_id IN (SELECT value FROM json_each(?2))",
    )?;
    let rows = by_id.query_map(params![account_id, list], |row| row.get::<_, String>(0))?;
    for row in rows {
        found.entry(row?).or_insert(Resolved::Stale);
    }
    Ok(found)
}

/// The name the server knows each of `ids` by now, for the ids that
/// have a ref.
pub fn remotes_of(
    conn: &Connection,
    account_id: AccountId,
    ids: &[String],
) -> Result<HashMap<String, String>> {
    let mut stmt = conn.prepare_cached(
        "SELECT message_id, remote FROM remote_refs \
         WHERE account_id = ?1 AND message_id IN (SELECT value FROM json_each(?2))",
    )?;
    let rows = stmt.query_map(params![account_id, json(ids)], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The stored messages located in `mailbox`, each with its name there.
pub fn in_mailbox(
    conn: &Connection,
    account_id: AccountId,
    mailbox: &str,
) -> Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare_cached(
        "SELECT message_id, remote FROM remote_refs \
         WHERE account_id = ?1 AND mailbox = ?2 ORDER BY message_id",
    )?;
    let rows = stmt.query_map(params![account_id, mailbox], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use mailrs_domain::{AccountId, Location, Memberships, MessageMeta};
    use rusqlite::Connection;

    use super::{Resolved, in_mailbox, locate, remotes_of, resolve};
    use crate::accounts;
    use crate::messages::{self, Change};

    fn meta(account_id: AccountId, id: &str) -> MessageMeta {
        MessageMeta {
            account_id,
            id: id.into(),
            thread_id: id.into(),
            rfc822_msgid: None,
            from: None,
            to: vec![],
            cc: vec![],
            subject: String::new(),
            date: 0,
            snippet: String::new(),
            size: 0,
            has_attachments: false,
            held: Memberships::default(),
            roles: vec![],
            list_unsubscribe: None,
            one_click: false,
        }
    }

    /// A store holding a message under each of `ids`.
    fn store(ids: &[&str]) -> (Connection, AccountId) {
        let conn = crate::open_in_memory().unwrap();
        let account_id = accounts::insert_account(&conn, "me@example.com", 0).unwrap();
        let changes: Vec<Change> = ids
            .iter()
            .map(|id| Change::Upsert {
                meta: Box::new(meta(account_id, id)),
                generation: 1,
            })
            .collect();
        messages::apply(&conn, account_id, &changes).unwrap();
        (conn, account_id)
    }

    fn at(mailbox: &str, uidvalidity: u32, uid: u32) -> Location {
        Location {
            mailbox: mailbox.into(),
            uidvalidity,
            uid,
        }
    }

    fn names(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    #[test]
    fn a_message_met_in_the_inbox_goes_by_its_first_name() {
        let (conn, account_id) = store(&["INBOX/7/42"]);
        locate(&conn, account_id, "INBOX/7/42", &at("INBOX", 7, 42)).unwrap();
        assert_eq!(
            resolve(&conn, account_id, &names(&["INBOX/7/42"])).unwrap(),
            HashMap::from([(
                "INBOX/7/42".to_string(),
                Resolved::Stored("INBOX/7/42".into())
            )])
        );
        assert_eq!(
            in_mailbox(&conn, account_id, "INBOX").unwrap(),
            [("INBOX/7/42".to_string(), "INBOX/7/42".to_string())]
        );
    }

    #[test]
    fn after_a_move_the_new_name_finds_the_message_and_the_old_one_is_stale() {
        let (conn, account_id) = store(&["INBOX/7/42"]);
        locate(&conn, account_id, "INBOX/7/42", &at("INBOX", 7, 42)).unwrap();
        locate(&conn, account_id, "INBOX/7/42", &at("Archive", 3, 10)).unwrap();
        assert_eq!(
            resolve(
                &conn,
                account_id,
                &names(&["Archive/3/10", "INBOX/7/42", "INBOX/7/43"])
            )
            .unwrap(),
            HashMap::from([
                (
                    "Archive/3/10".to_string(),
                    Resolved::Stored("INBOX/7/42".into())
                ),
                ("INBOX/7/42".to_string(), Resolved::Stale),
            ])
        );
        assert_eq!(
            remotes_of(&conn, account_id, &names(&["INBOX/7/42"])).unwrap(),
            HashMap::from([("INBOX/7/42".to_string(), "Archive/3/10".to_string())])
        );
        assert!(in_mailbox(&conn, account_id, "INBOX").unwrap().is_empty());
        assert_eq!(
            in_mailbox(&conn, account_id, "Archive").unwrap(),
            [("INBOX/7/42".to_string(), "Archive/3/10".to_string())]
        );
    }

    #[test]
    fn a_message_the_store_lacks_gets_no_ref() {
        let (conn, account_id) = store(&[]);
        locate(&conn, account_id, "INBOX/1/1", &at("INBOX", 1, 1)).unwrap();
        assert!(
            resolve(&conn, account_id, &names(&["INBOX/1/1"]))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn a_gmail_id_names_its_own_message() {
        let (conn, account_id) = store(&["18c2a4f0"]);
        assert_eq!(
            resolve(&conn, account_id, &names(&["18c2a4f0"])).unwrap(),
            HashMap::from([("18c2a4f0".to_string(), Resolved::Stored("18c2a4f0".into()))])
        );
    }
}
