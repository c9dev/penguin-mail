//! OpenPGP in the app: which call a message needs, what comes back when
//! gpg has run, and which recipients stand between a draft and encryption.
//!
//! `mailrs_pgp` runs gpg and `ui::pgp` draws the answer. What is left here
//! is the deciding and the wording, so both sit under plain unit tests and
//! neither needs a window. Every call below blocks, so the window hands
//! them to `Core::gpg` rather than running them itself.

use std::process::Command;

use mail_parser::{MessageParser, MimeHeaders};
use mailrs_domain::{MessageBody, Protection};
use mailrs_gmail::body::decode_charset;
use mailrs_pgp::{Pgp, PgpError, Recipient, Signature, Trust, Verdict, inline};

/// Which call of the engine one message needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Opening {
    /// `Pgp::verify`, over the first part of a `multipart/signed`.
    Verify,
    /// `Pgp::decrypt`, over the ciphertext of a `multipart/encrypted`.
    Decrypt,
    /// `Pgp::open_inline`, over armor written into the text.
    Inline,
}

/// What gpg made of one message: the mark to put above it, and the body to
/// draw in place of the one that arrived, when gpg opened something.
pub struct Read {
    pub mark: Mark,
    pub body: Option<MessageBody>,
}

/// What the card says about a message, and how loudly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mark {
    pub title: String,
    /// The line under the title, when there is more to say.
    pub detail: Option<String>,
    pub tone: Tone,
}

/// How much of the message the card is vouching for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// The text is the text the signer wrote.
    Good,
    /// Something is wrong with the message.
    Bad,
    /// Nothing on this computer could check it.
    Unchecked,
}

/// Which call `body` needs before it is drawn, in the order RFC 3156 reads
/// a message: the wrapper first, and the text only when there is none.
pub fn opening(body: &MessageBody) -> Option<Opening> {
    match body.protection {
        Some(Protection::Signed) => Some(Opening::Verify),
        Some(Protection::Encrypted) => Some(Opening::Decrypt),
        None => inline::armor(body.text.as_deref()?).map(|_| Opening::Inline),
    }
}

/// Runs the call `opening` asks for and says what the window should show.
///
/// `raw` is the message as it arrived, from `format=raw`, which is the one
/// copy whose bytes a signature still covers. Every branch has an answer,
/// including the ones where gpg refused, so the card never goes blank.
pub fn read(pgp: &Pgp, opening: Opening, raw: &[u8], body: &MessageBody) -> Read {
    match opening {
        Opening::Verify => {
            let Some((part, signature)) = wrapper_parts(raw) else {
                return mark_only(unreadable());
            };
            match pgp.verify(part, signature) {
                Ok(found) => mark_only(signed(&found)),
                Err(err) => mark_only(refused(&err)),
            }
        }
        Opening::Decrypt => {
            let Some((_, ciphertext)) = wrapper_parts(raw) else {
                return mark_only(unreadable());
            };
            match pgp.decrypt(ciphertext) {
                Ok(opened) => {
                    let inside = opened_body(&opened.part);
                    Read {
                        mark: encrypted(opened.signature.as_ref(), inside.attachments.len()),
                        body: Some(inside),
                    }
                }
                Err(err) => mark_only(refused(&err)),
            }
        }
        Opening::Inline => {
            let text = body.text.as_deref().unwrap_or_default();
            match pgp.open_inline(text) {
                Ok(opened) => Read {
                    mark: match opened.signature.as_ref() {
                        Some(signature) => signed(signature),
                        None => encrypted(None, 0),
                    },
                    // The armor said nothing about a character set, so these
                    // bytes are read the way a body with no charset is.
                    body: Some(MessageBody {
                        text: Some(decode_charset(&opened.text, None)),
                        ..body.clone()
                    }),
                },
                Err(err) => mark_only(refused(&err)),
            }
        }
    }
}

/// What gpg says it is, for Preferences. The version is the last word of
/// its first line, as in `gpg (GnuPG) 2.4.8`.
pub fn version(pgp: &Pgp) -> Option<String> {
    let run = Command::new(pgp.program()).arg("--version").output().ok()?;
    version_of(&String::from_utf8_lossy(&run.stdout))
}

