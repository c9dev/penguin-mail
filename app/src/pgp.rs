//! OpenPGP in the app: which call a message needs, what comes back when
//! gpg has run, and which recipients stand between a draft and encryption.
//!
//! It is one of the two adapters over `protection`, `smime` being the
//! other. It turns what gpg said into a [`Found`] in the words both
//! standards share, and `protection::read` alone turns that into the card
//! and the body, so a reader never has to know which standard a message
//! arrived under. `mailrs_pgp` runs gpg and `ui::pgp` draws the answer. Every call below blocks, so the
//! window hands them to `Core::gpg` rather than running them itself.

use std::process::Command;

use mailrs_domain::translate::{fill, gettext};
use mailrs_domain::{MessageBody, Protection};
use mailrs_gmail::body::decode_charset;
use mailrs_pgp::{Pgp, PgpError, Recipient, Signature, Trust, Verdict, inline};

use crate::protection::{self, Found, Part, Read, Refusal, Signed, Signer, Standard, Vouched};

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

/// Which call `body` needs before it is drawn, in the order RFC 3156 reads
/// a message: the wrapper first, and the text only when there is none.
pub fn opening(body: &MessageBody) -> Option<Opening> {
    match body.protection {
        Some(Protection::Signed) => Some(Opening::Verify),
        Some(Protection::Encrypted) => Some(Opening::Decrypt),
        // S/MIME is the other engine's work, and `protection::engine` is
        // where a message goes to find out which of the two it needs.
        Some(_) => None,
        None => inline::armor(body.text.as_deref()?).map(|_| Opening::Inline),
    }
}

/// Runs the call `opening` asks for and says what the window should show.
///
/// `raw` is the message as it arrived, from `format=raw`, which is the one
/// copy whose bytes a signature still covers. Every branch has an answer,
/// including the ones where gpg refused, so the card never goes blank.
pub fn read(pgp: &Pgp, opening: Opening, raw: &[u8], body: &MessageBody) -> Read {
    protection::read(Standard::Pgp, open(pgp, opening, raw, body), body)
}

/// What gpg found in the message, before anything is worded.
pub fn open(pgp: &Pgp, opening: Opening, raw: &[u8], body: &MessageBody) -> Result<Found, Refusal> {
    match opening {
        Opening::Verify => {
            let (part, signature) = protection::wrapper_parts(raw).ok_or(Refusal::Unreadable)?;
            let found = pgp.verify(part, signature).map_err(refusal)?;
            Ok(Found {
                encrypted: false,
                signature: Some(signed(&found)),
                part: Part::Entity(part.to_vec()),
            })
        }
        Opening::Decrypt => {
            let (_, ciphertext) = protection::wrapper_parts(raw).ok_or(Refusal::Unreadable)?;
            let opened = pgp.decrypt(ciphertext).map_err(refusal)?;
            Ok(Found {
                encrypted: true,
                signature: opened.signature.as_ref().map(signed),
                part: Part::Entity(opened.part),
            })
        }
        Opening::Inline => {
            let text = body.text.as_deref().unwrap_or_default();
            let opened = pgp.open_inline(text).map_err(refusal)?;
            Ok(Found {
                // Clearsigned text carries a signature and was never
                // encrypted; armor that opened without one was.
                encrypted: !matches!(inline::armor(text), Some(inline::Armor::Clearsigned)),
                signature: opened.signature.as_ref().map(signed),
                // The armor said nothing about a character set, so these
                // bytes are read the way a body with no charset is.
                part: Part::Text(decode_charset(&opened.text, None)),
            })
        }
    }
}

/// What gpg says it is, for Preferences. The version is the last word of
/// its first line, as in `gpg (GnuPG) 2.4.8`.
pub fn version(pgp: &Pgp) -> Option<String> {
    let run = Command::new(pgp.program()).arg("--version").output().ok()?;
    protection::version_of(&String::from_utf8_lossy(&run.stdout))
}

