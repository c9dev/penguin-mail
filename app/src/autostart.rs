//! The login item that starts Penguin Mail in the tray.
//!
//! Outside Flatpak the item is a desktop file in `~/.config/autostart`. In
//! a snap the same path lands in the snap's own config folder, where snapd
//! looks for the file `snap/snapcraft.yaml` names as the app's autostart
//! entry. Flatpak cannot write the host's folder, so there the file only
//! records the choice and the Background portal writes the real one.

use std::path::{Path, PathBuf};

#[cfg(not(windows))]
use gtk::{gio, glib, prelude::*};
#[cfg(not(windows))]
use mailrs_domain::translate::gettext;

#[cfg(not(windows))]
use crate::packaging::{BUILT_FOR, Packaging};

pub fn path() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        Some(PathBuf::from(windows::RUN_VALUE))
    }
    #[cfg(not(windows))]
    {
        Some(
            dirs_config()?
                .join("autostart")
                .join(format!("{}.desktop", crate::APP_ID)),
        )
    }
}

#[cfg(not(windows))]
fn dirs_config() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
}

pub fn is_enabled(path: &Path) -> bool {
    #[cfg(windows)]
    {
        let _ = path;
        windows::is_enabled()
    }
    #[cfg(not(windows))]
    {
        std::fs::read_to_string(path)
            .is_ok_and(|text| !text.contains("X-GNOME-Autostart-enabled=false"))
    }
}

/// Turns the login item on or off for this copy, however it was packaged.
pub fn apply(path: &Path, enabled: bool) -> std::io::Result<()> {
    let (exe, flags) = crate::exe::login().unwrap_or_else(|_| ("penguin-mail".into(), &[]));
    #[cfg(windows)]
    {
        let _ = path;
        windows::set_enabled(&exe, flags, enabled)
    }
    #[cfg(not(windows))]
    {
        set_enabled(path, &exe, flags, enabled)?;
        if BUILT_FOR == Packaging::Flatpak {
            ask_portal(enabled);
        }
        Ok(())
    }
}

/// Asks the Background portal to start Penguin Mail at login, or to stop.
/// The desktop may ask the person first; the answer arrives as a signal
/// nobody here waits for, because the switch already shows their choice.
#[cfg(not(windows))]
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

/// Writes or removes the login item. `exe` is the binary to start and
/// `flags` go before `--background`.
#[cfg(not(windows))]
pub fn set_enabled(path: &Path, exe: &Path, flags: &[&str], enabled: bool) -> std::io::Result<()> {
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
             Exec={}{} --background\nIcon={}\nNoDisplay=true\nX-GNOME-Autostart-enabled=true\n",
            exec_argument(exe),
            flags
                .iter()
                .map(|flag| format!(" {flag}"))
                .collect::<String>(),
            crate::APP_ID
        ),
    )
}

