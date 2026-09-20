//! S/MIME in the app: which call a message needs, what comes back when
//! gpgsm has run, and which standard a draft goes out under.
//!
//! It is `pgp`'s twin and borrows its vocabulary: a [`Mark`] from here
//! fills the same card, in the same three tones, so a reader never has to
//! know which standard a message arrived under. Choosing between the two
//! engines also happens here, since this is the module that knows both.
//! Every call below blocks, so the window hands them to `Core::gpgsm`
//! rather than running them itself.

use std::process::Command;

use mailrs_domain::{MessageBody, Protection};
use mailrs_smime::{Chain, Recipient, Signature, Smime, SmimeError, Verdict};

use crate::pgp::{self, Mark, Read, Tone};

/// Which call of the engine one message needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Opening {
    /// `Smime::verify`, over the first part of a `multipart/signed`.
    Verify,
    /// `Smime::open_signed`, over a blob holding the message and the
    /// signature together.
    Opaque,
    /// `Smime::decrypt`, over an enveloped blob.
    Decrypt,
}

/// Which engine a message needs, and which of its calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    Pgp(pgp::Opening),
    Smime(Opening),
}

/// Which standard a message goes out under.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Standard {
    #[default]
    Pgp,
    Smime,
}

/// What each engine holds for a set of addresses. An engine this computer
/// does not have answers nothing at all, which is not the same as holding
/// nothing.
#[derive(Debug, Clone, Default)]
pub struct Held {
    pub pgp: Option<Vec<mailrs_pgp::Recipient>>,
    pub smime: Option<Vec<Recipient>>,
}

/// Which call `body` needs before it is drawn, from whichever engine. The
/// wrapper the message arrived in names its standard, and a body with no
/// wrapper is left to OpenPGP, which is the only one of the two that also
/// lives in the text.
pub fn engine(body: &MessageBody) -> Option<Engine> {
    match body.protection {
        Some(Protection::SmimeSigned) => Some(Engine::Smime(Opening::Verify)),
        Some(Protection::SmimeOpaque) => Some(Engine::Smime(Opening::Opaque)),
        Some(Protection::SmimeEnveloped) => Some(Engine::Smime(Opening::Decrypt)),
        _ => pgp::opening(body).map(Engine::Pgp),
    }
}

/// Runs the call `opening` asks for and says what the window should show.
///
/// `raw` is the message as it arrived, from `format=raw`, which is the one
/// copy whose bytes a signature still covers. Every branch has an answer,
/// including the ones where gpgsm refused, so the card never goes blank.
pub fn read(smime: &Smime, opening: Opening, raw: &[u8]) -> Read {
    match opening {
        Opening::Verify => {
            let Some((part, signature)) = pgp::wrapper_parts(raw) else {
                return pgp::mark_only(unreadable());
            };
            match smime.verify(part, signature) {
                Ok(found) => pgp::mark_only(signed(&found)),
                Err(err) => pgp::mark_only(refused(&err)),
            }
        }
        Opening::Opaque => {
            let Some(blob) = blob(raw) else {
                return pgp::mark_only(unreadable());
            };
            match smime.open_signed(blob) {
                Ok(opened) => Read {
                    mark: signed(&opened.signature),
                    body: Some(pgp::opened_body(&opened.part)),
                },
                Err(err) => pgp::mark_only(refused(&err)),
            }
        }
        Opening::Decrypt => {
            let Some(blob) = blob(raw) else {
                return pgp::mark_only(unreadable());
            };
            match smime.decrypt(blob) {
                Ok(part) => opened(smime, &part),
                Err(err) => pgp::mark_only(refused(&err)),
            }
        }
    }
}

/// What gpgsm says it is, for Preferences. The version is the last word of
/// its first line, as in `gpgsm (GnuPG) 2.4.8`.
pub fn version(smime: &Smime) -> Option<String> {
    let run = Command::new(smime.program())
        .arg("--version")
        .output()
        .ok()?;
    pgp::version_of(&String::from_utf8_lossy(&run.stdout))
}

