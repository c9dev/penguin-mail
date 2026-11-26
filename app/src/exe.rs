//! Where the running program lives on disk.

#[cfg(any(not(feature = "packaging-appimage"), test))]
use std::ffi::OsStr;
use std::io;
use std::path::PathBuf;

/// Where the running binary lies, for finding what was installed beside
/// it; a relaunch uses [`launcher`] instead. Installing a new build replaces
/// the file under a running copy, and Linux then reports that copy's
/// executable as `<path> (deleted)`. Starting that path fails, so this
/// answers with the new file that took its place.
pub fn path() -> io::Result<PathBuf> {
    #[cfg(feature = "packaging-appimage")]
    {
        crate::appimage::binary()
    }
    #[cfg(not(feature = "packaging-appimage"))]
    {
        std::env::current_exe().map(replaced)
    }
}

/// `relative` under the folder above the program's own, where the Windows
/// installer puts `share/` and `libexec/`. `None` when the program's path
/// is unknown.
#[cfg(windows)]
pub fn beside(relative: &str) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.join("..").join(relative))
}

/// The command that starts Penguin Mail again: what a restart runs and
/// what the login item names. Every relaunch goes through here, so a
/// package that starts the app some other way changes this one function.
pub fn launcher() -> io::Result<PathBuf> {
    #[cfg(feature = "packaging-appimage")]
    {
        crate::appimage::launcher()
    }
    #[cfg(not(feature = "packaging-appimage"))]
    path().map(|exe| {
        launcher_path(
            exe,
            crate::packaging::BUILT_FOR,
            std::env::var_os("PATH").as_deref(),
        )
    })
}

/// The command and flags the login item runs. Only an AppImage started
/// without FUSE needs a flag, to unpack itself again at login.
pub fn login() -> io::Result<(PathBuf, &'static [&'static str])> {
    #[cfg(feature = "packaging-appimage")]
    {
        crate::appimage::login()
    }
    #[cfg(not(feature = "packaging-appimage"))]
    launcher().map(|exe| (exe, &[][..]))
}

/// Whether a restart must replace this process rather than start a copy
/// beside it and quit. An AppImage's files go away once this process
/// exits; see `appimage::launcher`.
pub fn restarts_in_place() -> bool {
    cfg!(feature = "packaging-appimage")
}

#[cfg(any(not(feature = "packaging-appimage"), test))]
fn launcher_path(
    exe: PathBuf,
    packaging: crate::packaging::Packaging,
    search_path: Option<&OsStr>,
) -> PathBuf {
    // Nix's GTK wrapper supplies the resources every fresh process needs.
    match packaging {
        crate::packaging::Packaging::Nix => {
            let wrapper = exe.with_file_name("penguin-mail");
            // A profile path survives upgrades and garbage collection. Use it
            // only when it launches this build, rather than another install.
            if let (Some(search), Ok(current)) = (search_path, wrapper.canonicalize()) {
                for folder in std::env::split_paths(search) {
                    let command = folder.join("penguin-mail");
                    if command.is_absolute()
                        && !command.starts_with("/nix/store")
                        && command.canonicalize().ok().as_ref() == Some(&current)
                    {
                        return command;
                    }
                }
            }
            wrapper
        }
        _ => exe,
    }
}

#[cfg(any(not(feature = "packaging-appimage"), test))]
fn replaced(exe: PathBuf) -> PathBuf {
    match exe.to_str().and_then(|s| s.strip_suffix(" (deleted)")) {
        Some(installed) => PathBuf::from(installed),
        None => exe,
    }
}

#[cfg(test)]
mod tests {
    use super::{launcher_path, replaced};
    use crate::packaging::Packaging;
    use std::path::PathBuf;

    #[test]
    fn a_nix_login_or_restart_runs_the_wrapper() {
        let prefix = "/nix/store/abc-penguin-mail-1.0.9/bin";
        assert_eq!(
            launcher_path(
                PathBuf::from(format!("{prefix}/.penguin-mail-wrapped")),
                Packaging::Nix,
                None,
            ),
            PathBuf::from(format!("{prefix}/penguin-mail")),
        );
        let native = PathBuf::from("/home/ann/.local/bin/penguin-mail");
        assert_eq!(
            launcher_path(native.clone(), Packaging::Native, None),
            native
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_nix_profile_keeps_the_login_command_valid_across_upgrades() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("store/bin");
        let profile = dir.path().join("profile/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&profile).unwrap();
        let wrapper = bin.join("penguin-mail");
        std::fs::write(&wrapper, "#!/bin/sh\n").unwrap();
        let command = profile.join("penguin-mail");
        std::os::unix::fs::symlink(&wrapper, &command).unwrap();
        let exe = bin.join(".penguin-mail-wrapped");
        assert_eq!(
            launcher_path(exe.clone(), Packaging::Nix, Some(profile.as_os_str())),
            command
        );

        std::fs::remove_file(&command).unwrap();
        let other = dir.path().join("another-build");
        std::fs::write(&other, "#!/bin/sh\n").unwrap();
        std::os::unix::fs::symlink(other, &command).unwrap();
        assert_eq!(
            launcher_path(exe, Packaging::Nix, Some(profile.as_os_str())),
            wrapper
        );
    }

    #[test]
    fn a_replaced_executable_resolves_to_the_file_that_replaced_it() {
        assert_eq!(
            replaced(PathBuf::from("/home/ann/.local/bin/penguin-mail (deleted)")),
            PathBuf::from("/home/ann/.local/bin/penguin-mail")
        );
    }

    #[test]
    fn an_executable_still_on_disk_keeps_its_path() {
        assert_eq!(
            replaced(PathBuf::from("/usr/bin/penguin-mail")),
            PathBuf::from("/usr/bin/penguin-mail")
        );
    }
}
