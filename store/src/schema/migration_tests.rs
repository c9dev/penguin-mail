//! Migration 26 over a store at version 25 holding every kind of label the
//! Gmail mapping names, and the safety copy around every migration.

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};

use mailrs_domain::Memberships;
use mailrs_gmail::labels as gmail;
use rusqlite::Connection;

use super::{MIGRATIONS, configure, forget_old_copies, migrate, open_with, schema_version};
use crate::StoreError;

const TO_MAILBOXES: &str = include_str!("26-mailboxes.sql");

/// Two accounts. Messages carry each role label, both keyword labels,
/// UNREAD, two categories, a person's label that is listed, one that is
/// not, and CHAT, which Gmail puts on mail but never lists. `t3` starts
/// in the Trash with its reply in the inbox; `t5` is a draft in Spam.
const FIXTURE: &str = "
INSERT INTO accounts (id, email, history_id, added_at) VALUES
    (1, 'me@example.com', 77, 0), (2, 'you@example.com', NULL, 0);
INSERT INTO labels (account_id, id, name, kind, color) VALUES
    (1, 'INBOX', 'INBOX', 'system', NULL), (1, 'SENT', 'SENT', 'system', NULL),
    (1, 'DRAFT', 'DRAFT', 'system', NULL), (1, 'TRASH', 'TRASH', 'system', NULL),
    (1, 'SPAM', 'SPAM', 'system', NULL), (1, 'IMPORTANT', 'IMPORTANT', 'system', NULL),
    (1, 'STARRED', 'STARRED', 'system', NULL), (1, 'UNREAD', 'UNREAD', 'system', NULL),
    (1, 'CATEGORY_SOCIAL', 'CATEGORY_SOCIAL', 'system', NULL),
    (1, 'Label_1', 'Work', 'user', '#16a766'), (2, 'INBOX', 'INBOX', 'system', NULL);
INSERT INTO messages (account_id, id, thread_id, to_addrs, cc_addrs, subject, date, snippet, size, has_attachments, sync_gen) VALUES
    (1, 'm1', 't1', '[]', '[]', 'One', 100, '', 1, 0, 1),
    (1, 'm2', 't2', '[]', '[]', 'Two', 200, '', 1, 0, 1),
    (1, 'm3', 't3', '[]', '[]', 'Three', 300, '', 1, 0, 1),
    (1, 'm4', 't3', '[]', '[]', 'Re: Three', 310, '', 1, 0, 1),
    (1, 'm5', 't5', '[]', '[]', 'Five', 500, '', 1, 0, 1),
    (2, 'n1', 'u1', '[]', '[]', 'Other', 600, '', 1, 0, 1);
INSERT INTO message_labels (account_id, message_id, label_id) VALUES
    (1, 'm1', 'INBOX'), (1, 'm1', 'UNREAD'), (1, 'm1', 'IMPORTANT'), (1, 'm1', 'CATEGORY_SOCIAL'), (1, 'm1', 'Label_1'),
    (1, 'm2', 'SENT'), (1, 'm2', 'STARRED'), (1, 'm2', 'MUTE'), (1, 'm2', 'CHAT'), (1, 'm2', 'Label_9'),
    (1, 'm3', 'INBOX'), (1, 'm3', 'TRASH'), (1, 'm3', 'UNREAD'),
    (1, 'm4', 'INBOX'),
    (1, 'm5', 'SPAM'), (1, 'm5', 'DRAFT'), (1, 'm5', 'CATEGORY_UPDATES'),
    (2, 'n1', 'INBOX'), (2, 'n1', 'UNREAD');
INSERT INTO threads (account_id, id, last_message_at, subject, snippet, from_display, message_count, unread, starred, has_attachments) VALUES
    (1, 't1', 100, 'One', '', '', 1, 1, 0, 0), (1, 't2', 200, 'Two', '', '', 1, 0, 1, 0),
    (1, 't3', 310, 'Three', '', '', 2, 1, 0, 0), (1, 't5', 500, 'Five', '', '', 1, 0, 0, 0),
    (2, 'u1', 600, 'Other', '', '', 1, 1, 0, 0);
INSERT INTO thread_labels (account_id, thread_id, label_id)
    SELECT DISTINCT m.account_id, m.thread_id, l.label_id FROM messages m
    JOIN message_labels l ON l.account_id = m.account_id AND l.message_id = m.id;
";

fn at_version_25() -> Connection {
    let mut conn = Connection::open_in_memory().unwrap();
    configure(&conn).unwrap();
    migrate(&mut conn, &MIGRATIONS[..25]).unwrap();
    assert_eq!(schema_version(&conn).unwrap(), 25);
    conn.execute_batch(FIXTURE).unwrap();
    conn
}

fn to_26(conn: &mut Connection) {
    let tx = conn.transaction().unwrap();
    tx.execute_batch(TO_MAILBOXES).unwrap();
    tx.pragma_update(None, "user_version", 26).unwrap();
    tx.commit().unwrap();
}

