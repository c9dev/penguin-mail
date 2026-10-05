//! Importing a certificate file the person picked, through a real gpgsm,
//! each run in a GnuPG home of its own.
//!
//! A PKCS#12 file opens with a passphrase, and gpgsm asks gpg-agent for
//! it, which asks a pinentry. These homes name a pinentry script that
//! answers from what the test wrote into it and puts nothing on a screen,
//! so the import runs the way the app runs it, with the person's own
//! pinentry allowed, and nobody is asked. Nothing touches the keybox of
//! whoever runs the tests. When this computer has no gpgsm, every test
//! says so and stops rather than failing.

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use mailrs_pgp::gnupg::Change;
use mailrs_smime::{Smime, SmimeError};

/// The passphrase the test PKCS#12 file opens with.
const PASSPHRASE: &str = "correct horse 7";

/// What a pinentry does when gpg-agent asks it for a passphrase.
enum Answer {
    /// Types this passphrase.
    Types(&'static str),
    /// Presses Cancel.
    Cancels,
}

/// A GnuPG home under a temp directory.
struct Home {
    dir: tempfile::TempDir,
    smime: Smime,
}

impl Home {
    /// `None` when this computer has no gpgsm, which is the one reason
    /// these tests skip.
    fn new(answer: Answer) -> Option<Home> {
        let Ok(smime) = Smime::find() else {
            eprintln!("skipping: no gpgsm on PATH, so the imports cannot run");
            require_crypto();
            return None;
        };
        let dir = tempfile::tempdir().expect("a temp directory");
        // gpgsm refuses a home anyone else can read.
        permit_owner_only(dir.path());
        let pinentry = dir.path().join("pinentry");
        std::fs::write(&pinentry, script(&answer)).expect("write");
        permit_owner_only(&pinentry);
        std::fs::write(
            dir.path().join("gpg-agent.conf"),
            format!("pinentry-program {}\n", pinentry.to_string_lossy()),
        )
        .expect("write");
        Some(Home {
            smime: smime.with_home(dir.path()),
            dir,
        })
    }

