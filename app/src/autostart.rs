//! The login item that starts Penguin Mail in the tray.
//!
//! Outside Flatpak the item is a desktop file in `~/.config/autostart`. In
//! a snap the same path lands in the snap's own config folder, where snapd
//! looks for the file `snap/snapcraft.yaml` names as the app's autostart
//! entry. Flatpak cannot write the host's folder, so there the file only
//! records the choice and the Background portal writes the real one.

use std::path::{Path, PathBuf};

use gtk::{gio, glib, prelude::*};
use mailrs_domain::translate::gettext;

use crate::packaging::{BUILT_FOR, Packaging};

const FILE: &str = "io.github.c9dev.PenguinMail.desktop";

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

/// Turns the login item on or off for this copy, however it was packaged.
pub fn apply(path: &Path, enabled: bool) -> std::io::Result<()> {
    let exe = crate::exe::launcher().unwrap_or_else(|_| "penguin-mail".into());
    set_enabled(path, &exe, enabled)?;
    if BUILT_FOR == Packaging::Flatpak {
        ask_portal(enabled);
    }
    Ok(())
}

/// Asks the Background portal to start Penguin Mail at login, or to stop.
/// The desktop may ask the person first; the answer arrives as a signal
/// nobody here waits for, because the switch already shows their choice.
fn ask_portal(enabled: bool) {
    let options = glib::VariantDict::new(None);
    options.insert_value(
        "reason",
        &gettext("Penguin Mail keeps syncing with no window open").to_variant(),
    );
    options.insert_value("autostart", &enabled.to_variant());
    options.insert_value(
        "commandline",
        &vec!["penguin-mail".to_string(), "--background".to_string()].to_variant(),
    );
    let parameters = glib::Variant::tuple_from_iter(["".to_variant(), options.end()]);
    glib::spawn_future_local(async move {
        let asked = async {
            let bus = gio::bus_get_future(gio::BusType::Session).await?;
            bus.call_future(
                Some("org.freedesktop.portal.Desktop"),
                "/org/freedesktop/portal/desktop",
                "org.freedesktop.portal.Background",
                "RequestBackground",
                Some(&parameters),
                None,
                gio::DBusCallFlags::NONE,
                -1,
            )
            .await
        };
        if let Err(err) = asked.await {
            tracing::warn!(error = %err, "the Background portal did not take the login item");
        }
    });
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
            "[Desktop Entry]\nType=Application\nName=Penguin Mail\nComment=Keeps Gmail in sync from the system tray\n\
             Exec={} --background\nIcon=io.github.c9dev.PenguinMail\nNoDisplay=true\nX-GNOME-Autostart-enabled=true\n",
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
            .join("io.github.c9dev.PenguinMail.desktop");
        assert!(!is_enabled(&path));
        set_enabled(&path, Path::new("/opt/penguin-mail/bin/penguin-mail"), true).unwrap();
        assert!(is_enabled(&path));
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("Exec=/opt/penguin-mail/bin/penguin-mail --background")
        );
        set_enabled(&path, Path::new("/x"), false).unwrap();
        assert!(!is_enabled(&path));
        set_enabled(&path, Path::new("/x"), false).unwrap();
    }
}
