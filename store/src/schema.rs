//! Connection setup and migrations, tracked with `PRAGMA user_version`.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use rusqlite::Connection;
use rusqlite::functions::FunctionFlags;

use crate::{Result, StoreError};

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
    // Gmail's two names for one draft, so that opening a draft stops
    // paging `drafts.list` over the whole account to find its id. A
    // message belongs to one draft and a draft to one message, and the
    // unique index is what makes a draft edited elsewhere move its pair to
    // the new message rather than keep both.
    r#"
CREATE TABLE drafts (
    account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    message_id TEXT NOT NULL,
    draft_id   TEXT NOT NULL,
    PRIMARY KEY (account_id, message_id)
);
CREATE UNIQUE INDEX drafts_by_draft ON drafts(account_id, draft_id);
"#,
    // Whether the store holds every message of a thread. The window keeps
    // messages by date, so a thread in it can lack its older replies, and
    // opening one may trust the store only once a fetch brought it whole.
    r#"
ALTER TABLE threads ADD COLUMN whole INTEGER NOT NULL DEFAULT 0;
"#,
    // What a message's `List-Unsubscribe` and `List-Unsubscribe-Post`
    // headers said. The same two live on `bodies`, but a body arrives only
    // when someone opens the message, and the newsletters list has to
    // answer for hundreds of senders at once. Every metadata fetch now
    // carries them, so mail synced from here on answers for free. Mail
    // stored before this keeps `list_unsubscribe` empty, and
    // `mailrs_sync::Newsletters::list` fills those in.
    r#"
ALTER TABLE messages ADD COLUMN list_unsubscribe TEXT;
ALTER TABLE messages ADD COLUMN one_click INTEGER NOT NULL DEFAULT 0;
"#,
    // Bodies read while a Content-ID alone made a part an attachment.
    // LinkedIn gives its text and its HTML one each and no name, so those
    // messages were stored with no body and the two halves filed as
    // attachments. Dropping the cached body makes the next open fetch it
    // again and read it the way it is read now.
    r#"
DELETE FROM bodies WHERE (account_id, message_id) IN (
    SELECT account_id, message_id FROM attachments
    WHERE content_id IS NOT NULL AND attachment_id IS NULL
      AND (mime_type LIKE 'text/plain%' OR mime_type LIKE 'text/html%')
);
DELETE FROM attachments
    WHERE content_id IS NOT NULL AND attachment_id IS NULL
      AND (mime_type LIKE 'text/plain%' OR mime_type LIKE 'text/html%');
"#,
    // Indexes in the exact order a list is shown, tie-breakers included,
    // so a list of a label that holds most of the mail can walk rows
    // newest first and stop at a full page instead of sorting them all.
    // The recency index covered the date alone; this one replaces it.
    r#"
DROP INDEX IF EXISTS threads_by_recency;
CREATE INDEX IF NOT EXISTS threads_by_order ON threads(last_message_at DESC, account_id, id);
CREATE INDEX IF NOT EXISTS messages_by_order ON messages(date DESC, account_id, id);
"#,
    // Mail from given people, for the VIP mailboxes and their counts,
    // found without reading every message. The thread id makes the index
    // enough to say which threads they wrote in.
    r#"
CREATE INDEX IF NOT EXISTS messages_by_sender ON messages(lower(from_addr), account_id, thread_id);
"#,
    // When the sync engine last pruned the account and checked its inbox
    // against Gmail's. The app restarts itself to give memory back after
    // its window closes, and without this each restart ran both again.
    r#"
ALTER TABLE accounts ADD COLUMN checked_at INTEGER;
"#,
    // When a caller took a waiting message to send it. The Outbox window
    // and the pass that sends what is due can reach the same message
    // together, and whoever writes the row first is the one that sends
    // it. A claim older than a few minutes belongs to a run that died
    // mid-send. A table of its own rather than a column, so running this
    // again over a database that has it changes nothing.
    r#"