/// Which standard would encrypt this draft, or what stands in the way.
///
/// OpenPGP wins when both could carry it, so that nothing about a message
/// the app already knew how to send changes the day gpgsm turns up.
/// `blind` says the draft carries a Bcc, which no encryption can keep
/// blind: both standards name everyone a message went to inside it.
pub fn encrypting(held: &Held, blind: bool) -> Result<Standard, String> {
    if blind {
        return Err(
            "An encrypted message names everyone it went to, so a blind copy would not stay \
             blind."
                .into(),
        );
    }
    let pgp = held.pgp.as_deref().map(pgp::cannot_encrypt);
    let smime = held.smime.as_deref().map(cannot_encrypt);
    match (pgp, smime) {
        (Some(None), _) => Ok(Standard::Pgp),
        (_, Some(None)) => Ok(Standard::Smime),
        (Some(Some(pgp)), Some(Some(smime))) => Err(neither(
            held.pgp.as_deref().unwrap_or_default(),
            held.smime.as_deref().unwrap_or_default(),
            &pgp,
            &smime,
        )),
        (Some(Some(problem)), None) | (None, Some(Some(problem))) => Err(problem),
        (None, None) => Err("This computer has nothing to encrypt with.".into()),
    }
}

/// Which standard signs a message from this address. The sender's own
/// holdings decide it, and OpenPGP wins a tie for the reason
/// [`encrypting`] gives.
pub fn signing(held: &Held) -> Standard {
    let key = held
        .pgp
        .as_deref()
        .is_some_and(|held| held.iter().any(|recipient| recipient.key.is_some()));
    let certificate = held
        .smime
        .as_deref()
        .is_some_and(|held| held.iter().any(|it| it.certificate.is_some()));
    match (key, certificate) {
        (false, true) => Standard::Smime,
        _ => Standard::Pgp,
    }
}

/// Why this draft cannot be encrypted under S/MIME, for the Encrypt button
/// to say. `None` means every recipient has a certificate.
pub fn cannot_encrypt(held: &[Recipient]) -> Option<String> {
    if held.is_empty() {
        return Some("Add a recipient whose certificate gpgsm holds.".into());
    }
    let missing: Vec<&str> = held
        .iter()
        .filter(|recipient| recipient.certificate.is_none())
        .map(|recipient| recipient.address.as_str())
        .collect();
    (!missing.is_empty())
        .then(|| format!("gpgsm holds no certificate for {}.", pgp::listed(&missing)))
}

/// What the Encrypt button says once it works, which names the standard
/// the message would go out under rather than making the writer guess.
pub fn encrypting_with(standard: Standard) -> &'static str {
    match standard {
        Standard::Pgp => "Encrypt this message to the recipients' keys",
        Standard::Smime => "Encrypt this message to the recipients' certificates",
    }
}

/// What Preferences says about the addresses this person sends from:
/// which of them gpgsm holds a certificate for, and which it holds none
/// for.
pub fn own_certificates(held: &[Recipient]) -> String {
    let addresses = |wanted: bool| -> Vec<&str> {
        held.iter()
            .filter(|recipient| recipient.certificate.is_some() == wanted)
            .map(|recipient| recipient.address.as_str())
            .collect()
    };
    let (mine, missing) = (addresses(true), addresses(false));
    match (mine.as_slice(), missing.as_slice()) {
        ([], _) => "gpgsm holds no certificate for any of the addresses you send from.".into(),
        (mine, []) => format!(
            "gpgsm holds a certificate for {}.",
            pgp::joined(mine, "and")
        ),
        (mine, missing) => format!(
            "gpgsm holds a certificate for {}, and none for {}.",
            pgp::joined(mine, "and"),
            pgp::joined(missing, "and")
        ),
    }
}

