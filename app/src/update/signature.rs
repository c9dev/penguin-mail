//! Checking that a release's `SHA256SUMS` comes from the project's own key
//! before any checksum in it is trusted.

use std::path::Path;
use std::process::Stdio;

use mailrs_domain::translate::{fill, gettext};

/// The key that signs each release's `SHA256SUMS`, which also signs the apt
/// and dnf repositories, as the binary keyring gpgv reads.
pub const RELEASE_KEY: &[u8] =
    include_bytes!("../../../packaging/apt/penguin-mail-archive-keyring.gpg");

/// The programs that can check the signature, best first. gpgv trusts the
/// keyring it is handed and nothing else, and apt needs it, so every
/// Debian and Ubuntu system has it. gpg does the same job where it is
/// missing.
const CHECKERS: [Checker; 2] = [Checker::Gpgv("gpgv"), Checker::Gpg("gpg")];

/// Hands `sums` back once `signature` proves `key` signed those exact
/// bytes, so no checksum from an unsigned or altered file gets used. A
/// release without a signature is refused too.
pub async fn signed_sums(
    work: &Path,
    sums: &str,
    signature: Option<&str>,
    key: &[u8],
) -> Result<String, String> {
    let Some(signature) = signature else {
        return Err(gettext(
            "This release has no signature, so Penguin Mail will not install it.",
        ));
    };
    check(work, sums, signature, key, &CHECKERS).await
}

