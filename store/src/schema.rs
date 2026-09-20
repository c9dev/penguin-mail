//! Connection setup and migrations, tracked with `PRAGMA user_version`.

use std::path::Path;
use std::time::Duration;

use rusqlite::Connection;

use crate::Result;

const MIGRATIONS: &[&str] = &[
    r#"
CREATE TABLE accounts (
    id              INTEGER PRIMARY KEY,
    email           TEXT NOT NULL UNIQUE,
    state           TEXT NOT NULL DEFAULT 'bootstrapping',
    history_id      INTEGER,
    backfill_cursor TEXT,
    backfill_done   INTEGER NOT NULL DEFAULT 0,
    sync_gen        INTEGER NOT NULL DEFAULT 1,
    added_at        INTEGER NOT NULL
);

CREATE TABLE labels (
    account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    id         TEXT NOT NULL,
    name       TEXT NOT NULL,
    kind       TEXT NOT NULL,
    PRIMARY KEY (account_id, id)
);

CREATE TABLE messages (
    account_id      INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    id              TEXT NOT NULL,
    thread_id       TEXT NOT NULL,
    rfc822_msgid    TEXT,
    from_name       TEXT,
    from_addr       TEXT,
    to_addrs        TEXT NOT NULL,
    cc_addrs        TEXT NOT NULL,
    subject         TEXT NOT NULL,
    date            INTEGER NOT NULL,
    snippet         TEXT NOT NULL,
    size            INTEGER NOT NULL,
    has_attachments INTEGER NOT NULL,
    sync_gen        INTEGER NOT NULL,
    PRIMARY KEY (account_id, id)
);
CREATE INDEX messages_by_thread ON messages(account_id, thread_id);
CREATE INDEX messages_by_gen ON messages(account_id, sync_gen);

CREATE TABLE message_labels (
    account_id INTEGER NOT NULL,
    message_id TEXT NOT NULL,
    label_id   TEXT NOT NULL,
    PRIMARY KEY (account_id, message_id, label_id),
    FOREIGN KEY (account_id, message_id) REFERENCES messages(account_id, id) ON DELETE CASCADE
);

CREATE TABLE threads (
    account_id      INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    id              TEXT NOT NULL,
    last_message_at INTEGER NOT NULL,
    subject         TEXT NOT NULL,
    snippet         TEXT NOT NULL,
    from_display    TEXT NOT NULL,
    message_count   INTEGER NOT NULL,
    unread          INTEGER NOT NULL,
    starred         INTEGER NOT NULL,
    has_attachments INTEGER NOT NULL,
    PRIMARY KEY (account_id, id)
);
CREATE INDEX threads_by_recency ON threads(last_message_at DESC);

CREATE TABLE thread_labels (
    account_id INTEGER NOT NULL,
    thread_id  TEXT NOT NULL,
    label_id   TEXT NOT NULL,
    PRIMARY KEY (account_id, thread_id, label_id),
    FOREIGN KEY (account_id, thread_id) REFERENCES threads(account_id, id) ON DELETE CASCADE
);
CREATE INDEX thread_labels_by_label ON thread_labels(label_id, account_id, thread_id);

CREATE TABLE bodies (
    account_id  INTEGER NOT NULL,
    message_id  TEXT NOT NULL,
    html        TEXT,
    text        TEXT,
    size        INTEGER NOT NULL,
    fetched_at  INTEGER NOT NULL,
    accessed_at INTEGER NOT NULL,
    PRIMARY KEY (account_id, message_id),
    FOREIGN KEY (account_id, message_id) REFERENCES messages(account_id, id) ON DELETE CASCADE
);
CREATE INDEX bodies_by_access ON bodies(accessed_at);

CREATE TABLE attachments (
    account_id    INTEGER NOT NULL,
    message_id    TEXT NOT NULL,
    part_id       TEXT NOT NULL,
    filename      TEXT NOT NULL,
    mime_type     TEXT NOT NULL,
    size          INTEGER NOT NULL,
    attachment_id TEXT,
    content_id    TEXT,
    PRIMARY KEY (account_id, message_id, part_id),
    FOREIGN KEY (account_id, message_id) REFERENCES messages(account_id, id) ON DELETE CASCADE
);
"#,
    r#"
CREATE INDEX message_labels_by_label ON message_labels(label_id, account_id, message_id);
"#,
    r#"
CREATE TABLE scheduled (
    account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    draft_id   TEXT NOT NULL,
    message_id TEXT NOT NULL,
    thread_id  TEXT NOT NULL,
    subject    TEXT NOT NULL,
    recipients TEXT NOT NULL,
    send_at    INTEGER NOT NULL,
    PRIMARY KEY (account_id, draft_id)
);
CREATE INDEX scheduled_by_time ON scheduled(send_at);
"#,
    r#"
ALTER TABLE bodies ADD COLUMN list_unsubscribe TEXT;
ALTER TABLE bodies ADD COLUMN one_click_unsubscribe INTEGER NOT NULL DEFAULT 0;
"#,
    r#"
CREATE TABLE flags (
    account_id INTEGER NOT NULL,
    message_id TEXT NOT NULL,
    color      TEXT NOT NULL,
    PRIMARY KEY (account_id, message_id),
    FOREIGN KEY (account_id, message_id) REFERENCES messages(account_id, id) ON DELETE CASCADE
);
CREATE TABLE reminders (
    account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    thread_id  TEXT NOT NULL,
    subject    TEXT NOT NULL,
    remind_at  INTEGER NOT NULL,
    PRIMARY KEY (account_id, thread_id)
);
"#,
    r#"
ALTER TABLE labels ADD COLUMN color TEXT;
"#,
    r#"
CREATE TABLE follow_up_dismissals (
    account_id   INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    thread_id    TEXT NOT NULL,
    dismissed_at INTEGER NOT NULL,
    PRIMARY KEY (account_id, thread_id)
);
"#,
    // List rows used to look these two up per thread, with a sort each.
    // `messages::refresh_thread` keeps them now.
    r#"
ALTER TABLE threads ADD COLUMN flag_color TEXT;
ALTER TABLE threads ADD COLUMN from_email TEXT NOT NULL DEFAULT '';
UPDATE threads SET
    flag_color = (SELECT f.color FROM messages m
                  JOIN flags f ON f.account_id = m.account_id AND f.message_id = m.id
                  WHERE m.account_id = threads.account_id AND m.thread_id = threads.id
                  ORDER BY m.date DESC, m.id DESC LIMIT 1),
    from_email = COALESCE((SELECT m.from_addr FROM messages m
                  WHERE m.account_id = threads.account_id AND m.thread_id = threads.id
                  ORDER BY m.date DESC, m.id DESC LIMIT 1), '');
"#,
    // The wider index lets eviction total and rank the cache without
    // reading a single body.
    r#"
DROP INDEX bodies_by_access;
CREATE INDEX bodies_by_access ON bodies(accessed_at, account_id, message_id, size);
"#,
    // The address book each account reads from Google, and the sync token
    // that makes the next read cheap.
    r#"
CREATE TABLE contacts (
    account_id   INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    resource     TEXT NOT NULL,
    name         TEXT,
    organization TEXT,
    phone        TEXT,
    photo_url    TEXT,
    photo_file   TEXT,
    PRIMARY KEY (account_id, resource)
);

CREATE TABLE contact_addresses (
    account_id INTEGER NOT NULL,
    resource   TEXT NOT NULL,
    email      TEXT NOT NULL,
    rank       INTEGER NOT NULL,
    PRIMARY KEY (account_id, resource, email),
    FOREIGN KEY (account_id, resource) REFERENCES contacts(account_id, resource) ON DELETE CASCADE
);
CREATE INDEX contact_addresses_by_email ON contact_addresses(email);

CREATE TABLE contact_books (
    account_id INTEGER PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE,
    sync_token TEXT,
    synced_at  INTEGER NOT NULL
);
"#,
    // The invitation a message carries, and the answer the user gave it.
    r#"
ALTER TABLE bodies ADD COLUMN calendar TEXT;

CREATE TABLE invitations (
    account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    uid        TEXT NOT NULL,
    sequence   INTEGER NOT NULL,
    starts_at  INTEGER,
    all_day    INTEGER NOT NULL DEFAULT 0,
    summary    TEXT NOT NULL DEFAULT '',
    cancelled  INTEGER NOT NULL DEFAULT 0,
    answer     TEXT,
    message_id TEXT NOT NULL,
    seen_at    INTEGER NOT NULL,
    news       TEXT,
    moved_from INTEGER,
    PRIMARY KEY (account_id, uid)
);
"#,
    // Canned replies. They follow the person rather than an account, so
    // this table has no account_id to cascade from.
    r#"
CREATE TABLE templates (
    id       INTEGER PRIMARY KEY,
    name     TEXT NOT NULL,
    subject  TEXT NOT NULL DEFAULT '',
    markdown TEXT NOT NULL DEFAULT ''
);
"#,
    // Senders whose remote images may load. Like templates, this follows
    // the person rather than an account. `sender` holds a lower-case
    // address, or a lower-case domain when `whole_domain` is set, and an
    // address always carries an `@`, so the two never collide.
    r#"
CREATE TABLE image_senders (
    sender       TEXT PRIMARY KEY,
    whole_domain INTEGER NOT NULL DEFAULT 0,
    allowed_at   INTEGER NOT NULL
);
"#,
    // The OpenPGP wrapper a message arrived in, so a body read back from
    // here says as much about the message as the one Gmail just handed over.
    r#"
ALTER TABLE bodies ADD COLUMN protection TEXT;
"#,
    // Send Later grows into the outbox. A message that could not go out is
    // waiting for the same reason a scheduled one is, so it lands in the
    // same table, with the bytes it will be sent from and the count of
    // tries behind it. Gmail holds a scheduled message as a draft, so its
    // draft id stays; a message that never reached Gmail has none, and the
    // row's own id names it instead.
    r#"
CREATE TABLE outbox (
    id         INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    draft_id   TEXT,
    message_id TEXT,
    thread_id  TEXT,
    subject    TEXT NOT NULL,
    recipients TEXT NOT NULL,
    send_at    INTEGER NOT NULL,
    raw        BLOB,
    composer   TEXT NOT NULL DEFAULT '',
    attempts   INTEGER NOT NULL DEFAULT 0,
    problem    TEXT
);
INSERT INTO outbox (account_id, draft_id, message_id, thread_id, subject, recipients, send_at)
    SELECT account_id, draft_id, message_id, thread_id, subject, recipients, send_at
    FROM scheduled;
DROP TABLE scheduled;
CREATE INDEX outbox_by_time ON outbox(send_at);
CREATE UNIQUE INDEX outbox_by_draft ON outbox(account_id, draft_id) WHERE draft_id IS NOT NULL;
"#,
    // What the headers say about where a message came from, for the
    // details panel. One column, since the three answers are read and
    // shown together and never queried on their own.
    r#"
ALTER TABLE bodies ADD COLUMN provenance TEXT;
"#,
];