/// What came out of an envelope: the message, and whatever signature
/// travelled inside with it.
///
/// gpgsm opens one wrapper at a time, so a message that was signed before
/// it was enveloped arrives here as a signed entity, and the signature
/// that matters is the one inside. An outer signature means nothing,
/// since anyone can wrap somebody else's ciphertext in one of their own.
fn opened(smime: &Smime, part: &[u8]) -> Read {
    let inside = match inner(part) {
        Some(Opening::Verify) => pgp::wrapper_parts(part)
            .map(|(signed, signature)| (signed.to_vec(), smime.verify(signed, signature))),
        Some(Opening::Opaque) => blob(part).map(|blob| match smime.open_signed(blob) {
            Ok(found) => (found.part, Ok(found.signature)),
            Err(err) => (part.to_vec(), Err(err)),
        }),
        _ => None,
    };
    let (part, signature) = match inside {
        Some((part, Ok(signature))) => (part, Some(signature)),
        // The envelope opened and the signature inside it did not. What
        // came out is still the closest thing to the message there is, so
        // it goes up with the card saying only that it arrived encrypted.
        Some((part, Err(_))) => (part, None),
        None => (part.to_vec(), None),
    };
    let body = pgp::opened_body(&part);
    Read {
        mark: enveloped(signature.as_ref(), body.attachments.len()),
        body: Some(body),
    }
}

/// Which call the entity inside an envelope needs, when it is signed.
fn inner(part: &[u8]) -> Option<Opening> {
    let blank = pgp::find(part, b"\r\n\r\n")?;
    let content_type = pgp::unfolded(&part[..blank], "content-type")?;
    let media = content_type.split(';').next()?.trim().to_ascii_lowercase();
    match media.as_str() {
        "multipart/signed" => Some(Opening::Verify),
        "application/pkcs7-mime" | "application/x-pkcs7-mime" => {
            pgp::param(&content_type, "smime-type")
                .filter(|kind| kind.eq_ignore_ascii_case("signed-data"))
                .map(|_| Opening::Opaque)
        }
        _ => None,
    }
}

/// The body of the one part `raw` holds: everything after the blank line
/// that ends its headers. An S/MIME blob is a part of its own rather than
/// one of several, so there is nothing to pick out beside it.
fn blob(raw: &[u8]) -> Option<&[u8]> {
    Some(&raw[pgp::find(raw, b"\r\n\r\n")? + 4..])
}

/// What the card says about a signature. The verdict and the chain answer
/// different questions, so the title carries the one and the line under it
/// the other.
fn signed(signature: &Signature) -> Mark {
    let who = signer(signature);
    match signature.verdict {
        Verdict::Good => Mark {
            title: format!("Signed by {who}"),
            detail: Some(vouching(signature.chain).into()),
            tone: match signature.chain {
                Chain::Trusted => Tone::Good,
                _ => Tone::Unchecked,
            },
        },
        Verdict::Bad => Mark {
            title: "This message changed after it was signed".into(),
            detail: Some(format!(
                "The signature of {who} does not match what arrived."
            )),
            tone: Tone::Bad,
        },
        Verdict::ExpiredCertificate => Mark {
            title: format!("Signed by {who}, whose certificate has run out"),
            detail: Some(
                "The text is as it was written, and the certificate behind it has expired.".into(),
            ),
            tone: Tone::Unchecked,
        },
        Verdict::RevokedCertificate => Mark {
            title: format!("Signed by {who}, whose certificate was taken back"),
            detail: Some(
                "Whoever issued it revoked it, so it says nothing about who wrote this.".into(),
            ),
            tone: Tone::Bad,
        },
        Verdict::Expired => Mark {
            title: format!("Signed by {who}, and the signature has run out"),
            detail: Some(
                "It carried a date to stop being good on, and that date has passed.".into(),
            ),
            tone: Tone::Unchecked,
        },
        Verdict::NoCertificate => Mark {
            title: "Signed by a certificate this computer does not hold".into(),
            detail: Some(
                "The message carried none either, so there is nothing here to check it against."
                    .into(),
            ),
            tone: Tone::Unchecked,
        },
        Verdict::Unchecked => Mark {
            title: "gpgsm could not check this signature".into(),
            detail: None,
            tone: Tone::Unchecked,
        },
    }
}

/// What the card says about a message that arrived enveloped, with the
/// signature that travelled inside it when it carried one.
fn enveloped(signature: Option<&Signature>, files: usize) -> Mark {
    let mut mark = match signature {
        Some(signature) if signature.verdict == Verdict::Good => Mark {
            title: format!("Encrypted, and signed by {}", signer(signature)),
            detail: Some(vouching(signature.chain).into()),
            tone: match signature.chain {
                Chain::Trusted => Tone::Good,
                _ => Tone::Unchecked,
            },
        },
        Some(signature) => {
            let found = signed(signature);
            Mark {
                title: format!("Encrypted. {}", found.title),
                ..found
            }
        }
        None => Mark {
            title: "This message arrived encrypted".into(),
            detail: Some("Nobody signed it, so it says nothing about who sent it.".into()),
            tone: Tone::Unchecked,
        },
    };
    if let Some(line) = pgp::files_line(files) {
        mark.detail = Some(match mark.detail {
            Some(detail) => format!("{detail} {line}"),
            None => line,
        });
    }
    mark
}

