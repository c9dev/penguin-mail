//! The sound a reminder window makes: the desktop's own, the same one
//! its notifications play. macOS beeps the system alert; Linux plays
//! the sound theme's message event through the sound server's own
//! player, which is what a notification server does for a banner.
//! Nothing is shipped and nothing decoded here: the file the theme
//! holds goes to the player the desktop already runs, and a desktop
//! with no player stays quiet, the reminder window saying it in the
//! corner.

use std::path::{Path, PathBuf};

#[cfg(not(target_os = "macos"))]
use gtk::gio;

/// Says a reminder out loud: the desktop's alert sound, once.
pub fn alert() {
    #[cfg(target_os = "macos")]
    {
        objc2_app_kit::NSSound::beep();
    }
    #[cfg(not(target_os = "macos"))]
    {
        if let Some(sound) = themed("message-new-instant") {
            play(&sound);
        } else {
            tracing::warn!("no sound theme holds a message sound");
        }
    }
}

/// The `event` sound of the theme the desktop names, looked up the way
/// the sound theme spec lays out: the theme from `XDG_SOUND_THEME`,
/// `freedesktop` when the desktop names none, under the data dirs'
/// `sounds`, in the spec's `stereo` layout.
#[cfg(not(target_os = "macos"))]
fn themed(event: &str) -> Option<PathBuf> {
    let theme = std::env::var("XDG_SOUND_THEME").unwrap_or_default();
    let theme = match theme.trim().is_empty() {
        true => "freedesktop",
        false => theme.trim(),
    };
    let dirs = std::env::var("XDG_DATA_DIRS")
        .map(|dirs| {
            dirs.split(':')
                .filter(|dir| !dir.is_empty())
                .map(PathBuf::from)
                .collect()
        })
        .unwrap_or_else(|_| {
            vec![
                PathBuf::from("/usr/local/share"),
                PathBuf::from("/usr/share"),
            ]
        });
    sound_of(&dirs, theme, event)
}

/// The first file holding `event`'s sound in one of the data `dirs`.
#[cfg(not(target_os = "macos"))]
fn sound_of(dirs: &[PathBuf], theme: &str, event: &str) -> Option<PathBuf> {
    for ending in ["oga", "ogg", "wav"] {
        for dir in dirs {
            let held = dir
                .join("sounds")
                .join(theme)
                .join("stereo")
                .join(format!("{event}.{ending}"));
            if held.is_file() {
                return Some(held);
            }
        }
    }
    None
}

/// Hands `sound` to the sound server's own player, the first one the
/// desktop runs: PipeWire's, then the PulseAudio compatibility one. A
/// desktop with neither stays quiet; the player is asked to finish on
/// its own, nothing waits on it.
#[cfg(not(target_os = "macos"))]
fn play(sound: &Path) {
    use std::ffi::OsStr;
    for player in ["pw-play", "paplay"] {
        let run = [OsStr::new(player), sound.as_os_str()];
        match gio::SubprocessLauncher::new(gio::SubprocessFlags::NONE).spawn(&run) {
            Ok(run) => {
                run.wait_async(gio::Cancellable::NONE, |_| {});
                return;
            }
            // The player is missing; the next kind is tried before the
            // reminder gives up on saying itself out loud.
            Err(err) => tracing::debug!(player, error = %err, "no player"),
        }
    }
    tracing::warn!("no player on this desktop plays a sound");
}

#[cfg(all(test, not(target_os = "macos")))]
mod tests {
    use super::*;

    /// A file the theme holds, in the spec's stereo layout, in the
    /// first data dir that holds it, in the order of the endings the
    /// spec's own themes write.
    #[test]
    fn the_theme_sound_comes_from_the_data_dirs() {
        let tree = std::env::temp_dir().join(format!("alert-{}", std::process::id()));
        let theme = tree.join("sounds/mytheme/stereo");
        std::fs::create_dir_all(&theme).unwrap();
        std::fs::write(theme.join("message-new-instant.oga"), b"sound").unwrap();
        let found = sound_of(
            &[tree.join("nope"), tree.clone()],
            "mytheme",
            "message-new-instant",
        );
        assert_eq!(found, Some(theme.join("message-new-instant.oga")));

        // Another ending, another theme: the first that exists wins.
        let other = tree.join("sounds/other/stereo");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("message-new-instant.ogg"), b"sound").unwrap();
        assert_eq!(
            sound_of(std::slice::from_ref(&tree), "other", "message-new-instant"),
            Some(other.join("message-new-instant.ogg"))
        );
        assert_eq!(
            sound_of(std::slice::from_ref(&tree), "missing", "message-new-instant"),
            None
        );
        std::fs::remove_dir_all(&tree).ok();
    }
}