fn strings(conn: &Connection, sql: &str, params: impl rusqlite::Params) -> Vec<String> {
    conn.prepare(sql)
        .unwrap()
        .query_map(params, |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

const MESSAGES: [(i64, &str); 6] = [
    (1, "m1"),
    (1, "m2"),
    (1, "m3"),
    (1, "m4"),
    (1, "m5"),
    (2, "n1"),
];

#[test]
fn every_label_lands_where_the_gmail_table_says() {
    let mut conn = at_version_25();
    let mut before: BTreeMap<(i64, &str), Vec<String>> = BTreeMap::new();
    for (account, id) in MESSAGES {
        before.insert(
            (account, id),
            strings(
                &conn,
                "SELECT label_id FROM message_labels WHERE account_id = ?1 AND message_id = ?2 ORDER BY label_id",
                (account, id),
            ),
        );
    }
    to_26(&mut conn);
    for (account, id) in MESSAGES {
        let held = Memberships {
            mailboxes: strings(
                &conn,
                "SELECT b.id FROM message_mailboxes l JOIN mailboxes b ON b.key = l.mailbox \
                 WHERE l.account_id = ?1 AND l.message_id = ?2",
                (account, id),
            ),
            keywords: strings(
                &conn,
                "SELECT keyword FROM message_keywords WHERE account_id = ?1 AND message_id = ?2",
                (account, id),
            ),
            categories: strings(
                &conn,
                "SELECT category FROM message_categories WHERE account_id = ?1 AND message_id = ?2",
                (account, id),
            ),
        };
        assert_eq!(gmail::labels(&held), before[&(account, id)], "{id}");
    }
}

#[test]
fn listed_labels_keep_their_kind_and_colour_and_carried_ones_become_unlisted_mailboxes() {
    let mut conn = at_version_25();
    to_26(&mut conn);
    let rows: Vec<String> = strings(
        &conn,
        "SELECT id || ' ' || COALESCE(role, '-') || ' ' || kind || ' ' || COALESCE(color, '-') || ' ' || named \
         FROM mailboxes WHERE account_id = ?1 ORDER BY id",
        [1],
    );
    assert_eq!(
        rows,
        [
            "CATEGORY_SOCIAL - system - 1",
            "CHAT - system - 0",
            "DRAFT drafts system - 1",
            "IMPORTANT important system - 1",
            "INBOX inbox system - 1",
            "Label_1 - label #16a766 1",
            "Label_9 - label - 0",
            "SENT sent system - 1",
            "SPAM junk system - 1",
            "STARRED - system - 1",
            "TRASH trash system - 1",
            "UNREAD - system - 1",
        ]
    );
}

#[test]
fn the_derived_rows_apply_the_trash_and_spam_rule() {
    let mut conn = at_version_25();
    to_26(&mut conn);
    let seen = strings(
        &conn,
        "SELECT id || ' ' || seen FROM messages ORDER BY account_id, id",
        [],
    );
    assert_eq!(seen, ["m1 0", "m2 1", "m3 0", "m4 1", "m5 1", "n1 0"]);
    let threads = strings(
        &conn,
        "SELECT id || ' muted ' || muted || ' listed ' || listed FROM threads ORDER BY account_id, id",
        [],
    );
    assert_eq!(
        threads,
        [
            "t1 muted 0 listed 1",
            "t2 muted 1 listed 1",
            "t3 muted 0 listed 1",
            "t5 muted 0 listed 0",
            "u1 muted 0 listed 1"
        ]
    );
    let by_mailbox = strings(
        &conn,
        "SELECT t.thread_id || ' ' || b.id || ' listed ' || t.listed || ' unread ' || t.unread \
         FROM thread_mailboxes t JOIN mailboxes b ON b.key = t.mailbox \
         WHERE t.account_id = 1 ORDER BY t.thread_id, b.id",
        [],
    );
    assert_eq!(
        by_mailbox,
        [
            "t1 IMPORTANT listed 1 unread 1",
            "t1 INBOX listed 1 unread 1",
            "t1 Label_1 listed 1 unread 1",
            "t2 CHAT listed 1 unread 0",
            "t2 Label_9 listed 1 unread 0",
            "t2 SENT listed 1 unread 0",
            "t3 INBOX listed 1 unread 1",
            "t3 TRASH listed 1 unread 1",
            "t5 DRAFT listed 0 unread 0",
            "t5 SPAM listed 1 unread 0",
        ]
    );
    let by_category = strings(
        &conn,
        "SELECT thread_id || ' ' || category || ' listed ' || listed FROM thread_categories ORDER BY thread_id",
        [],
    );
    assert_eq!(
        by_category,
        [
            "t1 CATEGORY_SOCIAL listed 1",
            "t5 CATEGORY_UPDATES listed 0"
        ]
    );
}

#[test]
fn the_history_id_becomes_sync_state_and_every_message_gets_its_remote_ref() {
    let mut conn = at_version_25();
    to_26(&mut conn);
    let accounts = strings(
        &conn,
        "SELECT id || ' ' || provider || ' ' || COALESCE(sync_state, '-') FROM accounts ORDER BY id",
        [],
    );
    assert_eq!(accounts, ["1 gmail {\"history_id\":77}", "2 gmail -"]);
    let columns = strings(&conn, "SELECT name FROM pragma_table_info('accounts')", []);
    assert!(!columns.contains(&"history_id".to_string()), "{columns:?}");
    let refs = strings(
        &conn,
        "SELECT message_id FROM remote_refs WHERE remote = message_id ORDER BY account_id, message_id",
        [],
    );
    assert_eq!(refs, ["m1", "m2", "m3", "m4", "m5", "n1"]);
}

#[test]
fn the_label_tables_go_and_every_key_holds() {
    let mut conn = at_version_25();
    to_26(&mut conn);
    let gone = strings(
        &conn,
        "SELECT name FROM sqlite_master WHERE name IN ('labels', 'message_labels', 'thread_labels')",
        [],
    );
    assert!(gone.is_empty(), "{gone:?}");
    let broken: i64 = conn
        .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(broken, 0);
}

#[test]
fn a_migration_takes_a_copy_first_and_a_new_store_takes_none() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mailrs.db");
    drop(open_with(&path, &MIGRATIONS[..25]).unwrap());
    assert!(
        !dir.path().join("mailrs.db.before-25").exists(),
        "a new store has nothing to guard"
    );
    let mut next = MIGRATIONS[..25].to_vec();
    next.push("CREATE TABLE later (x INTEGER);");
    drop(open_with(&path, &next).unwrap());
    let copy = Connection::open(dir.path().join("mailrs.db.before-26")).unwrap();
    assert_eq!(schema_version(&copy).unwrap(), 25);
}

#[test]
fn a_failed_migration_puts_the_copy_back_and_says_where_it_is() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mailrs.db");
    let first = open_with(&path, &MIGRATIONS[..25]).unwrap();
    first
        .execute(
            "INSERT INTO accounts (email, added_at) VALUES ('me@example.com', 0)",
            [],
        )
        .unwrap();
    drop(first);
    let mut failing = MIGRATIONS[..25].to_vec();
    failing.push("CREATE TABLE halfway (x INTEGER); INSERT INTO nowhere VALUES (1);");

    let err = open_with(&path, &failing).unwrap_err();

    let StoreError::Migration { version, kept, .. } = err else {
        panic!("{err:?}");
    };
    assert_eq!(version, 26);
    assert_eq!(kept, dir.path().join("mailrs.db.before-26"));
    assert!(kept.exists());
    let back = open_with(&path, &MIGRATIONS[..25]).unwrap();
    assert_eq!(schema_version(&back).unwrap(), 25);
    let accounts: i64 = back
        .query_row("SELECT COUNT(*) FROM accounts", [], |r| r.get(0))
        .unwrap();
    assert_eq!(accounts, 1);
    let halfway: i64 = back
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name = 'halfway'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(halfway, 0);
}