/// Opens the database at `path`, creating it if needed, switches it to WAL,
/// and applies pending migrations.
pub fn open_connection(path: &Path) -> Result<Connection> {
    let mut conn = Connection::open(path)?;
    configure(&conn)?;
    conn.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get::<_, String>(0))?;
    migrate(&mut conn)?;
    Ok(conn)
}

/// A migrated in-memory database, for tests.
pub fn open_in_memory() -> Result<Connection> {
    let mut conn = Connection::open_in_memory()?;
    configure(&conn)?;
    migrate(&mut conn)?;
    Ok(conn)
}

pub fn schema_version(conn: &Connection) -> Result<i64> {
    Ok(conn.pragma_query_value(None, "user_version", |row| row.get(0))?)
}

pub(crate) fn configure(conn: &Connection) -> Result<()> {
    conn.busy_timeout(Duration::from_secs(5))?;
    conn.pragma_update(None, "foreign_keys", true)?;
    // 1 is NORMAL: with WAL, a crash can lose the last commit but never corrupts.
    conn.pragma_update(None, "synchronous", 1)?;
    Ok(())
}

fn migrate(conn: &mut Connection) -> Result<()> {
    let current = usize::try_from(schema_version(conn)?).unwrap_or(0);
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(current) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", (index + 1) as i64)?;
        tx.commit()?;
    }
    Ok(())
}