CREATE TABLE IF NOT EXISTS outbox_claims (
    id         INTEGER PRIMARY KEY REFERENCES outbox(id) ON DELETE CASCADE,
    claimed_at INTEGER NOT NULL
);
"#,
    // Which Google client each account signed in with. A new account signs
    // in with the client compiled into the build, so that is the default;
    // every account already here came through the setup page, where the
    // person pasted a client of their own.
    r#"
ALTER TABLE accounts ADD COLUMN oauth_client TEXT NOT NULL DEFAULT 'built_in';
UPDATE accounts SET oauth_client = 'own';
"#,
    // Labels become server mailboxes with integer keys, keywords and
    // categories; history_id moves into sync_state. See the file.
    include_str!("schema/26-mailboxes.sql"),
    // Bodies decoded before the bytes won over the declared charset. A
    // mailer that sends UTF-8 under a windows-1252 label came out as
    // "DireÃ§Ã£o", and the cache kept that text for good. The patterns
    // are UTF-8 sequences read as windows-1252: a lead byte for two, three
    // or four bytes, then that many continuation bytes less one. Dropping
    // those bodies makes the next open fetch them again. A body that only
    // looks like this is fetched once more and loses nothing.
    r#"
DELETE FROM bodies WHERE EXISTS (
    SELECT 1 FROM (
        SELECT '[' || char(128) || '-¿€‚ƒ„…†‡ˆ‰Š‹ŒŽ‘’“”•–—˜™š›œžŸ]' AS c
    )
    WHERE bodies.html GLOB '*[Â-ß]' || c || '*'
       OR bodies.html GLOB '*[à-ï]' || c || c || '*'
       OR bodies.html GLOB '*[ð-ô]' || c || c || c || '*'
       OR bodies.text GLOB '*[Â-ß]' || c || '*'
       OR bodies.text GLOB '*[à-ï]' || c || c || '*'
       OR bodies.text GLOB '*[ð-ô]' || c || c || c || '*'
);
"#,
    // Gmail's API sends Google Calendar's invitation part by attachment
    // id, and bodies read before the sync layer fetched it hold no
    // calendar, so the invitation card never showed. Dropping the bodies
    // that carry a calendar file but no calendar makes the next open fetch
    // the part.
    r#"
DELETE FROM bodies WHERE calendar IS NULL AND EXISTS (
    SELECT 1 FROM attachments a
    WHERE a.account_id = bodies.account_id AND a.message_id = bodies.message_id
      AND (lower(a.mime_type) LIKE 'text/calendar%'
           OR lower(a.mime_type) = 'application/ics'
           OR lower(a.filename) LIKE '%.ics')
);
"#,
    // The lists the person left, by the sender's lower-case address, so
    // a conversation from a list already left stops offering Unsubscribe
    // and the assistant can say when and how they left it. `how` is
    // `one_click`, `page` or `email`.
    r#"
CREATE TABLE unsubscribes (
    account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    sender     TEXT NOT NULL,
    how        TEXT NOT NULL,
    left_at    INTEGER NOT NULL,
    PRIMARY KEY (account_id, sender)
);
"#,
    // Local threading, for a server that keeps no threads. Each message
    // threaded here records the Message-IDs it names in References and
    // In-Reply-To, and its subject's base. The indexes cover only those
    // messages, so a Gmail store pays nothing for them.
    r#"
CREATE TABLE message_links (
    account_id INTEGER NOT NULL,
    message_id TEXT NOT NULL,
    msgid      TEXT NOT NULL,
    PRIMARY KEY (account_id, message_id, msgid),
    FOREIGN KEY (account_id, message_id) REFERENCES messages(account_id, id) ON DELETE CASCADE
);
-- Covers the join back to messages, so a lookup by msgid never returns
-- to the table for the message_id it already has.
CREATE INDEX message_links_by_msgid ON message_links(account_id, msgid, message_id);
ALTER TABLE messages ADD COLUMN base_subject TEXT;
CREATE INDEX messages_local_by_msgid ON messages(account_id, rfc822_msgid)
    WHERE base_subject IS NOT NULL;
