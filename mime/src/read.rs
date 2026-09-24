//! A message as it arrived, in RFC 822 form, as parts. The part tree comes
//! from mail-parser; each part's bytes come from the raw message with the
//! transfer encoding undone here, and the charset stays for `body` to
//! read, since senders often mislabel it and mail-parser's own decoding
//! trusts the label.

use base64::Engine;
use base64::alphabet;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use mail_parser::decoders::quoted_printable::quoted_printable_decode;
use mail_parser::{Message, MessageParser, MessagePart, MimeHeaders};
use mailrs_domain::MessageBody;

use crate::parts::{Part, Parts, body};

const BASE64: GeneralPurpose = GeneralPurpose::new(
    &alphabet::STANDARD,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

/// The parts of `raw`, every one with its bytes. `None` for bytes
/// mail-parser cannot read at all.
pub fn parts(raw: &[u8]) -> Option<Parts> {
    let message = MessageParser::default().parse(raw)?;
    let headers = message
        .headers_raw()
        .map(|(name, value)| (name.to_string(), unfold(value)))
        .collect();
    // IMAP numbers the one body of a message that is not multipart "1",
    // and does not number a multipart root.
    let root_path = match message.root_part().is_multipart() {
        true => String::new(),
        false => "1".to_string(),
    };
    Some(Parts {
        headers,
        root: convert(&message, raw, 0, root_path),
    })
}

/// The body of `raw`. Bytes mail-parser cannot read at all give an empty
/// body.
pub fn read(raw: &[u8]) -> MessageBody {
    parts(raw).map(|p| body(&p)).unwrap_or_default()
}

/// The bytes of the part `path` names, with the transfer encoding undone
/// and the charset left alone, as a saved file holds them.
pub fn part(raw: &[u8], path: &str) -> Option<Vec<u8>> {
    parts(raw)?.find(path)?.data.clone()
}

/// The bytes of each file `read` lists, in the same order.
pub fn files(raw: &[u8]) -> Vec<Vec<u8>> {
    let Some(parts) = parts(raw) else {
        return Vec::new();
    };
    body(&parts)
        .attachments
        .iter()
        .map(|a| {
            parts
                .find(&a.part_id)
                .and_then(|p| p.data.clone())
                .unwrap_or_default()
        })
        .collect()
}

/// Part `id` and the parts under it. A nested `message/rfc822` is one
/// part, a file, as a server's structure lists it.
fn convert(message: &Message, raw: &[u8], id: u32, path: String) -> Part {
    let Some(part) = message.part(id) else {
        return Part::default();
    };
    let children = part
        .sub_parts()
        .unwrap_or_default()
        .iter()
        .enumerate()
        .map(|(i, child)| {
            let child_path = match path.as_str() {
                "" => (i + 1).to_string(),
                parent => format!("{parent}.{}", i + 1),
            };
            convert(message, raw, *child, child_path)
        })
        .collect();
    let data = (!part.is_multipart())
        .then(|| transfer_decoded(raw, part))
        .flatten();
    let attribute = |name: &str| {
        part.content_type()
            .and_then(|ct| ct.attribute(name))
            .map(str::to_string)
    };
    Part {
        path,
        mime_type: mime_type(part),
        charset: attribute("charset"),
        protocol: attribute("protocol"),
        smime_type: attribute("smime-type"),
        filename: part.attachment_name().map(str::to_string),
        content_id: part
            .content_id()
            .map(|v| v.trim().trim_start_matches('<').trim_end_matches('>').to_string()),
        attachment: part.content_disposition().is_some_and(|d| d.is_attachment()),
        size: data.as_ref().map_or(0, |d| d.len() as i64),
        data,
        children,
    }
}

/// A header's value with its folds undone.
fn unfold(value: &str) -> String {
    value.replace("\r\n", "").replace('\n', "").trim().to_string()
}

/// The part's media type in lower case, `text/plain` when it names none.
fn mime_type(part: &MessagePart) -> String {
    part.content_type()
        .map(|ct| match ct.subtype() {
            Some(sub) => format!("{}/{}", ct.ctype(), sub),
            None => ct.ctype().to_string(),
        })
        .unwrap_or_else(|| "text/plain".into())
        .to_ascii_lowercase()
}

/// A part's body with the transfer encoding undone, read from the raw
/// bytes. mail-parser's end offset stops before the line break that
/// belongs to the next boundary. `None` for base64 that does not decode.
///
/// The header, not `part.encoding`, says which transfer encoding to
/// undo: mail-parser rewrites `encoding` to `None` on base64 it could
/// not decode itself, and recovers the raw bytes as text, which would
/// otherwise show a corrupt attachment as garbled text instead of
/// leaving it unreadable.
fn transfer_decoded(raw: &[u8], part: &MessagePart) -> Option<Vec<u8>> {
    let start = part.raw_body_offset() as usize;
    let end = (part.raw_end_offset() as usize).min(raw.len());
    let bytes = raw.get(start..end)?;
    match part.content_transfer_encoding() {
        Some(cte) if cte.eq_ignore_ascii_case("base64") => {
            let clean: Vec<u8> = bytes.iter().copied().filter(|b| !b.is_ascii_whitespace()).collect();
            BASE64.decode(clean).ok()
        }
        Some(cte) if cte.eq_ignore_ascii_case("quoted-printable") => {
            Some(quoted_printable_decode(bytes).unwrap_or_else(|| bytes.to_vec()))
        }
        _ => Some(bytes.to_vec()),
    }
}
