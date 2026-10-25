//! A mail file opened from Files or the command line: reading its bytes
//! with a ceiling on their size, and the window that says why a file will
//! not open. The message itself opens in a reading window; nothing here
//! reads a message.

use std::path::Path;

use adw::prelude::*;
use mailrs_domain::translate::gettext;

/// The largest file read: the ceiling mail servers put on a message, so
/// nothing a mail program wrote is refused.
pub const MOST_BYTES: u64 = 64 * 1024 * 1024;

/// The file's bytes, or why they will not do: unreadable, empty, over
/// [`MOST_BYTES`], or without a single mail header.
pub fn read_message(path: &Path) -> Result<Vec<u8>, String> {
    read_message_within(path, MOST_BYTES)
}

/// [`read_message`] with the ceiling at `most` bytes. The size the file
/// reports is checked first, so a huge file costs nothing, and the read
/// itself stops one byte past the ceiling, so a file that grew since, or
/// a pipe that reports no size, is refused rather than read whole.
fn read_message_within(path: &Path, most: u64) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let size = std::fs::metadata(path)
        .map_err(|err| err.to_string())?
        .len();
    if size > most {
        return Err(gettext("This file is too large to be a message."));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|file| file.take(most + 1).read_to_end(&mut bytes))
        .map_err(|err| err.to_string())?;
    if bytes.len() as u64 > most {
        return Err(gettext("This file is too large to be a message."));
    }
    if bytes.is_empty() {
        return Err(gettext("This file is empty."));
    }
    // A file with no header the parser accepts is notes, not mail; shown
    // as a message it would be blank but for its text.
    let summary = mailrs_mime::headers(&bytes);
    let headed = summary.from.is_some()
        || !summary.to.is_empty()
        || !summary.cc.is_empty()
        || !summary.subject.is_empty()
        || summary.date.is_some()
        || summary.message_id.is_some();
    if !headed {
        return Err(gettext("This file does not hold a message."));
    }
    Ok(bytes)
}

/// A small window saying the file could not be opened, with `why` under
/// the title, named after the file.
pub fn show_unreadable(path: &Path, why: &str) -> adw::Window {
    crate::ensure_gtk();
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let title = if name.is_empty() {
        gettext("Message")
    } else {
        name
    };
    let (window, toasts) = crate::ui::file_window::shell(&title);
    toasts.set_child(Some(&crate::ui::file_window::status(
        "io.github.c9dev.PenguinMail-symbolic",
        &gettext("Could Not Open This File"),
        why,
    )));
    window.present();
    window
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_missing_or_oversized_file_is_refused_with_a_reason() {
        let dir = tempfile::tempdir().unwrap();
        let empty = dir.path().join("e.eml");
        std::fs::write(&empty, "").unwrap();
        assert!(read_message(&empty).is_err());
        assert!(read_message(&dir.path().join("missing.eml")).is_err());
        let ok = dir.path().join("ok.eml");
        std::fs::write(&ok, "Subject: x\r\n\r\nhi").unwrap();
        assert_eq!(read_message(&ok).unwrap(), b"Subject: x\r\n\r\nhi".to_vec());
    }

    /// The ceiling bounds the read itself, not only the size the file
    /// reported before it: a file that grew since, or a pipe that reports
    /// no size, is cut off and refused rather than read whole.
    #[test]
    fn the_read_itself_stops_at_the_ceiling() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("m.eml");
        let raw = b"Subject: x\r\n\r\nhello";
        std::fs::write(&file, raw).unwrap();
        assert!(read_message_within(&file, raw.len() as u64 - 1).is_err());
        assert_eq!(
            read_message_within(&file, raw.len() as u64).unwrap(),
            raw.to_vec()
        );
    }

    #[test]
    fn a_text_file_with_no_header_is_no_message() {
        let dir = tempfile::tempdir().unwrap();
        let text = dir.path().join("notes.eml");
        std::fs::write(&text, "just some notes\nnot a message\n").unwrap();
        assert!(read_message(&text).is_err());
        let bare = dir.path().join("bare.eml");
        std::fs::write(&bare, "Subject: only a subject\r\n\r\n").unwrap();
        assert!(
            read_message(&bare).is_ok(),
            "one header the parser accepts is enough"
        );
    }
}