CREATE INDEX messages_local_by_subject ON messages(account_id, base_subject, date)
    WHERE base_subject IS NOT NULL;
"#,
    // Both body paths now name each file by its MIME part path. Files
    // read before were named by Gmail attachment handles, which neither
    // path answers, so the bodies that carry files go with their file rows
    // and come back on the next open. Bodies without files stay.
    r#"
DELETE FROM bodies WHERE EXISTS (
    SELECT 1 FROM attachments a
    WHERE a.account_id = bodies.account_id AND a.message_id = bodies.message_id
);
DELETE FROM attachments;
"#,
    // IMAP accounts. `provider_name` is who runs the server as the person
    // knows them, such as Fastmail; a Gmail account leaves it empty. Each
    // IMAP account has one incoming and one outgoing server. The password
    // lives in the keyring and never here, and `pinned_certificate` waits
    // for a server with a certificate of its own, such as Proton Mail
    // Bridge.
    r#"
ALTER TABLE accounts ADD COLUMN provider_name TEXT;
CREATE TABLE account_servers (
    account_id         INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    role               TEXT NOT NULL CHECK (role IN ('imap', 'smtp')),
    host               TEXT NOT NULL,
    port               INTEGER NOT NULL,
    security           TEXT NOT NULL CHECK (security IN ('tls', 'starttls')),
    user_name          TEXT NOT NULL,
    pinned_certificate BLOB,
    PRIMARY KEY (account_id, role)
);
-- An IMAP folder's listing looks up its stored messages by location.
-- Gmail rows carry no mailbox, so the index leaves them out.
CREATE INDEX remote_refs_by_location ON remote_refs(account_id, mailbox, uidvalidity, uid)
    WHERE mailbox IS NOT NULL;
"#,
];

/// How long the copy taken before a migration stays once the store has
/// opened on the new version.
const KEEP_COPY_FOR: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Opens the database at `path`, creating it if needed, switches it to WAL,
/// applies pending migrations, and brings the planner's statistics up to
/// date. Before migrating a store that holds mail it copies it next to
/// itself, and puts the copy back if the migration fails.
pub fn open_connection(path: &Path) -> Result<Connection> {
    open_with(path, MIGRATIONS)
}

/// `open_connection` up to the end of `migrations`, so a test can hand it
/// one that fails.
fn open_with(path: &Path, migrations: &[&str]) -> Result<Connection> {
    let mut conn = Connection::open(path)?;
    configure(&conn)?;
    conn.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get::<_, String>(0))?;
    let current = usize::try_from(schema_version(&conn)?).unwrap_or(0);
    if current == 0 || current >= migrations.len() {
        // A new store has nothing to lose, and one already up to date
        // has nothing to migrate.
        migrate(&mut conn, migrations)?;
        forget_old_copies(path, SystemTime::now());
    } else {
        let copy = copy_path(path, migrations.len());
        safety_copy(&conn, &copy)?;
        if let Err(err) = migrate(&mut conn, migrations) {
            drop(conn);
            let reason = match restore(path, &copy) {
                Ok(()) => err.to_string(),
                Err(restore) => format!("{err}; putting the copy back failed too: {restore}"),
            };
            return Err(StoreError::Migration {
                version: migrations.len(),
                kept: copy,
                reason,
            });
        }
    }
    // 0x10000 looks at every table rather than the ones this connection
    // has queried, which on opening is none; 0x02 analyzes the ones whose
    // statistics are missing or stale. SQLite caps each analysis, so this
    // costs milliseconds and does nothing once the statistics are current.
    conn.execute_batch("PRAGMA optimize=0x10002")?;
    Ok(conn)
}

/// Where the copy taken before migrating to `version` goes: beside the
/// store, as `mailrs.db.before-26`.
fn copy_path(path: &Path, version: usize) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".before-{version}"));
    path.with_file_name(name)
}

