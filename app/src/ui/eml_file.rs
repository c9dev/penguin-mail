//! A mail file opened from Files or the command line: reading its bytes
//! with a ceiling on their size, and the window that says why a file will
//! not open. The message itself opens in a reading window; nothing here
//! reads a message.

use std::path::Path;

use adw::prelude::*;
use gtk::glib;
use mailrs_domain::translate::gettext;

/// The largest file read: the ceiling mail servers put on a message, so
/// nothing a mail program wrote is refused.
pub const MOST_BYTES: u64 = 64 * 1024 * 1024;

/// Whether a file of `len` bytes is past the ceiling.
pub fn too_large(len: u64) -> bool {
    len > MOST_BYTES
}

/// The file's bytes, or why they will not do: unreadable, empty, over
/// [`MOST_BYTES`], or without a single mail header. The size is checked
/// before the read, so a huge file costs nothing.
pub fn read_message(path: &Path) -> Result<Vec<u8>, String> {
    let size = std::fs::metadata(path).map_err(|err| err.to_string())?.len();
    if too_large(size) {
        return Err(gettext("This file is too large to be a message."));
    }
    let bytes = std::fs::read(path).map_err(|err| err.to_string())?;
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
    let window = adw::Window::builder()
        .title(if name.is_empty() {
            gettext("Message")
        } else {
            name
        })
        .default_width(520)
        .default_height(-1)
        .build();
    crate::ui::window::track_dark_class(&window);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    let status = adw::StatusPage::builder()
        .icon_name("io.github.c9dev.PenguinMail-symbolic")
        .title(glib::markup_escape_text(&gettext("Could Not Open This File")).as_str())
        .description(glib::markup_escape_text(why).as_str())
        .build();
    toolbar.set_content(Some(&status));
    window.set_content(Some(&toolbar));
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
        assert!(!too_large(MOST_BYTES));
        assert!(too_large(MOST_BYTES + 1));
    }

    #[test]
    fn a_text_file_with_no_header_is_no_message() {
        let dir = tempfile::tempdir().unwrap();
        let text = dir.path().join("notes.eml");
        std::fs::write(&text, "just some notes\nnot a message\n").unwrap();
        assert!(read_message(&text).is_err());
        let bare = dir.path().join("bare.eml");
        std::fs::write(&bare, "Subject: only a subject\r\n\r\n").unwrap();
        assert!(read_message(&bare).is_ok(), "one header the parser accepts is enough");
    }
}
