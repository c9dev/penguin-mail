//! Where each stored message sits on its server now. A Gmail message's
//! ref is its own id. An IMAP message's id is the location where the app
//! first met it, and its ref follows it from mailbox to mailbox, so the
//! engine can hand the server the name it knows the message by today and
//! read the server's answers back into store ids.

use std::collections::HashMap;
use std::ops::RangeInclusive;

use mailrs_domain::{AccountId, EpochMillis, Location};
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

/// The stored messages located in `mailbox` that `gone` picks, given each
/// one's UIDVALIDITY and UID there. The rows go through `gone` one at a
/// time and only the picked ids are kept, so a look at a mailbox of any
/// size holds what it deletes and nothing else.
pub fn in_mailbox_where(
    conn: &Connection,
    account_id: AccountId,
    mailbox: &str,
    mut gone: impl FnMut(u32, u32) -> bool,
) -> Result<Vec<String>> {
    let mut stmt = conn.prepare_cached(
        "SELECT uidvalidity, uid, message_id FROM remote_refs \
         WHERE account_id = ?1 AND mailbox = ?2 \
         AND uidvalidity IS NOT NULL AND uid IS NOT NULL ORDER BY message_id",
    )?;
    let mut rows = stmt.query(params![account_id, mailbox])?;
    let mut picked = Vec::new();
    while let Some(row) = rows.next()? {
        if gone(row.get(0)?, row.get(1)?) {
            picked.push(row.get(2)?);
        }
    }
    Ok(picked)
}

/// Hands `each` every stored message located in `mailbox` under
/// `uidvalidity` with a UID in `uids`, lowest UID first, as its UID, its
/// id and the keywords it carries, sorted. The rows are read one message
/// at a time, so a caller comparing a window of the server's flags holds
/// that window and nothing the store adds.
pub fn keywords_in(
    conn: &Connection,
    account_id: AccountId,
    mailbox: &str,
    uidvalidity: u32,
    uids: RangeInclusive<u32>,
    mut each: impl FnMut(u32, &str, &[String]),
) -> Result<()> {
    let mut stmt = conn.prepare_cached(
        "SELECT r.uid, r.message_id, k.keyword FROM remote_refs r \
         LEFT JOIN message_keywords k \
         ON k.account_id = r.account_id AND k.message_id = r.message_id \
         WHERE r.account_id = ?1 AND r.mailbox = ?2 AND r.uidvalidity = ?3 \
         AND r.uid BETWEEN ?4 AND ?5 ORDER BY r.uid, r.message_id, k.keyword",
    )?;
    let mut rows = stmt.query(params![
        account_id,
        mailbox,
        uidvalidity,
        uids.start(),
        uids.end()
    ])?;
    let mut current: Option<(u32, String)> = None;
    let mut keywords: Vec<String> = Vec::new();
    while let Some(row) = rows.next()? {
        let (uid, id): (u32, String) = (row.get(0)?, row.get(1)?);
        if current.as_ref().is_some_and(|(_, at)| *at != id) {
            if let Some((uid, id)) = current.take() {
                each(uid, &id, &keywords);
            }
            keywords.clear();
        }
        if let Some(keyword) = row.get::<_, Option<String>>(2)? {
            keywords.push(keyword);
        }
        if current.is_none() {
            current = Some((uid, id));
        }
    }
    if let Some((uid, id)) = current {
        each(uid, &id, &keywords);
    }
    Ok(())
}

/// Renames the mailbox `from` to `to` in every ref located in it, each
/// ref's text with it. A server that renames a mailbox keeps its UIDs, so
/// each message keeps its UIDVALIDITY and UID. A mailbox nested under
/// `from` keeps its own name here; the caller renames it on its own.
pub fn rename_mailbox(
    conn: &Connection,
    account_id: AccountId,
    from: &str,
    to: &str,
) -> Result<()> {
    // The text is `mailrs_domain::Location`'s: mailbox, UIDVALIDITY and
    // UID, joined by slashes.
    conn.prepare_cached(
        "UPDATE OR REPLACE remote_refs SET mailbox = ?3, \
         remote = ?3 || '/' || uidvalidity || '/' || uid \
         WHERE account_id = ?1 AND mailbox = ?2 \
         AND uidvalidity IS NOT NULL AND uid IS NOT NULL",
    )?
    .execute(params![account_id, from, to])?;
    Ok(())
}