#[test]
fn copies_go_once_they_are_a_week_old() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mailrs.db");
    let old = dir.path().join("mailrs.db.before-20");
    let new = dir.path().join("mailrs.db.before-26");
    let other = dir.path().join("notes.before-20");
    for file in [&old, &new, &other] {
        std::fs::write(file, "copy").unwrap();
    }
    let eight_days_ago = SystemTime::now() - Duration::from_secs(8 * 24 * 60 * 60);
    for file in [&old, &other] {
        std::fs::File::options()
            .write(true)
            .open(file)
            .unwrap()
            .set_modified(eight_days_ago)
            .unwrap();
    }

    forget_old_copies(&path, SystemTime::now());

    assert!(!old.exists());
    assert!(new.exists(), "a copy younger than a week stays");
    assert!(other.exists(), "only the store's own copies go");
}

#[test]
fn migration_26_is_the_one_these_tests_run() {
    assert_eq!(MIGRATIONS[25], TO_MAILBOXES);
}

/// Migration 25 gave every account a sign-in client, and an account that
/// was already there came through the setup page with a client of its own.
#[test]
fn an_account_from_before_the_migration_keeps_its_own_client() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mail.db");
    let conn = open_with(&path, &MIGRATIONS[..24]).unwrap();
    let id = crate::accounts::insert_account(&conn, "me@example.com", 0).unwrap();
    drop(conn);

    let conn = open_with(&path, MIGRATIONS).unwrap();
    assert_eq!(
        crate::accounts::sign_in_client(&conn, id).unwrap(),
        mailrs_domain::SignInClient::Own
    );
}

