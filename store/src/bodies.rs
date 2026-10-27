//! Message bodies, cached separately from metadata and evicted least recently read first.

use mailrs_domain::{AccountId, Attachment, EpochMillis, MessageBody, Protection};
use rusqlite::{Connection, OptionalExtension, params};

use crate::Result;

/// Stores a body and its attachment list. The message must already be stored.
pub fn put_body(
    conn: &Connection,
    account_id: AccountId,
    message_id: &str,
    body: &MessageBody,
    now: EpochMillis,
) -> Result<()> {
    put_body_at(conn, account_id, message_id, body, now, now)
}

/// Stores a body fetched ahead of a read. It starts behind every body a
/// person has read when the cache needs room.
pub fn put_prefetched_body(
    conn: &Connection,
    account_id: AccountId,
    message_id: &str,
    body: &MessageBody,
    now: EpochMillis,
) -> Result<()> {
    put_body_at(conn, account_id, message_id, body, now, 0)
}

fn put_body_at(
    conn: &Connection,
    account_id: AccountId,
    message_id: &str,
    body: &MessageBody,
    fetched_at: EpochMillis,
    accessed_at: EpochMillis,
) -> Result<()> {
    let size =
        body.html.as_ref().map_or(0, String::len) + body.text.as_ref().map_or(0, String::len);
    conn.execute(
        "INSERT INTO bodies (account_id, message_id, html, text, size, fetched_at, accessed_at, \
         list_unsubscribe, one_click_unsubscribe, calendar, protection, provenance, receipt_to, read_report) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14) \
         ON CONFLICT (account_id, message_id) DO UPDATE SET html = excluded.html, text = excluded.text, \
         size = excluded.size, fetched_at = excluded.fetched_at, \
         accessed_at = MAX(bodies.accessed_at, excluded.accessed_at), \
         list_unsubscribe = excluded.list_unsubscribe, \
         one_click_unsubscribe = excluded.one_click_unsubscribe, calendar = excluded.calendar, \
         protection = excluded.protection, provenance = excluded.provenance, \
         receipt_to = excluded.receipt_to, read_report = excluded.read_report",
        params![
            account_id,
            message_id,
            body.html,
            body.text,
            size as i64,
            fetched_at,
            accessed_at,
            body.list_unsubscribe,
            body.one_click_unsubscribe,
            body.calendar,
            body.protection.map(Protection::as_str),
            // Three short answers read and shown together, so they travel
            // as one JSON column rather than three of their own.
            (!body.provenance.is_empty())
                .then(|| serde_json::to_string(&body.provenance).ok())
                .flatten(),
            body.receipt_to,
            body.read_report.as_ref().and_then(|r| serde_json::to_string(r).ok())
        ],
    )?;
    conn.execute(
        "DELETE FROM attachments WHERE account_id = ?1 AND message_id = ?2",
        params![account_id, message_id],
    )?;
    let mut insert = conn.prepare_cached(
        "INSERT INTO attachments (account_id, message_id, part_id, filename, mime_type, size, attachment_id, \
         content_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )?;
    for a in &body.attachments {
        insert.execute(params![
            account_id,
            message_id,
            a.part_id,
            a.filename,
            a.mime_type,
            a.size,
            a.attachment_id,
            a.content_id
        ])?;
    }
    Ok(())
}

/// Returns a cached body and marks it as read at `now`.
pub fn get_body(
    conn: &Connection,
    account_id: AccountId,
    message_id: &str,
    now: EpochMillis,
) -> Result<Option<MessageBody>> {
    let body = peek_body(conn, account_id, message_id)?;
    if body.is_some() {
        conn.execute(
            "UPDATE bodies SET accessed_at = ?3 WHERE account_id = ?1 AND message_id = ?2",
            params![account_id, message_id, now],
        )?;
    }
    Ok(body)
}

/// Returns a cached body without recording a read, for readers that
/// cannot write.
pub fn peek_body(
    conn: &Connection,
    account_id: AccountId,
    message_id: &str,
) -> Result<Option<MessageBody>> {
    type BodyRow = (
        Option<String>,
        Option<String>,
        Option<String>,
        bool,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    );
    let row: Option<BodyRow> = conn
        .query_row(
            "SELECT html, text, list_unsubscribe, one_click_unsubscribe, calendar, protection, \
             provenance, receipt_to, read_report FROM bodies WHERE account_id = ?1 AND message_id = ?2",
            params![account_id, message_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                ))
            },
        )
        .optional()?;
    let Some((
        html,
        text,
        list_unsubscribe,
        one_click_unsubscribe,
        calendar,
        protection,
        provenance,
        receipt_to,
        read_report,
    )) = row
    else {
        return Ok(None);
    };
    let mut stmt = conn.prepare_cached(
        "SELECT part_id, filename, mime_type, size, attachment_id, content_id FROM attachments \
         WHERE account_id = ?1 AND message_id = ?2 ORDER BY part_id",
    )?;
    let attachments = stmt
        .query_map(params![account_id, message_id], |row| {
            Ok(Attachment {
                part_id: row.get(0)?,
                filename: row.get(1)?,
                mime_type: row.get(2)?,
                size: row.get(3)?,
                attachment_id: row.get(4)?,
                content_id: row.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(Some(MessageBody {
        html,
        text,
        attachments,
        list_unsubscribe,
        one_click_unsubscribe,
        calendar,
        protection: protection.and_then(|stored| stored.parse().ok()),
        provenance: provenance
            .and_then(|stored| serde_json::from_str(&stored).ok())
            .unwrap_or_default(),
        receipt_to,
        read_report: read_report.and_then(|stored| serde_json::from_str(&stored).ok()),
    }))
}

/// The newest `limit` Inbox messages and whether each body is cached.
/// Cached messages still count toward the limit.
pub fn recent_inbox(
    conn: &Connection,
    account_id: AccountId,
    limit: usize,
) -> Result<Vec<(String, bool)>> {
    let mut stmt = conn.prepare_cached(
        "SELECT recent.id, EXISTS (
             SELECT 1 FROM bodies y WHERE y.account_id = ?1 AND y.message_id = recent.id
         ) FROM (
             SELECT m.id, m.date FROM messages m
             WHERE m.account_id = ?1 AND EXISTS (
                 SELECT 1 FROM message_mailboxes l
                 JOIN mailboxes b ON b.key = l.mailbox
                 WHERE l.account_id = m.account_id AND l.message_id = m.id AND b.role = 'inbox'
             )
             ORDER BY m.date DESC, m.id DESC LIMIT ?2
         ) recent
         ORDER BY recent.date DESC, recent.id DESC",
    )?;
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    let rows = stmt.query_map(params![account_id, limit], |row| {
        Ok((row.get(0)?, row.get(1)?))
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Records reads of cached bodies: `(message id, when)` pairs. Bodies that
/// are gone or were read later are left alone.
pub fn touch_bodies(
    conn: &Connection,
    account_id: AccountId,
    reads: &[(String, EpochMillis)],
) -> Result<()> {
    let mut update = conn.prepare_cached(
        "UPDATE bodies SET accessed_at = ?3 \
         WHERE account_id = ?1 AND message_id = ?2 AND accessed_at < ?3",
    )?;
    for (message_id, at) in reads {
        update.execute(params![account_id, message_id, at])?;
    }
    Ok(())
}

/// Bodies that do not fit in `?1` bytes once the most recently read ones
/// are kept. `bodies_by_access` covers every column this reads.
const OVER_CAP: &str = "SELECT account_id, message_id FROM (SELECT account_id, message_id, \
     SUM(size) OVER (ORDER BY accessed_at DESC, account_id, message_id ROWS UNBOUNDED PRECEDING) \
     AS kept FROM bodies) WHERE kept > ?1";

/// Deletes the least recently read bodies until the rest fit in `max_bytes`.
/// Returns how many it deleted. Does nothing past a sum over the index while
/// the cache fits.
pub fn evict_bodies(conn: &Connection, max_bytes: i64) -> Result<usize> {
    let total: i64 = conn
        .prepare_cached("SELECT TOTAL(size) FROM bodies")?
        .query_row([], |row| row.get::<_, f64>(0))? as i64;
    if total <= max_bytes {
        return Ok(0);
    }
    conn.execute(
        &format!("DELETE FROM attachments WHERE (account_id, message_id) IN ({OVER_CAP})"),
        [max_bytes],
    )?;
    Ok(conn.execute(
        &format!("DELETE FROM bodies WHERE (account_id, message_id) IN ({OVER_CAP})"),
        [max_bytes],
    )?)
}

#[cfg(test)]
mod tests {
    use mailrs_domain::{Address, MessageMeta};
    use mailrs_gmail::labels::set_label_ids;

    use super::*;
    use crate::testing::list_gmail_roles;
    use crate::{accounts, messages, open_in_memory};

    fn message(account_id: AccountId, id: &str, date: i64, labels: &[&str]) -> MessageMeta {
        let mut meta = MessageMeta {
            account_id,
            id: id.into(),
            thread_id: format!("t{id}"),
            rfc822_msgid: None,
            from: Some(Address {
                name: None,
                email: "sender@example.com".into(),
            }),
            to: vec![],
            cc: vec![],
            subject: id.into(),
            date,
            snippet: String::new(),
            size: 100,
            has_attachments: false,
            held: Default::default(),
            roles: vec![],
            list_unsubscribe: None,
            one_click: false,
        };
        set_label_ids(
            &mut meta,
            &labels.iter().map(ToString::to_string).collect::<Vec<_>>(),
        );
        meta
    }

    #[test]
    fn prefetch_candidates_are_uncached_mail_among_the_newest_inbox_messages() {
        let conn = open_in_memory().unwrap();
        let account = accounts::insert_account(&conn, "me@example.com", 0).unwrap();
        list_gmail_roles(&conn, account);
        let changes = [
            message(account, "old", 10, &["INBOX"]),
            message(account, "middle", 20, &["INBOX"]),
            message(account, "new", 30, &["INBOX", "UNREAD"]),
            message(account, "archived", 40, &[]),
        ]
        .into_iter()
        .map(|meta| messages::Change::Upsert {
            meta: Box::new(meta),
            generation: 1,
        })
        .collect::<Vec<_>>();
        messages::apply(&conn, account, &changes).unwrap();

        assert_eq!(
            recent_inbox(&conn, account, 2).unwrap(),
            [("new".to_string(), false), ("middle".to_string(), false)]
        );
        put_prefetched_body(&conn, account, "new", &MessageBody::default(), 100).unwrap();
        assert_eq!(
            recent_inbox(&conn, account, 2).unwrap(),
            [("new".to_string(), true), ("middle".to_string(), false)]
        );
        let accessed: i64 = conn
            .query_row(
                "SELECT accessed_at FROM bodies WHERE account_id = ?1 AND message_id = 'new'",
                [account],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(accessed, 0);

        put_body(&conn, account, "new", &MessageBody::default(), 200).unwrap();
        let accessed: i64 = conn
            .query_row(
                "SELECT accessed_at FROM bodies WHERE account_id = ?1 AND message_id = 'new'",
                [account],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(accessed, 200);
    }
    #[test]
    fn prefetch_evicts_unread_text_before_text_a_person_opened() {
        let conn = open_in_memory().unwrap();
        let account = accounts::insert_account(&conn, "me@example.com", 0).unwrap();
        list_gmail_roles(&conn, account);
        let changes = ["opened", "prefetched"].map(|id| messages::Change::Upsert {
            meta: Box::new(message(account, id, 1, &["INBOX"])),
            generation: 1,
        });
        messages::apply(&conn, account, &changes).unwrap();
        let body = MessageBody {
            text: Some("text".into()),
            ..Default::default()
        };
        put_body(&conn, account, "opened", &body, 100).unwrap();
        put_prefetched_body(&conn, account, "prefetched", &body, 200).unwrap();
        assert_eq!(evict_bodies(&conn, 4).unwrap(), 1);
        assert!(peek_body(&conn, account, "opened").unwrap().is_some());
        assert!(peek_body(&conn, account, "prefetched").unwrap().is_none());
    }
}
