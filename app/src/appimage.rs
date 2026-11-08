//! Paths supplied by the AppImage launcher.
//!
//! The runtime starts the bundle in one of three ways. With FUSE it mounts
//! the image and names it in `APPIMAGE`. Without FUSE, under
//! `--appimage-extract-and-run`, it unpacks the image into
//! `appimage_extracted_<md5>` under the temporary folder and names the
//! image in `APPIMAGE` too. Its source deletes the folder when the app
//! exits, but the release in this package leaves all of it, over a
//! gigabyte, in a folder many systems keep in memory; `leave` frees it.
//! Someone who ran `--appimage-extract` themselves starts `AppRun`
//! directly, with no `APPIMAGE` at all.

use std::io;
use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;

/// The flag that makes the runtime unpack the image instead of mounting it.
#[cfg(unix)]
const EXTRACT_AND_RUN: &str = "--appimage-extract-and-run";

/// How the runtime names the folders it unpacks into.
const UNPACKED: &str = "appimage_extracted_";

#[derive(Debug, PartialEq)]
enum Start<'a> {
    Mounted { image: &'a Path },
    Extracted { image: &'a Path },
    Unpacked { root: &'a Path },
}

fn start<'a>(root: &'a Path, image: Option<&'a Path>) -> io::Result<Start<'a>> {
    if !root.is_absolute() || image.is_some_and(|path| !path.is_absolute()) {
        return Err(io::Error::other("AppImage paths must be absolute"));
    }
    let temporary = root
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with(UNPACKED));
    Ok(match image {
        Some(image) if temporary => Start::Extracted { image },
        Some(image) => Start::Mounted { image },
        None => Start::Unpacked { root },
    })
}

impl Start<'_> {
    /// What the login item runs. By the next login a mount or an extracted
    /// folder is gone, so the item names the image, and asks it to unpack
    /// again when this copy had to.
    #[cfg(unix)]
    fn login(&self) -> (PathBuf, &'static [&'static str]) {
        match self {
            Start::Mounted { image } => (image.to_path_buf(), &[]),
            Start::Extracted { image } => (image.to_path_buf(), &[EXTRACT_AND_RUN]),
            Start::Unpacked { root } => (root.join("AppRun"), &[]),
        }
    }
}

#[cfg(feature = "packaging-appimage")]
pub fn root() -> io::Result<PathBuf> {
    std::env::var_os("APPDIR")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| io::Error::other("start the AppImage through AppRun"))
}

/// What starts another copy while this one runs: a restart, or the bridge
/// Claude Code starts. The runtime keeps a mount while any process holds
/// its keepalive pipe, which every copy started from here inherits, and an
/// extracted folder is only freed once this process quits. Running the
/// `AppRun` already there needs neither a second mount nor a second
/// unpacking, so a restart has to replace this process rather than quit.
#[cfg(feature = "packaging-appimage")]
pub fn launcher() -> io::Result<PathBuf> {
    root().map(|root| root.join("AppRun"))
}

#[cfg(feature = "packaging-appimage")]
pub fn login() -> io::Result<(PathBuf, &'static [&'static str])> {
    let root = root()?;
    let image = std::env::var_os("APPIMAGE")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    start(&root, image.as_deref()).map(|start| start.login())
}

#[cfg(feature = "packaging-appimage")]
pub fn binary() -> io::Result<PathBuf> {
    root().map(|root| root.join("usr/bin/penguin-mail"))
}

#[cfg(feature = "packaging-appimage")]
pub fn prepare() -> io::Result<()> {
    let command = launcher()?;
    // The explicit dynamic loader is /proc/self/exe in this package, so
    // Claude Code needs the launcher named to start the tool bridge.
    if std::env::var_os("PENGUIN_MAIL_BRIDGE_COMMAND").is_none() {
        // SAFETY: main calls this first, before any other thread exists.
        unsafe { std::env::set_var("PENGUIN_MAIL_BRIDGE_COMMAND", command) };
    }
    Ok(())
}

/// Where the AppArmor profile that allows the sandbox goes.
const PROFILE: &str = "/etc/apparmor.d/penguin-mail-appimage";

