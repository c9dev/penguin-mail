//! The login item that starts mailrs in the tray.

use std::path::{Path, PathBuf};

const FILE: &str = "dev.mailrs.Mailrs.desktop";

pub fn path() -> Option<PathBuf> {
    Some(dirs_config()?.join("autostart").join(FILE))
}

fn dirs_config() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
}

pub fn is_enabled(path: &Path) -> bool {
    std::fs::read_to_string(path)
        .is_ok_and(|text| !text.contains("X-GNOME-Autostart-enabled=false"))
}

/// Writes or removes the login item. `exe` is the binary to start.
pub fn set_enabled(path: &Path, exe: &Path, enabled: bool) -> std::io::Result<()> {
    if !enabled {
        return match std::fs::remove_file(path) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err),
            _ => Ok(()),
        };
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(
        path,
        format!(
            "[Desktop Entry]\nType=Application\nName=mailrs\nComment=Keeps Gmail in sync from the system tray\n\
             Exec={} --background\nIcon=dev.mailrs.Mailrs\nNoDisplay=true\nX-GNOME-Autostart-enabled=true\n",
            exe.display()
        ),
    )
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{is_enabled, set_enabled};

    #[test]
    fn the_login_item_toggles() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir
            .path()
            .join("autostart")
            .join("dev.mailrs.Mailrs.desktop");
        assert!(!is_enabled(&path));
        set_enabled(&path, Path::new("/opt/mailrs/bin/mailrs"), true).unwrap();
        assert!(is_enabled(&path));
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("Exec=/opt/mailrs/bin/mailrs --background")
        );
        set_enabled(&path, Path::new("/x"), false).unwrap();
        assert!(!is_enabled(&path));
        set_enabled(&path, Path::new("/x"), false).unwrap();
    }
}
