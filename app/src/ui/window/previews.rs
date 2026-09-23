//! Scratch copies of attachments, which Quick Look and the desktop's own
//! programs open.
//!
//! A copy can hold what an encrypted message kept from Gmail, so only the
//! person may read it: the folder is 0700 and every file in it 0600. A
//! copy goes when its Quick Look window closes. One handed to another
//! program has no window to wait on, so the copies left from an earlier
//! run go when the next one starts, and a copy of a decrypted file goes
//! when the main window closes, whatever still has it open.

use std::cell::RefCell;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// The folder under the cache the copies go in.
pub(super) fn folder() -> PathBuf {
    gtk::glib::user_cache_dir()
        .join("penguin-mail")
        .join("previews")
}

/// Writes `data` under `dir` as `name`, or `name (2)` and so on when that
/// is taken, readable by the person alone. Runs on any thread.
pub(super) fn write(dir: &Path, name: &str, data: &[u8]) -> io::Result<PathBuf> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    // A folder an older version made carries the umask's permissions.
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    loop {
        let path = super::unique_path(dir, name);
        let opened = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path);
        match opened {
            Ok(mut file) => {
                file.write_all(data)?;
                return Ok(path);
            }
            // Another copy took the name between the look and the write.
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }
}

/// Deletes every copy in `dir`, which only an earlier run can have left.
pub(super) fn sweep(dir: &Path) -> io::Result<()> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };
    for entry in entries {
        let path = entry?.path();
        if path.is_file() {
            fs::remove_file(&path)?;
        }
    }
    Ok(())
}

/// The copies this run wrote, and which of them came out of encrypted
/// mail.
#[derive(Debug, Default)]
pub(super) struct Previews {
    kept: RefCell<Vec<(PathBuf, bool)>>,
}

impl Previews {
    /// Notes a copy written at `path`. `decrypted` marks one whose bytes
    /// Gmail never held.
    pub(super) fn kept(&self, path: PathBuf, decrypted: bool) {
        self.kept.borrow_mut().push((path, decrypted));
    }

    /// Deletes the copy at `path`, once the window showing it has closed.
    pub(super) fn forget(&self, path: &Path) {
        self.kept.borrow_mut().retain(|(held, _)| held != path);
        remove(path);
    }

    /// Deletes every decrypted copy, as the window that opened them
    /// closes. The rest wait for the next start.
    pub(super) fn forget_decrypted(&self) {
        self.kept.borrow_mut().retain(|(path, decrypted)| {
            if *decrypted {
                remove(path);
            }
            !decrypted
        });
    }
}

fn remove(path: &Path) {
    if let Err(err) = fs::remove_file(path)
        && err.kind() != io::ErrorKind::NotFound
    {
        tracing::warn!(error = %err, "could not delete an attachment preview");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn only_the_person_can_read_a_preview() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("previews");
        // An older version made the folder with the umask's permissions.
        fs::create_dir(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        let path = write(&dir, "salary.pdf", b"secret").unwrap();
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&path), 0o600);
        assert_eq!(fs::read(&path).unwrap(), b"secret");
    }

    #[test]
    fn a_second_preview_of_the_same_name_gets_a_name_of_its_own() {
        let home = tempfile::tempdir().unwrap();
        let first = write(home.path(), "a.png", b"1").unwrap();
        let second = write(home.path(), "a.png", b"2").unwrap();
        assert_ne!(first, second);
        assert_eq!(fs::read(&first).unwrap(), b"1");
        assert_eq!(fs::read(&second).unwrap(), b"2");
    }

    #[test]
    fn a_start_deletes_what_the_last_run_left_behind() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("previews");
        let left = write(&dir, "old.jpg", b"x").unwrap();
        sweep(&dir).unwrap();
        assert!(!left.exists());
        assert!(dir.exists(), "the folder stays for the next preview");
        sweep(&home.path().join("never-made")).unwrap();
    }

    #[test]
    fn a_preview_goes_when_its_window_closes() {
        let home = tempfile::tempdir().unwrap();
        let previews = Previews::default();
        let path = write(home.path(), "photo.jpg", b"x").unwrap();
        previews.kept(path.clone(), false);
        previews.forget(&path);
        assert!(!path.exists());
    }

    #[test]
    fn decrypted_previews_go_with_the_window_and_the_rest_wait() {
        let home = tempfile::tempdir().unwrap();
        let previews = Previews::default();
        let secret = write(home.path(), "contract.pdf", b"s").unwrap();
        let plain = write(home.path(), "menu.pdf", b"p").unwrap();
        previews.kept(secret.clone(), true);
        previews.kept(plain.clone(), false);
        previews.forget_decrypted();
        assert!(!secret.exists());
        assert!(plain.exists(), "another program may still have it open");
    }
}
