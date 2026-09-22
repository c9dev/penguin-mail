//! Carries a person's desktop choices over from the ID Penguin Mail had
//! before `io.github.c9dev.PenguinMail`. The login item and the default mail
//! handler both name the desktop file, and an upgrade renames that file.
//! install-files.sh does the same for a tarball, but a .deb upgrade runs as
//! root and cannot reach anyone's home folder, so the app does it on start.

use std::path::Path;

use crate::APP_ID;

const OLD_ID: &str = "dev.penguinmail.PenguinMail";

/// Moves the login item to the new name and points `mimeapps.list` at the
/// new desktop file. `config` is the XDG config folder, `~/.config`.
pub fn carry_over(config: &Path) {
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
        std::fs::write(&new, text.replace(OLD_ID, APP_ID))?;
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
    let old = format!("{OLD_ID}.desktop");
    if !text.contains(&old) {
        return Ok(());
    }
    std::fs::write(&list, text.replace(&old, &format!("{APP_ID}.desktop")))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOGIN_ITEM: &str = "[Desktop Entry]\nType=Application\nName=Penguin Mail\n\
        Exec=/usr/bin/penguin-mail --background\nIcon=dev.penguinmail.PenguinMail\n\
        NoDisplay=true\nX-GNOME-Autostart-enabled=false\n";

    #[test]
    fn a_login_item_moves_to_the_new_name_and_keeps_its_choice() {
        let config = tempfile::tempdir().unwrap();
        let folder = config.path().join("autostart");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("dev.penguinmail.PenguinMail.desktop"), LOGIN_ITEM).unwrap();
        carry_over(config.path());
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
        carry_over(config.path());
        assert!(!folder.join("dev.penguinmail.PenguinMail.desktop").exists());
        assert_eq!(
            std::fs::read_to_string(folder.join("io.github.c9dev.PenguinMail.desktop")).unwrap(),
            "new"
        );
    }

    #[test]
    fn the_mail_handler_follows_the_new_desktop_file() {
        let config = tempfile::tempdir().unwrap();
        let list = config.path().join("mimeapps.list");
        std::fs::write(
            &list,
            "[Default Applications]\nx-scheme-handler/mailto=dev.penguinmail.PenguinMail.desktop\n\
             text/html=firefox.desktop\n",
        )
        .unwrap();
        carry_over(config.path());
        assert_eq!(
            std::fs::read_to_string(&list).unwrap(),
            "[Default Applications]\nx-scheme-handler/mailto=io.github.c9dev.PenguinMail.desktop\n\
             text/html=firefox.desktop\n"
        );
    }

    #[test]
    fn nothing_to_carry_over_changes_nothing() {
        let config = tempfile::tempdir().unwrap();
        carry_over(config.path());
        assert_eq!(std::fs::read_dir(config.path()).unwrap().count(), 0);
    }
}