/// `exe` as one argument of a desktop entry's Exec line. The Desktop Entry
/// spec quotes an argument in double quotes with `"`, `` ` ``, `$` and `\`
/// escaped by a backslash, then applies the string rule, which doubles
/// every backslash again, and a literal `%` is written `%%`. Without the
/// quotes a path with a space would start a program named by its first
/// half.
#[cfg(not(windows))]
pub(crate) fn exec_argument(exe: &Path) -> String {
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

/// The login item on Windows: a value under the Run key in HKCU, whose
/// text is the command line the session runs when the person signs in.
#[cfg(windows)]
mod windows {
    use std::io;
    use std::path::Path;

    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows::Win32::System::Registry::*;
    use windows::core::{HSTRING, PCWSTR};

    /// The value's name in the Run key. `path()` hands it back shaped as
    /// a path because the only caller passes it straight through.
    pub const RUN_VALUE: &str = "Penguin Mail";
    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

    /// An open key that closes when the work is done.
    struct Key(HKEY);

    impl Key {
        /// The Run key, creating it when missing. HKCU opens for the
        /// signed-in person's full access, so no rights are named.
        fn open() -> io::Result<Key> {
            unsafe {
                let mut key = HKEY::default();
                RegCreateKeyW(
                    HKEY_CURRENT_USER,
                    PCWSTR(HSTRING::from(RUN_KEY).as_ptr()),
                    &mut key,
                )
                .ok()
                .map_err(io::Error::other)?;
                Ok(Key(key))
            }
        }
    }

    impl Drop for Key {
        fn drop(&mut self) {
            unsafe {
                let _ = RegCloseKey(self.0);
            }
        }
    }

    pub fn is_enabled() -> bool {
        let Ok(key) = Key::open() else {
            return false;
        };
        unsafe {
            RegQueryValueExW(
                key.0,
                PCWSTR(HSTRING::from(RUN_VALUE).as_ptr()),
                None,
                None,
                None,
                None,
            )
            .is_ok()
        }
    }

    /// Sets the Run value to `"exe" flags --background`, or removes it.
    pub fn set_enabled(exe: &Path, flags: &[&str], enabled: bool) -> io::Result<()> {
        let key = Key::open()?;
        let name = HSTRING::from(RUN_VALUE);
        unsafe {
            if !enabled {
                let gone = RegDeleteValueW(key.0, PCWSTR(name.as_ptr()));
                if gone == ERROR_SUCCESS || gone == ERROR_FILE_NOT_FOUND {
                    return Ok(());
                }
                return Err(io::Error::other(gone.to_hresult().message()));
            }
            let command = format!(
                "\"{}\"{} --background",
                exe.display(),
                flags
                    .iter()
                    .map(|flag| format!(" {flag}"))
                    .collect::<String>()
            );
            let wide: Vec<u16> = command.encode_utf16().chain(std::iter::once(0)).collect();
            let bytes = std::slice::from_raw_parts(
                wide.as_ptr() as *const u8,
                size_of_val(wide.as_slice()),
            );
            RegSetValueExW(key.0, PCWSTR(name.as_ptr()), None, REG_SZ, Some(bytes))
                .ok()
                .map_err(io::Error::other)
        }
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use std::path::PathBuf;

    /// Toggles the real Run value once, then puts the key back. Run by
    /// hand (`--ignored`), since it writes the signed-in person's hive.
    #[test]
    #[ignore = "writes the real HKCU Run key; run by hand"]
    fn the_run_value_toggles() {
        let key = super::windows::RUN_VALUE;
        assert!(!super::windows::is_enabled(), "{key} must start unset");
        super::windows::set_enabled(
            &PathBuf::from(r"C:\Program Files\Penguin Mail\penguin-mail.exe"),
            &[],
            true,
        )
        .unwrap();
        assert!(super::windows::is_enabled());
        super::windows::set_enabled(&PathBuf::new(), &[], false).unwrap();
        assert!(!super::windows::is_enabled());
    }
}

#[cfg(all(test, not(windows)))]
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
        set_enabled(
            &path,
            Path::new("/opt/penguin-mail/bin/penguin-mail"),
            &[],
            true,
        )
        .unwrap();
        assert!(is_enabled(&path));
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("Exec=\"/opt/penguin-mail/bin/penguin-mail\" --background")
        );
        set_enabled(&path, Path::new("/x"), &[], false).unwrap();
        assert!(!is_enabled(&path));
        set_enabled(&path, Path::new("/x"), &[], false).unwrap();
    }

    #[test]
    fn a_path_with_a_space_stays_one_argument() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("io.github.c9dev.PenguinMail.desktop");
        set_enabled(
            &path,
            Path::new("/home/ann/Penguin Mail/penguin-mail"),
            &[],
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
    fn an_appimage_without_fuse_unpacks_again_at_login() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("io.github.c9dev.PenguinMail.desktop");
        set_enabled(
            &path,
            Path::new("/home/ann/Mail.AppImage"),
            &["--appimage-extract-and-run"],
            true,
        )
        .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains(
                "\nExec=\"/home/ann/Mail.AppImage\" --appimage-extract-and-run --background\n"
            ),
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