/// Why this draft cannot be encrypted under OpenPGP, for the Encrypt
/// button to say. `None` means every recipient has a key, a blind copy's
/// included: gpg leaves that one's key id out of the message.
pub fn cannot_encrypt(held: &[Recipient]) -> Option<String> {
    if held.is_empty() {
        return Some(gettext("Add a recipient whose key gpg holds."));
    }
    let missing: Vec<&str> = held
        .iter()
        .filter(|recipient| recipient.key.is_none())
        .map(|recipient| recipient.address.as_str())
        .collect();
    (!missing.is_empty()).then(|| {
        fill(
            &gettext("gpg holds no key for {addresses}."),
            &[("addresses", &protection::listed(&missing))],
        )
    })
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
        ([], _) => gettext("gpg holds no key for any of the addresses you send from."),
        (mine, []) => fill(
            &gettext("gpg holds a key for {addresses}."),
            &[("addresses", &protection::joined(mine))],
        ),
        (mine, missing) => fill(
            &gettext("gpg holds a key for {addresses}, and none for {without}."),
            &[
                ("addresses", &protection::joined(mine)),
                ("without", &protection::joined(missing)),
            ],
        ),
    }
}

/// gpg's answer about a signature, in the words both standards share.
fn signed(signature: &Signature) -> Signed {
    Signed {
        verdict: match signature.verdict {
            Verdict::Good => protection::Verdict::Good,
            Verdict::Bad => protection::Verdict::Bad,
            Verdict::ExpiredKey => protection::Verdict::KeyExpired,
            Verdict::RevokedKey => protection::Verdict::KeyRevoked,
            Verdict::Expired => protection::Verdict::SignatureExpired,
            Verdict::NoKey => protection::Verdict::NoKey,
            Verdict::Unchecked => protection::Verdict::Unchecked,
        },
        signer: signer(signature),
        vouched: match signature.trust {
            Trust::Ultimate => Vouched::Own,
            Trust::Full => Vouched::Yes,
            Trust::Marginal => Vouched::Partly,
            Trust::Unknown => Vouched::Nobody,
            Trust::Never => Vouched::Never,
        },
    }
}

/// The name and address out of the user id gpg reports, which reads
/// `Ada Lovelace <ada@example.com>`, a bare address, or a bare name.
fn signer(signature: &Signature) -> Signer {
    let key = signature.key_id.clone();
    let Some(uid) = signature.signer.as_deref().map(str::trim) else {
        return Signer {
            key,
            ..Signer::default()
        };
    };
    if let Some((name, rest)) = uid.rsplit_once('<')
        && let Some(address) = rest.strip_suffix('>')
    {
        let name = name.trim();
        return Signer {
            name: (!name.is_empty()).then(|| name.to_string()),
            addresses: vec![address.trim().to_string()],
            key,
        };
    }
    match uid.contains('@') {
        true => Signer {
            addresses: vec![uid.to_string()],
            key,
            ..Signer::default()
        },
        false => Signer {
            name: Some(uid.to_string()),
            key,
            ..Signer::default()
        },
    }
}