/// Why this draft cannot be encrypted, for the Encrypt button to say.
/// `None` means every recipient has a key. `blind` says the draft carries
/// a Bcc, which encryption cannot keep blind: OpenPGP names every key a
/// message was encrypted to, so the other recipients would read it there.
pub fn cannot_encrypt(held: &[Recipient], blind: bool) -> Option<String> {
    if blind {
        return Some(
            "An encrypted message names every key it went to, so a blind copy would not stay \
             blind."
                .into(),
        );
    }
    if held.is_empty() {
        return Some("Add a recipient whose key gpg holds.".into());
    }
    let missing: Vec<&str> = held
        .iter()
        .filter(|recipient| recipient.key.is_none())
        .map(|recipient| recipient.address.as_str())
        .collect();
    (!missing.is_empty()).then(|| format!("gpg holds no key for {}.", listed(&missing)))
}

/// What Preferences says about the addresses this person sends from:
/// which of them gpg holds a key for, and which it holds nothing for.
pub fn own_keys(held: &[Recipient]) -> String {
    let addresses = |wanted: bool| -> Vec<&str> {
        held.iter()
            .filter(|recipient| recipient.key.is_some() == wanted)
            .map(|recipient| recipient.address.as_str())
            .collect()
    };
    let (mine, missing) = (addresses(true), addresses(false));
    match (mine.as_slice(), missing.as_slice()) {
        ([], _) => "gpg holds no key for any of the addresses you send from.".into(),
        (mine, []) => format!("gpg holds a key for {}.", joined(mine, "and")),
        (mine, missing) => format!(
            "gpg holds a key for {}, and none for {}.",
            joined(mine, "and"),
            joined(missing, "and")
        ),
    }
}

/// The two parts of the entity `raw` holds: the first whole, headers and
/// all, and the second's body on its own.
///
/// These are slices of the message rather than anything parsed and put
/// back together, because a signature covers the first part byte for byte.
/// The CRLF before a boundary belongs to the boundary, so it comes off
/// here, and a message whose lines end some other way never signed
/// anything a reader could check.
fn wrapper_parts(raw: &[u8]) -> Option<(&[u8], &[u8])> {
    let blank = find(raw, b"\r\n\r\n")?;
    let boundary = param(&unfolded(&raw[..blank], "content-type")?, "boundary")?;
    let open = format!("--{boundary}\r\n").into_bytes();
    let next = format!("\r\n--{boundary}").into_bytes();
    let body = &raw[blank + 4..];
    let first = &body[find(body, &open)? + open.len()..];
    let (first, after) = first.split_at(find(first, &next)?);
    let second = after.get(next.len()..)?.strip_prefix(b"\r\n")?;
    let second = &second[..find(second, &next)?];
    Some((first, &second[find(second, b"\r\n\r\n")? + 4..]))
}

/// What the card says about a signature over a message that arrived in the
/// clear. The verdict and the trust answer different questions, so the
/// title carries the one and the line under it the other.
fn signed(signature: &Signature) -> Mark {
    let who = signer(signature);
    match signature.verdict {
        Verdict::Good => Mark {
            title: format!("Signed by {who}"),
            detail: Some(vouching(signature.trust).into()),
            tone: match signature.trust {
                Trust::Never => Tone::Bad,
                _ => Tone::Good,
            },
        },
        Verdict::Bad => Mark {
            title: "This message changed after it was signed".into(),
            detail: Some(format!(
                "The signature of {who} does not match what arrived."
            )),
            tone: Tone::Bad,
        },
        Verdict::ExpiredKey => Mark {
            title: format!("Signed by {who}, whose key has expired"),
            detail: Some("The text is as it was written, and the key behind it ran out.".into()),
            tone: Tone::Unchecked,
        },
        Verdict::RevokedKey => Mark {
            title: format!("Signed by {who}, who took this key back"),
            detail: Some("The owner revoked it, so it says nothing about who wrote this.".into()),
            tone: Tone::Bad,
        },
        Verdict::Expired => Mark {
            title: format!("Signed by {who}, and the signature has run out"),
            detail: Some(
                "It carried a date to stop being good on, and that date has passed.".into(),
            ),
            tone: Tone::Unchecked,
        },
        Verdict::NoKey => Mark {
            title: "Signed by a key this computer does not hold".into(),
            detail: Some(format!(
                "Nothing here can check it. Ask gpg for key {}.",
                signature.key_id.as_deref().unwrap_or("it names")
            )),
            tone: Tone::Unchecked,
        },
        Verdict::Unchecked => Mark {
            title: "gpg could not check this signature".into(),
            detail: None,
            tone: Tone::Unchecked,
        },
    }
}

