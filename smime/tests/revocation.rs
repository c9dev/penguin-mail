//! Checking whether a signing certificate was revoked, through a real gpgsm
//! and dirmngr, each test in a GnuPG home of its own.
//!
//! Each home holds a certificate authority made in the test and a
//! certificate it issued for Ada, whose CRL distribution point is a port on
//! this computer that the test controls. The authority's own certificate is
//! the root in the trust list. The self-signed certificates in `gpgsm.rs`
//! name no distribution point, and gpgsm does not start dirmngr for a chain
//! that names none, which is why those tests never met a CRL. Their
//! `relax` flag in the trust list plays no part in that.
//!
//! What gpgsm 2.4.8 did in each case, measured while writing these tests:
//!
//! - A CRL server that accepts the connection and never answers: gpgsm
//!   writes `NEWSIG` and `PROGRESS starting_dirmngr`, then nothing, for as
//!   long as the connection stays open. A minute passed with no verdict.
//! - A CRL server that refuses the connection: at once, `GOODSIG`,
//!   `VALIDSIG` and `TRUST_UNDEFINED 32793` (`ECONNREFUSED`), exit code 2.
//! - A server with no CRL at that address (HTTP 404): `TRUST_UNDEFINED 95`
//!   (`GPG_ERR_NO_CRL_KNOWN`), exit code 2.
//! - No dirmngr to start: `TRUST_UNDEFINED 92`, exit code 2. dirmngr with
//!   `disable-http`: `TRUST_UNDEFINED 60`, exit code 2.
//! - A certificate with no CRL distribution point: gpgsm does not start
//!   dirmngr and writes `TRUST_FULLY 0 shell`, exit code 0.
//! - A CRL that lists the certificate: `GOODSIG`, `VALIDSIG` and
//!   `TRUST_NEVER 94` (`GPG_ERR_CERT_REVOKED`), exit code 2.
//! - A CRL that lists nothing: `TRUST_FULLY 0 shell`, exit code 0.
//! - Any of these with `--disable-crl-checks`: `TRUST_FULLY 0 shell` and
//!   exit code 0, and dirmngr is never asked.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use mailrs_smime::{Chain, Smime, Verdict};

/// How long these tests let gpgsm wait on a CRL. The app waits ten
/// seconds; two keep the suite quick and still show the limit working.
const WAIT: Duration = Duration::from_secs(2);

/// Time for the second gpgsm run with CRL checks off, and for a machine
/// busy with the rest of the suite.
const MARGIN: Duration = Duration::from_secs(4);

/// What the CRL distribution point on Ada's certificate does when dirmngr
/// comes asking.
enum Point {
    /// Accepts the connection and never says a word.
    Silent,
    /// Nothing listens there, so the connection is refused.
    Refused,
    /// Serves a CRL the authority signed, listing Ada's certificate or
    /// not.
    Crl { revoked: bool },
    /// Ada's certificate names no distribution point at all.
    None,
}

/// A GnuPG home with the authority's certificate as its trusted root and
/// Ada's certificate below it.
struct Home {
    dir: tempfile::TempDir,
    smime: Smime,
}

impl Home {
    /// `None` when this computer lacks gpgsm, or lacks openssl for a home
    /// that needs a CRL made; those are the reasons these tests skip.
    fn new(point: Point) -> Option<Home> {
        let smime = match Smime::find() {
            Ok(smime) => smime,
            Err(_) => {
                eprintln!("skipping: no gpgsm on PATH, so the revocation checks cannot run");
                require_crypto();
                return None;
            }
        };
        if matches!(point, Point::Crl { .. }) && !has_openssl() {
            eprintln!("skipping: no openssl on PATH to make the authority's CRL with");
            require_crypto();
            return None;
        }
        let dir = tempfile::tempdir().expect("a temp directory");
        permit_owner_only(dir.path());
        // gpg-agent asks the person things through a pinentry window, and
        // a test must never put one on somebody's screen.
        std::fs::write(
            dir.path().join("gpg-agent.conf"),
            "pinentry-program /bin/false\n",
        )
        .expect("write");
        let home = Home {
            smime: smime.with_home(dir.path()),
            dir,
        };
        // The port is taken before the certificate is made, since the
        // certificate names it.
        let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
        let url = format!(
            "http://127.0.0.1:{}/ca.crl",
            listener.local_addr().expect("an address").port()
        );
        let grip = home.authority();
        home.issue(&grip, (!matches!(point, Point::None)).then_some(url.as_str()));
        match point {
            Point::Silent => hold(listener),
            Point::Refused | Point::None => drop(listener),
            Point::Crl { revoked } => serve(listener, home.crl(revoked)),
        }
        Some(home)
    }