/// Why gpg would not answer, in the words both standards share.
fn refusal(err: PgpError) -> Refusal {
    match err {
        PgpError::NotForYou => Refusal::NotForYou,
        PgpError::NotPgp => Refusal::Unreadable,
        other => Refusal::Failed(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::process::Stdio;

    use mailrs_pgp::Key;

    use super::*;
    use crate::protection::tampered::{as_gmail_read_it, with_unsigned_part};
    use crate::protection::{Mark, Tone};

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
                require_crypto();
                return None;
            };
            let dir = tempfile::tempdir().expect("a temp directory");
            permit_owner_only(dir.path());
            // gpg-agent asks the person things through a pinentry window,
            // and a test must never put one on somebody's screen. A
            // pinentry that cannot run is a pinentry that cannot
            // interrupt: the agent gets an error instead.
            std::fs::write(
                dir.path().join("gpg-agent.conf"),
                "pinentry-program /bin/false\n",
            )
            .expect("write");
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

        /// `text` with the home's signature under it, as a mail client
        /// that writes inline PGP sends it.
        fn clearsign(&self, text: &str) -> String {
            use std::io::Write;
            let mut child = Command::new(self.pgp.program())
                .args(["--batch", "--no-tty", "--homedir"])
                .arg(self.dir.path())
                .args(["--clearsign", "--local-user", &self.address])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .expect("gpg runs");
            child
                .stdin
                .take()
                .expect("stdin")
                .write_all(text.as_bytes())
                .expect("write");
            let out = child.wait_with_output().expect("gpg runs");
            assert!(out.status.success(), "gpg could not clearsign");
            String::from_utf8(out.stdout).expect("armor is text")
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

    /// The card for a message that arrived in the clear with `signature`
    /// over it, through the one function both standards answer through.
    fn mark(signature: &Signature) -> Mark {
        protection::read(
            Standard::Pgp,
            Ok(Found {
                encrypted: false,
                signature: Some(signed(signature)),
                part: Part::Text("Meet at six.".into()),
            }),
            &MessageBody::default(),
        )
        .mark
    }

    /// The card for a message that arrived encrypted, with `signature`
    /// inside it when it carried one.
    fn sealed(signature: Option<&Signature>) -> Mark {
        protection::read(
            Standard::Pgp,
            Ok(Found {
                encrypted: true,
                signature: signature.map(signed),
                part: Part::Text("Meet at six.".into()),
            }),
            &MessageBody::default(),
        )
        .mark
    }

    #[test]
    fn a_good_signature_names_the_signer_and_how_far_the_key_is_trusted() {
        let mark = mark(&signature(Verdict::Good, Trust::Unknown));
        assert_eq!(mark.title, "Signed by Ada Lovelace <ada@example.test>");
        assert_eq!(
            mark.detail.as_deref(),
            Some("Nobody has vouched for this key, so it names no one.")
        );
        assert_eq!(mark.tone, Tone::Good);

        let vouched = self::mark(&signature(Verdict::Good, Trust::Full));
        assert_eq!(
            vouched.detail.as_deref(),
            Some("You have vouched for this key.")
        );
        assert_eq!(vouched.tone, Tone::Good);
    }

    #[test]
    fn a_bad_signature_says_so_plainly() {
        let mark = mark(&signature(Verdict::Bad, Trust::Full));
        assert_eq!(mark.title, "This message changed after it was signed");
        assert_eq!(mark.tone, Tone::Bad);
    }

    #[test]
    fn a_key_we_do_not_hold_is_its_own_answer() {
        let unknown = Signature {
            signer: None,
            ..signature(Verdict::NoKey, Trust::Unknown)
        };
        let mark = mark(&unknown);
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
        let alone = sealed(None);
        assert_eq!(alone.title, "This message arrived encrypted");
        assert_eq!(alone.tone, Tone::Unchecked);

        let inside = sealed(Some(&signature(Verdict::Good, Trust::Ultimate)));
        assert_eq!(
            inside.title,
            "Encrypted, and signed by Ada Lovelace <ada@example.test>"
        );
        assert_eq!(inside.tone, Tone::Good);

        let broken = sealed(Some(&signature(Verdict::Bad, Trust::Full)));
        assert_eq!(
            broken.title,
            "Encrypted. This message changed after it was signed"
        );
        assert_eq!(broken.tone, Tone::Bad);
    }

    #[test]
    fn a_user_id_gives_the_signer_a_name_and_an_address() {
        let named = signer(&signature(Verdict::Good, Trust::Full));
        assert_eq!(named.name.as_deref(), Some("Ada Lovelace"));
        assert_eq!(named.addresses, ["ada@example.test"]);
        let bare = signer(&Signature {
            signer: Some("ada@example.test".into()),
            ..signature(Verdict::Good, Trust::Full)
        });
        assert_eq!(bare.name, None);
        assert_eq!(bare.addresses, ["ada@example.test"]);
    }

    #[test]
    fn a_message_for_somebody_else_says_so_where_the_message_would_be() {
        let mark = protection::read(
            Standard::Pgp,
            Err(refusal(PgpError::NotForYou)),
            &MessageBody::default(),
        )
        .mark;
        assert_eq!(
            mark.title,
            "This message is encrypted to a key you do not hold"
        );
        assert_eq!(mark.tone, Tone::Unchecked);
    }

    #[test]
    fn encryption_waits_until_every_recipient_has_a_key() {
        assert_eq!(
            cannot_encrypt(&[]).as_deref(),
            Some("Add a recipient whose key gpg holds.")
        );
        assert_eq!(cannot_encrypt(&[recipient("ada@example.test", true)]), None);
        assert_eq!(
            cannot_encrypt(&[
                recipient("ada@example.test", true),
                recipient("bo@example.test", false),
            ])
            .as_deref(),
            Some("gpg holds no key for bo@example.test.")
        );
    }

    #[test]
    fn every_recipient_without_a_key_is_named() {
        let missing = cannot_encrypt(&[
            recipient("ann@example.test", false),
            recipient("bo@example.test", false),
            recipient("cy@example.test", false),
        ]);
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
    fn a_message_signed_by_the_engine_reads_back_as_signed_here() {
        let Some(home) = Home::new() else { return };
        let part = b"Content-Type: text/plain; charset=utf-8\r\n\r\nMeet at six.\r\n";
        let entity = home.pgp.sign(part, &home.address).expect("a signed body");
        let raw = home.message(&entity);

        let read = read(&home.pgp, Opening::Verify, &raw, &MessageBody::default());

        assert_eq!(read.mark.title, "Signed by Ada Lovelace <ada@example.test>");
        assert_eq!(read.mark.tone, Tone::Good);
        let shown = read.body.expect("the body is cut from what was signed");
        assert_eq!(shown.text.as_deref(), Some("Meet at six."));
    }

    #[test]
    fn a_part_nobody_signed_is_not_drawn_under_the_card() {
        let Some(home) = Home::new() else { return };
        let part = b"Content-Type: text/plain; charset=utf-8\r\n\r\nMeet at six.\r\n";
        let entity = home.pgp.sign(part, &home.address).expect("a signed body");
        let raw = home.message(&with_unsigned_part(&entity));

        let read = read(&home.pgp, Opening::Verify, &raw, &as_gmail_read_it());

        assert_eq!(read.mark.title, "Signed by Ada Lovelace <ada@example.test>");
        let shown = read.body.expect("the body is cut from what was signed");
        assert_eq!(shown.html, None, "{shown:?}");
        assert!(shown.attachments.is_empty(), "{shown:?}");
        assert!(
            shown
                .text
                .as_deref()
                .is_some_and(|text| text.starts_with("Meet at six.")),
            "{shown:?}"
        );
    }

    #[test]
    fn inline_armor_keeps_nothing_that_was_around_it() {
        let Some(home) = Home::new() else { return };
        let signed = home.clearsign("Meet at six.\n");
        let arrived = MessageBody {
            text: Some(format!("Mallory wrote this line.\n{signed}")),
            ..as_gmail_read_it()
        };

        let read = read(&home.pgp, Opening::Inline, &[], &arrived);

        let shown = read.body.expect("the text that was signed");
        assert_eq!(shown.html, None, "{shown:?}");
        assert!(shown.attachments.is_empty(), "{shown:?}");
        assert!(
            shown
                .text
                .as_deref()
                .is_some_and(|text| !text.contains("Mallory")),
            "{shown:?}"
        );
    }

    #[test]
    fn a_message_the_engine_encrypted_comes_back_readable() {
        let Some(home) = Home::new() else { return };
        let part = b"Content-Type: text/plain; charset=utf-8\r\n\r\nThe key is under the mat.\r\n";
        let entity = home
            .pgp
            .encrypt(
                part,
                &mailrs_pgp::Readers::named([home.address.clone()]),
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
                &mailrs_pgp::Readers::named([home.address.clone()]),
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
    fn an_encrypted_draft_waits_in_gmail_unreadable_and_reopens_as_written() {
        use crate::compose::OutgoingAttachment;
        use crate::protection::{Standard, draft};

        let Some(home) = Home::new() else { return };
        let mut written = draft_to(&home);
        written.cc = vec![mailrs_domain::Address {
            name: Some("Bo Peep".into()),
            email: "bo@example.test".into(),
        }];
        written.bcc = vec![mailrs_domain::Address {
            name: None,
            email: "cy@example.test".into(),
        }];
        written.in_reply_to = Some("<parent@example.test>".into());
        written.references = vec!["<root@example.test>".into(), "<parent@example.test>".into()];
        written.sign = true;
        written.encrypt = true;
        written.attachments = vec![OutgoingAttachment {
            filename: "plan.txt".into(),
            mime_type: "text/plain".into(),
            data: b"Under the mat.".to_vec(),
            content_id: None,
        }];
        let part = crate::compose::build_body_part(&written).expect("a body part");
        let entity = draft::for_writer_pgp(&home.pgp, &part, &home.address)
            .expect("gpg answers")
            .expect("gpg holds the writer's own key");
        let raw =
            draft::build(&written, 1_757_000_000, "<id@example.test>", entity).expect("a draft");

        // What Gmail holds: the headers, and none of the words or files.
        let held = String::from_utf8_lossy(&raw);
        assert!(!held.contains("Meet at six"), "{held}");
        assert!(!held.contains("plan.txt"), "{held}");
        assert!(
            held.contains("X-Penguin-Mail-Draft: encrypt; sign\r\n"),
            "{held}"
        );
        assert_eq!(draft::standard_of(&raw), Some(Standard::Pgp));

        let read = read(&home.pgp, Opening::Decrypt, &raw, &MessageBody::default());
        let mut reopened = crate::compose::Draft::new(1, written.from.clone());
        draft::reopen(&raw, Standard::Pgp, read, &mut reopened).expect("it opens");

        assert_eq!(reopened.to, written.to);
        assert_eq!(reopened.cc, written.cc);
        assert_eq!(
            reopened.bcc, written.bcc,
            "the blind copy survives the trip"
        );
        assert_eq!(reopened.subject, "Six");
        assert_eq!(reopened.markdown.trim(), "Meet at six.");
        assert_eq!(reopened.in_reply_to, written.in_reply_to);
        assert_eq!(reopened.references, written.references);
        assert_eq!(reopened.attachments, written.attachments);
        assert!(reopened.encrypt && reopened.sign);
        assert_eq!(reopened.standard, Standard::Pgp);

        // Sending it goes through the same engine as any other message.
        let part = crate::compose::build_body_part(&reopened).expect("a body part");
        let readers = crate::protection::Addressees::of(&reopened).readers(true);
        assert_eq!(readers.hidden, vec!["cy@example.test".to_string()]);
        home.pgp
            .encrypt(
                &part,
                &mailrs_pgp::Readers::named([home.address.clone()]),
                Some(&home.address),
            )
            .expect("the reopened draft encrypts again");
    }

    #[test]
    fn a_writer_with_no_key_of_their_own_gets_no_encrypted_draft() {
        let Some(home) = Home::new() else { return };
        let sealed = crate::protection::draft::for_writer_pgp(
            &home.pgp,
            b"Content-Type: text/plain\r\n\r\nHi\r\n",
            "nobody@example.test",
        )
        .expect("gpg answers");
        assert!(sealed.is_none());
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
                    &mailrs_pgp::Readers::named([home.address.clone()]),
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
