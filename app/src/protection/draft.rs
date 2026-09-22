//! Encrypted drafts: how a message meant to go out encrypted waits in
//! Gmail's Drafts, and how the composer gets it back.
//!
//! Gmail keeps drafts on its servers, so a draft saved as written would
//! sit there readable until it went out. Instead the body goes in
//! encrypted to the writer's own key or certificate and nobody else's,
//! with the headers left readable, as they are on the message that is
//! finally sent. The recipients' keys play no part: the writer may change
//! the recipients before sending, and the send encrypts to them anyway.
//! One more header, [`HEADER`], tells the composer that reopens the draft
//! to switch Encrypt, and Sign when it was on, back on.
//!
//! Encrypting to one's own key needs no passphrase, so saving never puts
//! a pinentry on the screen. Opening the draft again does, the way opening
//! any encrypted message does.

use mail_parser::MessageParser;
use mailrs_domain::translate::{fill, gettext};
use mailrs_domain::{Address, MessageBody, Protection};
use mailrs_pgp::{Pgp, PgpError, Readers};
use mailrs_smime::{Smime, SmimeError};

use super::{Read, Standard, find, param, unfolded};
use crate::compose::{self, Draft, OutgoingAttachment};
use crate::core::Core;

/// The header that marks a draft Penguin Mail saved encrypted.
pub const HEADER: &str = "X-Penguin-Mail-Draft";

/// What [`HEADER`] says: that the message goes out encrypted, and signed
/// as well when the writer asked for it.
const ENCRYPT: &str = "encrypt";
const ENCRYPT_AND_SIGN: &str = "encrypt; sign";

/// Whether a draft Gmail holds arrived encrypted, so that the composer has
/// to open it through an engine before anyone can edit it.
pub fn is_encrypted(body: &MessageBody) -> bool {
    matches!(
        body.protection,
        Some(Protection::Encrypted | Protection::SmimeEnveloped)
    )
}

/// `part` encrypted to `from` and nobody else. `None` when gpg holds no
/// key for that address.
pub fn for_writer_pgp(pgp: &Pgp, part: &[u8], from: &str) -> Result<Option<Vec<u8>>, PgpError> {
    let own = pgp
        .keys_for(&[from.to_string()])?
        .iter()
        .any(|held| held.key.is_some());
    if !own {
        return Ok(None);
    }
    pgp.encrypt(part, &Readers::named([from]), None).map(Some)
}

/// `part` enveloped for `from` and nobody else. `None` when gpgsm holds no
/// certificate for that address.
pub fn for_writer_smime(
    smime: &Smime,
    part: &[u8],
    from: &str,
) -> Result<Option<Vec<u8>>, SmimeError> {
    let own = smime
        .certificates_for(&[from.to_string()])?
        .iter()
        .any(|held| held.certificate.is_some());
    if !own {
        return Ok(None);
    }
    smime.encrypt(part, &[from.to_string()], None).map(Some)
}

/// The draft for Gmail to keep: the message `draft` describes, `entity`
/// as its body, and [`HEADER`] on top.
pub fn build(
    draft: &Draft,
    date_secs: i64,
    message_id: &str,
    entity: Vec<u8>,
) -> Result<Vec<u8>, String> {
    let mark = match draft.sign {
        true => ENCRYPT_AND_SIGN,
        false => ENCRYPT,
    };
    compose::build_protected_draft(draft, date_secs, message_id, entity, (HEADER, mark))
}

/// What Gmail keeps for `draft` while the writer means it to go out
/// encrypted. `preferred` is the standard to try first; the other one gets
/// a turn when the first holds nothing of the writer's own. The answer is
/// an error to show, in the writer's language, when neither can do it.
pub async fn sealed(
    core: &Core,
    draft: &Draft,
    preferred: Standard,
    date_secs: i64,
    message_id: &str,
) -> Result<Vec<u8>, String> {
    let failed = |reason: &str| fill(&gettext("Draft not saved: {reason}"), &[("reason", reason)]);
    let part = compose::build_body_part(draft).map_err(|err| failed(&err))?;
    let order = match preferred {
        Standard::Pgp => [Standard::Pgp, Standard::Smime],
        Standard::Smime => [Standard::Smime, Standard::Pgp],
    };
    for standard in order {
        let (part, from) = (part.clone(), draft.from.email.clone());
        let entity = match standard {
            Standard::Pgp if core.has_gpg() => {
                core.gpg(move |pgp| for_writer_pgp(pgp, &part, &from)).await
            }
            Standard::Smime if core.has_gpgsm() => {
                core.gpgsm(move |smime| for_writer_smime(smime, &part, &from))
                    .await
            }
            _ => continue,
        };
        match entity {
            Ok(Some(entity)) => {
                return build(draft, date_secs, message_id, entity).map_err(|err| failed(&err));
            }
            Ok(None) => continue,
            Err(err) => return Err(failed(&err.to_string())),
        }
    }
    Err(fill(
        &gettext(
            "Draft not saved. An encrypted message waits in Drafts encrypted to your own key, and \
             this computer holds no key or certificate for {address}.",
        ),
        &[("address", &draft.from.email)],
    ))
}