/// Bodies read before LinkedIn's text parts counted as the body are empty,
/// with the two halves filed as attachments. Migration 20 drops them so
/// the next open fetches them again; any other body stays.
#[test]
fn a_body_read_as_two_text_attachments_is_fetched_again() {
    use mailrs_domain::{Attachment, MessageBody};

    use crate::bodies;

    let part = |id: &str, mime: &str, content_id: &str| Attachment {
        part_id: id.into(),
        filename: format!("text-{content_id}.txt"),
        mime_type: mime.into(),
        size: 10,
        attachment_id: None,
        content_id: Some(content_id.into()),
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mail.db");
    let conn = open_with(&path, &MIGRATIONS[..19]).unwrap();
    conn.execute_batch(
        "INSERT INTO accounts (id, email, added_at) VALUES (1, 'me@example.com', 0);
         INSERT INTO messages (account_id, id, thread_id, to_addrs, cc_addrs, subject, date, snippet, size, has_attachments, sync_gen) VALUES
             (1, 'li', 't1', '[]', '[]', 'LinkedIn', 1, '', 1, 0, 1),
             (1, 'ok', 't2', '[]', '[]', 'Fine', 2, '', 1, 0, 1);",
    )
    .unwrap();
    let empty = MessageBody {
        attachments: vec![
            part("0", "text/plain", "text-body"),
            part("1", "text/html", "html-body"),
        ],
        ..MessageBody::default()
    };
    let fine = MessageBody {
        text: Some("fine".into()),
        ..MessageBody::default()
    };
    bodies::put_body(&conn, 1, "li", &empty, 1).unwrap();
    bodies::put_body(&conn, 1, "ok", &fine, 1).unwrap();
    drop(conn);

    let conn = open_with(&path, MIGRATIONS).unwrap();
    assert!(bodies::get_body(&conn, 1, "li", 2).unwrap().is_none());
    assert!(bodies::get_body(&conn, 1, "ok", 2).unwrap().is_some());
}

/// Bodies decoded under the charset they claimed rather than the one their
/// bytes showed hold "Ã§" where the sender wrote "ç". Migration 27 drops
/// them so the next open decodes them again; a body with its accents
/// intact stays.
#[test]
fn a_body_decoded_in_the_wrong_charset_is_fetched_again() {
    use mailrs_domain::MessageBody;

    use crate::bodies;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mail.db");
    let conn = open_with(&path, &MIGRATIONS[..26]).unwrap();
    conn.execute_batch(
        "INSERT INTO accounts (id, email, added_at) VALUES (1, 'me@example.com', 0);
         INSERT INTO messages (account_id, id, thread_id, to_addrs, cc_addrs, subject, date, snippet, size, has_attachments, sync_gen) VALUES
             (1, 'html', 't1', '[]', '[]', 'Pedido', 1, '', 1, 0, 1),
             (1, 'text', 't2', '[]', '[]', 'Aviso', 2, '', 1, 0, 1),
             (1, 'ok', 't3', '[]', '[]', 'Direção', 3, '', 1, 0, 1);",
    )
    .unwrap();
    let garbled_html = MessageBody {
        html: Some("<p>agradecemos devoluÃ§Ã£o e aviso</p>".into()),
        ..MessageBody::default()
    };
    let garbled_text = MessageBody {
        text: Some("agradecemos o seu contacto â€“ Tel.".into()),
        ..MessageBody::default()
    };
    let fine = MessageBody {
        text: Some("Direção de Recuperação de Crédito, «café» às três".into()),
        ..MessageBody::default()
    };
    bodies::put_body(&conn, 1, "html", &garbled_html, 1).unwrap();
    bodies::put_body(&conn, 1, "text", &garbled_text, 1).unwrap();
    bodies::put_body(&conn, 1, "ok", &fine, 1).unwrap();
    drop(conn);

    let conn = open_with(&path, MIGRATIONS).unwrap();
    assert!(bodies::get_body(&conn, 1, "html", 2).unwrap().is_none());
    assert!(bodies::get_body(&conn, 1, "text", 2).unwrap().is_none());
    assert!(bodies::get_body(&conn, 1, "ok", 2).unwrap().is_some());
}

/// Google Calendar's invitation part came by attachment id, so bodies read
/// before the sync layer fetched it hold no calendar and the card never
/// showed. Migration 28 drops them so the next open fetches the part; a
/// body whose invitation was read, and one with no calendar file, stay.
#[test]
fn an_invitation_read_without_its_calendar_is_fetched_again() {
    use mailrs_domain::{Attachment, MessageBody};

    use crate::bodies;

    let ics = |mime: &str| Attachment {
        part_id: "1".into(),
        filename: "invite.ics".into(),
        mime_type: mime.into(),
        size: 1962,
        attachment_id: Some("att".into()),
        content_id: None,
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mail.db");
    let conn = open_with(&path, &MIGRATIONS[..27]).unwrap();
    conn.execute_batch(
        "INSERT INTO accounts (id, email, added_at) VALUES (1, 'me@example.com', 0);
         INSERT INTO messages (account_id, id, thread_id, to_addrs, cc_addrs, subject, date, snippet, size, has_attachments, sync_gen) VALUES
             (1, 'missed', 't1', '[]', '[]', 'Invitation', 1, '', 1, 1, 1),
             (1, 'read', 't2', '[]', '[]', 'Invitation', 2, '', 1, 1, 1),
             (1, 'plain', 't3', '[]', '[]', 'Hello', 3, '', 1, 0, 1);",
    )
    .unwrap();
    let missed = MessageBody {
        html: Some("<p>Invitation</p>".into()),
        attachments: vec![ics("application/ics")],
        ..MessageBody::default()
    };
    let read = MessageBody {
        calendar: Some("BEGIN:VCALENDAR".into()),
        ..missed.clone()
    };
    let plain = MessageBody {
        text: Some("hello".into()),
        ..MessageBody::default()
    };
    bodies::put_body(&conn, 1, "missed", &missed, 1).unwrap();
    bodies::put_body(&conn, 1, "read", &read, 1).unwrap();
    bodies::put_body(&conn, 1, "plain", &plain, 1).unwrap();
    drop(conn);

    // Up to migration 28 alone: a later one drops every body with a file,
    // the invitation that was read among them.
    let conn = open_with(&path, &MIGRATIONS[..28]).unwrap();
    assert!(bodies::get_body(&conn, 1, "missed", 2).unwrap().is_none());
    assert!(bodies::get_body(&conn, 1, "read", 2).unwrap().is_some());
    assert!(bodies::get_body(&conn, 1, "plain", 2).unwrap().is_some());
}

/// Migration 30 adds local threading's table and column without touching a
/// message stored before it: nothing threaded that message locally, so it
/// stays untouched rather than joining a thread on the migration's say-so.
#[test]
fn a_message_from_before_local_threading_stays_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mail.db");
    let conn = open_with(&path, &MIGRATIONS[..29]).unwrap();
    conn.execute_batch(
        "INSERT INTO accounts (id, email, added_at) VALUES (1, 'me@example.com', 0);
         INSERT INTO messages (account_id, id, thread_id, to_addrs, cc_addrs, subject, date, snippet, size, has_attachments, sync_gen) \
         VALUES (1, 'a', 't1', '[]', '[]', 'Lunch', 1, '', 1, 0, 1);",
    )
    .unwrap();
    drop(conn);

    let conn = open_with(&path, &MIGRATIONS[..30]).unwrap();
    assert_eq!(schema_version(&conn).unwrap(), 30);
    let base_subject: Option<String> = conn
        .query_row(
            "SELECT base_subject FROM messages WHERE account_id = 1 AND id = 'a'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(base_subject, None);
    let links: i64 = conn
        .query_row("SELECT COUNT(*) FROM message_links", [], |row| row.get(0))
        .unwrap();
    assert_eq!(links, 0);
}

/// Files read before are named by Gmail attachment handles, which
/// neither path answers, so their bodies go and come back named by part
/// path. A body without files stays.
#[test]
fn bodies_with_files_named_by_gmail_handles_are_fetched_again() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mail.db");
    let conn = open_with(&path, &MIGRATIONS[..30]).unwrap();
    conn.execute_batch(
        "INSERT INTO accounts (id, email, added_at) VALUES (1, 'me@example.com', 0);
         INSERT INTO messages (account_id, id, thread_id, to_addrs, cc_addrs, subject, date,
             snippet, size, has_attachments, sync_gen)
             VALUES (1, 'a', 't', '[]', '[]', '', 0, '', 0, 1, 1),
                    (1, 'b', 't', '[]', '[]', '', 0, '', 0, 0, 1);
         INSERT INTO bodies (account_id, message_id, text, size, fetched_at, accessed_at)
             VALUES (1, 'a', 'with a file', 11, 0, 0), (1, 'b', 'plain', 5, 0, 0);
         INSERT INTO attachments (account_id, message_id, part_id, filename, mime_type, size,
             attachment_id) VALUES (1, 'a', '1', 'plan.pdf', 'application/pdf', 3, 'ANGjdJ8');",
    )
    .unwrap();
    drop(conn);
    let conn = open_with(&path, MIGRATIONS).unwrap();
    let bodies: Vec<String> = conn
        .prepare("SELECT message_id FROM bodies ORDER BY message_id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(bodies, ["b"]);
    let files: i64 = conn
        .query_row("SELECT COUNT(*) FROM attachments", [], |row| row.get(0))
        .unwrap();
    assert_eq!(files, 0);
}

/// Migration 32 adds IMAP's columns and table. A Gmail account from
/// before it stays a Gmail account with no provider name, and the store
/// is copied first, as before every migration of a store with mail.
#[test]
fn a_gmail_account_from_before_imap_stays_as_it_was() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mail.db");
    let conn = open_with(&path, &MIGRATIONS[..31]).unwrap();
    conn.execute_batch("INSERT INTO accounts (id, email, added_at) VALUES (1, 'me@gmail.com', 0);")
        .unwrap();
    drop(conn);

    let conn = open_with(&path, &MIGRATIONS[..32]).unwrap();
    assert_eq!(schema_version(&conn).unwrap(), 32);
    let (provider, name): (String, Option<String>) = conn
        .query_row(
            "SELECT provider, provider_name FROM accounts WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((provider.as_str(), name), ("gmail", None));
    assert!(dir.path().join("mail.db.before-32").exists());
}

/// A message's location is `remote_refs(account_id, mailbox, uidvalidity,
/// uid)`: an IMAP account looks up a mailbox's stored messages by that
/// tuple on every listing, so it needs an index rather than a table scan.
#[test]
fn migration_32_indexes_remote_refs_by_location() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mail.db");
    let conn = open_with(&path, &MIGRATIONS[..32]).unwrap();
    let indexed_columns = |index: &str| -> Vec<String> {
        conn.prepare(&format!("PRAGMA index_info({index})"))
            .unwrap()
            .query_map([], |row| row.get::<_, String>(2))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    };
    let indexes: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name = 'remote_refs'")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert!(
        indexes
            .iter()
            .any(|index| indexed_columns(index)
                == ["account_id", "mailbox", "uidvalidity", "uid"]),
        "remote_refs has no index on (account_id, mailbox, uidvalidity, uid): {indexes:?}"
    );
}