/// What the card says when gpgsm would not open a message.
fn refused(err: &SmimeError) -> Mark {
    match err {
        SmimeError::NotForYou => Mark {
            title: "This message is encrypted to a certificate you do not hold".into(),
            detail: Some("Whoever sent it used a certificate gpgsm has no secret key for.".into()),
            tone: Tone::Unchecked,
        },
        SmimeError::NotSmime => unreadable(),
        other => Mark {
            title: "gpgsm could not open this message".into(),
            detail: Some(other.to_string()),
            tone: Tone::Unchecked,
        },
    }
}

/// What the card says about a message whose parts are not where S/MIME
/// keeps them.
fn unreadable() -> Mark {
    Mark {
        title: "This message says it is S/MIME and is not".into(),
        detail: Some("Its parts are not where a signed or encrypted message keeps them.".into()),
        tone: Tone::Unchecked,
    }
}

/// How far the chain behind the certificate reaches, said plainly. A
/// certificate whose chain reaches no root this computer trusts still
/// signs; the two are separate answers and running them together tells
/// people the wrong thing.
fn vouching(chain: Chain) -> &'static str {
    match chain {
        Chain::Trusted => "Its certificate leads back to an authority you trust.",
        Chain::Untrusted => "Its certificate leads back to nobody you trust, so it names no one.",
        Chain::Unknown => "Nothing here says who that certificate belongs to.",
    }
}

/// Who gpgsm says signed: the address on the certificate, and the subject
/// behind it when there is one to give.
fn signer(signature: &Signature) -> String {
    match (&signature.email, &signature.subject) {
        (Some(email), Some(subject)) => format!("{} <{email}>", name(subject)),
        (Some(email), None) => email.clone(),
        (None, Some(subject)) => name(subject).to_string(),
        (None, None) => "a certificate gpgsm would not name".into(),
    }
}

/// The common name out of a distinguished name, which gpgsm writes as
/// `/CN=Ada Lovelace/O=Example`. A subject with no common name goes in as
/// it came, since something is better than a blank.
fn name(subject: &str) -> &str {
    subject
        .split('/')
        .find_map(|piece| piece.trim().strip_prefix("CN="))
        .unwrap_or(subject)
        .trim()
}

/// What to say when neither standard reaches every recipient. Somebody
/// nothing here can reach is the likelier answer, so that is the one the
/// button gives; a draft that each standard covers half of gets its own
/// sentence, because adding a key would not fix it.
fn neither(
    keys: &[mailrs_pgp::Recipient],
    certificates: &[Recipient],
    pgp: &str,
    smime: &str,
) -> String {
    let unreachable: Vec<&str> = keys
        .iter()
        .filter(|recipient| recipient.key.is_none())
        .filter(|recipient| {
            certificates
                .iter()
                .any(|other| other.address == recipient.address && other.certificate.is_none())
        })
        .map(|recipient| recipient.address.as_str())
        .collect();
    if !unreachable.is_empty() {
        return format!(
            "gpg holds no key and gpgsm no certificate for {}.",
            pgp::listed(&unreachable)
        );
    }
    if keys.is_empty() || certificates.is_empty() {
        return pgp.to_string();
    }
    format!("A message goes out under one standard or the other. {pgp} {smime}")
}

#[cfg(test)]
mod tests {
    use std::process::Stdio;

    use mailrs_smime::Certificate;

    use super::*;

    /// A GnuPG home under a temp directory, with one certificate in it. It
    /// touches no keybox of whoever runs the tests, and the round trips
    /// below say so and stop when this computer has no gpgsm.
    struct Home {
        dir: tempfile::TempDir,
        smime: Smime,
        address: String,
    }