    /// Makes the authority's self-signed certificate and puts it in the
    /// trust list. Its keygrip comes back, which is how gpgsm is told to
    /// sign Ada's certificate with that key.
    fn authority(&self) -> String {
        self.generate(
            "ca",
            "Key-Usage: cert, sign\nName-DN: CN=Test Authority\nNot-After: 2038-01-01\n",
        );
        let listed = self
            .gpgsm()
            .args(["--with-colons", "--with-keygrip", "--list-keys"])
            .arg("CN=Test Authority")
            .stdout(Stdio::piped())
            .output()
            .expect("gpgsm runs");
        let listed = String::from_utf8_lossy(&listed.stdout).into_owned();
        let field = |record: &str| {
            listed
                .lines()
                .find_map(|line| line.strip_prefix(record))
                .and_then(|rest| rest.split(':').nth(8))
                .map(str::to_string)
                .expect("a listing of the authority")
        };
        let spaced: Vec<String> = field("fpr:")
            .as_bytes()
            .chunks(2)
            .map(|pair| String::from_utf8_lossy(pair).into_owned())
            .collect();
        // No `relax` here: that flag loosens the checks on the root, and
        // these tests want gpgsm's checks as a person's own setup has them.
        std::fs::write(
            self.dir.path().join("trustlist.txt"),
            format!("{} S\n", spaced.join(":")),
        )
        .expect("write");
        field("grp:")
    }

    /// Makes Ada's certificate, signed by the authority's key, with `url`
    /// as its CRL distribution point.
    fn issue(&self, authority: &str, url: Option<&str>) {
        let point = url
            .map(|url| format!("Extension: 2.5.29.31 n {}\n", crl_points(url)))
            .unwrap_or_default();
        self.generate(
            "ada",
            &format!(
                "Key-Usage: sign, encrypt\nName-DN: CN=Ada Lovelace\n\
                 Name-Email: ada@example.test\nIssuer-DN: CN=Test Authority\n\
                 Signing-Key: {authority}\nNot-After: 2037-01-01\n{point}"
            ),
        );
    }

    /// Runs gpgsm's certificate generation with `params` and imports what
    /// it made, which lands in `<name>.pem`.
    fn generate(&self, name: &str, params: &str) {
        let file = self.dir.path().join(format!("{name}.params"));
        std::fs::write(
            &file,
            format!("Key-Type: RSA\nKey-Length: 2048\nSerial: random\n{params}%commit\n"),
        )
        .expect("write");
        let certificate = self.dir.path().join(format!("{name}.pem"));
        // No passphrase: an empty stdin under a loopback pinentry says so.
        let made = self
            .gpgsm()
            .args(["--pinentry-mode", "loopback", "--passphrase-fd", "0"])
            .args(["--armor", "--generate-key", "--output"])
            .arg(&certificate)
            .arg(&file)
            .stdin(Stdio::null())
            .status()
            .expect("gpgsm runs");
        assert!(made.success(), "gpgsm could not make the {name} certificate");
        let imported = self
            .gpgsm()
            .arg("--import")
            .arg(&certificate)
            .status()
            .expect("gpgsm runs");
        assert!(imported.success(), "gpgsm could not import {name}");
    }