/// The AppArmor profile that lets `attach`, and the programs it starts,
/// make the user namespaces bubblewrap needs. Ubuntu 24.04 allows that
/// only to programs a profile names, and has none for bubblewrap.
fn profile(attach: &Path) -> String {
    let mut quoted = String::new();
    for c in attach.to_string_lossy().chars() {
        if matches!(c, '"' | '\\' | '*' | '?' | '[' | ']' | '{' | '}' | '^') {
            quoted.push('\\');
        }
        quoted.push(c);
    }
    format!(
        "abi <abi/4.0>,\ninclude <tunables/global>\n\n\
         profile penguin-mail-appimage \"{quoted}\" flags=(unconfined) {{\n  userns,\n}}\n"
    )
}

/// The commands that install `profile` for `attach` and load it.
fn allow_command(attach: &Path) -> String {
    format!(
        "sudo tee {PROFILE} >/dev/null <<'EOF'\n{}EOF\nsudo apparmor_parser -r {PROFILE}",
        profile(attach)
    )
}

/// Whether WebKit and the image loaders can start their sandboxes.
#[cfg(feature = "packaging-appimage")]
pub enum Sandbox {
    Ready,
    /// The desktop has no `/usr/bin/bwrap`, or no `/usr/bin/xdg-dbus-proxy`,
    /// which WebKit's sandbox starts to reach the session bus.
    Missing,
    /// It has one, but may not make namespaces; the command allows them.
    Blocked {
        command: String,
    },
}