/// Which engine opens `raw`, going by the wrapper around its body.
pub fn standard_of(raw: &[u8]) -> Option<Standard> {
    let blank = find(raw, b"\r\n\r\n")?;
    let content_type = unfolded(&raw[..blank], "content-type")?;
    let media = content_type.split(';').next()?.trim().to_ascii_lowercase();
    match media.as_str() {
        "multipart/encrypted" => Some(Standard::Pgp),
        "application/pkcs7-mime" | "application/x-pkcs7-mime" => param(&content_type, "smime-type")
            .filter(|kind| kind.eq_ignore_ascii_case("enveloped-data"))
            .map(|_| Standard::Smime),
        _ => None,
    }
}

/// Opens the encrypted draft `raw` and fills `draft` from it. The engine
/// may ask for a passphrase, as it does for any encrypted message. The
/// error is what to tell the writer when it would not open.
pub async fn opened(core: &Core, raw: Vec<u8>, draft: &mut Draft) -> Result<(), String> {
    let unopened = |reason: &str| {
        fill(
            &gettext("Could not open the draft: {reason}"),
            &[("reason", reason)],
        )
    };
    let standard = standard_of(&raw).ok_or_else(|| {
        unopened(&gettext(
            "It is encrypted in a way Penguin Mail does not read.",
        ))
    })?;
    let ciphertext = raw.clone();
    let read = match standard {
        Standard::Pgp => {
            core.gpg(move |pgp| {
                Ok::<_, PgpError>(crate::pgp::read(
                    pgp,
                    crate::pgp::Opening::Decrypt,
                    &ciphertext,
                    &MessageBody::default(),
                ))
            })
            .await
        }
        Standard::Smime => {
            core.gpgsm(move |smime| {
                Ok::<_, SmimeError>(crate::smime::read(
                    smime,
                    crate::smime::Opening::Decrypt,
                    &ciphertext,
                ))
            })
            .await
        }
    }
    .map_err(|err| unopened(&err.to_string()))?;
    reopen(&raw, standard, read, draft).map_err(|reason| unopened(&reason))
}

/// Fills `draft` from the encrypted draft `raw` and what `standard`'s
/// engine found inside it: the people and subject from the headers, which
/// were never encrypted, and the words and files from inside.
///
/// A draft without [`HEADER`] still reopens encrypted, such as a Send Later
/// message whose Gmail draft holds the bytes that go out. Nothing says
/// whether that one was signed, so Sign is left to the composer's default.
pub fn reopen(raw: &[u8], standard: Standard, read: Read, draft: &mut Draft) -> Result<(), String> {
    let Some(body) = read.body else {
        return Err(read.mark.title);
    };
    let blank = find(raw, b"\r\n\r\n").unwrap_or(raw.len());
    let headers = &raw[..blank];
    if let Some(parsed) = MessageParser::default().parse_headers(raw) {
        draft.to = addresses(parsed.to());
        draft.cc = addresses(parsed.cc());
        draft.bcc = addresses(parsed.bcc());
        draft.subject = parsed.subject().unwrap_or_default().to_string();
    }
    draft.in_reply_to = unfolded(headers, "in-reply-to");
    draft.references = unfolded(headers, "references")
        .map(|ids| ids.split_whitespace().map(str::to_string).collect())
        .unwrap_or_default();
    draft.take_body(&body);
    draft.attachments = body
        .attachments
        .iter()
        .zip(read.files)
        .map(|(found, data)| OutgoingAttachment {
            filename: found.filename.clone(),
            mime_type: found.mime_type.clone(),
            data,
            content_id: found.content_id.clone(),
        })
        .collect();
    draft.encrypt = true;
    draft.standard = standard;
    draft.sign = unfolded(headers, HEADER)
        .is_some_and(|mark| mark.split(';').any(|word| word.trim() == "sign"));
    Ok(())
}

fn addresses(list: Option<&mail_parser::Address>) -> Vec<Address> {
    list.map(|list| {
        list.iter()
            .filter_map(|found| {
                Some(Address {
                    name: found.name().map(str::to_string),
                    email: found.address()?.to_string(),
                })
            })
            .collect()
    })
    .unwrap_or_default()
}