/// A Gmail message has no mailbox in `remote_refs`, so the location index
/// leaves its rows out: a Gmail account writes nothing to it. A lookup by
/// mailbox still finds the index, since `mailbox = ?` rules out NULL.
#[test]
fn the_location_index_leaves_gmail_rows_out_and_still_serves_a_listing() {
    let dir = tempfile::tempdir().unwrap();
    let conn = open_with(&dir.path().join("mail.db"), &MIGRATIONS[..32]).unwrap();
    let partial: bool = conn
        .query_row(
            "SELECT partial FROM pragma_index_list('remote_refs') \
             WHERE name = 'remote_refs_by_location'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(partial, "the location index covers Gmail rows too");
    let plan: Vec<String> = conn
        .prepare(
            "EXPLAIN QUERY PLAN SELECT message_id FROM remote_refs \
             WHERE account_id = ?1 AND mailbox = ?2 AND uidvalidity = ?3",
        )
        .unwrap()
        .query_map([1, 2, 3], |row| row.get(3))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert!(
        plan.iter().any(|step| step.contains("remote_refs_by_location")),
        "{plan:?}"
    );
}

/// Migration 33 adds the calendar's tables. A store from before it, Gmail
/// account and IMAP account alike, opens with none of the three empty and
/// the IMAP account still deletes cleanly through its foreign keys.
#[test]
fn a_store_from_before_the_calendar_opens_with_no_calendars() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mail.db");
    let conn = open_with(&path, &MIGRATIONS[..32]).unwrap();
    conn.execute_batch(
        "INSERT INTO accounts (id, email, added_at) VALUES (1, 'me@gmail.com', 0);
         INSERT INTO accounts (id, email, added_at, provider_name) \
             VALUES (2, 'me@fastmail.com', 0, 'Fastmail');
         INSERT INTO account_servers (account_id, role, host, port, security, user_name) \
             VALUES (2, 'imap', 'imap.fastmail.com', 993, 'tls', 'me@fastmail.com');",
    )
    .unwrap();
    drop(conn);

    let conn = open_with(&path, MIGRATIONS).unwrap();
    assert_eq!(schema_version(&conn).unwrap(), MIGRATIONS.len() as i64);
    for table in ["calendars", "events", "calendar_changes"] {
        let count: i64 = conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0, "{table}");
    }
    conn.execute("DELETE FROM accounts WHERE id = 2", []).unwrap();
    let servers: i64 = conn
        .query_row("SELECT COUNT(*) FROM account_servers WHERE account_id = 2", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(servers, 0);
}

