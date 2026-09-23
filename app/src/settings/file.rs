//! The settings file on disk. A save writes a new file beside the old one
//! and renames it into place, so a crash or a full disk leaves the old
//! preferences rather than half of the new ones. A file that no longer
//! parses is moved aside under a dated name before anything saves over it,
//! and the app says where it went.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::Settings;

/// What opening the settings file found.
#[derive(Debug)]
pub struct Opened {
    pub settings: Settings,
    /// Where an unreadable file went, which the person should hear about
    /// once. The settings are the defaults when this is set.
    pub broken: Option<PathBuf>,
}

/// Reads the settings, moving a file that does not parse to
/// `settings.toml.broken-<seconds>` so the next save cannot overwrite what
/// the person had. A missing file gives the defaults.
pub fn open(path: &Path) -> Opened {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(_) => {
            return Opened {
                settings: Settings::default(),
                broken: None,
            };
        }
    };
    match toml::from_str(&text) {
        Ok(settings) => Opened {
            settings,
            broken: None,
        },
        Err(err) => {
            let seconds = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            let aside = aside_name(path, seconds);
            tracing::warn!(path = %path.display(), aside = %aside.display(), error = %err,
                "settings do not parse; keeping them aside and starting from the defaults");
            let broken = match std::fs::rename(path, &aside) {
                Ok(()) => Some(aside),
                Err(err) => {
                    tracing::warn!(error = %err, "could not move the unreadable settings aside");
                    None
                }
            };
            Opened {
                settings: Settings::default(),
                broken,
            }
        }
    }
}

fn aside_name(path: &Path, seconds: u64) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".broken-{seconds}"));
    path.with_file_name(name)
}

/// Replaces `path` with `text` in one step: a temporary file in the same
/// folder, flushed to disk, then renamed over the old one.
pub fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    let folder = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(folder)?;
    let mut name = std::ffi::OsString::from(".");
    name.push(path.file_name().unwrap_or_default());
    name.push(format!(".{}.tmp", std::process::id()));
    let temporary = folder.join(name);
    let written = (|| {
        let mut file = std::fs::File::create(&temporary)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    written?;
    // The rename itself lives in the folder, so the folder is flushed too.
    if let Ok(dir) = std::fs::File::open(folder) {
        let _ = dir.sync_all();
    }
    Ok(())
}

enum Job {
    Write(Box<Settings>),
    Flush(mpsc::Sender<()>),
}

/// Writes the settings on a thread of its own, so a save never holds up
/// the window. Saves land in the order they were asked for, and a burst of
/// them writes only the newest.
pub struct Saver {
    jobs: mpsc::Sender<Job>,
}

impl Saver {
    pub fn new(path: PathBuf) -> Saver {
        let (jobs, queue) = mpsc::channel::<Job>();
        let spawned = std::thread::Builder::new()
            .name("settings-saver".into())
            .spawn(move || {
                while let Ok(job) = queue.recv() {
                    let mut newest = None;
                    let mut waiting = Vec::new();
                    let mut next = Some(job);
                    while let Some(job) = next {
                        match job {
                            Job::Write(settings) => newest = Some(settings),
                            Job::Flush(done) => waiting.push(done),
                        }
                        next = queue.try_recv().ok();
                    }
                    if let Some(settings) = newest
                        && let Err(err) = settings.save(&path)
                    {
                        tracing::warn!(error = %err, "could not save preferences");
                    }
                    for done in waiting {
                        let _ = done.send(());
                    }
                }
            });
        if let Err(err) = spawned {
            tracing::warn!(error = %err, "could not start the settings saver");
        }
        Saver { jobs }
    }

    /// Queues `settings` to be written.
    pub fn save(&self, settings: &Settings) {
        let _ = self.jobs.send(Job::Write(Box::new(settings.clone())));
    }

    /// Waits, up to two seconds, for every queued save to reach the disk.
    /// The app calls it on the way out, since the thread dies with it.
    pub fn flush(&self) {
        let (done, wait) = mpsc::channel();
        if self.jobs.send(Job::Flush(done)).is_ok() {
            let _ = wait.recv_timeout(Duration::from_secs(2));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::TextSize;

    fn files_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn a_save_replaces_the_file_and_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        std::fs::write(&path, "# a much longer file than the new one\n".repeat(200)).unwrap();
        let settings = Settings {
            threading: false,
            ..Settings::default()
        };
        settings.save(&path).unwrap();
        assert_eq!(files_in(dir.path()), ["settings.toml"]);
        assert_eq!(Settings::load(&path), settings);
    }

    #[test]
    fn a_broken_file_is_kept_aside_and_not_saved_over() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        std::fs::write(&path, "threading = maybe").unwrap();
        let opened = open(&path);
        assert_eq!(opened.settings, Settings::default());
        let aside = opened.broken.expect("the broken file moved aside");
        assert!(
            aside
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("settings.toml.broken-"),
            "{}",
            aside.display()
        );
        assert_eq!(
            std::fs::read_to_string(&aside).unwrap(),
            "threading = maybe"
        );

        opened.settings.save(&path).unwrap();
        assert_eq!(
            std::fs::read_to_string(&aside).unwrap(),
            "threading = maybe"
        );
        assert_eq!(files_in(dir.path()).len(), 2);
        // The file is readable again, so the next start says nothing.
        assert!(open(&path).broken.is_none());
    }

    #[test]
    fn a_missing_file_is_not_broken() {
        let dir = tempfile::tempdir().unwrap();
        let opened = open(&dir.path().join("settings.toml"));
        assert_eq!(opened.settings, Settings::default());
        assert!(opened.broken.is_none());
    }

    #[test]
    fn the_saver_writes_the_newest_settings_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        let saver = Saver::new(path.clone());
        for size in [
            TextSize::Small,
            TextSize::Large,
            TextSize::Normal,
            TextSize::Larger,
        ] {
            saver.save(&Settings {
                text_size: size,
                ..Settings::default()
            });
        }
        saver.flush();
        assert_eq!(Settings::load(&path).text_size, TextSize::Larger);
        assert_eq!(files_in(dir.path()), ["settings.toml"]);
    }
}