    /// The authority's CRL, DER as dirmngr fetches it, listing Ada's
    /// certificate when `revoked`. gpgsm makes no CRLs, so openssl signs
    /// one with the authority's key, which gpgsm hands over unprotected
    /// since it has no passphrase.
    fn crl(&self, revoked: bool) -> Vec<u8> {
        let dir = self.dir.path();
        let key = self
            .gpgsm()
            .args(["--pinentry-mode", "loopback", "--passphrase-fd", "0", "--armor"])
            .args(["--export-secret-key-raw", "CN=Test Authority"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .output()
            .expect("gpgsm runs");
        assert!(key.status.success(), "gpgsm would not export the key");
        std::fs::write(dir.join("ca.key"), &key.stdout).expect("write");
        let serial = Command::new("openssl")
            .args(["x509", "-noout", "-serial", "-in"])
            .arg(dir.join("ada.pem"))
            .output()
            .expect("openssl runs");
        let serial = String::from_utf8_lossy(&serial.stdout)
            .trim()
            .trim_start_matches("serial=")
            .to_string();
        // openssl's own database of what the authority issued: one line per
        // certificate, `R` for revoked, then its expiry, when it was
        // revoked, and its serial.
        let index = if revoked {
            format!("R\t370101000000Z\t260101000000Z\t{serial}\tunknown\t/CN=Ada Lovelace\n")
        } else {
            String::new()
        };
        std::fs::write(dir.join("index.txt"), index).expect("write");
        std::fs::write(dir.join("crlnumber"), "01\n").expect("write");
        std::fs::write(
            dir.join("ca.cnf"),
            format!(
                "[ca]\ndefault_ca = authority\n[authority]\ndatabase = {}\n\
                 crlnumber = {}\ndefault_md = sha256\ndefault_crl_days = 30\n",
                dir.join("index.txt").display(),
                dir.join("crlnumber").display(),
            ),
        )
        .expect("write");
        let made = Command::new("openssl")
            .args(["ca", "-batch", "-gencrl", "-config"])
            .arg(dir.join("ca.cnf"))
            .arg("-keyfile")
            .arg(dir.join("ca.key"))
            .arg("-cert")
            .arg(dir.join("ca.pem"))
            .arg("-out")
            .arg(dir.join("ca.crl"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("openssl runs");
        assert!(made.success(), "openssl could not make the CRL");
        let der = Command::new("openssl")
            .args(["crl", "-outform", "DER", "-in"])
            .arg(dir.join("ca.crl"))
            .output()
            .expect("openssl runs");
        assert!(der.status.success(), "openssl could not convert the CRL");
        der.stdout
    }

    /// A signature over `data` by Ada, base64 as it would arrive in mail.
    /// The test's own gpgsm signs with CRL checks off, since signing is not
    /// what these tests are about.
    fn signed(&self, data: &[u8], how: &str) -> Vec<u8> {
        let file = self.dir.path().join("signed");
        std::fs::write(&file, data).expect("write");
        let out = self
            .gpgsm()
            .args(["--disable-crl-checks", how])
            .args(["--local-user", "ada@example.test", "--output", "-"])
            .arg(&file)
            .stdout(Stdio::piped())
            .output()
            .expect("gpgsm runs");
        assert!(out.status.success(), "gpgsm could not sign");
        mailrs_smime::mime::base64(&out.stdout)
    }

    /// gpgsm as the test drives it, rather than as the code under test
    /// does: dirmngr stays out of it.
    fn gpgsm(&self) -> Command {
        let mut command = Command::new(self.smime.program());
        command
            .args(["--batch", "--no-tty", "--disable-dirmngr", "--homedir"])
            .arg(self.dir.path())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        // gpgsm started gpg-agent and dirmngr for this home, and a dirmngr
        // still waiting on the silent port would otherwise outlive the test.
        let _ = Command::new("gpgconf")
            .arg("--homedir")
            .arg(self.dir.path())
            .args(["--kill", "all"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// The CRL distribution points extension naming one URL, as the hex gpgsm
/// takes in an `Extension:` line: a sequence of one distribution point
/// whose full name is that URI.
fn crl_points(url: &str) -> String {
    fn wrapped(tag: u8, inner: &[u8]) -> Vec<u8> {
        let length = u8::try_from(inner.len()).expect("a short URL");
        let mut out = vec![tag, length];
        out.extend_from_slice(inner);
        out
    }
    let uri = wrapped(0x86, url.as_bytes());
    let full_name = wrapped(0xa0, &uri);
    let point_name = wrapped(0xa0, &full_name);
    let point = wrapped(0x30, &point_name);
    wrapped(0x30, &point)
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect()
}

/// Accepts every connection and keeps it open without a word, the way a
/// server behind a broken proxy or a captive portal can.
fn hold(listener: TcpListener) {
    std::thread::spawn(move || {
        let mut held: Vec<TcpStream> = Vec::new();
        for stream in listener.incoming().flatten() {
            held.push(stream);
        }
    });
}

/// Answers every request with `crl`.
fn serve(listener: TcpListener, crl: Vec<u8>) {
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut request = [0u8; 1024];
            let _ = stream.read(&mut request);
            let _ = write!(
                stream,
                "HTTP/1.0 200 OK\r\nContent-Type: application/pkix-crl\r\n\
                 Content-Length: {}\r\n\r\n",
                crl.len()
            );
            let _ = stream.write_all(&crl);
        }
    });
}

fn has_openssl() -> bool {
    Command::new("openssl")
        .arg("version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(unix)]
fn permit_owner_only(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).expect("chmod");
}

#[cfg(not(unix))]
fn permit_owner_only(_path: &Path) {}

/// Stops a run meant to exercise the real thing from passing on a computer
/// that cannot; see the same function in `gpgsm.rs`.
fn require_crypto() {
    if std::env::var_os("PENGUIN_MAIL_REQUIRE_CRYPTO").is_some() {
        panic!(
            "PENGUIN_MAIL_REQUIRE_CRYPTO is set and gpgsm or openssl is not on PATH, \
             so these tests would have proved nothing"
        );
    }
}

const PART: &[u8] = b"Content-Type: text/plain\r\n\r\nMeet at six.\r\n";

#[test]
fn a_crl_server_that_never_answers_holds_the_check_up_for_the_limit_only() {
    let Some(home) = Home::new(Point::Silent) else {
        return;
    };
    let signature = home.signed(PART, "--detach-sign");
    let started = Instant::now();

    let found = home
        .smime
        .clone()
        .with_revocation_wait(WAIT)
        .verify(PART, &signature)
        .expect("a verdict");

    let waited = started.elapsed();
    eprintln!("gpgsm answered in {waited:?}");
    assert!(waited >= WAIT, "answered in {waited:?}, before the limit");
    assert!(waited < WAIT + MARGIN, "answered in {waited:?}");
    assert_eq!(found.verdict, Verdict::Good);
    assert_eq!(found.chain, Chain::RevocationUnknown);
    assert_eq!(
        found.emails.first().map(String::as_str),
        Some("ada@example.test")
    );
}

#[test]
fn a_crl_server_that_refuses_the_connection_says_the_same_at_once() {
    let Some(home) = Home::new(Point::Refused) else {
        return;
    };
    let signature = home.signed(PART, "--detach-sign");
    let started = Instant::now();

    // The limit the app uses, to show a refusal never comes near it.
    let found = home.smime.verify(PART, &signature).expect("a verdict");

    let waited = started.elapsed();
    eprintln!("gpgsm answered in {waited:?}");
    assert!(waited < MARGIN, "answered in {waited:?}");
    assert_eq!(found.verdict, Verdict::Good);
    assert_eq!(found.chain, Chain::RevocationUnknown);
}

#[test]
fn an_opaque_signature_whose_crl_never_arrives_still_opens() {
    let Some(home) = Home::new(Point::Silent) else {
        return;
    };
    let blob = home.signed(PART, "--sign");

    let opened = home
        .smime
        .clone()
        .with_revocation_wait(WAIT)
        .open_signed(&blob)
        .expect("the part inside");

    assert_eq!(opened.part, PART);
    assert_eq!(opened.signature.verdict, Verdict::Good);
    assert_eq!(opened.signature.chain, Chain::RevocationUnknown);
}

/// A CRL that could not be fetched says nothing about a chain that would
/// have failed anyway, so the second run does not lift it to a trusted
/// one.
#[test]
fn a_root_nobody_trusts_stays_untrusted_when_the_crl_is_out_of_reach() {
    let Some(home) = Home::new(Point::Refused) else {
        return;
    };
    let signature = home.signed(PART, "--detach-sign");
    std::fs::write(home.dir.path().join("trustlist.txt"), "").expect("write");
    // gpg-agent read the trust list when the test signed, and keeps what
    // it read until it starts again.
    let _ = Command::new("gpgconf")
        .arg("--homedir")
        .arg(home.dir.path())
        .args(["--kill", "gpg-agent"])
        .status();

    let found = home.smime.verify(PART, &signature).expect("a verdict");

    assert_eq!(found.verdict, Verdict::Good);
    assert_eq!(found.chain, Chain::Untrusted);
}

#[test]
fn a_crl_that_lists_nothing_leaves_the_chain_trusted() {
    let Some(home) = Home::new(Point::Crl { revoked: false }) else {
        return;
    };
    let signature = home.signed(PART, "--detach-sign");

    let found = home.smime.verify(PART, &signature).expect("a verdict");

    assert_eq!(found.verdict, Verdict::Good);
    assert_eq!(found.chain, Chain::Trusted);
}

#[test]
fn a_certificate_with_no_crl_to_ask_about_is_trusted_as_before() {
    let Some(home) = Home::new(Point::None) else {
        return;
    };
    let signature = home.signed(PART, "--detach-sign");

    let found = home.smime.verify(PART, &signature).expect("a verdict");

    assert_eq!(found.verdict, Verdict::Good);
    assert_eq!(found.chain, Chain::Trusted);
}

#[test]
fn a_certificate_the_crl_lists_is_revoked() {
    let Some(home) = Home::new(Point::Crl { revoked: true }) else {
        return;
    };
    let signature = home.signed(PART, "--detach-sign");

    let found = home.smime.verify(PART, &signature).expect("a verdict");

    assert_eq!(found.verdict, Verdict::RevokedCertificate);
    assert!(!found.is_good());
    assert_eq!(
        found.emails.first().map(String::as_str),
        Some("ada@example.test")
    );
}