/// What the card says about a message that arrived encrypted, with the
/// signature that travelled inside it when it carried one. A signature
/// wrapped around somebody else's ciphertext means nothing, so only the
/// inner one is shown.
fn encrypted(signature: Option<&Signature>, files: usize) -> Mark {
    let mut mark = match signature {
        Some(signature) if signature.verdict == Verdict::Good => Mark {
            title: format!("Encrypted, and signed by {}", signer(signature)),
            detail: Some(vouching(signature.trust).into()),
            tone: match signature.trust {
                Trust::Never => Tone::Bad,
                _ => Tone::Good,
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
    if let Some(line) = files_line(files) {
        mark.detail = Some(match mark.detail {
            Some(detail) => format!("{detail} {line}"),
            None => line,
        });
    }
    mark
}

/// What the card says when gpg would not open a message.
fn refused(err: &PgpError) -> Mark {
    match err {
        PgpError::NotForYou => Mark {
            title: "This message is encrypted to a key you do not hold".into(),
            detail: Some("Whoever sent it used a key gpg has no secret half of.".into()),
            tone: Tone::Unchecked,
        },
        PgpError::NotPgp => unreadable(),
        other => Mark {
            title: "gpg could not open this message".into(),
            detail: Some(other.to_string()),
            tone: Tone::Unchecked,
        },
    }
}

/// What the card says about a message whose parts are not where RFC 3156
/// says they are.
fn unreadable() -> Mark {
    Mark {
        title: "This message says it is OpenPGP and is not".into(),
        detail: Some("Its parts are not where a signed or encrypted message keeps them.".into()),
        tone: Tone::Unchecked,
    }
}

/// The files inside an encrypted message, which stay inside it: they never
/// reach Gmail, so nothing in the window can fetch one.
fn files_line(files: usize) -> Option<String> {
    match files {
        0 => None,
        1 => Some("It carries a file this window cannot open yet.".into()),
        many => Some(format!(
            "It carries {many} files this window cannot open yet."
        )),
    }
}

/// How far the trust database vouches for the key's owner, said plainly.
/// A key nobody has vouched for still signs; the two are separate answers
/// and running them together tells people the wrong thing.
fn vouching(trust: Trust) -> &'static str {
    match trust {
        Trust::Ultimate => "This is one of your own keys.",
        Trust::Full => "You have vouched for this key.",
        Trust::Marginal => "People you trust have vouched for this key.",
        Trust::Unknown => "Nobody has vouched for this key, so it names no one.",
        Trust::Never => "You marked this key as one not to trust.",
    }
}

/// Who gpg says signed, as their user id, or as the key when it has no
/// name for them.
fn signer(signature: &Signature) -> String {
    match (&signature.signer, &signature.key_id) {
        (Some(signer), _) => signer.clone(),
        (None, Some(key_id)) => format!("key {key_id}"),
        (None, None) => "a key gpg would not name".into(),
    }
}

/// The message inside the encryption, read as the mail it is.
fn opened_body(part: &[u8]) -> MessageBody {
    let Some(parsed) = MessageParser::default().parse(part) else {
        return MessageBody {
            text: Some(String::from_utf8_lossy(part).into_owned()),
            ..MessageBody::default()
        };
    };
    MessageBody {
        html: parsed.body_html(0).map(|html| html.into_owned()),
        text: parsed.body_text(0).map(|text| text.into_owned()),
        attachments: parsed
            .attachments()
            .map(|attachment| mailrs_domain::Attachment {
                part_id: String::new(),
                filename: attachment
                    .attachment_name()
                    .unwrap_or("attachment")
                    .to_string(),
                mime_type: String::new(),
                size: attachment.len() as i64,
                attachment_id: None,
                content_id: None,
            })
            .collect(),
        ..MessageBody::default()
    }
}

fn mark_only(mark: Mark) -> Read {
    Read { mark, body: None }
}

/// "ann@example.com", "ann@example.com or bo@example.com", and with more
/// than two, commas until the last.
fn listed(names: &[&str]) -> String {
    joined(names, "or")
}

fn joined(names: &[&str], last_word: &str) -> String {
    match names {
        [] => String::new(),
        [one] => (*one).to_string(),
        [rest @ .., last] => format!("{} {last_word} {last}", rest.join(", ")),
    }
}

/// The version out of `gpg --version`, which leads with `gpg (GnuPG) 2.4.8`.
fn version_of(output: &str) -> Option<String> {
    let line = output.lines().next()?.trim();
    let version = line.rsplit(' ').next()?;
    version
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_digit())
        .then(|| version.to_string())
}

/// The value of one header, the lines it folds onto joined back on. Header
/// values are only read here, so bytes that are not UTF-8 lose nothing.
fn unfolded(headers: &[u8], name: &str) -> Option<String> {
    let text = String::from_utf8_lossy(headers)
        .replace("\r\n ", " ")
        .replace("\r\n\t", " ");
    text.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.trim()
            .eq_ignore_ascii_case(name)
            .then(|| value.trim().to_string())
    })
}

