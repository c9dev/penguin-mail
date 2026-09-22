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
            exec_argument(exe)
        ),
    )
}

/// `exe` as one argument of a desktop entry's Exec line. The Desktop Entry
/// spec quotes an argument in double quotes with `"`, `` ` ``, `$` and `\`
/// escaped by a backslash, then applies the string rule, which doubles
/// every backslash again, and a literal `%` is written `%%`. Without the
/// quotes a path with a space would start a program named by its first
/// half.
fn exec_argument(exe: &Path) -> String {
    let mut quoted = String::from("\"");
    for c in exe.to_string_lossy().chars() {
        match c {
            '"' | '`' | '$' | '\\' => {
                quoted.push_str("\\\\");
                if c == '\\' {
                    quoted.push_str("\\\\");
                } else {
                    quoted.push(c);
                }
            }
            '%' => quoted.push_str("%%"),
            '\n' => quoted.push_str("\\n"),
            '\t' => quoted.push_str("\\t"),
            _ => quoted.push(c),
        }
    }
    quoted.push('"');
    quoted
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{exec_argument, is_enabled, set_enabled};

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
                .contains("Exec=\"/opt/penguin-mail/bin/penguin-mail\" --background")
        );
        set_enabled(&path, Path::new("/x"), false).unwrap();
        assert!(!is_enabled(&path));
        set_enabled(&path, Path::new("/x"), false).unwrap();
    }

    #[test]
    fn a_path_with_a_space_stays_one_argument() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("io.github.c9dev.PenguinMail.desktop");
        set_enabled(
            &path,
            Path::new("/home/ann/Penguin Mail/penguin-mail"),
            true,
        )
        .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("\nExec=\"/home/ann/Penguin Mail/penguin-mail\" --background\n"),
            "{text}"
        );
    }

    #[test]
    fn the_desktop_entry_escapes_survive_both_rounds() {
        // The spec's quoting rule escapes ", `, $ and \ with a backslash,
        // then the string rule doubles every backslash, and % stands for
        // itself only doubled.
        assert_eq!(
            exec_argument(Path::new(r#"/a/$b`c"d\e%f"#)),
            r#""/a/\\$b\\`c\\"d\\\\e%%f""#
        );
    }
}
