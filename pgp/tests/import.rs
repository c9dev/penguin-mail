//! Importing a key file the person picked, read from the status lines and
//! then through a real gpg, each run in a GnuPG home of its own.
//!
//! Nothing here touches the keyring of whoever runs the tests. When this
//! computer has no gpg at all, the round trips say so and stop rather than
//! failing.

use std::path::Path;
use std::process::{Command, Stdio};

use mailrs_pgp::gnupg::{Change, Import, import_counts, imports};
use mailrs_pgp::{Pgp, PgpError};

const ADA: &str = "0D89BEDC149B58A80A07BF825DC5D8408E896ECA";

fn lines(text: &str) -> Vec<String> {
    text.lines().map(str::to_string).collect()
}

#[test]
fn a_key_new_to_the_keyring_reads_as_new() {
    let found = imports(&lines(&format!(
        "KEY_CONSIDERED {ADA} 0\nIMPORTED 5DC5D8408E896ECA Ada Lovelace <ada@example.test>\n\
         IMPORT_OK 1 {ADA}\nIMPORT_RES 1 0 1 0 0 0 0 0 0 0 0 0 0 0 0"
    )));
    assert_eq!(
        found,
        [Import {
            fingerprint: ADA.into(),
            change: Change::New,
            secret: false,
            name: None,
            address: None,
        }]
    );
}

#[test]
fn a_key_the_keyring_already_holds_reads_as_unchanged() {
    let found = imports(&lines(&format!("IMPORT_OK 0 {ADA}")));
    assert_eq!(found[0].change, Change::Unchanged);
}

#[test]
fn new_user_ids_signatures_or_subkeys_read_as_updated() {
    for reason in ["2", "4", "8", "14"] {
        let found = imports(&lines(&format!("IMPORT_OK {reason} {ADA}")));
        assert_eq!(found[0].change, Change::Updated, "reason {reason}");
    }
}

#[test]
fn a_secret_key_reported_beside_its_public_half_is_one_import() {
    // gpg writes a line for the public half and another for the secret
    // half of the same key.
    let found = imports(&lines(&format!(
        "IMPORT_OK 0 {ADA}\nKEY_CONSIDERED {ADA} 0\nIMPORT_OK 17 {ADA}\n\
         IMPORT_RES 1 0 0 0 1 0 0 0 0 1 1 0 0 0 0"
    )));
    assert_eq!(found.len(), 1);
    assert!(found[0].secret);
    assert_eq!(found[0].change, Change::New);
}

#[test]
fn a_secret_key_the_keyring_already_holds_stays_unchanged() {
    let found = imports(&lines(&format!("IMPORT_OK 0 {ADA}\nIMPORT_OK 16 {ADA}")));
    assert!(found[0].secret);
    assert_eq!(found[0].change, Change::Unchanged);
}

#[test]
fn the_counts_say_how_many_secret_keys_were_read() {
    let counts =
        import_counts(&lines("IMPORT_RES 2 0 1 0 0 0 0 0 0 1 1 0 0 0")).expect("a count line");
    assert_eq!(counts.considered, 2);
    assert_eq!(counts.secret_read, 1);
    assert_eq!(counts.secret_imported, 1);
    assert!(import_counts(&lines("NODATA 1")).is_none());
}

/// A GnuPG home under a temp directory. It touches no keyring of whoever
/// runs the tests.
struct Home {
    dir: tempfile::TempDir,
    pgp: Pgp,
}

impl Home {
    /// `None` when this computer has no gpg, which is the one reason these
    /// tests skip.
    fn new() -> Option<Home> {
        let Ok(pgp) = Pgp::find() else {
            eprintln!("skipping: no gpg on PATH, so the imports cannot run");
            require_crypto();
            return None;
        };
        let dir = tempfile::tempdir().expect("a temp directory");
        // gpg refuses a home anyone else can read.
        permit_owner_only(dir.path());
        // gpg-agent asks the person things through a pinentry window, and
        // a test must never put one on somebody's screen.
        std::fs::write(
            dir.path().join("gpg-agent.conf"),
            "pinentry-program /bin/false\n",
        )
        .expect("write");
        Some(Home {
            pgp: pgp.with_home(dir.path()),
            dir,
        })
    }