/// One parameter of a header value, without its quotes.
fn param(value: &str, name: &str) -> Option<String> {
    value.split(';').skip(1).find_map(|parameter| {
        let (key, value) = parameter.split_once('=')?;
        key.trim()
            .eq_ignore_ascii_case(name)
            .then(|| value.trim().trim_matches('"').to_string())
    })
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use std::process::Stdio;

    use mailrs_pgp::Key;

    use super::*;

    /// A GnuPG home under a temp directory, with one key in it. It touches
    /// no keyring of whoever runs the tests, and the round trips below say
    /// so and stop when this computer has no gpg.
    struct Home {
        dir: tempfile::TempDir,
        pgp: Pgp,
        address: String,
    }

    impl Home {
        fn new() -> Option<Home> {
            let Ok(pgp) = Pgp::find() else {
                eprintln!("skipping: no gpg on PATH, so the round trips cannot run");
                return None;
            };
            let dir = tempfile::tempdir().expect("a temp directory");
            permit_owner_only(dir.path());
            let address = "ada@example.test";
            let made = Command::new(pgp.program())
                .args(["--batch", "--no-tty", "--homedir"])
                .arg(dir.path())
                .args([
                    "--passphrase",
                    "",
                    "--quick-generate-key",
                    &format!("Ada Lovelace <{address}>"),
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
            // The agent gpg started holds sockets under the temp directory.
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
    fn permit_owner_only(path: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    }

    #[cfg(not(unix))]
    fn permit_owner_only(_path: &std::path::Path) {}

    fn body(text: &str) -> MessageBody {
        MessageBody {
            text: Some(text.to_string()),
            ..MessageBody::default()
        }
    }

    fn signature(verdict: Verdict, trust: Trust) -> Signature {
        Signature {
            verdict,
            signer: Some("Ada Lovelace <ada@example.test>".into()),
            fingerprint: Some("F".repeat(40)),
            key_id: Some("1234567890ABCDEF".into()),
            trust,
        }
    }

    fn recipient(address: &str, key: bool) -> Recipient {
        Recipient {
            address: address.to_string(),
            key: key.then(|| Key {
                fingerprint: "F".repeat(40),
                user_id: format!("<{address}>"),
                trust: Trust::Unknown,
            }),
        }
    }

    #[test]
    fn the_wrapper_decides_which_call_a_message_needs() {
        let signed = MessageBody {
            protection: Some(Protection::Signed),
            ..body("Meet at six.")
        };
        let encrypted = MessageBody {
            protection: Some(Protection::Encrypted),
            ..MessageBody::default()
        };
        assert_eq!(opening(&signed), Some(Opening::Verify));
        assert_eq!(opening(&encrypted), Some(Opening::Decrypt));
    }

    #[test]
    fn armor_in_the_text_needs_opening_too() {
        let armored = body(
            "Sent from my telephone\n-----BEGIN PGP MESSAGE-----\nwcBM\n-----END PGP MESSAGE-----\n",
        );
        assert_eq!(opening(&armored), Some(Opening::Inline));
    }

    #[test]
    fn a_plain_message_needs_nothing() {
        assert_eq!(opening(&body("Meet at six.")), None);
        assert_eq!(opening(&MessageBody::default()), None);
    }

    #[test]
    fn the_signed_part_comes_out_as_it_arrived() {
        let raw = b"From: ada@example.test\r\n\
                    Content-Type: multipart/signed; micalg=pgp-sha256;\r\n \
                    protocol=\"application/pgp-signature\"; boundary=\"edge\"\r\n\
                    \r\n\
                    --edge\r\n\
                    Content-Type: text/plain\r\n\
                    \r\n\
                    Meet at six.\r\n\
                    --edge\r\n\
                    Content-Type: application/pgp-signature\r\n\
                    \r\n\
                    -----BEGIN PGP SIGNATURE-----\r\n\
                    -----END PGP SIGNATURE-----\r\n\
                    --edge--\r\n";
        let (part, signature) = wrapper_parts(raw).expect("two parts");
        assert_eq!(part, b"Content-Type: text/plain\r\n\r\nMeet at six.");
        assert_eq!(
            signature,
            b"-----BEGIN PGP SIGNATURE-----\r\n-----END PGP SIGNATURE-----"
        );
    }

    #[test]
    fn a_message_missing_its_boundary_gives_back_nothing() {
        let raw = b"Content-Type: multipart/signed\r\n\r\nMeet at six.\r\n";
        assert!(wrapper_parts(raw).is_none());
    }

    #[test]
    fn a_good_signature_names_the_signer_and_how_far_the_key_is_trusted() {
        let mark = signed(&signature(Verdict::Good, Trust::Unknown));
        assert_eq!(mark.title, "Signed by Ada Lovelace <ada@example.test>");
        assert_eq!(
            mark.detail.as_deref(),
            Some("Nobody has vouched for this key, so it names no one.")
        );
        assert_eq!(mark.tone, Tone::Good);

        let vouched = signed(&signature(Verdict::Good, Trust::Full));
        assert_eq!(
            vouched.detail.as_deref(),
            Some("You have vouched for this key.")
        );
        assert_eq!(vouched.tone, Tone::Good);
    }

    #[test]
    fn a_bad_signature_says_so_plainly() {
        let mark = signed(&signature(Verdict::Bad, Trust::Full));
        assert_eq!(mark.title, "This message changed after it was signed");
        assert_eq!(mark.tone, Tone::Bad);
    }

    #[test]
    fn a_key_we_do_not_hold_is_its_own_answer() {
        let unknown = Signature {
            signer: None,
            ..signature(Verdict::NoKey, Trust::Unknown)
        };
        let mark = signed(&unknown);
        assert_eq!(mark.title, "Signed by a key this computer does not hold");
        assert!(
            mark.detail
                .as_deref()
                .is_some_and(|detail| detail.contains("1234567890ABCDEF")),
            "{mark:?}"
        );
        assert_eq!(mark.tone, Tone::Unchecked);
    }

    #[test]
    fn an_encrypted_message_says_it_arrived_that_way() {
        let alone = encrypted(None, 0);
        assert_eq!(alone.title, "This message arrived encrypted");
        assert_eq!(alone.tone, Tone::Unchecked);

        let inside = encrypted(Some(&signature(Verdict::Good, Trust::Ultimate)), 0);
        assert_eq!(
            inside.title,
            "Encrypted, and signed by Ada Lovelace <ada@example.test>"
        );
        assert_eq!(inside.tone, Tone::Good);

        let broken = encrypted(Some(&signature(Verdict::Bad, Trust::Full)), 0);
        assert_eq!(
            broken.title,
            "Encrypted. This message changed after it was signed"
        );
        assert_eq!(broken.tone, Tone::Bad);
    }

    #[test]
    fn files_inside_the_encryption_are_owned_up_to() {
        let one = encrypted(None, 1);
        assert!(
            one.detail
                .as_deref()
                .is_some_and(|detail| detail.ends_with("a file this window cannot open yet.")),
            "{one:?}"
        );
        let three = encrypted(None, 3);
        assert!(
            three
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains("3 files")),
            "{three:?}"
        );
    }

    #[test]
    fn a_message_for_somebody_else_says_so_where_the_message_would_be() {
        let mark = refused(&PgpError::NotForYou);
        assert_eq!(
            mark.title,
            "This message is encrypted to a key you do not hold"
        );
        assert_eq!(mark.tone, Tone::Unchecked);
    }

    #[test]
    fn encryption_waits_until_every_recipient_has_a_key() {
        assert_eq!(
            cannot_encrypt(&[], false).as_deref(),
            Some("Add a recipient whose key gpg holds.")
        );
        assert_eq!(
            cannot_encrypt(&[recipient("ada@example.test", true)], false),
            None
        );
        assert_eq!(
            cannot_encrypt(
                &[
                    recipient("ada@example.test", true),
                    recipient("bo@example.test", false),
                ],
                false
            )
            .as_deref(),
            Some("gpg holds no key for bo@example.test.")
        );
    }

    #[test]
    fn a_blind_copy_and_encryption_do_not_go_together() {
        let problem = cannot_encrypt(&[recipient("ada@example.test", true)], true);
        assert!(
            problem
                .as_deref()
                .is_some_and(|problem| problem.contains("blind")),
            "{problem:?}"
        );
    }

    #[test]
    fn every_recipient_without_a_key_is_named() {
        let missing = cannot_encrypt(
            &[
                recipient("ann@example.test", false),
                recipient("bo@example.test", false),
                recipient("cy@example.test", false),
            ],
            false,
        );
        assert_eq!(
            missing.as_deref(),
            Some("gpg holds no key for ann@example.test, bo@example.test or cy@example.test.")
        );
    }

    #[test]
    fn preferences_say_which_of_your_own_addresses_gpg_has_a_key_for() {
        assert_eq!(
            own_keys(&[recipient("ada@example.test", false)]),
            "gpg holds no key for any of the addresses you send from."
        );
        assert_eq!(
            own_keys(&[
                recipient("ada@example.test", true),
                recipient("work@example.test", true),
            ]),
            "gpg holds a key for ada@example.test and work@example.test."
        );
        assert_eq!(
            own_keys(&[
                recipient("ada@example.test", true),
                recipient("work@example.test", false),
            ]),
            "gpg holds a key for ada@example.test, and none for work@example.test."
        );
    }

    #[test]
    fn the_version_is_the_last_word_of_the_first_line() {
        assert_eq!(
            version_of("gpg (GnuPG) 2.4.8\nlibgcrypt 1.12.0\n").as_deref(),
            Some("2.4.8")
        );
        assert_eq!(version_of("").as_deref(), None);
        assert_eq!(version_of("gpg: no such option").as_deref(), None);
    }

    #[test]
    fn a_message_signed_by_the_engine_reads_back_as_signed_here() {
        let Some(home) = Home::new() else { return };
        let part = b"Content-Type: text/plain; charset=utf-8\r\n\r\nMeet at six.\r\n";
        let entity = home.pgp.sign(part, &home.address).expect("a signed body");
        let raw = home.message(&entity);

        let read = read(&home.pgp, Opening::Verify, &raw, &MessageBody::default());

        assert_eq!(read.mark.title, "Signed by Ada Lovelace <ada@example.test>");
        assert_eq!(read.mark.tone, Tone::Good);
        assert!(read.body.is_none());
    }

    #[test]
    fn a_message_the_engine_encrypted_comes_back_readable() {
        let Some(home) = Home::new() else { return };
        let part = b"Content-Type: text/plain; charset=utf-8\r\n\r\nThe key is under the mat.\r\n";
        let entity = home
            .pgp
            .encrypt(
                part,
                std::slice::from_ref(&home.address),
                Some(&home.address),
            )
            .expect("an encrypted body");
        let raw = home.message(&entity);

        let read = read(&home.pgp, Opening::Decrypt, &raw, &MessageBody::default());

        assert_eq!(
            read.mark.title,
            "Encrypted, and signed by Ada Lovelace <ada@example.test>"
        );
        assert_eq!(read.mark.tone, Tone::Good);
        let inside = read.body.expect("the message that was inside");
        assert_eq!(
            inside.text.as_deref(),
            Some("The key is under the mat.\r\n")
        );
    }

    /// A draft addressed to the test key's own address, as the composer
    /// would hand it over.
    fn draft_to(home: &Home) -> crate::compose::Draft {
        let me = mailrs_domain::Address {
            name: Some("Ada Lovelace".into()),
            email: home.address.clone(),
        };
        let mut draft = crate::compose::Draft::new(1, me.clone());
        draft.to = vec![me];
        draft.subject = "Six".into();
        draft.markdown = "Meet at six.".into();
        draft
    }

    #[test]
    fn a_draft_signed_on_its_way_out_verifies_on_its_way_in() {
        let Some(home) = Home::new() else { return };
        let draft = draft_to(&home);
        let part = crate::compose::build_body_part(&draft).expect("a body part");
        let entity = home.pgp.sign(&part, &home.address).expect("a signed body");
        let raw =
            crate::compose::build_protected(&draft, 1_757_000_000, "<id@example.test>", entity)
                .expect("a message");

        let read = read(&home.pgp, Opening::Verify, &raw, &MessageBody::default());

        assert_eq!(read.mark.title, "Signed by Ada Lovelace <ada@example.test>");
        assert_eq!(read.mark.tone, Tone::Good, "{:?}", read.mark);
    }

    #[test]
    fn a_draft_encrypted_on_its_way_out_opens_on_its_way_in() {
        let Some(home) = Home::new() else { return };
        let draft = draft_to(&home);
        let part = crate::compose::build_body_part(&draft).expect("a body part");
        let entity = home
            .pgp
            .encrypt(
                &part,
                std::slice::from_ref(&home.address),
                Some(&home.address),
            )
            .expect("an encrypted body");
        let raw =
            crate::compose::build_protected(&draft, 1_757_000_000, "<id@example.test>", entity)
                .expect("a message");

        let read = read(&home.pgp, Opening::Decrypt, &raw, &MessageBody::default());

        assert_eq!(read.mark.tone, Tone::Good, "{:?}", read.mark);
        let inside = read.body.expect("the message that was inside");
        assert!(
            inside
                .text
                .as_deref()
                .is_some_and(|text| text.contains("Meet at six.")),
            "{inside:?}"
        );
    }

    #[test]
    fn armor_in_the_text_opens_with_its_signature() {
        let Some(home) = Home::new() else { return };
        // What a mail client that writes inline PGP sends: the armor in the
        // body, with its own words around it.
        let armor = String::from_utf8(
            home.pgp
                .encrypt(
                    b"Meet at six.\r\n",
                    std::slice::from_ref(&home.address),
                    Some(&home.address),
                )
                .expect("an encrypted body"),
        )
        .expect("armor is ascii");
        let block = armor
            .split_once("-----BEGIN PGP MESSAGE-----")
            .map(|(_, rest)| format!("-----BEGIN PGP MESSAGE-----{rest}"))
            .expect("armor in the entity");
        let arrived = body(&format!("Sent from my telephone\n{block}"));
        assert_eq!(opening(&arrived), Some(Opening::Inline));

        // Inline PGP is read out of the text, so nothing here needs the raw
        // message.
        let read = read(&home.pgp, Opening::Inline, &[], &arrived);

        assert_eq!(read.mark.tone, Tone::Good);
        let inside = read.body.expect("the text that was inside");
        assert_eq!(inside.text.as_deref(), Some("Meet at six.\r\n"));
    }
}
