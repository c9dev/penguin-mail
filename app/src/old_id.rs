//! Carries a person's desktop choices over from the ID Penguin Mail had
//! before `io.github.c9dev.PenguinMail`. The login item and the default mail
//! handler both name the desktop file, and an upgrade renames that file.
//! install-files.sh does the same for a tarball, but a .deb upgrade runs as
//! root and cannot reach anyone's home folder, so the app does it on start.

use std::path::{Path, PathBuf};

use crate::APP_ID;

const OLD_ID: &str = "dev.penguinmail.PenguinMail";

/// Runs the carry-over for this copy when it applies. Only the .deb and
/// the tarball ever shipped the old ID, and a copy run from a build tree
/// installs no desktop file, so pointing the mail handler at one would
/// leave `mailto:` links with no handler at all.
pub fn carry_over_on_start() {
    if crate::packaging::BUILT_FOR != crate::packaging::Packaging::Native {
        return;
    }
    let mut data_dirs = vec![gtk::glib::user_data_dir()];
    data_dirs.extend(gtk::glib::system_data_dirs());
    carry_over(&gtk::glib::user_config_dir(), &data_dirs);
}

/// Moves the login item to the new name and points `mimeapps.list` at the
/// new desktop file, once one of `data_dirs` holds that file. `config` is
/// the XDG config folder, `~/.config`.
fn carry_over(config: &Path, data_dirs: &[PathBuf]) {
    let desktop = format!("{APP_ID}.desktop");
    if !data_dirs
        .iter()
        .any(|dir| dir.join("applications").join(&desktop).is_file())
    {
        return;
    }
    if let Err(err) = login_item(config) {
        tracing::warn!(error = %err, "could not move the login item to the new name");
    }
    if let Err(err) = mail_handler(config) {
        tracing::warn!(error = %err, "could not move the mail handler to the new name");
    }
}

/// A login item under the old name keeps whatever the person chose, on or
/// off, under the new one. When both exist the new one wins.
fn login_item(config: &Path) -> std::io::Result<()> {
    let folder = config.join("autostart");
    let old = folder.join(format!("{OLD_ID}.desktop"));
    let Ok(text) = std::fs::read_to_string(&old) else {
        return Ok(());
    };
    let new = folder.join(format!("{APP_ID}.desktop"));
    if !new.exists() {
        replace_file(&new, &text.replace(OLD_ID, APP_ID))?;
    }
    std::fs::remove_file(&old)
}

/// `xdg-mime default` writes the desktop file's name into `mimeapps.list`,
/// and a name with no file behind it leaves `mailto:` links with no handler.
fn mail_handler(config: &Path) -> std::io::Result<()> {
    let list = config.join("mimeapps.list");
    let Ok(text) = std::fs::read_to_string(&list) else {
        return Ok(());
    };
    match renamed_handlers(&text) {
        Some(changed) => replace_file(&list, &changed),
        None => Ok(()),
    }
}

/// `text` with the old desktop file's name replaced in the values of its
/// entries, or None when no entry names it. Comments, section headers and
/// every other line stay as they were, down to their line endings.
fn renamed_handlers(text: &str) -> Option<String> {
    let (old, new) = (format!("{OLD_ID}.desktop"), format!("{APP_ID}.desktop"));
    let mut changed = false;
    let lines: Vec<String> = text
        .split_inclusive('\n')
        .map(|line| {
            let entry = !line.trim_start().starts_with(['#', '[']);
            match line.split_once('=') {
                Some((key, value)) if entry && value.contains(&old) => {
                    changed = true;
                    format!("{key}={}", value.replace(&old, &new))
                }
                _ => line.to_string(),
            }
        })
        .collect();
    changed.then(|| lines.concat())
}