/// The stored messages located in `mailbox` under a UIDVALIDITY other
/// than `uidvalidity` whose Message-ID is one of `message_ids`, each as
/// its Message-ID, date and id. After the server renumbers a mailbox, a
/// relisting matches what it lists against these; a message it matched
/// already sits under `uidvalidity` and is left out.
pub fn renumbered_by_message_id(
    conn: &Connection,
    account_id: AccountId,
    mailbox: &str,
    uidvalidity: u32,
    message_ids: &[String],
) -> Result<Vec<(String, EpochMillis, String)>> {
    let mut stmt = conn.prepare_cached(
        "SELECT m.rfc822_msgid, m.date, m.id FROM remote_refs r \
         JOIN messages m ON m.account_id = r.account_id AND m.id = r.message_id \
         WHERE r.account_id = ?1 AND r.mailbox = ?2 AND r.uidvalidity <> ?3 \
         AND m.rfc822_msgid IN (SELECT value FROM json_each(?4))",
    )?;
    let rows = stmt.query_map(
        params![account_id, mailbox, uidvalidity, json(message_ids)],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use mailrs_domain::{AccountId, Location, Memberships, MessageMeta};
    use rusqlite::Connection;

    use super::{
        Resolved, in_mailbox_where, keywords_in, locate, remotes_of, rename_mailbox,
        renumbered_by_message_id, resolve,
    };
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
            in_mailbox_where(&conn, account_id, "INBOX", |_, _| true).unwrap(),
            ["INBOX/7/42"]
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
        assert!(
            in_mailbox_where(&conn, account_id, "INBOX", |_, _| true)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            in_mailbox_where(&conn, account_id, "Archive", |_, _| true).unwrap(),
            ["INBOX/7/42"]
        );
    }

    #[test]
    fn a_look_in_a_mailbox_picks_by_uidvalidity_and_uid() {
        let (conn, account_id) = store(&["INBOX/7/1", "INBOX/7/2", "INBOX/6/3"]);
        locate(&conn, account_id, "INBOX/7/1", &at("INBOX", 7, 1)).unwrap();
        locate(&conn, account_id, "INBOX/7/2", &at("INBOX", 7, 2)).unwrap();
        locate(&conn, account_id, "INBOX/6/3", &at("INBOX", 6, 3)).unwrap();
        assert_eq!(
            in_mailbox_where(&conn, account_id, "INBOX", |uidvalidity, uid| {
                uidvalidity != 7 || uid == 2
            })
            .unwrap(),
            ["INBOX/6/3", "INBOX/7/2"]
        );
    }

    #[test]
    fn a_renumbered_mailbox_offers_the_messages_located_under_its_old_uidvalidity() {
        let (conn, account_id) = store(&[]);
        let with_msgid = |id: &str, msgid: &str, date: i64| {
            let mut meta = meta(account_id, id);
            meta.rfc822_msgid = Some(msgid.into());
            meta.date = date;
            Change::Upsert {
                meta: Box::new(meta),
                generation: 1,
            }
        };
        messages::apply(
            &conn,
            account_id,
            &[
                with_msgid("INBOX/7/1", "<a@x>", 10),
                with_msgid("INBOX/7/2", "<b@x>", 20),
                with_msgid("INBOX/7/3", "<a@x>", 30),
                with_msgid("Sent/3/1", "<a@x>", 10),
            ],
        )
        .unwrap();
        locate(&conn, account_id, "INBOX/7/1", &at("INBOX", 7, 1)).unwrap();
        locate(&conn, account_id, "INBOX/7/2", &at("INBOX", 7, 2)).unwrap();
        // Already matched by an earlier relisting under UIDVALIDITY 8.
        locate(&conn, account_id, "INBOX/7/3", &at("INBOX", 8, 2)).unwrap();
        locate(&conn, account_id, "Sent/3/1", &at("Sent", 3, 1)).unwrap();

        let mut held = renumbered_by_message_id(
            &conn,
            account_id,
            "INBOX",
            8,
            &names(&["<a@x>", "<c@x>"]),
        )
        .unwrap();
        held.sort();

        assert_eq!(held, [("<a@x>".to_string(), 10, "INBOX/7/1".to_string())]);
    }

    #[test]
    fn renaming_a_mailbox_renames_the_refs_located_in_it() {
        let (conn, account_id) = store(&["INBOX/7/1", "Projects/3/1", "Projects/2026/4/1"]);
        locate(&conn, account_id, "INBOX/7/1", &at("Projects", 3, 2)).unwrap();
        locate(&conn, account_id, "Projects/3/1", &at("Projects", 3, 1)).unwrap();
        locate(
            &conn,
            account_id,
            "Projects/2026/4/1",
            &at("Projects/2026", 4, 1),
        )
        .unwrap();

        rename_mailbox(&conn, account_id, "Projects", "Work").unwrap();

        assert_eq!(
            remotes_of(
                &conn,
                account_id,
                &names(&["INBOX/7/1", "Projects/3/1", "Projects/2026/4/1"])
            )
            .unwrap(),
            HashMap::from([
                ("INBOX/7/1".to_string(), "Work/3/2".to_string()),
                ("Projects/3/1".to_string(), "Work/3/1".to_string()),
                (
                    "Projects/2026/4/1".to_string(),
                    "Projects/2026/4/1".to_string()
                ),
            ])
        );
        assert_eq!(
            in_mailbox_where(&conn, account_id, "Work", |_, _| true).unwrap(),
            ["INBOX/7/1", "Projects/3/1"]
        );
    }

    #[test]
    fn the_keywords_of_a_range_of_uids_come_a_message_at_a_time_in_uid_order() {
        let (conn, account_id) = store(&[]);
        let carrying = |id: &str, keywords: &[&str]| {
            let mut meta = meta(account_id, id);
            meta.held.keywords = keywords.iter().map(|k| k.to_string()).collect();
            Change::Upsert {
                meta: Box::new(meta),
                generation: 1,
            }
        };
        messages::apply(
            &conn,
            account_id,
            &[
                carrying("INBOX/7/1", &["$seen", "$flagged"]),
                carrying("INBOX/7/2", &[]),
                carrying("INBOX/7/9", &["$seen"]),
                carrying("INBOX/6/2", &["$seen"]),
                carrying("Sent/7/1", &["$seen"]),
            ],
        )
        .unwrap();
        locate(&conn, account_id, "INBOX/7/1", &at("INBOX", 7, 1)).unwrap();
        locate(&conn, account_id, "INBOX/7/2", &at("INBOX", 7, 2)).unwrap();
        locate(&conn, account_id, "INBOX/7/9", &at("INBOX", 7, 60_000)).unwrap();
        locate(&conn, account_id, "INBOX/6/2", &at("INBOX", 6, 2)).unwrap();
        locate(&conn, account_id, "Sent/7/1", &at("Sent", 7, 1)).unwrap();

        let mut seen = Vec::new();
        keywords_in(&conn, account_id, "INBOX", 7, 1..=50_000, |uid, id, keywords| {
            seen.push((uid, id.to_string(), keywords.to_vec()));
        })
        .unwrap();

        assert_eq!(
            seen,
            [
                (
                    1,
                    "INBOX/7/1".to_string(),
                    vec!["$flagged".to_string(), "$seen".to_string()]
                ),
                (2, "INBOX/7/2".to_string(), vec![]),
            ]
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