    fn gpgsm(&self) -> Command {
        let mut command = Command::new(self.smime.program());
        command
            .args(["--batch", "--no-tty", "--homedir"])
            .arg(self.dir.path())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        command
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        // The agent gpgsm started holds sockets open under the temp
        // directory.
        let _ = Command::new("gpgconf")
            .arg("--homedir")
            .arg(self.dir.path())
            .args(["--kill", "gpg-agent"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// A pinentry speaks the Assuan protocol on its stdin and stdout: it
/// greets, then answers each command with `OK`, and `GETPIN` with the
/// passphrase as a `D` line first. A wrong passphrase gets the same
/// answer each time the agent asks again.
fn script(answer: &Answer) -> String {
    let getpin = match answer {
        Answer::Types(passphrase) => format!("echo 'D {passphrase}'; echo OK"),
        Answer::Cancels => "echo 'ERR 83886179 Operation cancelled'".to_string(),
    };
    format!(
        "#!/bin/sh\necho 'OK ready'\nwhile read -r command rest; do\n\
         case \"$command\" in\n\
         GETPIN) {getpin};;\n\
         BYE) echo OK; exit 0;;\n\
         *) echo OK;;\n\
         esac\ndone\n"
    )
}

/// One certificate with its key, as a certificate authority's files
/// carry it: the certificate alone in PEM, and certificate and key in a
/// PKCS#12 file under [`PASSPHRASE`].
struct Files {
    pem: Vec<u8>,
    p12: Vec<u8>,
}

/// The files, made once for every test here: an RSA key takes a moment.
/// `None` when this computer has no gpgsm.
fn files() -> Option<&'static Files> {
    static FILES: OnceLock<Option<Files>> = OnceLock::new();
    FILES.get_or_init(make).as_ref()
}

fn make() -> Option<Files> {
    let maker = Home::new(Answer::Types(PASSPHRASE))?;
    let params = maker.dir.path().join("params");
    std::fs::write(
        &params,
        "Key-Type: RSA\nKey-Length: 2048\nKey-Usage: sign, encrypt\n\
         Serial: random\nName-DN: CN=Ada Lovelace,O=Example\n\
         Name-Email: ada@example.test\nNot-After: 2038-01-01\n%commit\n",
    )
    .expect("write");
    let pem = maker.dir.path().join("certificate.pem");
    // The key itself gets no passphrase: under a loopback pinentry an
    // empty stdin says there is none.
    let made = maker
        .gpgsm()
        .args(["--pinentry-mode", "loopback", "--passphrase-fd", "0"])
        .args(["--armor", "--generate-key", "--output"])
        .arg(&pem)
        .arg(&params)
        .stdin(Stdio::null())
        .status()
        .expect("gpgsm runs");
    assert!(made.success(), "gpgsm could not generate a certificate");
    let imported = maker
        .gpgsm()
        .arg("--import")
        .arg(&pem)
        .status()
        .expect("gpgsm runs");
    assert!(imported.success(), "gpgsm could not import it");
    // The export asks the pinentry for a passphrase to put on the file,
    // and the script types it.
    let p12 = maker
        .gpgsm()
        .args(["--export-secret-key-p12", "ada@example.test"])
        .output()
        .expect("gpgsm runs");
    assert!(
        p12.status.success() && !p12.stdout.is_empty(),
        "gpgsm could not export a PKCS#12 file"
    );
    Some(Files {
        pem: std::fs::read(&pem).expect("read"),
        p12: p12.stdout,
    })
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

#[test]
fn a_certificate_comes_in_with_its_name_and_address() {
    let (Some(files), Some(mine)) = (files(), Home::new(Answer::Cancels)) else {
        return;
    };

    let found = mine.smime.import(&files.pem).expect("an import");

    assert_eq!(found.len(), 1);
    assert_eq!(found[0].change, Change::New);
    assert!(!found[0].secret, "a PEM certificate carries no key");
    assert_eq!(found[0].name.as_deref(), Some("Ada Lovelace"));
    assert_eq!(found[0].address.as_deref(), Some("ada@example.test"));
}

#[test]
fn the_same_certificate_twice_comes_back_unchanged() {
    let (Some(files), Some(mine)) = (files(), Home::new(Answer::Cancels)) else {
        return;
    };
    mine.smime.import(&files.pem).expect("an import");

    let found = mine.smime.import(&files.pem).expect("an import");

    assert_eq!(found[0].change, Change::Unchanged);
}

#[test]
fn a_pkcs12_file_brings_its_secret_key_once_the_passphrase_is_typed() {
    let (Some(files), Some(mine)) = (files(), Home::new(Answer::Types(PASSPHRASE))) else {
        return;
    };

    let found = mine.smime.import(&files.p12).expect("an import");

    assert_eq!(found.len(), 1);
    assert!(found[0].secret);
    assert_eq!(found[0].change, Change::New);
    assert_eq!(found[0].name.as_deref(), Some("Ada Lovelace"));
    let signing = mine
        .smime
        .signing_certificates(&["ada@example.test".into()])
        .expect("a listing");
    assert!(
        signing[0].certificate.is_some(),
        "the key can sign as Ada now"
    );
}

#[test]
fn a_pkcs12_file_over_a_certificate_already_held_still_brings_the_key() {
    let (Some(files), Some(mine)) = (files(), Home::new(Answer::Types(PASSPHRASE))) else {
        return;
    };
    mine.smime.import(&files.pem).expect("an import");

    let found = mine.smime.import(&files.p12).expect("an import");

    assert!(found[0].secret);
    assert_eq!(found[0].change, Change::New, "the secret key is new");
}

#[test]
fn a_pkcs12_file_imported_twice_comes_back_unchanged() {
    let (Some(files), Some(mine)) = (files(), Home::new(Answer::Types(PASSPHRASE))) else {
        return;
    };
    mine.smime.import(&files.p12).expect("an import");

    let found = mine.smime.import(&files.p12).expect("an import");

    assert!(found[0].secret);
    assert_eq!(found[0].change, Change::Unchanged);
}

#[test]
fn a_wrong_passphrase_says_so() {
    let (Some(files), Some(mine)) = (files(), Home::new(Answer::Types("not it"))) else {
        return;
    };

    let refused = mine.smime.import(&files.p12);

    assert!(
        matches!(refused, Err(SmimeError::WrongPassphrase)),
        "{refused:?}"
    );
}

#[test]
fn a_canceled_passphrase_says_no_passphrase_was_given() {
    let (Some(files), Some(mine)) = (files(), Home::new(Answer::Cancels)) else {
        return;
    };

    let refused = mine.smime.import(&files.p12);

    assert!(
        matches!(refused, Err(SmimeError::NoPassphrase)),
        "{refused:?}"
    );
}

#[test]
fn a_file_that_holds_no_certificate_says_so() {
    let Some(mine) = Home::new(Answer::Cancels) else {
        return;
    };

    let refused = mine.smime.import(b"Dear Ada, here is my certificate.\n");

    assert!(
        matches!(refused, Err(SmimeError::NotACertificate)),
        "{refused:?}"
    );
}

#[test]
fn a_pkcs12_file_is_told_from_other_files_by_its_first_bytes() {
    let Some(files) = files() else {
        return;
    };
    assert!(mailrs_smime::import::is_pkcs12(&files.p12));
    assert!(!mailrs_smime::import::is_pkcs12(&files.pem));
    assert!(!mailrs_smime::import::is_pkcs12(b""));
    assert!(!mailrs_smime::import::is_pkcs12(&[0x30, 0x03, 0x02, 0x01]));
}

#[test]
fn the_common_name_comes_out_of_a_subject_with_an_escaped_comma() {
    use mailrs_smime::import::common_name;
    assert_eq!(
        common_name("CN=Lovelace\\, Ada,O=Example").as_deref(),
        Some("Lovelace, Ada")
    );
    assert_eq!(common_name("O=Example,CN=Ada").as_deref(), Some("Ada"));
    assert_eq!(common_name("O=Example"), None);
}