/// Starts bubblewrap the way the sandboxes do. WebKit aborts the app the
/// first time a message opens when it cannot, so the app checks first.
#[cfg(feature = "packaging-appimage")]
pub fn sandbox() -> Sandbox {
    let bwrap = Path::new("/usr/bin/bwrap");
    if !bwrap.exists() || !Path::new("/usr/bin/xdg-dbus-proxy").exists() {
        return Sandbox::Missing;
    }
    let started = std::process::Command::new(bwrap)
        .args([
            "--unshare-all",
            "--die-with-parent",
            "--ro-bind",
            "/",
            "/",
            "true",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    if started {
        return Sandbox::Ready;
    }
    let attach = login().map_or_else(|_| PathBuf::from("Penguin Mail.AppImage"), |(path, _)| path);
    Sandbox::Blocked {
        command: allow_command(&attach),
    }
}

/// Frees the folders extraction leaves in the temporary folder once the
/// app quits for good: this copy's files, and other Penguin Mail folders
/// there no process has open, such as an older version's. This copy's
/// folder stays, empty, so a runtime that deletes it finds it. A launch
/// of the same image unpacks only what is missing, so the next one
/// unpacks again into the folder.
fn release(root: &Path, in_use: impl Fn(&Path) -> bool) {
    let remove = |path: &Path| {
        let removed = match std::fs::symlink_metadata(path) {
            Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(path),
            Ok(_) => std::fs::remove_file(path),
            Err(err) => Err(err),
        };
        if let Err(err) = removed {
            tracing::warn!(path = %path.display(), error = %err, "could not free the unpacked AppImage");
        }
    };
    for entry in std::fs::read_dir(root).into_iter().flatten().flatten() {
        remove(&entry.path());
    }
    let Some(parent) = root.parent() else { return };
    for entry in std::fs::read_dir(parent).into_iter().flatten().flatten() {
        let dir = entry.path();
        let ours = dir
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with(UNPACKED))
            && dir.join("usr/bin/penguin-mail").is_file();
        if ours && dir != root && !in_use(&dir) {
            remove(&dir);
        }
    }
}

/// Whether any process has a file under `dir` mapped, open, or as its
/// working folder.
#[cfg(feature = "packaging-appimage")]
fn in_use(dir: &Path) -> bool {
    let Ok(procs) = std::fs::read_dir("/proc") else {
        return true;
    };
    let inside = |path: &Path| path.starts_with(dir);
    procs.flatten().any(|proc| {
        let proc = proc.path();
        let maps = std::fs::read_to_string(proc.join("maps")).unwrap_or_default();
        maps.lines()
            .filter_map(|line| line.split_whitespace().nth(5))
            .any(|path| inside(Path::new(path)))
            || std::fs::read_link(proc.join("cwd")).is_ok_and(|cwd| inside(&cwd))
            || std::fs::read_dir(proc.join("fd"))
                .into_iter()
                .flatten()
                .flatten()
                .any(|fd| std::fs::read_link(fd.path()).is_ok_and(|path| inside(&path)))
    })
}

/// Runs as the app quits for good, not when it replaces itself.
#[cfg(feature = "packaging-appimage")]
pub fn leave() {
    let Ok(root) = root() else { return };
    let image = std::env::var_os("APPIMAGE")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    if let Ok(Start::Extracted { .. }) = start(&root, image.as_deref()) {
        release(&root, in_use);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn a_mounted_image_starts_at_login_by_its_name_including_spaces() {
        let image = Path::new("/home/ann/My Apps/Mail.AppImage");
        let start = start(Path::new("/tmp/.mount_penguin"), Some(image)).unwrap();
        assert_eq!(start, Start::Mounted { image });
        assert_eq!(start.login(), (image.to_path_buf(), &[][..]));
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_unpacked_by_hand_starts_at_login_through_its_launcher() {
        let start = start(Path::new("/home/ann/Extracted Mail"), None).unwrap();
        assert_eq!(
            start.login(),
            (PathBuf::from("/home/ann/Extracted Mail/AppRun"), &[][..])
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_image_run_without_fuse_unpacks_again_at_login() {
        let root = Path::new("/tmp/appimage_extracted_926583bf6315cbaa");
        let image = Path::new("/home/ann/My Apps/Mail.AppImage");
        let start = start(root, Some(image)).unwrap();
        assert_eq!(start, Start::Extracted { image });
        assert_eq!(
            start.login(),
            (image.to_path_buf(), &["--appimage-extract-and-run"][..])
        );
    }

    fn unpacked(dir: &Path, app: &str) {
        std::fs::create_dir_all(dir.join("usr/bin")).unwrap();
        std::fs::write(dir.join("usr/bin").join(app), b"elf").unwrap();
        std::fs::write(dir.join("AppRun"), b"#!/bin/sh").unwrap();
    }

    #[test]
    fn quitting_frees_this_copy_and_older_unused_ones_only() {
        let temp = tempfile::tempdir().unwrap();
        let this = temp.path().join("appimage_extracted_this");
        let older = temp.path().join("appimage_extracted_older");
        let running = temp.path().join("appimage_extracted_running");
        let other_app = temp.path().join("appimage_extracted_other");
        let kept_by_hand = temp.path().join("Penguin Mail");
        for dir in [&this, &older, &running, &kept_by_hand] {
            unpacked(dir, "penguin-mail");
        }
        unpacked(&other_app, "editor");

        release(&this, |dir| dir == running.as_path());

        // The runtime may still remove the folder itself, so it stays.
        assert!(this.is_dir());
        assert_eq!(std::fs::read_dir(&this).unwrap().count(), 0);
        assert!(!older.exists());
        assert!(running.join("usr/bin/penguin-mail").exists());
        assert!(other_app.join("usr/bin/editor").exists());
        assert!(kept_by_hand.join("usr/bin/penguin-mail").exists());
    }

    #[test]
    fn the_profile_lets_only_this_file_make_namespaces() {
        let text = profile(Path::new("/home/ann/My Apps/Mail.AppImage"));
        assert!(text.contains(
            "profile penguin-mail-appimage \"/home/ann/My Apps/Mail.AppImage\" flags=(unconfined) {"
        ));
        assert!(text.contains("\n  userns,\n"));
        assert!(text.starts_with("abi <abi/4.0>,\n"));
    }

    #[test]
    fn the_profile_keeps_a_strange_name_literal() {
        // AppArmor reads these as quoting or as patterns that would match
        // other files.
        let text = profile(Path::new(r#"/home/ann/"Mail" [1]*?{a,b}\x.AppImage"#));
        assert!(
            text.contains(r#""/home/ann/\"Mail\" \[1\]\*\?\{a,b\}\\x.AppImage""#),
            "{text}"
        );
    }

    #[test]
    fn the_command_writes_the_profile_and_loads_it() {
        let command = allow_command(Path::new("/home/ann/Mail.AppImage"));
        assert!(
            command
                .starts_with("sudo tee /etc/apparmor.d/penguin-mail-appimage >/dev/null <<'EOF'\n")
        );
        assert!(command.contains(&profile(Path::new("/home/ann/Mail.AppImage"))));
        assert!(
            command.ends_with("EOF\nsudo apparmor_parser -r /etc/apparmor.d/penguin-mail-appimage")
        );
    }

    #[test]
    fn relative_runtime_paths_are_rejected() {
        assert!(start(Path::new("relative"), None).is_err());
        assert!(start(Path::new("/tmp/bundle"), Some(Path::new("mail.AppImage"))).is_err());
    }
}