/// Migration 34 adds the two consent columns to every existing account,
/// with nothing known yet: `granted_scopes` waits for that account's next
/// token refresh, and `asked_scopes` for its next sign-in or Grant
/// Access, since only those ask Google for every scope in one visit.
#[test]
fn migration_34_leaves_every_account_s_scopes_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mail.db");
    let conn = open_with(&path, &MIGRATIONS[..33]).unwrap();
    conn.execute(
        "INSERT INTO accounts (id, email, added_at) VALUES (1, 'me@gmail.com', 0)",
        [],
    )
    .unwrap();
    drop(conn);

    let conn = open_with(&path, MIGRATIONS).unwrap();
    assert_eq!(schema_version(&conn).unwrap(), MIGRATIONS.len() as i64);
    let (granted, asked): (Option<String>, Option<String>) = conn
        .query_row(
            "SELECT granted_scopes, asked_scopes FROM accounts WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(granted, None);
    assert_eq!(asked, None);
}

/// Migration 39 forgets every calendar's sync token. Events Google sent
/// without a zone of their own were stored in UTC, and a change read
/// never brings an unchanged event back, so only a whole read files them
/// in their calendar's zone. The events stay until that read replaces
/// them.
#[test]
fn migration_39_reads_every_calendar_whole_again() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mail.db");
    let conn = open_with(&path, &MIGRATIONS[..38]).unwrap();
    conn.execute_batch(
        "INSERT INTO accounts (id, email, added_at) VALUES (1, 'me@gmail.com', 0);
         INSERT INTO calendars (account_id, id, name, color, access, zone, is_primary, sync_token, synced_at) \
             VALUES (1, 'primary', 'Me', '#fff', 'owner', 'Europe/Lisbon', 1, 't9', 5);
         INSERT INTO events (account_id, calendar, id, uid, etag, starts_at, ends_at, zone, all_day, \
             title, place, description, busy, status, private, rules, pending, seen_at) \
             VALUES (1, 'primary', 'a', 'a@google.com', '\"1\"', 0, 1, 'UTC', 0, 'Dentist', '', '', 1, \
             'confirmed', 0, '', 0, 5);",
    )
    .unwrap();
    drop(conn);

    let conn = open_with(&path, MIGRATIONS).unwrap();
    assert_eq!(schema_version(&conn).unwrap(), MIGRATIONS.len() as i64);
    let token: Option<String> = conn
        .query_row("SELECT sync_token FROM calendars WHERE id = 'primary'", [], |row| row.get(0))
        .unwrap();
    assert_eq!(token, None);
    let events: i64 = conn.query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0)).unwrap();
    assert_eq!(events, 1);
}