/// Writes `contents` to `path` by writing a file beside it and renaming it
/// over, so a crash halfway leaves the old file whole. A symlink is
/// followed and its target written, so a person who keeps these files in a
/// dotfiles checkout keeps the link.
fn replace_file(path: &Path, contents: &str) -> std::io::Result<()> {
    let target = match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => std::fs::canonicalize(path)?,
        _ => path.to_path_buf(),
    };
    let name = target
        .file_name()
        .ok_or_else(|| std::io::Error::other("the path names no file"))?
        .to_string_lossy();
    let staged = target.with_file_name(format!(".{name}.penguin-mail-{}", std::process::id()));
    let written = std::fs::write(&staged, contents).and_then(|()| {
        if let Ok(meta) = std::fs::metadata(&target) {
            std::fs::set_permissions(&staged, meta.permissions())?;
        }
        std::fs::rename(&staged, &target)
    });
    if written.is_err() {
        let _ = std::fs::remove_file(&staged);
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOGIN_ITEM: &str = "[Desktop Entry]\nType=Application\nName=Penguin Mail\n\
        Exec=/usr/bin/penguin-mail --background\nIcon=dev.penguinmail.PenguinMail\n\
        NoDisplay=true\nX-GNOME-Autostart-enabled=false\n";

    /// A data folder holding the new desktop file, as an installed copy has.
    fn installed() -> tempfile::TempDir {
        let data = tempfile::tempdir().unwrap();
        let apps = data.path().join("applications");
        std::fs::create_dir_all(&apps).unwrap();
        std::fs::write(apps.join("io.github.c9dev.PenguinMail.desktop"), "").unwrap();
        data
    }

    fn run(config: &Path, data: &tempfile::TempDir) {
        carry_over(config, &[data.path().to_path_buf()]);
    }

    #[test]
    fn a_login_item_moves_to_the_new_name_and_keeps_its_choice() {
        let config = tempfile::tempdir().unwrap();
        let folder = config.path().join("autostart");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("dev.penguinmail.PenguinMail.desktop"), LOGIN_ITEM).unwrap();
        run(config.path(), &installed());
        assert!(!folder.join("dev.penguinmail.PenguinMail.desktop").exists());
        let moved =
            std::fs::read_to_string(folder.join("io.github.c9dev.PenguinMail.desktop")).unwrap();
        assert!(moved.contains("Icon=io.github.c9dev.PenguinMail\n"), "{moved}");
        assert!(moved.contains("X-GNOME-Autostart-enabled=false"), "{moved}");
        assert!(moved.contains("Exec=/usr/bin/penguin-mail --background"), "{moved}");
    }

    #[test]
    fn a_login_item_already_under_the_new_name_wins() {
        let config = tempfile::tempdir().unwrap();
        let folder = config.path().join("autostart");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("dev.penguinmail.PenguinMail.desktop"), LOGIN_ITEM).unwrap();
        std::fs::write(folder.join("io.github.c9dev.PenguinMail.desktop"), "new").unwrap();
        run(config.path(), &installed());
        assert!(!folder.join("dev.penguinmail.PenguinMail.desktop").exists());
        assert_eq!(
            std::fs::read_to_string(folder.join("io.github.c9dev.PenguinMail.desktop")).unwrap(),
            "new"
        );
    }

    #[test]
    fn the_mail_handler_follows_the_new_desktop_file_and_nothing_else_moves() {
        let config = tempfile::tempdir().unwrap();
        let list = config.path().join("mimeapps.list");
        let before = "# Set by dev.penguinmail.PenguinMail.desktop in 2025\r\n\
            [Default Applications]\r\n\
            x-scheme-handler/mailto=dev.penguinmail.PenguinMail.desktop\r\n\
            text/html=firefox.desktop\r\n\
            \r\n\
            [Added Associations]\n\
            x-scheme-handler/mailto=dev.penguinmail.PenguinMail.desktop;thunderbird.desktop;\n";
        std::fs::write(&list, before).unwrap();
        run(config.path(), &installed());
        assert_eq!(
            std::fs::read_to_string(&list).unwrap(),
            "# Set by dev.penguinmail.PenguinMail.desktop in 2025\r\n\
            [Default Applications]\r\n\
            x-scheme-handler/mailto=io.github.c9dev.PenguinMail.desktop\r\n\
            text/html=firefox.desktop\r\n\
            \r\n\
            [Added Associations]\n\
            x-scheme-handler/mailto=io.github.c9dev.PenguinMail.desktop;thunderbird.desktop;\n"
        );
        let left: Vec<_> = std::fs::read_dir(config.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(left, vec![std::ffi::OsString::from("mimeapps.list")]);
    }

    #[test]
    fn a_copy_with_no_new_desktop_file_changes_nothing() {
        let config = tempfile::tempdir().unwrap();
        let folder = config.path().join("autostart");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("dev.penguinmail.PenguinMail.desktop"), LOGIN_ITEM).unwrap();
        let list = config.path().join("mimeapps.list");
        let text = "[Default Applications]\nx-scheme-handler/mailto=dev.penguinmail.PenguinMail.desktop\n";
        std::fs::write(&list, text).unwrap();
        // A build tree: the data folders hold no io.github.c9dev.PenguinMail.desktop.
        let empty = tempfile::tempdir().unwrap();
        run(config.path(), &empty);
        assert_eq!(std::fs::read_to_string(&list).unwrap(), text);
        assert!(folder.join("dev.penguinmail.PenguinMail.desktop").exists());
        assert!(!folder.join("io.github.c9dev.PenguinMail.desktop").exists());
    }

    #[test]
    fn a_symlinked_mail_handler_list_stays_a_symlink() {
        let config = tempfile::tempdir().unwrap();
        let dotfiles = tempfile::tempdir().unwrap();
        let real = dotfiles.path().join("mimeapps.list");
        std::fs::write(
            &real,
            "[Default Applications]\nx-scheme-handler/mailto=dev.penguinmail.PenguinMail.desktop\n",
        )
        .unwrap();
        let link = config.path().join("mimeapps.list");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        run(config.path(), &installed());
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            std::fs::read_to_string(&real).unwrap(),
            "[Default Applications]\nx-scheme-handler/mailto=io.github.c9dev.PenguinMail.desktop\n"
        );
    }

    #[test]
    fn nothing_to_carry_over_changes_nothing() {
        let config = tempfile::tempdir().unwrap();
        run(config.path(), &installed());
        assert_eq!(std::fs::read_dir(config.path()).unwrap().count(), 0);
    }
}
