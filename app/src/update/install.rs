//! Installing a downloaded release over the running copy.

use std::path::{Component, Path, PathBuf};
use std::process::Stdio;

use sha2::{Digest, Sha256};

use super::version::{Method, Version};
use crate::packaging::Packaging;

/// How this copy updates itself, or None when it does not. The rpm, a
/// Flatpak and a snap leave it to dnf or their store. An AppImage replaces
/// the file `crate::exe::appimage` found, passed here as `appimage`;
/// without one it runs unpacked and cannot update. Otherwise the binary's
/// path decides: under `/usr` it came from the .deb, inside a cargo
/// `target` directory it is a build tree, which never updates, and
/// anywhere else it came from a tarball or `scripts/install.sh`, into the
/// prefix two levels above it.
pub fn method_for(packaging: Packaging, exe: &Path, appimage: Option<&Path>) -> Option<Method> {
    if packaging.updated_by().is_some() {
        return None;
    }
    if packaging == Packaging::AppImage {
        return appimage.map(|file| Method::AppImage {
            file: file.to_path_buf(),
        });
    }
    if exe.starts_with("/usr") {
        return Some(Method::Deb);
    }
    if exe
        .components()
        .any(|c| c == Component::Normal("target".as_ref()))
    {
        return None;
    }
    let prefix = exe.parent()?.parent()?;
    Some(Method::Local {
        prefix: prefix.to_path_buf(),
    })
}

/// The checksum `SHA256SUMS` gives for one file, in `sha256sum`'s format.
pub fn expected_sum(sums: &str, file: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (sum, name) = line.split_once(char::is_whitespace)?;
        (name.trim_start_matches([' ', '*']) == file).then(|| sum.to_lowercase())
    })
}

/// Refuses a download whose bytes do not hash to what the release lists.
pub fn verify(path: &Path, sums: &str, file: &str) -> Result<(), String> {
    let expected =
        expected_sum(sums, file).ok_or_else(|| format!("SHA256SUMS does not list {file}"))?;
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let actual: String = Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{file} does not match its checksum"))
    }
}

/// Why an install stopped, and the log that says more.
#[derive(Debug)]
pub struct Failed {
    pub reason: String,
    pub log: PathBuf,
}

/// pkexec exits 126 when the person dismisses the password dialog, and 127
/// when they may not act as root at all.
const PKEXEC_REFUSED: [i32; 2] = [126, 127];

/// Installs `package` over this copy, writing everything the installer says
/// to `install.log` in `work`. A .deb goes through apt as root. A tarball
/// unpacks into `work` and runs its own `install-files.sh`, which leaves the
/// person's login item as it is. An AppImage replaces the running file.
pub async fn run(
    method: &Method,
    version: Version,
    package: &Path,
    work: &Path,
) -> Result<(), Failed> {
    let log = work.join("install.log");
    let fail = |reason: String| Failed {
        reason,
        log: log.clone(),
    };
    std::fs::create_dir_all(work).map_err(|e| fail(e.to_string()))?;
    let out = std::fs::File::create(&log).map_err(|e| fail(e.to_string()))?;
    let output = || out.try_clone().map_err(|e| fail(e.to_string()));
    let mut command = match method {
        Method::AppImage { file } => {
            return replace_appimage(package, file)
                .map_err(|e| fail(format!("could not replace {}: {e}", file.display())));
        }
        Method::Deb => {
            let mut apt = tokio::process::Command::new("pkexec");
            apt.args(["apt-get", "install", "-y"]).arg(package);
            apt
        }
        Method::Local { prefix } => {
            let unpacked = tokio::process::Command::new("tar")
                .arg("-xzf")
                .arg(package)
                .arg("-C")
                .arg(work)
                .stdin(Stdio::null())
                .stdout(output()?)
                .stderr(output()?)
                .status()
                .await
                .map_err(|e| fail(e.to_string()))?;
            if !unpacked.success() {
                return Err(fail(format!("could not unpack {}", package.display())));
            }
            let tree = work.join(format!("penguin-mail-{version}-x86_64"));
            let mut script = tokio::process::Command::new(tree.join("install-files.sh"));
            script
                .arg(&tree)
                .env("PREFIX", prefix)
                .env("NO_AUTOSTART", "1");
            script
        }
    };
    let status = command
        .stdin(Stdio::null())
        .stdout(output()?)
        .stderr(output()?)
        .status()
        .await
        .map_err(|e| fail(e.to_string()))?;
    match status.code() {
        Some(0) => Ok(()),
        Some(code) if *method == Method::Deb && PKEXEC_REFUSED.contains(&code) => {
            Err(fail("the password prompt was dismissed".into()))
        }
        _ => Err(fail(format!("the installer stopped with {status}"))),
    }
}

