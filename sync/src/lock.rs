//! One process syncs a store at a time. The app and `penguin-mail-cli sync`
//! both run a sync engine over the same SQLite file, and two engines would
//! replay the same history and fetch the same mail twice, each moving the
//! cursor under the other. Each takes an advisory lock on a file beside the
//! store before it starts, and the one that arrives second stops.

use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::path::Path;

/// The file beside the store that the lock is taken on.
pub const LOCK_FILE: &str = "mailrs.lock";

/// The lock on a store's folder. The operating system lets go of it when
/// this is dropped or the process ends, however it ends, so a crash never
/// leaves a store locked.
#[derive(Debug)]
pub struct SyncLock {
    _file: File,
}

/// Why the lock could not be taken.
#[derive(Debug, thiserror::Error)]
pub enum LockError {
    /// Another process syncs this store.
    #[error("another Penguin Mail is syncing this mail")]
    Held,
    #[error("could not open {LOCK_FILE}: {0}")]
    Io(#[from] io::Error),
}

impl SyncLock {
    /// Takes the lock on the store in `dir`, or says who has it. It never
    /// waits: a process that finds the lock taken stops rather than sync
    /// after the other one finishes.
    pub fn take(dir: &Path) -> Result<SyncLock, LockError> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join(LOCK_FILE))?;
        match file.try_lock() {
            Ok(()) => Ok(SyncLock { _file: file }),
            Err(TryLockError::WouldBlock) => Err(LockError::Held),
            Err(TryLockError::Error(err)) => Err(err.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_taker_is_refused_until_the_first_lets_go() {
        let dir = tempfile::tempdir().unwrap();
        let first = SyncLock::take(dir.path()).unwrap();
        assert!(dir.path().join(LOCK_FILE).exists());
        assert!(matches!(SyncLock::take(dir.path()), Err(LockError::Held)));
        drop(first);
        let again = SyncLock::take(dir.path());
        assert!(again.is_ok(), "{again:?}");
    }

    #[test]
    fn stores_in_different_folders_lock_apart() {
        let (one, two) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let _first = SyncLock::take(one.path()).unwrap();
        assert!(SyncLock::take(two.path()).is_ok());
    }

    #[test]
    fn a_folder_that_is_not_there_is_an_error_and_not_a_held_lock() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("gone");
        assert!(matches!(SyncLock::take(&missing), Err(LockError::Io(_))));
    }
}
