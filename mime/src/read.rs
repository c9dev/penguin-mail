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
    GeneralPurposeConfig::new()
        .with_decode_padding_mode(DecodePaddingMode::Indifferent)
        .with_decode_allow_trailing_bits(true),
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

/// Part `id` and the parts under it.
fn convert(message: &Message, raw: &[u8], id: u32, path: String) -> Part {
    let Some(part) = message.part(id) else {
        return Part::default();
    };
    if let Some(nested) = part.message() {
        return nested_message(part, nested, raw, path);
    }
    let children = part
        .sub_parts()
        .unwrap_or_default()
        .iter()
        .enumerate()
        .map(|(i, child)| convert(message, raw, *child, numbered(&path, i)))
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

/// IMAP's number for child `i` (zero-based) under `parent`: bare under an
/// unnumbered multipart root, dotted under anything else.
fn numbered(parent: &str, i: usize) -> String {
    match parent {
        "" => (i + 1).to_string(),
        parent => format!("{parent}.{}", i + 1),
    }
}

/// A `message/rfc822` part, walked into for its own files rather than
/// listed as one, the way Gmail expands a forwarded message. IMAP
/// addresses what is inside it starting one level under this part's own
/// number: "4.1" for a single-part encapsulated message, "4.1", "4.2"
/// for a multipart one; "4" itself stays the whole encapsulated message,
/// which nothing here lists as a file.
fn nested_message(part: &MessagePart, nested: &Message, raw: &[u8], path: String) -> Part {
    let root = nested.root_part();
    let children = if root.is_multipart() {
        root.sub_parts()
            .unwrap_or_default()
            .iter()
            .enumerate()
            .map(|(i, child)| convert(nested, raw, *child, numbered(&path, i)))
            .collect()
    } else {
        vec![convert(nested, raw, 0, format!("{path}.1"))]
    };
    Part {
        path,
        mime_type: mime_type(part),
        children,
        ..Part::default()
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
/// belongs to the next boundary. `None` for base64 that does not decode
/// at all.
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
        Some(cte) if cte.eq_ignore_ascii_case("base64") => base64_decoded(bytes),
        Some(cte) if cte.eq_ignore_ascii_case("quoted-printable") => Some(
            quoted_printable_decode(bytes).unwrap_or_else(|| quoted_printable_lenient(bytes)),
        ),
        _ => Some(bytes.to_vec()),
    }
}

/// Base64, undone one line at a time. A sender that pads every wrapped
/// line, not only the last, leaves a `=` in the middle of the stream
/// that a single whole-body decode refuses; each line still stands on
/// its own. Decoding stops at the first line that is not base64, rather
/// than losing the lines that came before it, the way a mailing list's
/// footer or a trailer after the encoded body would. `None` only when
/// nothing at all decoded, so genuinely corrupt data still gives no text.
fn base64_decoded(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut decoded = Vec::with_capacity(bytes.len());
    for line in bytes.split(|&b| b == b'\n') {
        let clean: Vec<u8> = line.iter().copied().filter(|b| !b.is_ascii_whitespace()).collect();
        if clean.is_empty() {
            continue;
        }
        match BASE64.decode(clean) {
            Ok(bytes) => decoded.extend(bytes),
            Err(_) => break,
        }
    }
    (!decoded.is_empty()).then_some(decoded)
}

/// A lenient fallback for quoted-printable content mail-parser's own
/// strict decoder refuses outright over one bad escape. Everything else
/// still decodes; the bad escape (`=` not followed by two hex digits, or
/// by a line break) shows through exactly as written, since there is no
/// way to know what the sender meant by it.
fn quoted_printable_lenient(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'=' {
            if bytes[i + 1..].starts_with(b"\r\n") {
                i += 3;
                continue;
            }
            if bytes.get(i + 1) == Some(&b'\n') {
                i += 2;
                continue;
            }
            if let Some(byte) = bytes.get(i + 1..i + 3).and_then(hex_byte) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

/// The byte two hex digits spell, or `None` when they are not both hex.
fn hex_byte(pair: &[u8]) -> Option<u8> {
    let hi = (pair[0] as char).to_digit(16)?;
    let lo = (pair[1] as char).to_digit(16)?;
    Some(((hi as u8) << 4) | lo as u8)
}