/// Migration 46 gives events their attachments. A row stored before it
/// reads them as unknown, so a write of that row leaves Google's files
/// alone, and every calendar's token goes so the next read fills them in.
#[test]
fn migration_46_leaves_attachments_unknown_until_a_whole_read() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mail.db");
    let conn = open_with(&path, &MIGRATIONS[..45]).unwrap();
    conn.execute_batch(
        "INSERT INTO accounts (id, email, added_at) VALUES (1, 'me@gmail.com', 0);
         INSERT INTO calendars (account_id, id, name, color, access, zone, is_primary, sync_token, synced_at) \
             VALUES (1, 'primary', 'Me', '#fff', 'owner', 'Europe/Lisbon', 1, 't9', 5);
         INSERT INTO events (account_id, calendar, id, uid, etag, starts_at, ends_at, zone, all_day, \
             title, place, description, busy, status, private, rules, pending, seen_at) \
             VALUES (1, 'primary', 'a', 'a@google.com', '\"1\"', 0, 1, 'UTC', 0, 'Review', '', '', 1, \
             'confirmed', 0, '', 0, 5);",
    )
    .unwrap();
    drop(conn);

    let conn = open_with(&path, &MIGRATIONS[..46]).unwrap();
    assert_eq!(schema_version(&conn).unwrap(), 46);
    let token: Option<String> = conn
        .query_row("SELECT sync_token FROM calendars WHERE id = 'primary'", [], |row| row.get(0))
        .unwrap();
    assert_eq!(token, None);
    let event = crate::calendar::event(&conn, 1, "primary", "a").unwrap().unwrap();
    assert_eq!(event.attachments, None);
}

/// Migration 47 gives each server mailbox a place among its siblings in
/// the sidebar. A mailbox stored before it has none, so the sidebar lists
/// it by name as before.
#[test]
fn migration_47_leaves_every_mailbox_unplaced() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mail.db");
    let conn = open_with(&path, &MIGRATIONS[..46]).unwrap();
    conn.execute_batch(
        "INSERT INTO accounts (id, email, added_at) VALUES (1, 'me@gmail.com', 0);
         INSERT INTO mailboxes (account_id, id, name, kind) VALUES (1, 'Label_1', 'Work', 'label');",
    )
    .unwrap();
    drop(conn);

    let conn = open_with(&path, &MIGRATIONS[..47]).unwrap();
    assert_eq!(schema_version(&conn).unwrap(), 47);
    assert!(crate::labels::positions(&conn, 1).unwrap().is_empty());
}

/// Migration 48 adds the found servers, the local rules with their
/// watermark and ran-set, and the queue of rule changes. An IMAP account
/// from before it has none of them, and deleting the account takes every
/// row with it through the foreign keys.
#[test]
fn migration_48_adds_the_services_and_rules_tables() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mail.db");
    let conn = open_with(&path, &MIGRATIONS[..47]).unwrap();
    conn.execute(
        "INSERT INTO accounts (id, email, added_at, provider, provider_name) \
         VALUES (2, 'me@fastmail.com', 0, 'imap', 'Fastmail')",
        [],
    )
    .unwrap();
    drop(conn);

    let conn = open_with(&path, &MIGRATIONS[..48]).unwrap();
    assert_eq!(schema_version(&conn).unwrap(), 48);
    let ran: Option<i64> = conn
        .query_row("SELECT rules_ran_until FROM accounts WHERE id = 2", [], |row| row.get(0))
        .unwrap();
    assert_eq!(ran, None, "an old account has run no local rules yet");
    conn.execute_batch(
        "INSERT INTO account_services (account_id, service, url) VALUES (2, 'caldav', 'https://caldav.fastmail.com/');
         INSERT INTO local_rules (account_id, id, position, filter) VALUES (2, 'local-1', 0, '{}');
         INSERT INTO local_rules_ran (account_id, message_id, date) VALUES (2, 'INBOX/1/1', 0);
         INSERT INTO rule_changes (account_id, kind, filter_id) VALUES (2, 'delete', 'r1');",
    )
    .unwrap();
    conn.execute("DELETE FROM accounts WHERE id = 2", []).unwrap();
    for table in ["account_services", "local_rules", "local_rules_ran", "rule_changes"] {
        let left: i64 = conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row.get(0))
            .unwrap();
        assert_eq!(left, 0, "{table}");
    }
}

/// Migration 49 adds what a POP3 account keeps: the UIDLs downloaded, the
/// ones that failed, the raw copies, the removal setting, and a 'pop3'
/// server row. An account from before it leaves mail on the server, and
/// its servers come through the rebuilt table unchanged.
#[test]
fn migration_49_adds_the_pop3_tables_and_columns() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mail.db");
    let conn = open_with(&path, &MIGRATIONS[..48]).unwrap();
    conn.execute_batch(
        "INSERT INTO accounts (id, email, added_at, provider, provider_name) VALUES (2, 'me@fastmail.com', 0, 'imap', 'Fastmail');
         INSERT INTO account_servers (account_id, role, host, port, security, user_name)
             VALUES (2, 'imap', 'imap.fastmail.com', 993, 'tls', 'me@fastmail.com');",
    )
    .unwrap();
    drop(conn);

    let conn = open_with(&path, &MIGRATIONS[..49]).unwrap();
    assert_eq!(schema_version(&conn).unwrap(), 49);
    let (mode, days): (String, Option<i64>) = conn
        .query_row("SELECT pop3_remove, pop3_remove_days FROM accounts WHERE id = 2", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .unwrap();
    assert_eq!((mode.as_str(), days), ("never", None));
    let kept: String = conn
        .query_row("SELECT host FROM account_servers WHERE account_id = 2 AND role = 'imap'", [], |row| row.get(0))
        .unwrap();
    assert_eq!(kept, "imap.fastmail.com");
    conn.execute_batch(
        "INSERT INTO account_servers (account_id, role, host, port, security, user_name)
             VALUES (2, 'pop3', 'pop.fastmail.com', 995, 'tls', 'me@fastmail.com');
         INSERT INTO pop3_seen (account_id, uidl, downloaded_at) VALUES (2, 'u1', 0);
         INSERT INTO pop3_failures (account_id, uidl, failures, last_error) VALUES (2, 'u2', 1, 'gone');
         INSERT INTO local_messages (account_id, message_id, raw) VALUES (2, 'pop3/u1', X'00');",
    )
    .unwrap();
    assert!(conn.execute("UPDATE accounts SET pop3_remove = 'sometimes' WHERE id = 2", []).is_err());
    conn.execute("DELETE FROM accounts WHERE id = 2", []).unwrap();
    for table in ["pop3_seen", "pop3_failures", "local_messages", "account_servers"] {
        let left: i64 = conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row.get(0))
            .unwrap();
        assert_eq!(left, 0, "{table}");
    }
}

