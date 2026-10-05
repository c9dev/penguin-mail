//! Async access to SQLite. One thread owns the only write connection and runs
//! each write in a transaction; reads use pooled read-only connections in
//! `spawn_blocking`. WAL mode lets reads run during a write.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, mpsc};

use rusqlite::Connection;

use crate::schema::{configure, open_connection, optimize};
use crate::{Result, StoreError};

type Job = Box<dyn FnOnce(&mut Connection) + Send>;

/// Rows the writer changes between refreshes of the planner's statistics.
/// A bootstrap passes it many times over; a day of history replay may not.
const OPTIMIZE_AFTER: u64 = 10_000;

/// Read connections open at once. A read that finds them all busy waits
/// for one, on its blocking thread, rather than opening another: a burst
/// of reads would otherwise open a connection each, and SQLite gives each
/// its own page cache.
const READER_POOL: usize = 4;

#[derive(Clone)]
pub struct Db {
    inner: Arc<Inner>,
}

struct Inner {
    path: PathBuf,
    writer: mpsc::Sender<Job>,
    readers: Mutex<Readers>,
    /// Signalled when a read gives its connection back.
    returned: Condvar,
}

/// The read connections: the idle ones, and how many are open in all.
#[derive(Default)]
struct Readers {
    idle: Vec<Connection>,
    open: usize,
}

impl Db {
    /// Opens and migrates the database, then starts the writer thread.
    pub fn open(path: &Path) -> Result<Db> {
        let mut writer = open_connection(path)?;
        let (sender, jobs) = mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("mailrs-db-writer".into())
            .spawn(move || {
                let mut analyzed_at = writer.total_changes();
                for job in jobs {
                    job(&mut writer);
                    // Statistics taken while a table was empty would steer
                    // the planner long after a bootstrap filled it.
                    if writer.total_changes() - analyzed_at >= OPTIMIZE_AFTER {
                        analyzed_at = writer.total_changes();
                        // A failed refresh keeps the old statistics, which
                        // costs speed and nothing else.
                        let _ = optimize(&writer);
                    }
                }
                // Records table statistics for what this session queried, so
                // the next run plans those queries from real row counts.
                let _ = optimize(&writer);
            })
            .map_err(|_| StoreError::Closed)?;
        Ok(Db {
            inner: Arc::new(Inner {
                path: path.to_path_buf(),
                writer: sender,
                readers: Mutex::default(),
                returned: Condvar::new(),
            }),
        })
    }

    /// Runs `f` in a transaction on the writer thread. An `Err` rolls it back.
    pub async fn write<F, R>(&self, f: F) -> Result<R>
    where
        F: FnOnce(&Connection) -> Result<R> + Send + 'static,
        R: Send + 'static,
    {
        let (done, result) = tokio::sync::oneshot::channel();
        let job: Job = Box::new(move |conn| {
            let outcome = (|| -> Result<R> {
                let tx = conn.transaction()?;
                let value = f(&tx)?;
                tx.commit()?;
                Ok(value)
            })();
            let _ = done.send(outcome);
        });
        self.inner
            .writer
            .send(job)
            .map_err(|_| StoreError::Closed)?;
        result.await.map_err(|_| StoreError::Closed)?
    }

    /// Copies every commit so far from the write-ahead log into the
    /// database file and syncs both, on the writer thread. True when the
    /// whole log went in; false when a reader held an older snapshot past
    /// the busy timeout and some commits are still only in the log.
    ///
    /// With WAL and `synchronous = NORMAL` a commit is not synced, so a
    /// power cut can lose it. A caller about to delete the only other copy
    /// of what it stored (a POP3 DELE) calls this first. A checkpoint
    /// covers every earlier commit at once. Switching one write to
    /// `synchronous = FULL` would sync the log only if that write changed
    /// a row, which the write before a DELE need not.
    pub async fn checkpoint(&self) -> Result<bool> {
        let (done, result) = tokio::sync::oneshot::channel();
        let job: Job = Box::new(move |conn| {
            let outcome = conn
                .query_row("PRAGMA wal_checkpoint(FULL)", [], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                })
                .map(|(busy, log, copied)| busy == 0 && log == copied)
                .map_err(StoreError::from);
            let _ = done.send(outcome);
        });
        self.inner
            .writer
            .send(job)
            .map_err(|_| StoreError::Closed)?;
        result.await.map_err(|_| StoreError::Closed)?
    }

    /// Runs `f` on a read-only connection.
    pub async fn read<F, R>(&self, f: F) -> Result<R>
    where
        F: FnOnce(&Connection) -> Result<R> + Send + 'static,
        R: Send + 'static,
    {
        let inner = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let lease = inner.checkout()?;
            f(&lease)
        })
        .await
        .map_err(|_| StoreError::Closed)?
    }
}

impl Inner {
    /// An idle connection, a new one while fewer than [`READER_POOL`] are
    /// open, or else the next one a read gives back.
    fn checkout(&self) -> Result<Lease<'_>> {
        let mut readers = self.readers.lock().expect("reader pool poisoned");
        loop {
            if let Some(conn) = readers.idle.pop() {
                return Ok(Lease {
                    inner: self,
                    conn: Some(conn),
                });
            }
            if readers.open < READER_POOL {
                readers.open += 1;
                drop(readers);
                return match self.open_reader() {
                    Ok(conn) => Ok(Lease {
                        inner: self,
                        conn: Some(conn),
                    }),
                    Err(err) => {
                        self.readers.lock().expect("reader pool poisoned").open -= 1;
                        self.returned.notify_one();
                        Err(err)
                    }
                };
            }
            readers = self.returned.wait(readers).expect("reader pool poisoned");
        }
    }

    fn open_reader(&self) -> Result<Connection> {
        let conn = Connection::open(&self.path)?;
        configure(&conn)?;
        conn.pragma_update(None, "query_only", true)?;
        Ok(conn)
    }
}

/// A read connection out of the pool. Dropping it gives the connection
/// back, also when the read panics, so a failed read never shrinks the
/// pool.
struct Lease<'a> {
    inner: &'a Inner,
    conn: Option<Connection>,
}

impl std::ops::Deref for Lease<'_> {
    type Target = Connection;

    fn deref(&self) -> &Connection {
        self.conn
            .as_ref()
            .expect("a lease holds its connection until dropped")
    }
}

impl Drop for Lease<'_> {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            // Panicking here while a failed read unwinds would abort the
            // process, so a poisoned lock is used as it stands.
            let mut readers = match self.inner.readers.lock() {
                Ok(readers) => readers,
                Err(poisoned) => poisoned.into_inner(),
            };
            readers.idle.push(conn);
            self.inner.returned.notify_one();
        }
    }
}
