//! Round trips through a real gpgsm, each in a GnuPG home of its own.
//!
//! Every certificate here is made in the test: a key, a self-signed
//! certificate over it, and a trust list naming that certificate as a root.
//! Nothing touches the keybox of whoever runs the tests. When this computer
//! has no gpgsm at all, every test says so and stops rather than failing.

use std::path::Path;
use std::process::{Command, Stdio};

use mailrs_smime::{Chain, Smime, Verdict};

/// A GnuPG home under a temp directory, with one certificate in it.
struct Home {
    dir: tempfile::TempDir,
    smime: Smime,
    address: String,
    /// The day this home's certificate was still good, for one whose
    /// window has closed. The test's own gpgsm signs back then; the code
    /// under test always verifies against the real clock.
    signing_day: Option<String>,
}

impl Home {
    /// `None` when this computer has no gpgsm, which is the one reason
    /// these tests skip.
    fn new(name: &str, address: &str) -> Option<Home> {
        Home::made(name, address, None)
    }

    /// A home whose certificate ran out before it was ever used: its
    /// window opened and closed in the past, so the expiry is there from
    /// the start and no test has to wait for one.
    fn expired(name: &str, address: &str) -> Option<Home> {
        Home::made(name, address, Some(("2019-01-01", "2020-01-01")))
    }

    fn made(name: &str, address: &str, window: Option<(&str, &str)>) -> Option<Home> {
        let smime = match Smime::find() {
            Ok(smime) => smime,
            Err(_) => {
                eprintln!("skipping: no gpgsm on PATH, so the round trips cannot run");
                return None;
            }
        };
        let dir = tempfile::tempdir().expect("a temp directory");
        // gpgsm refuses a home anyone else can read.
        permit_owner_only(dir.path());
        let dates = match window {
            Some((from, until)) => format!("Not-Before: {from}\nNot-After: {until}\n"),
            None => "Not-After: 2038-01-01\n".to_string(),
        };
        let params = dir.path().join("params");
        std::fs::write(
            &params,
            format!(
                "Key-Type: RSA\nKey-Length: 2048\nKey-Usage: sign, encrypt\n\
                 Serial: random\nName-DN: CN={name}\nName-Email: {address}\n{dates}%commit\n"
            ),
        )
        .expect("write");
        let certificate = dir.path().join("certificate.pem");
        // The key is made without a passphrase: gpgsm asks the agent for
        // one, and an empty stdin under a loopback pinentry is how a test
        // says there is none.
        let made = gpgsm(&smime, dir.path())
            .args(["--pinentry-mode", "loopback", "--passphrase-fd", "0"])
            .args(["--armor", "--generate-key", "--output"])
            .arg(&certificate)
            .arg(&params)
            .stdin(Stdio::null())
            .status()
            .expect("gpgsm runs");
        assert!(
            made.success(),
            "gpgsm could not generate a test certificate"
        );
        let imported = gpgsm(&smime, dir.path())
            .arg("--import")
            .arg(&certificate)
            .status()
            .expect("gpgsm runs");
        assert!(imported.success(), "gpgsm could not import the certificate");
        let home = Home {
            smime: smime.with_home(dir.path()),
            dir,
            address: address.to_string(),
            signing_day: window.map(|_| "20190601T120000".to_string()),
        };
        trust(&home, &home.fingerprint());
        Some(home)
    }

    /// The fingerprint of the one certificate in this home.
    fn fingerprint(&self) -> String {
        let out = gpgsm(&self.smime, self.dir.path())
            .args(["--with-colons", "--list-keys", &self.address])
            .stdout(Stdio::piped())
            .output()
            .expect("gpgsm runs");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .find_map(|record| record.strip_prefix("fpr:"))
            .and_then(|rest| rest.split(':').nth(8).map(str::to_string))
            .expect("a fingerprint")
    }

    /// The certificate as it would arrive in somebody else's mail.
    fn certificate(&self) -> Vec<u8> {
        std::fs::read(self.dir.path().join("certificate.pem")).expect("read")
    }

