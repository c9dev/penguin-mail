//! The folder a demo run keeps its store and settings in.
//!
//! Each run gets a folder of its own, named for its process, and removes
//! it when the core closes. A run that dies before that leaves its folder
//! behind, so the next run sweeps every demo folder whose process is gone.
//! The temporary directory is often in memory (tmpfs), so a folder left
//! there costs RAM until the next reboot.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

const PREFIX: &str = "penguin-mail-demo-";

/// Tells apart the folders of cores opened one after another in one
/// process, as the tests do.
static OPENED: AtomicUsize = AtomicUsize::new(0);

/// A demo run's folder, removed with everything in it on drop.
pub struct DemoFolder {
    path: PathBuf,
}

impl DemoFolder {
    /// A new, empty folder in the system's temporary directory, after
    /// sweeping away what dead demo runs left there.
    pub fn make() -> io::Result<DemoFolder> {
        DemoFolder::make_in(&std::env::temp_dir())
    }

    fn make_in(base: &Path) -> io::Result<DemoFolder> {
        sweep(base);
        let n = OPENED.fetch_add(1, Ordering::Relaxed);
        let path = base.join(format!("{PREFIX}{}-{n}", std::process::id()));
        // A folder by this name can only be a leftover from an earlier
        // process that had the same id, so its store is stale.
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path)?;
        Ok(DemoFolder { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for DemoFolder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Removes the demo folders and settings files in `base` whose process
/// no longer runs. Older builds named them `penguin-mail-demo-<pid>` and
/// `penguin-mail-demo-<pid>-settings.toml`; this build uses
/// `penguin-mail-demo-<pid>-<n>`. All three start with the process id.
fn sweep(base: &Path) {
    let Ok(entries) = std::fs::read_dir(base) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(owner) else {
            continue;
        };
        if Path::new("/proc").join(pid.to_string()).exists() {
            continue;
        }
        let path = entry.path();
        let _ = if path.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
    }
}

/// The process id a demo file or folder name starts with.
fn owner(name: &str) -> Option<u32> {
    let rest = name.strip_prefix(PREFIX)?;
    let digits = rest.split('-').next()?;
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Above Linux's highest process id, so never a running process.
    const DEAD: u32 = 999_999_999;

    #[test]
    fn each_core_gets_its_own_folder_and_takes_it_away() {
        let base = tempfile::tempdir().unwrap();
        let first = DemoFolder::make_in(base.path()).unwrap();
        let second = DemoFolder::make_in(base.path()).unwrap();
        assert_ne!(first.path(), second.path());
        assert!(first.path().is_dir() && second.path().is_dir());

        let gone = first.path().to_path_buf();
        drop(first);
        assert!(!gone.exists());
        assert!(second.path().is_dir());
    }

    /// The failure this replaces: a store deleted without its write-ahead
    /// log, whose old pages SQLite then read into the new store as
    /// corruption.
    #[test]
    fn a_stale_folder_from_the_same_process_id_starts_empty() {
        let base = tempfile::tempdir().unwrap();
        let n = OPENED.load(Ordering::Relaxed);
        let stale = base.path().join(format!("{PREFIX}{}-{n}", std::process::id()));
        std::fs::create_dir_all(&stale).unwrap();
        std::fs::write(stale.join("mailrs.db-wal"), b"old pages").unwrap();

        let folder = DemoFolder::make_in(base.path()).unwrap();
        assert_eq!(std::fs::read_dir(folder.path()).unwrap().count(), 0);
    }

    #[test]
    fn a_dead_run_s_leftovers_go_and_a_live_run_s_stay() {
        let base = tempfile::tempdir().unwrap();
        let dead_new = base.path().join(format!("{PREFIX}{DEAD}-0"));
        let dead_old = base.path().join(format!("{PREFIX}{DEAD}"));
        let dead_settings = base.path().join(format!("{PREFIX}{DEAD}-settings.toml"));
        let live = base.path().join(format!("{PREFIX}{}-other", std::process::id()));
        let unrelated = base.path().join("penguin-mail-demonstration");
        for dir in [&dead_new, &dead_old, &live, &unrelated] {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(dir.join("mailrs.db"), b"").unwrap();
        }
        std::fs::write(&dead_settings, b"").unwrap();

        let _folder = DemoFolder::make_in(base.path()).unwrap();

        assert!(!dead_new.exists());
        assert!(!dead_old.exists());
        assert!(!dead_settings.exists());
        assert!(live.is_dir());
        assert!(unrelated.is_dir());
    }
}
