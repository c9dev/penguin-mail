//! Contact photos as the conversation page takes them: `data:` URIs, read
//! from disk off the main thread and kept for the rest of the run. A
//! conversation asks with the addresses it shows and gets what is already
//! read; the rest are read in the background, and the app redraws the open
//! conversations once they arrive.

use std::collections::HashMap;
use std::path::PathBuf;

use base64::Engine;

/// Photos read so far, by lower-case address. `None` marks a file that
/// could not be read, so a missing photo is asked for once rather than on
/// every redraw.
#[derive(Default)]
pub(crate) struct PhotoCache {
    read: HashMap<String, Option<String>>,
    reading: std::collections::HashSet<String>,
}

/// What a conversation gets now, and the files still to read for it.
pub(crate) struct Lookup {
    pub found: HashMap<String, String>,
    pub to_read: Vec<(String, PathBuf)>,
}

impl PhotoCache {
    /// Answers `addresses` from what is read, and names the files to read
    /// for the others that have a photo at `path_of`. A file already being
    /// read is not named twice.
    pub(crate) fn lookup(
        &mut self,
        addresses: impl Iterator<Item = String>,
        path_of: impl Fn(&str) -> Option<PathBuf>,
    ) -> Lookup {
        let mut found = HashMap::new();
        let mut to_read = Vec::new();
        for address in addresses {
            let key = address.trim().to_lowercase();
            if key.is_empty() || found.contains_key(&key) {
                continue;
            }
            match self.read.get(&key) {
                Some(Some(data)) => {
                    found.insert(key, data.clone());
                }
                Some(None) => {}
                None => {
                    if let Some(path) = path_of(&key)
                        && self.reading.insert(key.clone())
                    {
                        to_read.push((key, path));
                    }
                }
            }
        }
        Lookup { found, to_read }
    }

    /// Keeps what a background read brought back.
    pub(crate) fn store(&mut self, read: Vec<(String, Option<String>)>) {
        for (key, data) in read {
            self.reading.remove(&key);
            self.read.insert(key, data);
        }
    }

    /// Forgets every photo, after the address books change.
    pub(crate) fn clear(&mut self) {
        self.read.clear();
        self.reading.clear();
    }
}

/// Reads each file as a `data:` URI. Runs on a blocking thread.
pub(crate) fn read(files: Vec<(String, PathBuf)>) -> Vec<(String, Option<String>)> {
    files
        .into_iter()
        .map(|(key, path)| {
            let data = std::fs::read(&path).ok().map(|bytes| {
                let data = base64::engine::general_purpose::STANDARD.encode(bytes);
                format!("data:image/jpeg;base64,{data}")
            });
            (key, data)
        })
        .collect()
}

/// Keeps the contacts whose photo file is on disk, by lower-case address.
/// Runs on a blocking thread, since it asks the disk once per contact.
pub(crate) fn on_disk(dir: PathBuf, files: Vec<(String, String)>) -> HashMap<String, PathBuf> {
    files
        .into_iter()
        .filter_map(|(email, name)| {
            let file = dir.join(name);
            file.exists().then(|| (email.to_lowercase(), file))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_photo_is_read_once_and_then_answered_from_memory() {
        let dir = tempfile::tempdir().unwrap();
        let ann = dir.path().join("ann.jpg");
        std::fs::write(&ann, b"jpeg").unwrap();
        let paths: HashMap<String, PathBuf> = [
            ("ann@example.com".to_string(), ann.clone()),
            ("bo@example.com".to_string(), dir.path().join("gone.jpg")),
        ]
        .into();
        let path_of = |key: &str| paths.get(key).cloned();
        let mut cache = PhotoCache::default();
        let asked = || {
            [" Ann@Example.com", "bo@example.com", "cy@example.com"]
                .into_iter()
                .map(String::from)
        };

        let first = cache.lookup(asked(), path_of);
        assert!(first.found.is_empty());
        assert_eq!(first.to_read.len(), 2, "cy has no photo to read");
        // A redraw while the read runs asks for nothing more.
        assert!(cache.lookup(asked(), path_of).to_read.is_empty());

        cache.store(read(first.to_read));
        let second = cache.lookup(asked(), path_of);
        assert!(
            second.to_read.is_empty(),
            "a missing file is not read again"
        );
        assert_eq!(
            second.found.get("ann@example.com").map(String::as_str),
            Some("data:image/jpeg;base64,anBlZw==")
        );
        assert_eq!(second.found.len(), 1);

        cache.clear();
        assert_eq!(cache.lookup(asked(), path_of).to_read.len(), 2);
    }

    #[test]
    fn only_photos_on_disk_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ann.jpg"), b"jpeg").unwrap();
        let kept = on_disk(
            dir.path().to_path_buf(),
            vec![
                ("Ann@Example.com".into(), "ann.jpg".into()),
                ("bo@example.com".into(), "bo.jpg".into()),
            ],
        );
        assert_eq!(
            kept,
            [("ann@example.com".to_string(), dir.path().join("ann.jpg"))].into()
        );
    }
}
