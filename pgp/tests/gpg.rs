//! Round trips through a real gpg, each in a GnuPG home of its own.
//!
//! Nothing here touches the keyring of whoever runs the tests. When this
//! computer has no gpg at all, every test says so and stops rather than
//! failing.

use std::path::Path;
use std::process::{Command, Stdio};

use mailrs_pgp::{Pgp, Verdict};

/// A GnuPG home under a temp directory, with one key in it.
struct Home {
    dir: tempfile::TempDir,
    pgp: Pgp,
    address: String,
}

impl Home {
    /// `None` when this computer has no gpg, which is the one reason these
    /// tests skip.
    fn new(name: &str, address: &str) -> Option<Home> {
        let pgp = match Pgp::find() {
            Ok(pgp) => pgp,
            Err(_) => {
                eprintln!("skipping: no gpg on PATH, so the round trips cannot run");
                return None;
            }
        };
        let dir = tempfile::tempdir().expect("a temp directory");
        // gpg refuses a home anyone else can read.
        permit_owner_only(dir.path());
        // `future-default` is an ed25519 key with a cv25519 subkey to encrypt
        // to. Asking for `ed25519` by name gives a key that can only sign.
        let made = Command::new(pgp.program())
            .args(["--batch", "--no-tty", "--homedir"])
            .arg(dir.path())
            .args([
                "--passphrase",
                "",
                "--quick-generate-key",
                &format!("{name} <{address}>"),
                "future-default",
                "default",
                "0",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("gpg runs");
        assert!(made.success(), "gpg could not generate a test key");
        Some(Home {
            pgp: pgp.with_home(dir.path()),
            dir,
            address: address.to_string(),
        })
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        // The agent gpg started holds sockets open under the temp directory.
        // Killing it leaves nothing running once the directory goes.
        let _ = Command::new("gpgconf")
            .arg("--homedir")
            .arg(self.dir.path())
            .args(["--kill", "gpg-agent"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

#[cfg(unix)]
fn permit_owner_only(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).expect("chmod");
}

#[cfg(not(unix))]
fn permit_owner_only(_path: &Path) {}

/// A detached signature over `data`, made by gpg itself rather than by the
/// code under test, so verification is checked against the real thing.
fn detached(home: &Home, data: &[u8]) -> Vec<u8> {
    let file = home.dir.path().join("signed");
    std::fs::write(&file, data).expect("write");
    let out = Command::new(home.pgp.program())
        .args(["--batch", "--no-tty", "--homedir"])
        .arg(home.dir.path())
        // Without `--output -` gpg writes beside the file it read.
        .args([
            "--armor",
            "--output",
            "-",
            "--detach-sign",
            "--local-user",
            &home.address,
        ])
        .arg(&file)
        .output()
        .expect("gpg runs");
    assert!(out.status.success(), "gpg could not sign");
    out.stdout
}

/// Ciphertext for `home`'s own key, made by gpg itself.
fn sealed(home: &Home, data: &[u8], sign: bool) -> Vec<u8> {
    let file = home.dir.path().join("plain");
    std::fs::write(&file, data).expect("write");
    let mut command = Command::new(home.pgp.program());
    command
        .args(["--batch", "--no-tty", "--homedir"])
        .arg(home.dir.path())
        .args([
            "--armor",
            "--output",
            "-",
            "--encrypt",
            "--recipient",
            &home.address,
        ]);
    if sign {
        command.args(["--sign", "--local-user", &home.address]);
    }
    let out = command.arg(&file).output().expect("gpg runs");
    assert!(out.status.success(), "gpg could not encrypt");
    out.stdout
}

#[test]
fn a_signature_over_the_part_verifies_and_names_its_signer() {
    let Some(home) = Home::new("Ada Lovelace", "ada@example.test") else {
        return;
    };
    let part = b"Content-Type: text/plain\r\n\r\nMeet at six.\r\n";
    let signature = detached(&home, part);

    let found = home.pgp.verify(part, &signature).expect("a verdict");

    assert_eq!(found.verdict, Verdict::Good);
    assert!(found.is_good());
    assert_eq!(
        found.signer.as_deref(),
        Some("Ada Lovelace <ada@example.test>")
    );
    assert_eq!(found.trust, mailrs_pgp::Trust::Ultimate);
    assert!(found.fingerprint.is_some());
}

#[test]
fn a_body_changed_after_signing_fails_to_verify() {
    let Some(home) = Home::new("Ada Lovelace", "ada@example.test") else {
        return;
    };
    let part = b"Content-Type: text/plain\r\n\r\nMeet at six.\r\n";
    let signature = detached(&home, part);
    let tampered = b"Content-Type: text/plain\r\n\r\nMeet at nine.\r\n";

    let found = home.pgp.verify(tampered, &signature).expect("a verdict");

    assert_eq!(found.verdict, Verdict::Bad);
    assert!(!found.is_good());
}

#[test]
fn a_signature_from_a_key_we_do_not_hold_says_so() {
    let Some(stranger) = Home::new("Grace Hopper", "grace@example.test") else {
        return;
    };
    let Some(mine) = Home::new("Ada Lovelace", "ada@example.test") else {
        return;
    };
    let part = b"Content-Type: text/plain\r\n\r\nFrom someone else.\r\n";
    let signature = detached(&stranger, part);

    let found = mine.pgp.verify(part, &signature).expect("a verdict");

    assert_eq!(found.verdict, Verdict::NoKey);
    assert!(!found.is_good());
    assert!(found.key_id.is_some());
}

#[test]
fn decrypting_gives_back_the_part_that_was_inside() {
    let Some(home) = Home::new("Ada Lovelace", "ada@example.test") else {
        return;
    };
    let part = b"Content-Type: text/plain\r\n\r\nThe key is under the mat.\r\n";
    let ciphertext = sealed(&home, part, false);

    let opened = home.pgp.decrypt(&ciphertext).expect("the part inside");

    assert_eq!(opened.part, part);
    assert!(opened.signature.is_none());
}

#[test]
fn a_signature_inside_the_encryption_comes_back_with_it() {
    let Some(home) = Home::new("Ada Lovelace", "ada@example.test") else {
        return;
    };
    let part = b"Content-Type: text/plain\r\n\r\nSigned and sealed.\r\n";
    let ciphertext = sealed(&home, part, true);

    let opened = home.pgp.decrypt(&ciphertext).expect("the part inside");

    assert_eq!(opened.part, part);
    let signature = opened.signature.expect("a signature");
    assert!(signature.is_good());
    assert_eq!(
        signature.signer.as_deref(),
        Some("Ada Lovelace <ada@example.test>")
    );
}

#[test]
fn a_message_sealed_for_somebody_else_says_so() {
    let Some(stranger) = Home::new("Grace Hopper", "grace@example.test") else {
        return;
    };
    let Some(mine) = Home::new("Ada Lovelace", "ada@example.test") else {
        return;
    };
    let ciphertext = sealed(&stranger, b"Not for you.\r\n", false);

    let err = mine.pgp.decrypt(&ciphertext).expect_err("no key for it");

    assert!(
        matches!(err, mailrs_pgp::PgpError::NotForYou),
        "expected NotForYou, got {err}"
    );
}
