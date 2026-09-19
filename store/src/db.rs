//! Async access to SQLite. One thread owns the only write connection and runs
//! each write in a transaction; reads use pooled read-only connections in
//! `spawn_blocking`. WAL mode lets reads run during a write.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};

use rusqlite::Connection;

use crate::schema::{configure, open_connection};
use crate::{Result, StoreError};

type Job = Box<dyn FnOnce(&mut Connection) + Send>;

/// Idle read connections kept open.
const READER_POOL: usize = 4;

#[derive(Clone)]
pub struct Db {
    inner: Arc<Inner>,
}

struct Inner {
    path: PathBuf,
    writer: mpsc::Sender<Job>,
    readers: Mutex<Vec<Connection>>,
}

impl Db {
    /// Opens and migrates the database, then starts the writer thread.
    pub fn open(path: &Path) -> Result<Db> {
        let mut writer = open_connection(path)?;
        let (sender, jobs) = mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("mailrs-db-writer".into())
            .spawn(move || {
                for job in jobs {
                    job(&mut writer);
                }
                // Records table statistics for what this session queried, so
                // the next run plans those queries from real row counts.
                let _ = writer.execute_batch("PRAGMA optimize");
            })
            .map_err(|_| StoreError::Closed)?;
        Ok(Db {
            inner: Arc::new(Inner {
                path: path.to_path_buf(),
                writer: sender,
                readers: Mutex::new(Vec::new()),
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

    /// Runs `f` on a read-only connection.
    pub async fn read<F, R>(&self, f: F) -> Result<R>
    where
        F: FnOnce(&Connection) -> Result<R> + Send + 'static,
        R: Send + 'static,
    {
        let inner = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let conn = inner.checkout()?;
            let result = f(&conn);
            inner.checkin(conn);
            result
        })
        .await
        .map_err(|_| StoreError::Closed)?
    }
}

impl Inner {
    fn checkout(&self) -> Result<Connection> {
        if let Some(conn) = self.readers.lock().expect("reader pool poisoned").pop() {
            return Ok(conn);
        }
        let conn = Connection::open(&self.path)?;
        configure(&conn)?;
        conn.pragma_update(None, "query_only", true)?;
        Ok(conn)
    }

    fn checkin(&self, conn: Connection) {
        let mut readers = self.readers.lock().expect("reader pool poisoned");
        if readers.len() < READER_POOL {
            readers.push(conn);
        }
    }
}
