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
