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
//!
//! A readable draft comes back through the same headers and the same
//! attachment list, read from the message Gmail holds, so [`reopened`]
//! opens either kind.

use mail_parser::MessageParser;
use mailrs_domain::translate::{fill, gettext, with_reason};
use mailrs_domain::{Address, MessageBody};
use mailrs_pgp::{Pgp, PgpError, Readers};
use mailrs_smime::{Smime, SmimeError};
use mailrs_sync::{SavedDraft, now_millis};

use super::{Read, Standard, find, param, unfolded};
use crate::compose::{self, Draft, OutgoingAttachment};
use crate::core::Core;

/// The header that marks a draft Penguin Mail saved encrypted.
pub const HEADER: &str = "X-Penguin-Mail-Draft";

/// What [`HEADER`] says: that the message goes out encrypted, and signed
/// as well when the writer asked for it.
const ENCRYPT: &str = "encrypt";
const ENCRYPT_AND_SIGN: &str = "encrypt; sign";

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

/// Saves `draft` into Gmail's Drafts and gives back where Gmail keeps it.
/// With `secret`, the body goes in encrypted to the writer, trying
/// `standard` first, as [`sealed`] says; otherwise it goes in readable.
/// The composer's Save Draft and the assistant's edit of a draft both come
/// through here. The error is what to tell the writer.
pub async fn save(
    core: &Core,
    draft: &Draft,
    secret: bool,
    standard: Standard,
) -> Result<SavedDraft, String> {
    let Some(account) = core.account(draft.account_id) else {
        return Err(gettext("That account is not connected."));
    };
    let (date, message_id) = (now_millis() / 1000, compose::new_message_id(&draft.from.email));
    let raw = match secret {
        true => {
            let mut kept = draft.clone();
            kept.encrypt = true;
            sealed(core, &kept, standard, date, &message_id).await?
        }
        false => compose::build_mime(draft, date, &message_id)
            .map_err(|err| fill(&gettext("Could not save: {reason}"), &[("reason", &err)]))?,
    };
    let (thread, draft_id) = (draft.thread_id.clone(), draft.draft_id.clone());
    let saved = core
        .call(async move { account.save_draft(raw, thread, draft_id).await })
        .await
        .map_err(|err| with_reason(&gettext("Draft not saved: {reason}"), &err, &[]))?;
    // A Send Later message waiting on this draft now names its new message.
    let (outbox, account_id, kept) = (core.outbox(), draft.account_id, saved.clone());
    core.spawn(async move {
        if let Err(err) = outbox.draft_saved(account_id, kept).await {
            tracing::warn!(error = %err, "could not update the store");
        }
    });
    core.poke(draft.account_id);
    Ok(saved)
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

/// Opens the draft `raw`, encrypted under `standard`, and fills `draft`
/// from it. The engine may ask for a passphrase, as it does for any
/// encrypted message. The error is what to tell the writer when it would
/// not open.
async fn opened(
    core: &Core,
    raw: Vec<u8>,
    standard: Standard,
    draft: &mut Draft,
) -> Result<(), String> {
    let unopened = |reason: &str| {
        fill(
            &gettext("Could not open the draft: {reason}"),
            &[("reason", reason)],
        )
    };
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
    take_saved(raw, &body, read.files, draft);
    draft.encrypt = true;
    draft.standard = standard;
    draft.sign = unfolded(headers_of(raw), HEADER)
        .is_some_and(|mark| mark.split(';').any(|word| word.trim() == "sign"));
    Ok(())
}

/// Fills `draft` from the readable draft `raw`. The message as Gmail holds
/// it is the only place the Bcc, the reply headers and the bytes of the
/// files survive; the metadata the list keeps has none of them.
pub fn reopen_plain(raw: &[u8], draft: &mut Draft) {
    let (body, files) = super::opened_body(raw);
    take_saved(raw, &body, files, draft);
}

/// Opens the draft `raw` whichever way Gmail holds it, and fills `draft`
/// from it. The error is what to tell the writer when it would not open.
pub async fn reopened(core: &Core, raw: Vec<u8>, draft: &mut Draft) -> Result<(), String> {
    match standard_of(&raw) {
        Some(standard) => opened(core, raw, standard, draft).await,
        None => {
            reopen_plain(&raw, draft);
            Ok(())
        }
    }
}

/// What both kinds of draft share: the people, subject and reply headers
/// from the top of `raw`, and the words and files from `body` and `files`,
/// which list the attachments in the same order.
fn take_saved(raw: &[u8], body: &MessageBody, files: Vec<Vec<u8>>, draft: &mut Draft) {
    let headers = headers_of(raw);
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
    draft.take_body(body);
    draft.attachments = body
        .attachments
        .iter()
        .zip(files)
        .map(|(found, data)| OutgoingAttachment {
            filename: found.filename.clone(),
            mime_type: found.mime_type.clone(),
            data,
            content_id: found.content_id.clone(),
        })
        .collect();
}

fn headers_of(raw: &[u8]) -> &[u8] {
    let blank = find(raw, b"\r\n\r\n").unwrap_or(raw.len());
    &raw[..blank]
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

#[cfg(test)]
mod tests {
    use super::*;

    fn address(name: Option<&str>, email: &str) -> Address {
        Address {
            name: name.map(str::to_string),
            email: email.to_string(),
        }
    }

    #[test]
    fn a_plain_draft_reopens_with_its_blind_copy_its_thread_and_its_files() {
        let mut written = Draft::new(1, address(Some("Ann"), "ann@example.test"));
        written.to = vec![address(Some("Bo Peep"), "bo@example.test")];
        written.cc = vec![address(None, "cy@example.test")];
        written.bcc = vec![address(None, "di@example.test")];
        written.subject = "Six".into();
        written.markdown = "Meet at six.".into();
        written.in_reply_to = Some("<parent@example.test>".into());
        written.references = vec!["<root@example.test>".into(), "<parent@example.test>".into()];
        written.attachments = vec![OutgoingAttachment {
            filename: "plan.txt".into(),
            mime_type: "text/plain".into(),
            data: b"Under the mat.".to_vec(),
            content_id: None,
        }];
        let raw =
            compose::build_mime(&written, 1_757_000_000, "<id@example.test>").expect("a draft");
        assert_eq!(standard_of(&raw), None, "Gmail holds it readable");

        let mut reopened = Draft::new(1, written.from.clone());
        reopen_plain(&raw, &mut reopened);

        assert_eq!(reopened.to, written.to);
        assert_eq!(reopened.cc, written.cc);
        assert_eq!(reopened.bcc, written.bcc);
        assert_eq!(reopened.subject, "Six");
        assert_eq!(reopened.markdown.trim(), "Meet at six.");
        assert_eq!(reopened.in_reply_to, written.in_reply_to);
        assert_eq!(reopened.references, written.references);
        assert_eq!(reopened.attachments, written.attachments);
        assert!(!reopened.encrypt);
    }
}