    impl Home {
        fn new() -> Option<Home> {
            let Ok(smime) = Smime::find() else {
                eprintln!("skipping: no gpgsm on PATH, so the round trips cannot run");
                require_crypto();
                return None;
            };
            let dir = tempfile::tempdir().expect("a temp directory");
            permit_owner_only(dir.path());
            let address = "ada@example.test";
            let params = dir.path().join("params");
            std::fs::write(
                &params,
                format!(
                    "Key-Type: RSA\nKey-Length: 2048\nKey-Usage: sign, encrypt\n\
                     Serial: random\nName-DN: CN=Ada Lovelace\nName-Email: {address}\n\
                     Not-After: 2038-01-01\n%commit\n"
                ),
            )
            .expect("write");
            let certificate = dir.path().join("certificate.pem");
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
            };
            home.trust();
            Some(home)
        }

        /// Marks the home's own certificate as a root worth believing,
        /// which is what a chain has to reach.
        fn trust(&self) {
            let out = gpgsm(&self.smime, self.dir.path())
                .args(["--with-colons", "--list-keys", &self.address])
                .stdout(Stdio::piped())
                .output()
                .expect("gpgsm runs");
            let fingerprint = String::from_utf8_lossy(&out.stdout)
                .lines()
                .find_map(|record| record.strip_prefix("fpr:"))
                .and_then(|rest| rest.split(':').nth(8).map(str::to_string))
                .expect("a fingerprint");
            let spaced: Vec<String> = fingerprint
                .as_bytes()
                .chunks(2)
                .map(|pair| String::from_utf8_lossy(pair).into_owned())
                .collect();
            std::fs::write(
                self.dir.path().join("trustlist.txt"),
                format!("{} S relax\n", spaced.join(":")),
            )
            .expect("write");
        }

