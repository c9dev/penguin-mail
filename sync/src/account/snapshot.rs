//! A list of message ids kept in a temporary SQLite file rather than in
//! memory, so an action over a whole mailbox holds one batch at a time.
//! Empty Trash and Mark All as Read list into one before they change
//! anything, and read it back a batch at a time.

use std::sync::Mutex;

use mailrs_domain::{AccountId, Target};
use mailrs_store::{Result as StoreResult, StoreError};
use rusqlite::{Connection, params};

use crate::RemoteRef;

/// SQLite's unnamed temporary database is deleted on close, including on cancellation.
pub(crate) struct Snapshot(Mutex<Connection>);

impl Snapshot {
    pub(crate) fn new() -> StoreResult<Self> {
        let c = Connection::open("")?;
        c.execute_batch("PRAGMA temp_store = FILE; PRAGMA cache_size = -1024;
            CREATE TABLE refs (seq INTEGER PRIMARY KEY, id TEXT UNIQUE, thread TEXT, checked INTEGER DEFAULT 0);
            CREATE TABLE cursors (value TEXT PRIMARY KEY);")?;
        Ok(Self(Mutex::new(c)))
    }

    pub(crate) fn append(&self, refs: &[RemoteRef]) -> StoreResult<()> {
        let mut c = self.0.lock().expect("snapshot poisoned");
        let tx = c.transaction()?;
        for hit in refs {
            tx.execute(
                "INSERT OR IGNORE INTO refs (id, thread) VALUES (?1, ?2)",
                params![hit.id, hit.thread_id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn local(
        &self,
        source: &Connection,
        account: AccountId,
        trash: &str,
    ) -> StoreResult<()> {
        let mut c = self.0.lock().expect("snapshot poisoned");
        let tx = c.transaction()?;
        let mut query = source.prepare(
            "SELECT m.id, m.thread_id FROM messages m
            JOIN message_mailboxes f ON f.account_id = m.account_id AND f.message_id = m.id
            JOIN mailboxes b ON b.key = f.mailbox WHERE m.account_id = ?1 AND b.id = ?2",
        )?;
        let mut rows = query.query(params![account, trash])?;
        while let Some(row) = rows.next()? {
            tx.execute(
                "INSERT OR IGNORE INTO refs (id, thread) VALUES (?1, ?2)",
                params![row.get::<_, String>(0)?, row.get::<_, String>(1)?],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn cursor(&self, cursor: &str) -> StoreResult<bool> {
        Ok(self
            .0
            .lock()
            .expect("snapshot poisoned")
            .execute("INSERT OR IGNORE INTO cursors VALUES (?1)", [cursor])?
            != 0)
    }

    pub(crate) fn batch(
        &self,
        after: i64,
        limit: usize,
        checked: bool,
    ) -> StoreResult<(i64, Vec<RemoteRef>)> {
        let c = self.0.lock().expect("snapshot poisoned");
        let mut query = c.prepare("SELECT seq, id, thread FROM refs WHERE seq > ?1 AND (?2 = 0 OR checked = 1) ORDER BY seq LIMIT ?3")?;
        let mut rows = query.query(params![after, checked, limit as i64])?;
        let mut refs = Vec::new();
        let mut last = after;
        while let Some(row) = rows.next()? {
            last = row.get(0)?;
            refs.push(RemoteRef {
                id: row.get(1)?,
                thread_id: row.get(2)?,
            });
        }
        Ok((last, refs))
    }

    pub(crate) fn checked(&self, targets: &[Target]) -> StoreResult<()> {
        let mut c = self.0.lock().expect("snapshot poisoned");
        let tx = c.transaction()?;
        for target in targets {
            let id = target.message_id.as_ref().expect("message target");
            tx.execute("UPDATE refs SET checked = 1 WHERE id = ?1", [id])?;
        }
        tx.commit().map_err(StoreError::from)
    }

    /// Drops the rows of `erased`: the one message a target names, or its
    /// whole thread when it names none. Says whether a checked row is left.
    pub(crate) fn forget<'a>(&self, erased: impl Iterator<Item = &'a Target>) -> StoreResult<bool> {
        let mut c = self.0.lock().expect("snapshot poisoned");
        let tx = c.transaction()?;
        for target in erased {
            match &target.message_id {
                Some(id) => tx.execute("DELETE FROM refs WHERE id = ?1", [id])?,
                None => tx.execute("DELETE FROM refs WHERE thread = ?1", [&target.thread_id])?,
            };
        }
        tx.commit()?;
        Ok(c.query_row(
            "SELECT EXISTS(SELECT 1 FROM refs WHERE checked = 1)",
            [],
            |row| row.get(0),
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_large_snapshot_deduplicates_and_reads_only_one_batch() {
        let snapshot = Snapshot::new().unwrap();
        for page in 0..50 {
            let refs: Vec<_> = (page * 500..(page + 1) * 500)
                .map(|n| RemoteRef {
                    id: format!("m{n}"),
                    thread_id: format!("t{n}"),
                })
                .collect();
            snapshot.append(&refs).unwrap();
            snapshot.append(&refs).unwrap();
        }
        assert!(snapshot.cursor("page-2").unwrap());
        assert!(!snapshot.cursor("page-2").unwrap());
        let mut after = 0;
        let mut count = 0;
        loop {
            let (last, refs) = snapshot.batch(after, 100, false).unwrap();
            if refs.is_empty() {
                break;
            }
            assert!(refs.len() <= 100);
            count += refs.len();
            after = last;
        }
        assert_eq!(count, 25_000);
    }
}