    fn gpg(&self) -> Command {
        let mut command = Command::new(self.pgp.program());
        command
            .args(["--batch", "--no-tty", "--homedir"])
            .arg(self.dir.path())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        command
    }

    /// Makes a key protected by a passphrase, as a person's own key is.
    fn generate(&self, user_id: &str) {
        let made = self
            .gpg()
            .args([
                "--passphrase",
                "secret",
                "--quick-generate-key",
                user_id,
                "future-default",
                "default",
                "0",
            ])
            .status()
            .expect("gpg runs");
        assert!(made.success(), "gpg could not generate a test key");
    }

    fn export(&self, what: &str) -> Vec<u8> {
        let out = self
            .gpg()
            .args(["--pinentry-mode", "loopback", "--passphrase", "secret"])
            .args(["--armor", what, "--"])
            .output()
            .expect("gpg runs");
        assert!(out.status.success(), "gpg could not export");
        out.stdout
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        // The agent gpg started holds sockets open under the temp directory.
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

fn require_crypto() {
    if std::env::var_os("PENGUIN_MAIL_REQUIRE_CRYPTO").is_some() {
        panic!(
            "PENGUIN_MAIL_REQUIRE_CRYPTO is set and GnuPG is not on PATH, \
             so these tests would have proved nothing"
        );
    }
}

/// Two homes, one holding Ada's key and one empty, for moving a key file
/// from one to the other.
fn two_homes() -> Option<(Home, Home)> {
    let theirs = Home::new()?;
    let mine = Home::new()?;
    theirs.generate("Ada Lovelace <ada@example.test>");
    Some((theirs, mine))
}

#[test]
fn a_public_key_comes_in_with_its_name_and_address() {
    let Some((theirs, mine)) = two_homes() else {
        return;
    };
    let file = theirs.export("--export");

    let found = mine.pgp.import(&file).expect("an import");

    assert_eq!(found.len(), 1);
    assert_eq!(found[0].change, Change::New);
    assert!(!found[0].secret);
    assert_eq!(found[0].name.as_deref(), Some("Ada Lovelace"));
    assert_eq!(found[0].address.as_deref(), Some("ada@example.test"));
    let held = mine
        .pgp
        .keys_for(&["ada@example.test".into()])
        .expect("a listing");
    assert!(held[0].key.is_some(), "the key is in the keyring");
}

#[test]
fn the_same_key_twice_comes_back_unchanged() {
    let Some((theirs, mine)) = two_homes() else {
        return;
    };
    let file = theirs.export("--export");
    mine.pgp.import(&file).expect("an import");

    let found = mine.pgp.import(&file).expect("an import");

    assert_eq!(found[0].change, Change::Unchanged);
}

#[test]
fn a_secret_key_comes_in_without_asking_for_its_passphrase() {
    // The pinentry in these homes cannot run, so an import that asked for
    // the passphrase would fail. gpg keeps the key under the passphrase it
    // came with and asks for it the first time the key signs.
    let Some((theirs, mine)) = two_homes() else {
        return;
    };
    let file = theirs.export("--export-secret-keys");

    let found = mine.pgp.import(&file).expect("an import");

    assert_eq!(found.len(), 1);
    assert!(found[0].secret);
    assert_eq!(found[0].change, Change::New);
    assert_eq!(found[0].name.as_deref(), Some("Ada Lovelace"));
}

#[test]
fn a_file_that_holds_no_key_says_so() {
    let Some(mine) = Home::new() else {
        return;
    };

    let refused = mine.pgp.import(b"Dear Ada, here is my key.\n");

    assert!(matches!(refused, Err(PgpError::NotAKey)), "{refused:?}");
}

#[test]
fn an_empty_file_holds_no_key_either() {
    let Some(mine) = Home::new() else {
        return;
    };

    let refused = mine.pgp.import(b"");

    assert!(matches!(refused, Err(PgpError::NotAKey)), "{refused:?}");
}