/// Copies the store into `copy` as one file, write-ahead log included. A
/// copy an earlier failed attempt left is the same store, since that
/// attempt put it back, so it is replaced.
fn safety_copy(conn: &Connection, copy: &Path) -> Result<()> {
    let target = copy
        .to_str()
        .ok_or_else(|| StoreError::SafetyCopy(format!("{} is not UTF-8", copy.display())))?;
    remove_if_present(copy).map_err(|err| StoreError::SafetyCopy(err.to_string()))?;
    conn.execute("VACUUM INTO ?1", [target])
        .map_err(|err| StoreError::SafetyCopy(err.to_string()))?;
    Ok(())
}

/// Puts `copy` back over the store. The write-ahead log and its index
/// belong to the store the failed migration left, so they go first.
fn restore(path: &Path, copy: &Path) -> io::Result<()> {
    for suffix in ["-wal", "-shm"] {
        let mut side = path.as_os_str().to_os_string();
        side.push(suffix);
        remove_if_present(Path::new(&side))?;
    }
    std::fs::copy(copy, path).map(drop)
}

fn remove_if_present(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
        _ => Ok(()),
    }
}

/// Removes the copies taken before earlier migrations once they are a
/// week old, since the store has opened on the new version at every start
/// since. A copy that cannot be read or removed stays, which costs disk
/// space and nothing else.
fn forget_old_copies(path: &Path, now: SystemTime) {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return;
    };
    let prefix = format!("{}.before-", name.to_string_lossy());
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry.file_name().to_string_lossy().starts_with(&prefix) {
            continue;
        }
        let aged = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|at| now.duration_since(at).ok())
            .is_some_and(|age| age > KEEP_COPY_FOR);
        if aged {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Refreshes the planner's statistics for the tables this connection has
/// used, when their row counts have moved far enough to matter.
pub(crate) fn optimize(conn: &Connection) -> Result<()> {
    conn.execute_batch("PRAGMA optimize")?;
    Ok(())
}

/// A migrated in-memory database, for tests.
pub fn open_in_memory() -> Result<Connection> {
    let mut conn = Connection::open_in_memory()?;
    configure(&conn)?;
    migrate(&mut conn, MIGRATIONS)?;
    Ok(conn)
}

pub fn schema_version(conn: &Connection) -> Result<i64> {
    Ok(conn.pragma_query_value(None, "user_version", |row| row.get(0))?)
}

/// The size in bytes the write-ahead log is cut back to after a checkpoint.
const JOURNAL_SIZE_LIMIT: i64 = 16 << 20;

pub(crate) fn configure(conn: &Connection) -> Result<()> {
    conn.busy_timeout(Duration::from_secs(5))?;
    conn.pragma_update(None, "foreign_keys", true)?;
    // 1 is NORMAL: with WAL, a crash can lose the last commit but never corrupts.
    conn.pragma_update(None, "synchronous", 1)?;
    // A bootstrap can grow the write-ahead log by hundreds of megabytes,
    // and SQLite keeps the file at its largest size unless told a limit
    // to cut it back to after a checkpoint.
    conn.pragma_update(None, "journal_size_limit", JOURNAL_SIZE_LIMIT)?;
    // SQLite's `lower` folds ASCII letters alone, so a search for "élia"
    // would miss "Élia". Query trees fold text through this instead.
    conn.create_scalar_function(
        crate::query::FOLD,
        1,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| Ok(ctx.get::<Option<String>>(0)?.unwrap_or_default().to_lowercase()),
    )?;
    Ok(())
}

fn migrate(conn: &mut Connection, migrations: &[&str]) -> Result<()> {
    let current = usize::try_from(schema_version(conn)?).unwrap_or(0);
    for (index, sql) in migrations.iter().enumerate().skip(current) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", (index + 1) as i64)?;
        tx.commit()?;
    }
    Ok(())
}

#[cfg(test)]
mod migration_tests;