enum Checker {
    Gpgv(&'static str),
    Gpg(&'static str),
}

impl Checker {
    /// The command that checks a detached signature against `keyring` alone.
    /// `home` is empty and belongs to this check, so neither the person's
    /// keys nor their GnuPG settings take part, and nothing goes to a key
    /// server.
    fn command(&self, home: &Path, keyring: &Path) -> tokio::process::Command {
        match self {
            Checker::Gpgv(program) => {
                let mut command = tokio::process::Command::new(program);
                command
                    .arg("--homedir")
                    .arg(home)
                    .arg("--keyring")
                    .arg(keyring);
                command
            }
            Checker::Gpg(program) => {
                let mut command = tokio::process::Command::new(program);
                command
                    .arg("--homedir")
                    .arg(home)
                    .args(["--batch", "--no-tty", "--no-auto-key-retrieve"])
                    .args(["--no-default-keyring", "--trust-model", "always"])
                    .arg("--keyring")
                    .arg(keyring)
                    .arg("--verify");
                command
            }
        }
    }
}

/// Checks the signature in a folder of its own under `work`, removed
/// afterwards.
async fn check(
    work: &Path,
    sums: &str,
    signature: &str,
    key: &[u8],
    checkers: &[Checker],
) -> Result<String, String> {
    let dir = work.join("signature");
    let _ = tokio::fs::remove_dir_all(&dir).await;
    let checked = check_in(&dir, sums, signature, key, checkers).await;
    let _ = tokio::fs::remove_dir_all(&dir).await;
    checked.map(|()| sums.to_string())
}

async fn check_in(
    dir: &Path,
    sums: &str,
    signature: &str,
    key: &[u8],
    checkers: &[Checker],
) -> Result<(), String> {
    let could_not = |err: std::io::Error| {
        fill(
            &gettext("Penguin Mail could not check the release's signature: {reason}"),
            &[("reason", &err.to_string())],
        )
    };
    let home = dir.join("home");
    tokio::fs::create_dir_all(&home).await.map_err(could_not)?;
    // GnuPG warns about a home other people can read.
    use std::os::unix::fs::PermissionsExt;
    tokio::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700))
        .await
        .map_err(could_not)?;
    let keyring = dir.join("release-key.gpg");
    let sums_file = dir.join("SHA256SUMS");
    let signature_file = dir.join("SHA256SUMS.asc");
    tokio::fs::write(&keyring, key).await.map_err(could_not)?;
    tokio::fs::write(&sums_file, sums)
        .await
        .map_err(could_not)?;
    tokio::fs::write(&signature_file, signature)
        .await
        .map_err(could_not)?;
    for checker in checkers {
        let output = checker
            .command(&home, &keyring)
            .args(["--status-fd", "1"])
            .arg(&signature_file)
            .arg(&sums_file)
            .stdin(Stdio::null())
            .output()
            .await;
        let output = match output {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => return Err(could_not(err)),
            Ok(output) => output,
        };
        // A good signature exits 0 and says VALIDSIG on the status line;
        // both have to hold.
        let valid = String::from_utf8_lossy(&output.stdout)
            .lines()
            .any(|line| line.starts_with("[GNUPG:] VALIDSIG "));
        if output.status.success() && valid {
            return Ok(());
        }
        tracing::warn!(
            stderr = %String::from_utf8_lossy(&output.stderr).trim(),
            "the release's signature did not check out"
        );
        return Err(gettext(
            "This release is not signed with Penguin Mail's key, so Penguin Mail will not install it.",
        ));
    }
    Err(gettext(
        "Checking the release's signature needs gpgv or gpg, and neither is installed.",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    const SUMS: &str = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824  \
                        penguin-mail_9.9.9_amd64.deb\n";

    /// A throwaway GnuPG home with one signing key and no passphrase.
    struct Signer {
        home: tempfile::TempDir,
    }

    impl Signer {
        fn new() -> Option<Signer> {
            if Command::new("gpg").arg("--version").output().is_err() {
                eprintln!("skipping: no gpg on PATH, so no test release can be signed");
                require_crypto();
                return None;
            }
            let home = tempfile::tempdir().unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
            // A pinentry that cannot run cannot put a dialog on anyone's
            // screen; the agent gets an error instead.
            std::fs::write(
                home.path().join("gpg-agent.conf"),
                "pinentry-program /bin/false\n",
            )
            .unwrap();
            let signer = Signer { home };
            assert!(
                signer
                    .gpg(&[
                        "--passphrase",
                        "",
                        "--quick-generate-key",
                        "Test Release <release@example.test>",
                        "ed25519",
                        "sign",
                        "never",
                    ])
                    .status
                    .success(),
                "gpg could not generate a test key"
            );
            Some(signer)
        }

        fn gpg(&self, args: &[&str]) -> std::process::Output {
            Command::new("gpg")
                .args(["--batch", "--no-tty", "--homedir"])
                .arg(self.home.path())
                .args(args)
                .stdin(Stdio::null())
                .output()
                .expect("gpg runs")
        }

        /// The public key, exported the way the .gpg keyring in
        /// packaging/apt is.
        fn key(&self) -> Vec<u8> {
            let out = self.gpg(&["--export"]);
            assert!(out.status.success() && !out.stdout.is_empty());
            out.stdout
        }

        /// A detached, armored signature over `text`, as the release
        /// workflow writes SHA256SUMS.asc.
        fn sign(&self, text: &str) -> String {
            let file = self.home.path().join("to-sign");
            std::fs::write(&file, text).unwrap();
            let out = self.gpg(&[
                "--pinentry-mode",
                "loopback",
                "--passphrase",
                "",
                "--armor",
                "--detach-sign",
                "--output",
                "-",
                file.to_str().unwrap(),
            ]);
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8(out.stdout).unwrap()
        }
    }

    impl Drop for Signer {
        fn drop(&mut self) {
            let _ = Command::new("gpgconf")
                .arg("--homedir")
                .arg(self.home.path())
                .args(["--kill", "all"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }

    #[tokio::test]
    async fn sums_signed_by_the_key_are_accepted() {
        let Some(signer) = Signer::new() else { return };
        let work = tempfile::tempdir().unwrap();
        let signature = signer.sign(SUMS);
        let sums = signed_sums(work.path(), SUMS, Some(&signature), &signer.key())
            .await
            .unwrap();
        assert_eq!(sums, SUMS);
    }

    #[tokio::test]
    async fn sums_changed_after_signing_are_refused() {
        let Some(signer) = Signer::new() else { return };
        let work = tempfile::tempdir().unwrap();
        let signature = signer.sign(SUMS);
        let tampered = SUMS.replace("2cf2", "0000");
        let refused = signed_sums(work.path(), &tampered, Some(&signature), &signer.key())
            .await
            .unwrap_err();
        assert!(refused.contains("not signed"), "{refused}");
    }

    #[tokio::test]
    async fn sums_without_a_signature_are_refused() {
        let work = tempfile::tempdir().unwrap();
        let refused = signed_sums(work.path(), SUMS, None, RELEASE_KEY)
            .await
            .unwrap_err();
        assert!(refused.contains("no signature"), "{refused}");
    }

    #[tokio::test]
    async fn sums_signed_by_another_key_are_refused() {
        let Some(signer) = Signer::new() else { return };
        let Some(stranger) = Signer::new() else {
            return;
        };
        let work = tempfile::tempdir().unwrap();
        let signature = stranger.sign(SUMS);
        let refused = signed_sums(work.path(), SUMS, Some(&signature), &signer.key())
            .await
            .unwrap_err();
        assert!(refused.contains("not signed"), "{refused}");
        // Nor does the real key accept a stranger's signature.
        let refused = signed_sums(work.path(), SUMS, Some(&signature), RELEASE_KEY)
            .await
            .unwrap_err();
        assert!(refused.contains("not signed"), "{refused}");
    }

    #[tokio::test]
    async fn gpg_checks_the_signature_where_gpgv_is_missing() {
        let Some(signer) = Signer::new() else { return };
        let work = tempfile::tempdir().unwrap();
        let signature = signer.sign(SUMS);
        let key = signer.key();
        let checkers = [
            Checker::Gpgv("penguin-mail-no-such-gpgv"),
            Checker::Gpg("gpg"),
        ];
        let sums = check(work.path(), SUMS, &signature, &key, &checkers)
            .await
            .unwrap();
        assert_eq!(sums, SUMS);
        let refused = check(
            work.path(),
            &SUMS.replace("2cf2", "0000"),
            &signature,
            &key,
            &checkers,
        )
        .await
        .unwrap_err();
        assert!(refused.contains("not signed"), "{refused}");
    }

    #[test]
    fn the_compiled_in_key_is_the_release_key() {
        if Command::new("gpg").arg("--version").output().is_err() {
            require_crypto();
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let keyring = dir.path().join("key.gpg");
        std::fs::write(&keyring, RELEASE_KEY).unwrap();
        let out = Command::new("gpg")
            .args(["--batch", "--no-tty", "--homedir"])
            .arg(dir.path())
            .args(["--with-colons", "--show-keys"])
            .arg(&keyring)
            .output()
            .unwrap();
        let listing = String::from_utf8_lossy(&out.stdout);
        let fingerprints: Vec<&str> = listing
            .lines()
            .filter(|l| l.starts_with("fpr:"))
            .filter_map(|l| l.split(':').nth(9))
            .collect();
        assert_eq!(
            fingerprints.first().copied(),
            Some("FE3C3B6E699AF939DC4670DCF3A853035C3E2B8E"),
            "{listing}"
        );
        assert!(!listing.lines().any(|l| l.starts_with("sec")));
    }

    /// Stops a build machine from passing these tests by skipping them.
    /// Setting `PENGUIN_MAIL_REQUIRE_CRYPTO` turns a missing gpg into a
    /// failure.
    fn require_crypto() {
        if std::env::var_os("PENGUIN_MAIL_REQUIRE_CRYPTO").is_some() {
            panic!(
                "PENGUIN_MAIL_REQUIRE_CRYPTO is set and gpg is not on PATH, \
                 so these tests would have proved nothing"
            );
        }
    }
}