/// Puts the new AppImage where the running one lies, under the same name,
/// so a launcher or login item pointing at it starts the new version. It
/// lands beside the old file first and is renamed over it, so a copy that
/// fails halfway leaves the old AppImage whole. The running copy keeps the
/// old file open and goes on working until it restarts.
fn replace_appimage(package: &Path, file: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let name = file
        .file_name()
        .ok_or_else(|| std::io::Error::other("the AppImage path has no file name"))?;
    let mut staged_name = std::ffi::OsString::from(".");
    staged_name.push(name);
    staged_name.push(".new");
    let staged = file.with_file_name(staged_name);
    let placed = std::fs::copy(package, &staged)
        .and_then(|_| std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755)))
        .and_then(|()| std::fs::rename(&staged, file));
    if placed.is_err() {
        let _ = std::fs::remove_file(&staged);
    }
    placed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_binary_path_says_how_this_copy_was_installed() {
        let native = |exe: &str| method_for(Packaging::Native, Path::new(exe), None);
        assert_eq!(native("/usr/bin/penguin-mail"), Some(Method::Deb));
        assert_eq!(
            native("/home/ann/.local/bin/penguin-mail"),
            Some(Method::Local {
                prefix: "/home/ann/.local".into()
            })
        );
        assert_eq!(native("/home/ann/mail/target/release/penguin-mail"), None);
    }

    #[test]
    fn a_package_something_else_updates_has_no_method() {
        // The rpm installs under /usr as the .deb does, and dnf, not apt,
        // brings its new versions.
        assert_eq!(
            method_for(Packaging::Rpm, Path::new("/usr/bin/penguin-mail"), None),
            None
        );
        assert_eq!(
            method_for(Packaging::Flatpak, Path::new("/app/bin/penguin-mail"), None),
            None
        );
        assert_eq!(
            method_for(
                Packaging::Snap,
                Path::new("/snap/penguin-mail/12/usr/bin/penguin-mail"),
                None
            ),
            None
        );
    }

    #[test]
    fn an_appimage_updates_the_file_it_runs_from() {
        let mounted = Path::new("/tmp/.mount_penguiXYZ/usr/bin/penguin-mail");
        let file = Path::new("/home/ann/Applications/penguin-mail.AppImage");
        assert_eq!(
            method_for(Packaging::AppImage, mounted, Some(file)),
            Some(Method::AppImage {
                file: file.to_path_buf()
            })
        );
        // Unpacked with --appimage-extract, there is no file to replace.
        assert_eq!(method_for(Packaging::AppImage, mounted, None), None);
    }

    #[tokio::test]
    async fn a_new_appimage_takes_the_old_ones_place_and_name() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("Penguin Mail.AppImage");
        std::fs::write(&file, "old").unwrap();
        let package = dir.path().join("penguin-mail-9.9.9-x86_64.AppImage");
        std::fs::write(&package, "new").unwrap();
        let method = Method::AppImage { file: file.clone() };
        let work = dir.path().join("work");
        run(&method, Version::parse("9.9.9").unwrap(), &package, &work)
            .await
            .unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "new");
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755);
        let left: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter(|name| name.ends_with(".new"))
            .collect();
        assert!(left.is_empty(), "{left:?}");
    }

    #[tokio::test]
    async fn an_appimage_in_a_folder_it_cannot_write_keeps_the_old_one() {
        let dir = tempfile::tempdir().unwrap();
        let package = dir.path().join("new.AppImage");
        std::fs::write(&package, "new").unwrap();
        let method = Method::AppImage {
            file: dir.path().join("missing-folder").join("penguin-mail.AppImage"),
        };
        let work = dir.path().join("work");
        let failed = run(&method, Version::parse("9.9.9").unwrap(), &package, &work)
            .await
            .unwrap_err();
        assert!(failed.reason.contains("could not replace"), "{}", failed.reason);
    }

    #[test]
    fn the_sums_file_names_each_file_once() {
        let sums = "AA11  penguin-mail_0.2.0_amd64.deb\nbb22 *penguin-mail-0.2.0-x86_64.tar.gz\n";
        assert_eq!(
            expected_sum(sums, "penguin-mail-0.2.0-x86_64.tar.gz").as_deref(),
            Some("bb22")
        );
        assert_eq!(
            expected_sum(sums, "penguin-mail_0.2.0_amd64.deb").as_deref(),
            Some("aa11")
        );
        assert_eq!(expected_sum(sums, "other"), None);
    }

    #[test]
    fn a_download_that_does_not_match_its_sum_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("pkg");
        std::fs::write(&file, b"hello").unwrap();
        let good = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824  pkg\n";
        assert!(verify(&file, good, "pkg").is_ok());
        assert!(verify(&file, &good.replace("2cf2", "0000"), "pkg").is_err());
        assert!(verify(&file, good, "missing").is_err());
    }

    #[tokio::test]
    async fn a_tarball_installs_into_the_prefix_through_its_own_script() {
        let dir = tempfile::tempdir().unwrap();
        let name = "penguin-mail-9.9.9-x86_64";
        let tree = dir.path().join("src").join(name);
        std::fs::create_dir_all(tree.join("bin")).unwrap();
        std::fs::write(tree.join("bin/penguin-mail"), "new").unwrap();
        // Stands in for install-files.sh: copies the binary and says what
        // it was told about the login item.
        let script = tree.join("install-files.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\nset -e\nmkdir -p \"$PREFIX/bin\"\n\
             cp \"$1/bin/penguin-mail\" \"$PREFIX/bin/\"\n\
             echo \"autostart=$NO_AUTOSTART\"\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let package = dir.path().join(format!("{name}.tar.gz"));
        assert!(
            std::process::Command::new("tar")
                .arg("-czf")
                .arg(&package)
                .arg("-C")
                .arg(dir.path().join("src"))
                .arg(name)
                .status()
                .unwrap()
                .success()
        );
        let prefix = dir.path().join("prefix");
        let work = dir.path().join("work");
        let method = Method::Local {
            prefix: prefix.clone(),
        };
        run(&method, Version::parse("9.9.9").unwrap(), &package, &work)
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(prefix.join("bin/penguin-mail")).unwrap(),
            "new"
        );
        let log = std::fs::read_to_string(work.join("install.log")).unwrap();
        assert!(log.contains("autostart=1"), "{log}");
    }

    #[tokio::test]
    async fn a_package_that_will_not_unpack_reports_its_log() {
        let dir = tempfile::tempdir().unwrap();
        let method = Method::Local {
            prefix: dir.path().join("prefix"),
        };
        let missing = dir.path().join("nothing.tar.gz");
        let work = dir.path().join("work");
        let failed = run(&method, Version::parse("9.9.9").unwrap(), &missing, &work)
            .await
            .unwrap_err();
        assert_eq!(failed.log, work.join("install.log"));
        assert!(failed.reason.contains("could not unpack"));
    }
}