    fn import(&self, certificate: &[u8]) {
        let file = self.dir.path().join("theirs.pem");
        std::fs::write(&file, certificate).expect("write");
        let imported = gpgsm(&self.smime, self.dir.path())
            .arg("--import")
            .arg(&file)
            .status()
            .expect("gpgsm runs");
        assert!(imported.success(), "gpgsm could not import a certificate");
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        // The agent gpgsm started holds sockets open under the temp
        // directory. Killing it leaves nothing running once the directory
        // goes.
        let _ = Command::new("gpgconf")
            .arg("--homedir")
            .arg(self.dir.path())
            .args(["--kill", "all"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// gpgsm as the test drives it, rather than as the code under test does.
fn gpgsm(smime: &Smime, home: &Path) -> Command {
    let mut command = Command::new(smime.program());
    command
        .args(["--batch", "--no-tty", "--disable-dirmngr", "--homedir"])
        .arg(home)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

/// Puts a certificate in the home's trust list, which is what gpgsm reads
/// to decide a chain reaches a root worth believing. A self-signed
/// certificate is its own root, so this is the whole chain.
fn trust(home: &Home, fingerprint: &str) {
    let spaced: Vec<String> = fingerprint
        .as_bytes()
        .chunks(2)
        .map(|pair| String::from_utf8_lossy(pair).into_owned())
        .collect();
    std::fs::write(
        home.dir.path().join("trustlist.txt"),
        format!("{} S relax\n", spaced.join(":")),
    )
    .expect("write");
}

#[cfg(unix)]
fn permit_owner_only(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).expect("chmod");
}

#[cfg(not(unix))]
fn permit_owner_only(_path: &Path) {}

/// A detached signature over `data`, base64 as it would arrive in mail,
/// made by gpgsm itself rather than by the code under test.
fn detached(home: &Home, data: &[u8]) -> Vec<u8> {
    signed_by(home, data, &["--detach-sign"])
}

/// An opaque signature: the one Outlook sends, with the message inside the
/// blob rather than beside it.
fn opaque(home: &Home, data: &[u8]) -> Vec<u8> {
    signed_by(home, data, &["--sign"])
}

fn signed_by(home: &Home, data: &[u8], how: &[&str]) -> Vec<u8> {
    let file = home.dir.path().join("signed");
    std::fs::write(&file, data).expect("write");
    let mut command = gpgsm(&home.smime, home.dir.path());
    // A certificate that has run out signs nothing today, so a home whose
    // window has closed signs back inside it.
    if let Some(day) = &home.signing_day {
        command.args(["--faked-system-time", day]);
    }
    let out = command
        .args(how)
        .args(["--local-user", &home.address, "--output", "-"])
        .arg(&file)
        .stdout(Stdio::piped())
        .output()
        .expect("gpgsm runs");
    assert!(out.status.success(), "gpgsm could not sign");
    mailrs_smime::mime::base64(&out.stdout)
}

/// Ciphertext for `home`'s own certificate, made by gpgsm itself.
fn sealed(home: &Home, data: &[u8]) -> Vec<u8> {
    let file = home.dir.path().join("plain");
    std::fs::write(&file, data).expect("write");
    let out = gpgsm(&home.smime, home.dir.path())
        .args(["--encrypt", "--always-trust", "--recipient", &home.address])
        .args(["--output", "-"])
        .arg(&file)
        .stdout(Stdio::piped())
        .output()
        .expect("gpgsm runs");
    assert!(out.status.success(), "gpgsm could not encrypt");
    mailrs_smime::mime::base64(&out.stdout)
}

/// The parts of a multipart entity, each as the bytes that sit between its
/// boundaries. A mail client reads a received message this way, and the
/// first part of a `multipart/signed` is what the signature covers, so this
/// takes the bytes as they are rather than parsing and rebuilding them.
fn parts(entity: &[u8]) -> Vec<Vec<u8>> {
    let head = String::from_utf8_lossy(&entity[..entity.len().min(400)]).to_string();
    let boundary = head
        .split("boundary=\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("a boundary");
    let open = format!("--{boundary}\r\n").into_bytes();
    let separator = format!("\r\n--{boundary}").into_bytes();
    let mut rest = &entity[find(entity, &open).expect("a first boundary") + open.len()..];
    let mut out = Vec::new();
    while let Some(at) = find(rest, &separator) {
        out.push(rest[..at].to_vec());
        let after = &rest[at + separator.len()..];
        match after.strip_prefix(b"\r\n") {
            Some(next) => rest = next,
            None => break,
        }
    }
    out
}

/// The body of one part, without the headers that name it.
fn body_of(part: &[u8]) -> Vec<u8> {
    let at = find(part, b"\r\n\r\n").expect("a blank line");
    part[at + 4..].to_vec()
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// What a mail server does to a line that ends in whitespace.
fn strip_trailing_whitespace(part: &[u8]) -> Vec<u8> {
    String::from_utf8_lossy(part)
        .split("\r\n")
        .map(|line| line.trim_end_matches([' ', '\t']))
        .collect::<Vec<_>>()
        .join("\r\n")
        .into_bytes()
}

#[test]
fn a_signature_over_the_part_verifies_and_names_its_signer() {
    let Some(home) = Home::new("Ada Lovelace", "ada@example.test") else {
        return;
    };
    let part = b"Content-Type: text/plain\r\n\r\nMeet at six.\r\n";
    let signature = detached(&home, part);

    let found = home.smime.verify(part, &signature).expect("a verdict");

    assert_eq!(found.verdict, Verdict::Good);
    assert!(found.is_good());
    assert_eq!(found.subject.as_deref(), Some("/CN=Ada Lovelace"));
    assert_eq!(found.email.as_deref(), Some("ada@example.test"));
    assert_eq!(found.chain, Chain::Trusted);
    assert_eq!(
        found.fingerprint.as_deref(),
        Some(home.fingerprint().as_str())
    );
}

#[test]
fn a_body_changed_after_signing_fails_to_verify() {
    let Some(home) = Home::new("Ada Lovelace", "ada@example.test") else {
        return;
    };
    let part = b"Content-Type: text/plain\r\n\r\nMeet at six.\r\n";
    let signature = detached(&home, part);
    let tampered = b"Content-Type: text/plain\r\n\r\nMeet at nine.\r\n";

    let found = home.smime.verify(tampered, &signature).expect("a verdict");

    assert_eq!(found.verdict, Verdict::Bad);
    assert!(!found.is_good());
}

#[test]
fn a_signature_from_a_certificate_we_do_not_hold_says_so() {
    let Some(stranger) = Home::new("Grace Hopper", "hopper@example.test") else {
        return;
    };
    let Some(mine) = Home::new("Ada Lovelace", "ada@example.test") else {
        return;
    };
    let part = b"Content-Type: text/plain\r\n\r\nFrom someone else.\r\n";
    let signature = detached(&stranger, part);

    let found = mine.smime.verify(part, &signature).expect("a verdict");

    assert_eq!(found.verdict, Verdict::NoCertificate);
    assert!(!found.is_good());
    assert_eq!(found.email, None);
}

#[test]
fn a_chain_that_reaches_no_root_we_trust_says_so_beside_a_good_signature() {
    let Some(stranger) = Home::new("Grace Hopper", "hopper@example.test") else {
        return;
    };
    let Some(mine) = Home::new("Ada Lovelace", "ada@example.test") else {
        return;
    };
    let part = b"Content-Type: text/plain\r\n\r\nFrom someone else.\r\n";
    let signature = detached(&stranger, part);
    // Their certificate arrives, but nobody here has said their root is
    // one to believe.
    mine.import(&stranger.certificate());

    let found = mine.smime.verify(part, &signature).expect("a verdict");

    assert_eq!(found.verdict, Verdict::Good);
    assert_eq!(found.chain, Chain::Untrusted);
    assert_eq!(found.email.as_deref(), Some("hopper@example.test"));
}

#[test]
fn a_certificate_that_has_run_out_still_names_its_signer() {
    let Some(home) = Home::expired("Grace Hopper", "grace@example.test") else {
        return;
    };
    let part = b"Content-Type: text/plain\r\n\r\nOld news.\r\n";
    let signature = detached(&home, part);

    let found = home.smime.verify(part, &signature).expect("a verdict");

    assert_eq!(found.verdict, Verdict::ExpiredCertificate);
    assert!(!found.is_good());
    assert_eq!(found.email.as_deref(), Some("grace@example.test"));
}

#[test]
fn an_opaque_signature_gives_back_the_message_inside_it() {
    let Some(home) = Home::new("Ada Lovelace", "ada@example.test") else {
        return;
    };
    let part = b"Content-Type: text/plain\r\n\r\nMeet at six.\r\n";
    let blob = opaque(&home, part);

    let opened = home.smime.open_signed(&blob).expect("the part inside");

    assert_eq!(opened.part, part);
    let signature = opened.signature.expect("a signature");
    assert!(signature.is_good());
    assert_eq!(signature.email.as_deref(), Some("ada@example.test"));
}

#[test]
fn a_part_signed_here_verifies_here() {
    let Some(home) = Home::new("Ada Lovelace", "ada@example.test") else {
        return;
    };
    let body = home
        .smime
        .sign(
            b"Content-Type: text/plain\r\n\r\nMeet at six.\r\n",
            &home.address,
        )
        .expect("a signed body");

    let head = String::from_utf8_lossy(&body);
    assert!(
        head.starts_with("Content-Type: multipart/signed;"),
        "{head}"
    );
    assert!(
        head.contains("protocol=\"application/pkcs7-signature\""),
        "{head}"
    );
    assert!(head.contains("micalg=sha-256"), "{head}");
    let parts = parts(&body);
    assert_eq!(parts.len(), 2, "{head}");
    let found = home
        .smime
        .verify(&parts[0], &body_of(&parts[1]))
        .expect("a verdict");
    assert!(found.is_good(), "{found:?}");
    assert_eq!(found.chain, Chain::Trusted);
}

#[test]
fn a_part_written_with_unix_line_endings_verifies_once_it_is_mail() {
    let Some(home) = Home::new("Ada Lovelace", "ada@example.test") else {
        return;
    };
    let part = b"Content-Type: text/plain\n\nMeet at six.\nBring tea.\n";
    let body = home.smime.sign(part, &home.address).expect("a signed body");

    let parts = parts(&body);
    assert!(!parts[0].windows(2).any(|pair| pair == b"\n\n"));
    let found = home
        .smime
        .verify(&parts[0], &body_of(&parts[1]))
        .expect("a verdict");
    assert!(found.is_good(), "{found:?}");
}

#[test]
fn a_body_whose_lines_end_in_whitespace_survives_a_server_stripping_it() {
    let Some(home) = Home::new("Ada Lovelace", "ada@example.test") else {
        return;
    };
    let part = b"Content-Type: text/plain\r\n\r\nMeet at six.   \r\nBring tea.\t\r\n";
    let body = home.smime.sign(part, &home.address).expect("a signed body");

    let parts = parts(&body);
    let signed = &parts[0];
    // The whitespace is encoded, so the server that strips it finds none.
    assert_eq!(&strip_trailing_whitespace(signed), signed);
    assert!(String::from_utf8_lossy(signed).contains("=20"));
    let found = home
        .smime
        .verify(&strip_trailing_whitespace(signed), &body_of(&parts[1]))
        .expect("a verdict");
    assert!(found.is_good(), "{found:?}");
}

#[test]
fn signing_as_an_address_with_no_certificate_says_so() {
    let Some(home) = Home::new("Ada Lovelace", "ada@example.test") else {
        return;
    };
    let err = home
        .smime
        .sign(
            b"Content-Type: text/plain\r\n\r\nHello.\r\n",
            "nobody@example.test",
        )
        .expect_err("no certificate to sign with");

    assert!(
        matches!(err, mailrs_smime::SmimeError::CannotSign(ref who) if who.contains("nobody")),
        "expected CannotSign, got {err}"
    );
}

#[test]
fn a_part_enveloped_here_opens_here_with_its_signature() {
    let Some(home) = Home::new("Ada Lovelace", "ada@example.test") else {
        return;
    };
    let part = b"Content-Type: text/plain\r\n\r\nThe key is under the mat.\r\n";
    let body = home
        .smime
        .encrypt(
            part,
            std::slice::from_ref(&home.address),
            Some(&home.address),
        )
        .expect("an enveloped body");

    let head = String::from_utf8_lossy(&body[..body.len().min(200)]).to_string();
    assert!(head.contains("smime-type=enveloped-data"), "{head}");
    // The signature travelled inside the envelope, so what comes out is the
    // signed entity and checking it is the next call.
    let inside = home
        .smime
        .decrypt(&body_of(&body))
        .expect("the part inside");
    let parts = parts(&inside);
    assert_eq!(parts.len(), 2, "{}", String::from_utf8_lossy(&inside));
    assert_eq!(parts[0], part.strip_suffix(b"\r\n").expect("a part"));
    let found = home
        .smime
        .verify(&parts[0], &body_of(&parts[1]))
        .expect("a verdict");
    assert!(found.is_good(), "{found:?}");
}

#[test]
fn enveloping_without_a_sender_certificate_leaves_the_message_unsigned() {
    let Some(home) = Home::new("Ada Lovelace", "ada@example.test") else {
        return;
    };
    let part = b"Content-Type: text/plain\r\n\r\nNo name on this.\r\n";
    let body = home
        .smime
        .encrypt(part, std::slice::from_ref(&home.address), None)
        .expect("an enveloped body");

    let inside = home
        .smime
        .decrypt(&body_of(&body))
        .expect("the part inside");

    assert_eq!(inside, part);
}

#[test]
fn enveloping_to_somebody_with_no_certificate_names_them() {
    let Some(home) = Home::new("Ada Lovelace", "ada@example.test") else {
        return;
    };
    let err = home
        .smime
        .encrypt(
            b"Content-Type: text/plain\r\n\r\nHello.\r\n",
            &["stranger@example.test".to_string()],
            Some(&home.address),
        )
        .expect_err("no certificate for the stranger");

    assert!(
        matches!(err, mailrs_smime::SmimeError::NoCertificateFor(ref who) if who.contains("stranger")),
        "expected NoCertificateFor, got {err}"
    );
}

#[test]
fn a_message_enveloped_for_somebody_else_says_so() {
    let Some(stranger) = Home::new("Grace Hopper", "hopper@example.test") else {
        return;
    };
    let Some(mine) = Home::new("Ada Lovelace", "ada@example.test") else {
        return;
    };
    let enveloped = sealed(&stranger, b"Not for you.\r\n");

    let err = mine
        .smime
        .decrypt(&enveloped)
        .expect_err("no certificate for it");

    assert!(
        matches!(err, mailrs_smime::SmimeError::NotForYou),
        "expected NotForYou, got {err}"
    );
}

#[test]
fn every_address_gets_an_answer_in_the_order_it_was_asked_about() {
    let Some(home) = Home::new("Ada Lovelace", "ada@example.test") else {
        return;
    };
    let asked = [
        "stranger@example.test".to_string(),
        home.address.clone(),
        "nobody@example.test".to_string(),
    ];

    let held = home
        .smime
        .certificates_for(&asked)
        .expect("an answer for each");

    assert_eq!(held.len(), 3);
    assert_eq!(held[0].address, "stranger@example.test");
    assert!(held[0].certificate.is_none());
    assert_eq!(held[1].address, home.address);
    let certificate = held[1].certificate.as_ref().expect("my own certificate");
    assert_eq!(certificate.subject, "CN=Ada Lovelace");
    assert_eq!(certificate.email, "ada@example.test");
    assert_eq!(certificate.fingerprint, home.fingerprint());
    assert!(held[2].certificate.is_none());
}

#[test]
fn the_addresses_this_computer_can_sign_as_are_the_ones_with_a_secret_key() {
    let Some(home) = Home::new("Ada Lovelace", "ada@example.test") else {
        return;
    };
    let Some(stranger) = Home::new("Grace Hopper", "hopper@example.test") else {
        return;
    };
    // Somebody else's certificate holds no secret key, so it signs nothing.
    home.import(&stranger.certificate());
    let asked = [home.address.clone(), stranger.address.clone()];

    let mine = home.smime.own_certificates(&asked).expect("an answer");

    assert!(mine[0].certificate.is_some(), "{mine:?}");
    assert!(mine[1].certificate.is_none(), "{mine:?}");
}