        /// The message a send would put on the wire: the headers of the
        /// message, then the entity the engine handed back, byte for byte.
        fn message(&self, entity: &[u8]) -> Vec<u8> {
            let mut raw = format!(
                "From: Ada Lovelace <{0}>\r\nTo: Ada Lovelace <{0}>\r\n\
                 Subject: Six\r\nMIME-Version: 1.0\r\n",
                self.address
            )
            .into_bytes();
            raw.extend_from_slice(entity);
            raw
        }
    }

    impl Drop for Home {
        fn drop(&mut self) {
            // The agent gpgsm started holds sockets under the temp
            // directory.
            let _ = Command::new("gpgconf")
                .arg("--homedir")
                .arg(self.dir.path())
                .args(["--kill", "all"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }

    /// gpgsm as the test drives it, rather than as the code under test
    /// does.
    fn gpgsm(smime: &Smime, home: &std::path::Path) -> Command {
        let mut command = Command::new(smime.program());
        command
            .args(["--batch", "--no-tty", "--disable-dirmngr", "--homedir"])
            .arg(home)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command
    }

    #[cfg(unix)]
    fn permit_owner_only(path: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    }

    #[cfg(not(unix))]
    fn permit_owner_only(_path: &std::path::Path) {}

    fn signature(verdict: Verdict, chain: Chain) -> Signature {
        Signature {
            verdict,
            subject: Some("/CN=Ada Lovelace/O=Example".into()),
            email: Some("ada@example.test".into()),
            fingerprint: Some("F".repeat(40)),
            chain,
        }
    }

    fn certificate(address: &str, held: bool) -> Recipient {
        Recipient {
            address: address.to_string(),
            certificate: held.then(|| Certificate {
                fingerprint: "F".repeat(40),
                subject: format!("CN={address}"),
                email: address.to_string(),
            }),
        }
    }

    fn key(address: &str, held: bool) -> mailrs_pgp::Recipient {
        mailrs_pgp::Recipient {
            address: address.to_string(),
            key: held.then(|| mailrs_pgp::Key {
                fingerprint: "F".repeat(40),
                user_id: format!("<{address}>"),
                trust: mailrs_pgp::Trust::Unknown,
            }),
        }
    }

    fn arrived(protection: Protection) -> MessageBody {
        MessageBody {
            protection: Some(protection),
            ..MessageBody::default()
        }
    }

    #[test]
    fn the_wrapper_decides_which_engine_a_message_needs() {
        assert_eq!(
            engine(&arrived(Protection::SmimeSigned)),
            Some(Engine::Smime(Opening::Verify))
        );
        assert_eq!(
            engine(&arrived(Protection::SmimeOpaque)),
            Some(Engine::Smime(Opening::Opaque))
        );
        assert_eq!(
            engine(&arrived(Protection::SmimeEnveloped)),
            Some(Engine::Smime(Opening::Decrypt))
        );
        assert_eq!(
            engine(&arrived(Protection::Signed)),
            Some(Engine::Pgp(pgp::Opening::Verify))
        );
        assert_eq!(
            engine(&arrived(Protection::Encrypted)),
            Some(Engine::Pgp(pgp::Opening::Decrypt))
        );
        assert_eq!(engine(&MessageBody::default()), None);
    }

    #[test]
    fn a_good_signature_names_the_signer_and_how_far_the_chain_reached() {
        let mark = signed(&signature(Verdict::Good, Chain::Trusted));
        assert_eq!(mark.title, "Signed by Ada Lovelace <ada@example.test>");
        assert_eq!(
            mark.detail.as_deref(),
            Some("Its certificate leads back to an authority you trust.")
        );
        assert_eq!(mark.tone, Tone::Good);
    }

    #[test]
    fn a_chain_that_reached_nobody_we_trust_leaves_the_card_unchecked() {
        let mark = signed(&signature(Verdict::Good, Chain::Untrusted));
        assert_eq!(mark.title, "Signed by Ada Lovelace <ada@example.test>");
        assert_eq!(mark.tone, Tone::Unchecked);
        assert!(
            mark.detail
                .as_deref()
                .is_some_and(|detail| detail.contains("nobody you trust")),
            "{mark:?}"
        );
    }

    #[test]
    fn a_bad_signature_says_so_plainly() {
        let mark = signed(&signature(Verdict::Bad, Chain::Trusted));
        assert_eq!(mark.title, "This message changed after it was signed");
        assert_eq!(mark.tone, Tone::Bad);
    }

    #[test]
    fn a_certificate_that_ran_out_is_not_a_bad_signature() {
        let mark = signed(&signature(Verdict::ExpiredCertificate, Chain::Trusted));
        assert!(
            mark.title.ends_with("whose certificate has run out"),
            "{mark:?}"
        );
        assert_eq!(mark.tone, Tone::Unchecked);
    }

    #[test]
    fn a_certificate_we_do_not_hold_is_its_own_answer() {
        let unknown = Signature {
            subject: None,
            email: None,
            fingerprint: None,
            ..signature(Verdict::NoCertificate, Chain::Unknown)
        };
        let mark = signed(&unknown);
        assert_eq!(
            mark.title,
            "Signed by a certificate this computer does not hold"
        );
        assert_eq!(mark.tone, Tone::Unchecked);
    }

    #[test]
    fn an_enveloped_message_says_it_arrived_that_way() {
        let alone = enveloped(None, 0);
        assert_eq!(alone.title, "This message arrived encrypted");
        assert_eq!(alone.tone, Tone::Unchecked);

        let inside = enveloped(Some(&signature(Verdict::Good, Chain::Trusted)), 0);
        assert_eq!(
            inside.title,
            "Encrypted, and signed by Ada Lovelace <ada@example.test>"
        );
        assert_eq!(inside.tone, Tone::Good);

        let broken = enveloped(Some(&signature(Verdict::Bad, Chain::Trusted)), 0);
        assert_eq!(
            broken.title,
            "Encrypted. This message changed after it was signed"
        );
        assert_eq!(broken.tone, Tone::Bad);
    }

    #[test]
    fn a_message_for_somebody_else_says_so_where_the_message_would_be() {
        let mark = refused(&SmimeError::NotForYou);
        assert_eq!(
            mark.title,
            "This message is encrypted to a certificate you do not hold"
        );
        assert_eq!(mark.tone, Tone::Unchecked);
    }

    #[test]
    fn openpgp_carries_the_message_when_both_standards_could() {
        let held = Held {
            pgp: Some(vec![key("ada@example.test", true)]),
            smime: Some(vec![certificate("ada@example.test", true)]),
        };
        assert_eq!(encrypting(&held, false), Ok(Standard::Pgp));
    }

    #[test]
    fn smime_carries_it_when_it_is_the_one_that_reaches_everybody() {
        let held = Held {
            pgp: Some(vec![key("ada@example.test", false)]),
            smime: Some(vec![certificate("ada@example.test", true)]),
        };
        assert_eq!(encrypting(&held, false), Ok(Standard::Smime));
    }

    #[test]
    fn a_recipient_neither_standard_reaches_is_named_once() {
        let held = Held {
            pgp: Some(vec![
                key("ada@example.test", true),
                key("bo@example.test", false),
            ]),
            smime: Some(vec![
                certificate("ada@example.test", false),
                certificate("bo@example.test", false),
            ]),
        };
        assert_eq!(
            encrypting(&held, false),
            Err("gpg holds no key and gpgsm no certificate for bo@example.test.".into())
        );
    }

    #[test]
    fn a_draft_each_standard_covers_half_of_says_what_that_means() {
        let held = Held {
            pgp: Some(vec![
                key("ada@example.test", true),
                key("bo@example.test", false),
            ]),
            smime: Some(vec![
                certificate("ada@example.test", false),
                certificate("bo@example.test", true),
            ]),
        };
        let problem = encrypting(&held, false).expect_err("neither reaches both");
        assert!(
            problem.starts_with("A message goes out under one standard or the other."),
            "{problem}"
        );
        assert!(problem.contains("bo@example.test"), "{problem}");
    }

    #[test]
    fn the_only_engine_on_this_computer_is_the_one_that_answers() {
        let smime_alone = Held {
            pgp: None,
            smime: Some(vec![certificate("bo@example.test", false)]),
        };
        assert_eq!(
            encrypting(&smime_alone, false),
            Err("gpgsm holds no certificate for bo@example.test.".into())
        );
        let pgp_alone = Held {
            pgp: Some(vec![key("bo@example.test", false)]),
            smime: None,
        };
        assert_eq!(
            encrypting(&pgp_alone, false),
            Err("gpg holds no key for bo@example.test.".into())
        );
    }

    #[test]
    fn a_blind_copy_and_encryption_do_not_go_together_under_either_standard() {
        let held = Held {
            pgp: Some(vec![key("ada@example.test", true)]),
            smime: Some(vec![certificate("ada@example.test", true)]),
        };
        let problem = encrypting(&held, true).expect_err("a blind copy stops it");
        assert!(problem.contains("blind"), "{problem}");
    }

    #[test]
    fn the_sender_decides_which_standard_signs() {
        let both = Held {
            pgp: Some(vec![key("ada@example.test", true)]),
            smime: Some(vec![certificate("ada@example.test", true)]),
        };
        assert_eq!(signing(&both), Standard::Pgp);

        let certificate_only = Held {
            pgp: Some(vec![key("ada@example.test", false)]),
            smime: Some(vec![certificate("ada@example.test", true)]),
        };
        assert_eq!(signing(&certificate_only), Standard::Smime);

        assert_eq!(signing(&Held::default()), Standard::Pgp);
    }

    #[test]
    fn preferences_say_which_of_your_own_addresses_gpgsm_has_a_certificate_for() {
        assert_eq!(
            own_certificates(&[certificate("ada@example.test", false)]),
            "gpgsm holds no certificate for any of the addresses you send from."
        );
        assert_eq!(
            own_certificates(&[
                certificate("ada@example.test", true),
                certificate("work@example.test", true),
            ]),
            "gpgsm holds a certificate for ada@example.test and work@example.test."
        );
        assert_eq!(
            own_certificates(&[
                certificate("ada@example.test", true),
                certificate("work@example.test", false),
            ]),
            "gpgsm holds a certificate for ada@example.test, and none for work@example.test."
        );
    }

    #[test]
    fn a_message_signed_by_the_engine_reads_back_as_signed_here() {
        let Some(home) = Home::new() else { return };
        let part = b"Content-Type: text/plain; charset=utf-8\r\n\r\nMeet at six.\r\n";
        let entity = home.smime.sign(part, &home.address).expect("a signed body");
        let raw = home.message(&entity);

        let read = read(&home.smime, Opening::Verify, &raw);

        assert_eq!(read.mark.title, "Signed by Ada Lovelace <ada@example.test>");
        assert_eq!(read.mark.tone, Tone::Good, "{:?}", read.mark);
        assert!(read.body.is_none());
    }

    #[test]
    fn a_message_the_engine_enveloped_comes_back_readable_and_signed() {
        let Some(home) = Home::new() else { return };
        let part = b"Content-Type: text/plain; charset=utf-8\r\n\r\nThe key is under the mat.\r\n";
        let entity = home
            .smime
            .encrypt(
                part,
                std::slice::from_ref(&home.address),
                Some(&home.address),
            )
            .expect("an enveloped body");
        let raw = home.message(&entity);

        let read = read(&home.smime, Opening::Decrypt, &raw);

        assert_eq!(
            read.mark.title,
            "Encrypted, and signed by Ada Lovelace <ada@example.test>"
        );
        assert_eq!(read.mark.tone, Tone::Good, "{:?}", read.mark);
        let inside = read.body.expect("the message that was inside");
        // The blank line at the end of a signed part is not part of it, so
        // what was signed, enveloped and opened again ends at the full stop.
        assert_eq!(inside.text.as_deref(), Some("The key is under the mat."));
    }

    #[test]
    fn a_message_inside_its_own_signature_is_drawn_from_what_was_in_there() {
        let Some(home) = Home::new() else { return };
        let part = b"Content-Type: text/plain; charset=utf-8\r\n\r\nMeet at six.\r\n";
        let file = home.dir.path().join("signed");
        std::fs::write(&file, part).expect("write");
        // What Outlook sends: the message inside the blob, base64, with the
        // headers of one part around it.
        let out = gpgsm(&home.smime, home.dir.path())
            .args(["--sign", "--local-user", &home.address, "--output", "-"])
            .arg(&file)
            .stdout(Stdio::piped())
            .output()
            .expect("gpgsm runs");
        assert!(out.status.success(), "gpgsm could not sign");
        let mut entity = b"Content-Type: application/pkcs7-mime; smime-type=signed-data;\r\n \
             name=\"smime.p7m\"\r\nContent-Transfer-Encoding: base64\r\n\r\n"
            .to_vec();
        entity.extend_from_slice(&mailrs_smime::mime::base64(&out.stdout));
        let raw = home.message(&entity);

        let read = read(&home.smime, Opening::Opaque, &raw);

        assert_eq!(read.mark.title, "Signed by Ada Lovelace <ada@example.test>");
        assert_eq!(read.mark.tone, Tone::Good, "{:?}", read.mark);
        let inside = read.body.expect("the message that was inside");
        assert_eq!(inside.text.as_deref(), Some("Meet at six.\r\n"));
    }

    #[test]
    fn a_draft_signed_on_its_way_out_verifies_on_its_way_in() {
        let Some(home) = Home::new() else { return };
        let me = mailrs_domain::Address {
            name: Some("Ada Lovelace".into()),
            email: home.address.clone(),
        };
        let mut draft = crate::compose::Draft::new(1, me.clone());
        draft.to = vec![me];
        draft.subject = "Six".into();
        draft.markdown = "Meet at six.".into();
        let part = crate::compose::build_body_part(&draft).expect("a body part");
        let entity = home
            .smime
            .sign(&part, &home.address)
            .expect("a signed body");
        let raw =
            crate::compose::build_protected(&draft, 1_757_000_000, "<id@example.test>", entity)
                .expect("a message");

        let read = read(&home.smime, Opening::Verify, &raw);

        assert_eq!(read.mark.title, "Signed by Ada Lovelace <ada@example.test>");
        assert_eq!(read.mark.tone, Tone::Good, "{:?}", read.mark);
    }

    #[test]
    fn a_message_that_says_it_is_smime_and_is_not_says_so() {
        let Some(home) = Home::new() else { return };
        let raw = home.message(b"Content-Type: multipart/signed\r\n\r\nMeet at six.\r\n");

        let read = read(&home.smime, Opening::Verify, &raw);

        assert_eq!(read.mark.title, "This message says it is S/MIME and is not");
        assert_eq!(read.mark.tone, Tone::Unchecked);
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
}