/// Migration 50 records whether a POP3 account's first check finished. An
/// account that already holds downloaded mail counts as finished, so it
/// does not start announcing the mail it already has; one with nothing
/// downloaded has not finished.
#[test]
fn migration_50_counts_an_account_with_downloaded_mail_as_finished() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mail.db");
    let conn = open_with(&path, &MIGRATIONS[..49]).unwrap();
    conn.execute_batch(
        "INSERT INTO accounts (id, email, added_at, provider, provider_name) VALUES
             (2, 'old@example.org', 0, 'imap', 'Example'),
             (3, 'new@example.org', 0, 'imap', 'Example');
         INSERT INTO pop3_seen (account_id, uidl, downloaded_at) VALUES (2, 'u1', 0);",
    )
    .unwrap();
    drop(conn);

    let conn = open_with(&path, &MIGRATIONS[..50]).unwrap();
    assert_eq!(schema_version(&conn).unwrap(), 50);
    let finished = |id: i64| -> i64 {
        conn.query_row("SELECT pop3_first_check_done FROM accounts WHERE id = ?1", [id], |row| row.get(0)).unwrap()
    };
    assert_eq!(finished(2), 1, "it has downloaded mail already");
    assert_eq!(finished(3), 0, "it has downloaded nothing");
}

/// Migration 51 gives each downloaded UIDL the store id of the message it
/// brought, so a removal asked for by message finds its row without
/// reading the UIDL out of the id. Rows from before held `pop3/<uidl>`.
#[test]
fn migration_51_names_the_message_each_downloaded_uidl_brought() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mail.db");
    let conn = open_with(&path, &MIGRATIONS[..50]).unwrap();
    conn.execute_batch(
        "INSERT INTO accounts (id, email, added_at, provider, provider_name) VALUES
             (2, 'me@example.org', 0, 'pop3', 'Example');
         INSERT INTO pop3_seen (account_id, uidl, downloaded_at) VALUES (2, 'u1', 0), (2, 'a/b', 0);",
    )
    .unwrap();
    drop(conn);

    let conn = open_with(&path, &MIGRATIONS[..51]).unwrap();
    assert_eq!(schema_version(&conn).unwrap(), 51);
    let named: Vec<(String, String)> = conn
        .prepare("SELECT uidl, message_id FROM pop3_seen ORDER BY uidl")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(
        named,
        [("a/b".to_string(), "pop3/a/b".to_string()), ("u1".to_string(), "pop3/u1".to_string())]
    );
}

/// Migration 52 keeps why each message failed as a code, translated when
/// shown. An over-size failure stored before it held the words in English
/// or European Portuguese, which become the code.
#[test]
fn migration_52_turns_stored_over_size_words_into_a_reason() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mail.db");
    let conn = open_with(&path, &MIGRATIONS[..51]).unwrap();
    conn.execute_batch(
        "INSERT INTO accounts (id, email, added_at, provider, provider_name) VALUES
             (2, 'me@example.org', 0, 'pop3', 'Example');
         INSERT INTO pop3_failures (account_id, uidl, failures, last_error) VALUES
             (2, 'big', 3, 'The message is larger than Penguin Mail downloads.'),
             (2, 'grande', 3, 'A mensagem é maior do que o Penguin Mail transfere.'),
             (2, 'locked', 3, 'mailbox locked');",
    )
    .unwrap();
    drop(conn);

    let conn = open_with(&path, &MIGRATIONS[..52]).unwrap();
    assert_eq!(schema_version(&conn).unwrap(), 52);
    let rows: Vec<(String, String, String)> = conn
        .prepare("SELECT uidl, reason, last_error FROM pop3_failures ORDER BY uidl")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let row = |uidl: &str, reason: &str, words: &str| (uidl.to_string(), reason.to_string(), words.to_string());
    assert_eq!(
        rows,
        [row("big", "too_large", ""), row("grande", "too_large", ""), row("locked", "refused", "mailbox locked")]
    );
    assert!(
        conn.execute("UPDATE pop3_failures SET reason = 'tired'", []).is_err(),
        "a reason is one of the codes"
    );
}
