//! The shell scripts a release runs: `changelog.sh`, which writes the
//! release notes, and `stage.sh` with `install-files.sh`, which lay out the
//! tarball and install it.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const ID: &str = "io.github.c9dev.PenguinMail";

fn scripts() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../scripts")
}

fn succeeded(output: &Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout.clone()).unwrap()
}

const CHANGELOG: &str = "# Changelog

Each release, newest first.

## Unreleased

## 1.1.0 (2026-01-02)

### New

- One.

### Fixed

- Two.


## 1.0.0 (2026-01-01)

- Zero.


";

/// Runs `changelog.sh section <version>` against `CHANGELOG` in a copy of
/// the repository's layout, since the script reads the file beside its own
/// folder. The copies run through bash: executing a file just written can
/// fail with "text file busy" while another test forks.
fn section(version: &str) -> Output {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("scripts")).unwrap();
    let script = dir.path().join("scripts/changelog.sh");
    std::fs::copy(scripts().join("changelog.sh"), &script).unwrap();
    std::fs::write(dir.path().join("CHANGELOG.md"), CHANGELOG).unwrap();
    Command::new("bash")
        .arg(&script)
        .args(["section", version])
        .output()
        .unwrap()
}

#[test]
fn a_changelog_section_is_its_body_without_the_blank_lines_around_it() {
    assert_eq!(
        succeeded(&section("1.1.0")),
        "### New\n\n- One.\n\n### Fixed\n\n- Two.\n"
    );
    assert_eq!(succeeded(&section("1.0.0")), "- Zero.\n");
    assert_eq!(succeeded(&section("Unreleased")), "");
}

/// The release workflow writes the notes with this command, so a version
/// with no section must fail the run rather than publish empty notes. A
/// prefix of a version is not that version.
#[test]
fn a_version_with_no_changelog_section_fails() {
    for version in ["9.9.9", "1.1"] {
        let output = section(version);
        assert!(!output.status.success(), "{version} found a section");
        assert!(output.stdout.is_empty(), "{version}");
    }
}

/// Stages a build the way the release workflow does, packs it the way
/// `package.sh` packs the tarball, and installs it with the tarball's own
/// `./install-files.sh .`, as its README says.
#[test]
fn a_staged_tree_installs_from_the_tarball_folder() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target");
    std::fs::create_dir_all(target.join("release")).unwrap();
    for name in ["penguin-mail", "penguin-mail-cli"] {
        let binary = target.join("release").join(name);
        std::fs::write(&binary, format!("#!/bin/sh\necho {name}\n")).unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let folder = dir.path().join("penguin-mail-9.9.9-x86_64");
    succeeded(
        &Command::new(scripts().join("stage.sh"))
            .arg(&folder)
            .env("CARGO_TARGET_DIR", &target)
            .output()
            .unwrap(),
    );
    assert!(
        folder
            .join(format!("share/metainfo/{ID}.metainfo.xml"))
            .is_file()
    );
    std::fs::copy(
        scripts().join("install-files.sh"),
        folder.join("install-files.sh"),
    )
    .unwrap();

    let prefix = dir.path().join("prefix");
    let home = dir.path().join("home");
    succeeded(
        &Command::new("bash")
            .args(["install-files.sh", "."])
            .current_dir(&folder)
            .env("PREFIX", &prefix)
            .env("HOME", &home)
            .env_remove("NO_AUTOSTART")
            .output()
            .unwrap(),
    );

    for name in ["penguin-mail", "penguin-mail-cli"] {
        let installed = prefix.join("bin").join(name);
        assert_eq!(
            std::fs::read_to_string(&installed).unwrap(),
            format!("#!/bin/sh\necho {name}\n")
        );
        let mode = std::fs::metadata(&installed).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755, "{name}");
    }
    let icons = prefix.join("share/icons/hicolor");
    assert!(icons.join(format!("scalable/apps/{ID}.svg")).is_file());
    assert!(
        icons
            .join(format!("symbolic/apps/{ID}-symbolic.svg"))
            .is_file()
    );

    // The launcher starts the installed file, since ~/.local/bin is not on
    // the PATH a desktop session starts with.
    let exe = format!("\"{}\"", prefix.join("bin/penguin-mail").display());
    let launcher =
        std::fs::read_to_string(prefix.join(format!("share/applications/{ID}.desktop"))).unwrap();
    assert!(
        launcher.contains(&format!("\nExec={exe} %u\n")),
        "{launcher}"
    );
    assert!(!launcher.contains("Exec=penguin-mail"), "{launcher}");
    let login =
        std::fs::read_to_string(home.join(format!(".config/autostart/{ID}.desktop"))).unwrap();
    assert!(
        login.contains(&format!("\nExec={exe} --background\n")),
        "{login}"
    );

    // Without msgfmt stage.sh leaves the translations out on purpose.
    let has_msgfmt = Command::new("msgfmt").arg("--version").output().is_ok();
    if has_msgfmt {
        assert!(
            prefix
                .join("share/locale/pt_PT/LC_MESSAGES/penguin-mail.mo")
                .is_file()
        );
        assert!(launcher.contains("Name[pt_PT]="), "{launcher}");
    }
}
