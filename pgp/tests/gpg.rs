//! Round trips through a real gpg, each in a GnuPG home of its own.
//!
//! Nothing here touches the keyring of whoever runs the tests. When this
//! computer has no gpg at all, every test says so and stops rather than
//! failing.

use std::path::Path;
use std::process::{Command, Stdio};

use mailrs_pgp::inline::{Armor, armor};
use mailrs_pgp::{Pgp, Readers, Verdict};

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
                require_crypto();
                return None;
            }
        };
        let dir = tempfile::tempdir().expect("a temp directory");
        // gpg refuses a home anyone else can read.
        permit_owner_only(dir.path());
        // gpg-agent asks the person things through a pinentry window, and
        // a test must never put one on somebody's screen. A pinentry that
        // cannot run is a pinentry that cannot interrupt: the agent gets
        // an error instead, which is the answer these tests want anyway.
        std::fs::write(
            dir.path().join("gpg-agent.conf"),
            "pinentry-program /bin/false\n",
        )
        .expect("write");
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

/// `text` with a signature written under it, the way an older mail client
/// puts one in the body. gpg makes it, not the code under test.
fn clearsigned(home: &Home, text: &str) -> String {
    let file = home.dir.path().join("clear");
    std::fs::write(&file, text).expect("write");
    let out = Command::new(home.pgp.program())
        .args(["--batch", "--no-tty", "--homedir"])
        .arg(home.dir.path())
        .args([
            "--output",
            "-",
            "--clearsign",
            "--local-user",
            &home.address,
        ])
        .arg(&file)
        .output()
        .expect("gpg runs");
    assert!(out.status.success(), "gpg could not clearsign");
    String::from_utf8(out.stdout).expect("utf-8")
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

    let found = home
        .pgp
        .verify(part, &signature)
        .expect("a verdict")
        .remove(0);

    assert_eq!(found.verdict, Verdict::Good);
    assert!(found.is_good());
    assert_eq!(
        found.signer.as_deref(),
        Some("Ada Lovelace <ada@example.test>")
    );
    assert_eq!(found.trust, mailrs_pgp::Trust::Ultimate);
    assert!(found.fingerprint.is_some());
    // Every name on the key, so a reader can tell whether it names the
    // address the message came from.
    assert_eq!(
        found.user_ids,
        [mailrs_pgp::UserId {
            user_id: "Ada Lovelace <ada@example.test>".into(),
            trust: mailrs_pgp::Trust::Ultimate,
        }]
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

    let found = home
        .pgp
        .verify(tampered, &signature)
        .expect("a verdict")
        .remove(0);

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

    let found = mine
        .pgp
        .verify(part, &signature)
        .expect("a verdict")
        .remove(0);

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
    assert!(opened.signatures.is_empty());
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
    let signature = opened.signatures.into_iter().next().expect("a signature");
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

#[test]
fn a_part_signed_here_verifies_here() {
    let Some(home) = Home::new("Ada Lovelace", "ada@example.test") else {
        return;
    };
    let body = home
        .pgp
        .sign(
            b"Content-Type: text/plain\r\n\r\nMeet at six.\r\n",
            &home.address,
        )
        .expect("a signed body");

    assert!(
        String::from_utf8_lossy(&body).starts_with("Content-Type: multipart/signed;"),
        "{}",
        String::from_utf8_lossy(&body)
    );
    let parts = parts(&body);
    assert_eq!(parts.len(), 2, "{}", String::from_utf8_lossy(&body));
    let found = home
        .pgp
        .verify(&parts[0], &body_of(&parts[1]))
        .expect("a verdict")
        .remove(0);
    assert!(found.is_good(), "{found:?}");
    assert_eq!(
        found.signer.as_deref(),
        Some("Ada Lovelace <ada@example.test>")
    );
}

#[test]
fn a_part_written_with_unix_line_endings_verifies_once_it_is_mail() {
    let Some(home) = Home::new("Ada Lovelace", "ada@example.test") else {
        return;
    };
    // What the composer holds. Every hop it crosses will make these CRLF,
    // so they are CRLF before the signature is made over them.
    let part = b"Content-Type: text/plain\n\nMeet at six.\nBring tea.\n";
    let body = home.pgp.sign(part, &home.address).expect("a signed body");

    let parts = parts(&body);
    assert!(!parts[0].windows(2).any(|pair| pair == b"\n\n"));
    let found = home
        .pgp
        .verify(&parts[0], &body_of(&parts[1]))
        .expect("a verdict")
        .remove(0);
    assert!(found.is_good(), "{found:?}");
}

#[test]
fn a_body_whose_lines_end_in_whitespace_survives_a_server_stripping_it() {
    let Some(home) = Home::new("Ada Lovelace", "ada@example.test") else {
        return;
    };
    let part = b"Content-Type: text/plain\r\n\r\nMeet at six.   \r\nBring tea.\t\r\n";
    let body = home.pgp.sign(part, &home.address).expect("a signed body");

    let parts = parts(&body);
    let signed = &parts[0];
    // The whitespace is encoded, so the server that strips it finds none.
    assert_eq!(&strip_trailing_whitespace(signed), signed);
    assert!(String::from_utf8_lossy(signed).contains("=20"));
    let found = home
        .pgp
        .verify(&strip_trailing_whitespace(signed), &body_of(&parts[1]))
        .expect("a verdict")
        .remove(0);
    assert!(found.is_good(), "{found:?}");
}

#[test]
fn signing_as_an_address_with_no_secret_key_says_so() {
    let Some(home) = Home::new("Ada Lovelace", "ada@example.test") else {
        return;
    };
    let err = home
        .pgp
        .sign(
            b"Content-Type: text/plain\r\n\r\nHello.\r\n",
            "nobody@example.test",
        )
        .expect_err("no key to sign with");

    assert!(
        matches!(err, mailrs_pgp::PgpError::CannotSign(ref who) if who.contains("nobody")),
        "expected CannotSign, got {err}"
    );
}

#[test]
fn a_part_encrypted_here_opens_here_with_its_signature() {
    let Some(home) = Home::new("Ada Lovelace", "ada@example.test") else {
        return;
    };
    let part = b"Content-Type: text/plain\r\n\r\nThe key is under the mat.\r\n";
    let body = home
        .pgp
        .encrypt(
            part,
            &Readers::named([home.address.clone()]),
            Some(&home.address),
        )
        .expect("an encrypted body");

    let parts = parts(&body);
    assert_eq!(parts.len(), 2, "{}", String::from_utf8_lossy(&body));
    assert!(String::from_utf8_lossy(&parts[0]).contains("Version: 1"));
    let opened = home
        .pgp
        .decrypt(&body_of(&parts[1]))
        .expect("the part inside");
    assert_eq!(opened.part, part);
    assert!(
        opened
            .signatures
            .into_iter()
            .next()
            .expect("a signature")
            .is_good()
    );
}

#[test]
fn encrypting_without_a_sender_key_leaves_the_message_unsigned() {
    let Some(home) = Home::new("Ada Lovelace", "ada@example.test") else {
        return;
    };
    let part = b"Content-Type: text/plain\r\n\r\nNo name on this.\r\n";
    let body = home
        .pgp
        .encrypt(part, &Readers::named([home.address.clone()]), None)
        .expect("an encrypted body");

    let opened = home
        .pgp
        .decrypt(&body_of(&parts(&body)[1]))
        .expect("the part inside");
    assert_eq!(opened.part, part);
    assert!(opened.signatures.is_empty());
}

#[test]
fn encrypting_to_somebody_with_no_key_names_them() {
    let Some(home) = Home::new("Ada Lovelace", "ada@example.test") else {
        return;
    };
    let err = home
        .pgp
        .encrypt(
            b"Content-Type: text/plain\r\n\r\nHello.\r\n",
            &Readers::named(["stranger@example.test"]),
            Some(&home.address),
        )
        .expect_err("no key for the stranger");

    assert!(
        matches!(err, mailrs_pgp::PgpError::NoKeyFor(ref who) if who.contains("stranger")),
        "expected NoKeyFor, got {err}"
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

    let held = home.pgp.keys_for(&asked).expect("an answer for each");

    assert_eq!(held.len(), 3);
    assert_eq!(held[0].address, "stranger@example.test");
    assert!(held[0].key.is_none());
    assert_eq!(held[1].address, home.address);
    let key = held[1].key.as_ref().expect("my own key");
    assert_eq!(key.user_id, "Ada Lovelace <ada@example.test>");
    assert_eq!(key.trust, mailrs_pgp::Trust::Ultimate);
    assert_eq!(key.fingerprint.len(), 40);
    assert!(held[2].key.is_none());
}

#[test]
fn a_clearsigned_body_opens_with_its_signature() {
    let Some(home) = Home::new("Ada Lovelace", "ada@example.test") else {
        return;
    };
    let body = clearsigned(&home, "Meet at six.\n");

    assert_eq!(armor(&body), Some(Armor::Clearsigned));
    let opened = home.pgp.open_inline(&body).expect("the text inside");

    assert_eq!(String::from_utf8_lossy(&opened.text), "Meet at six.\n");
    assert!(
        opened
            .signatures
            .into_iter()
            .next()
            .expect("a signature")
            .is_good()
    );
}

#[test]
fn a_clearsigned_body_changed_on_the_way_still_shows_its_text() {
    let Some(home) = Home::new("Ada Lovelace", "ada@example.test") else {
        return;
    };
    let body = clearsigned(&home, "Meet at six.\n").replace("six", "nine");

    let opened = home.pgp.open_inline(&body).expect("the text inside");

    assert_eq!(String::from_utf8_lossy(&opened.text), "Meet at nine.\n");
    let signature = opened.signatures.into_iter().next().expect("a signature");
    assert!(!signature.is_good(), "{signature:?}");
    assert_eq!(signature.verdict, Verdict::Bad);
}

#[test]
fn an_encrypted_body_opens_even_with_a_mail_client_writing_around_it() {
    let Some(home) = Home::new("Ada Lovelace", "ada@example.test") else {
        return;
    };
    let ciphertext = String::from_utf8(sealed(&home, b"The key is under the mat.\n", true))
        .expect("armor is ascii");
    let body = format!("Sent from my telephone\n{ciphertext}Excuse the brevity.\n");

    assert_eq!(armor(&body), Some(Armor::Message));
    let opened = home.pgp.open_inline(&body).expect("the text inside");

    assert_eq!(
        String::from_utf8_lossy(&opened.text),
        "The key is under the mat.\n"
    );
    assert!(
        opened
            .signatures
            .into_iter()
            .next()
            .expect("a signature")
            .is_good()
    );
}

#[test]
fn a_body_with_no_armor_in_it_is_not_opened() {
    let Some(home) = Home::new("Ada Lovelace", "ada@example.test") else {
        return;
    };
    let err = home
        .pgp
        .open_inline("Meet at six.\n")
        .expect_err("nothing to open");

    assert!(
        matches!(err, mailrs_pgp::PgpError::NotPgp),
        "expected NotPgp, got {err}"
    );
}

/// The armored public key of `home`'s own key, for another home to import.
fn public_key(home: &Home) -> Vec<u8> {
    let out = Command::new(home.pgp.program())
        .args(["--batch", "--no-tty", "--homedir"])
        .arg(home.dir.path())
        .args(["--armor", "--export", &home.address])
        .output()
        .expect("gpg runs");
    assert!(out.status.success(), "gpg could not export a key");
    out.stdout
}

fn import(home: &Home, key: &[u8]) {
    let mut child = Command::new(home.pgp.program())
        .args(["--batch", "--no-tty", "--homedir"])
        .arg(home.dir.path())
        .arg("--import")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("gpg runs");
    std::io::Write::write_all(&mut child.stdin.take().expect("stdin"), key).expect("write");
    assert!(
        child.wait().expect("gpg ends").success(),
        "gpg could not import a key"
    );
}

/// The long key ids of the subkeys `home` encrypts to, as
/// `--list-packets` prints them.
fn encryption_key_ids(home: &Home) -> Vec<String> {
    let out = Command::new(home.pgp.program())
        .args(["--batch", "--no-tty", "--homedir"])
        .arg(home.dir.path())
        .args(["--with-colons", "--list-keys", &home.address])
        .output()
        .expect("gpg runs");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|line| line.starts_with("sub:"))
        .filter(|line| {
            line.split(':')
                .nth(11)
                .is_some_and(|abilities| abilities.contains('e'))
        })
        .filter_map(|line| line.split(':').nth(4).map(str::to_string))
        .collect()
}

/// What `gpg --list-packets` makes of `ciphertext`, run in `home`, in
/// capitals so key ids compare whichever case gpg prints them in.
fn packets(home: &Home, ciphertext: &[u8]) -> String {
    let file = home.dir.path().join("listed");
    std::fs::write(&file, ciphertext).expect("write");
    let out = Command::new(home.pgp.program())
        .args(["--batch", "--no-tty", "--homedir"])
        .arg(home.dir.path())
        .args(["--list-only", "--list-packets"])
        .arg(&file)
        .output()
        .expect("gpg runs");
    String::from_utf8_lossy(&out.stdout).to_uppercase()
}

#[test]
fn a_blind_copy_opens_for_its_reader_and_leaves_their_key_out_of_the_message() {
    let Some(ada) = Home::new("Ada Lovelace", "ada@example.test") else {
        return;
    };
    let Some(bo) = Home::new("Bo Peep", "bo@example.test") else {
        return;
    };
    let Some(cy) = Home::new("Cy Young", "cy@example.test") else {
        return;
    };
    import(&ada, &public_key(&bo));
    import(&ada, &public_key(&cy));
    let part = b"Content-Type: text/plain\r\n\r\nBo must not know you read this.\r\n";
    let readers = Readers {
        named: vec![bo.address.clone(), ada.address.clone()],
        hidden: vec![cy.address.clone()],
    };

    let body = ada
        .pgp
        .encrypt(part, &readers, Some(&ada.address))
        .expect("an encrypted body");
    let ciphertext = body_of(&parts(&body)[1]);

    // The blind copy's reader opens it with nothing but their own key.
    let opened = cy.pgp.decrypt(&ciphertext).expect("cy reads it");
    assert_eq!(opened.part, part);
    let opened = bo.pgp.decrypt(&ciphertext).expect("bo reads it");
    assert_eq!(opened.part, part);

    // What Bo can learn of who else holds a key to it: himself, Ada, and a
    // reader with a key id of zero.
    let listed = packets(&bo, &ciphertext);
    for key in encryption_key_ids(&bo)
        .iter()
        .chain(&encryption_key_ids(&ada))
    {
        assert!(
            listed.contains(key.as_str()),
            "{key} missing from\n{listed}"
        );
    }
    let hidden = encryption_key_ids(&cy);
    assert!(!hidden.is_empty(), "cy has a key to encrypt to");
    for key in &hidden {
        assert!(!listed.contains(key.as_str()), "{key} named in\n{listed}");
    }
    assert!(
        listed.contains("KEYID 0000000000000000"),
        "the hidden reader is there without a name\n{listed}"
    );
}

/// Stops a run that was meant to exercise the real thing from passing on a
/// computer that cannot. The round trips skip when GnuPG is missing, so a
/// developer without it can still run the suite; that same skip would let
/// a build machine report a green S/MIME and OpenPGP suite having tested
/// nothing. Setting `PENGUIN_MAIL_REQUIRE_CRYPTO` turns the skip into a
/// failure, which is what a build machine should do.
fn require_crypto() {
    if std::env::var_os("PENGUIN_MAIL_REQUIRE_CRYPTO").is_some() {
        panic!(
            "PENGUIN_MAIL_REQUIRE_CRYPTO is set and GnuPG is not on PATH, \
             so these tests would have proved nothing"
        );
    }
}

#[test]
fn a_part_two_keys_signed_reports_both_signatures() {
    let Some(home) = Home::new("Ada Lovelace", "ada@example.test") else {
        return;
    };
    let made = Command::new(home.pgp.program())
        .args(["--batch", "--no-tty", "--homedir"])
        .arg(home.dir.path())
        .args(["--passphrase", "", "--quick-generate-key"])
        .args([
            "Bo Peep <bo@example.test>",
            "future-default",
            "default",
            "0",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("gpg runs");
    assert!(made.success(), "gpg could not generate the second key");
    let part = b"Content-Type: text/plain\r\n\r\nWe both say so.\r\n";
    let file = home.dir.path().join("signed");
    std::fs::write(&file, part).expect("write");
    let out = Command::new(home.pgp.program())
        .args(["--batch", "--no-tty", "--homedir"])
        .arg(home.dir.path())
        .args(["--armor", "--output", "-", "--detach-sign"])
        .args([
            "--local-user",
            "ada@example.test",
            "--local-user",
            "bo@example.test",
        ])
        .arg(&file)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .expect("gpg runs");
    assert!(out.status.success(), "gpg could not sign");

    let found = home.pgp.verify(part, &out.stdout).expect("verdicts");

    assert_eq!(found.len(), 2, "{found:?}");
    assert!(
        found.iter().all(|signature| signature.is_good()),
        "{found:?}"
    );
}
