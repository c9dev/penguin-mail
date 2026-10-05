//! A message as it arrived, in RFC 822 form, as parts. The part tree comes
//! from mail-parser; each part's bytes come from the raw message with the
//! transfer encoding undone here, and the charset stays for `body` to
//! read, since senders often mislabel it and mail-parser's own decoding
//! trusts the label.

use std::io::{self, Read, Seek, SeekFrom};

use base64::Engine;
use base64::alphabet;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use mail_parser::decoders::quoted_printable::quoted_printable_decode;
use mail_parser::parsers::MessageStream;
use mail_parser::{Message, MessageParser, MessagePart, MimeHeaders, PartType};
use mailrs_domain::MessageBody;

use crate::parts::{
    MAX_DEPTH, Part, Parts, body, content_id, is_attachment, is_calendar, is_readable, numbered,
};

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
    let root = convert(&message, raw, 0, root_path, 0);
    dismantle(message);
    Some(Parts {
        headers,
        root,
        incomplete: false,
    })
}

/// The parts of `raw` with the bytes of its text alone: the text and HTML a
/// body may show, and any calendar. A file keeps `data` at `None` and is
/// never decoded, as in a server's structure. Each part's headers are read
/// on their own, and its end found by scanning for the boundary as
/// mail-parser does, so a message with a large file costs its own bytes
/// and its text. A forwarded message is one file; nothing inside it is
/// read. `None` for bytes mail-parser cannot read at all.
pub(crate) fn skim(raw: &[u8]) -> Option<Parts> {
    Skim::default().message(raw)
}

/// The parts of `raw` as a server's structure gives them: every part with
/// its type, name and decoded size, and the bytes of its text alone, read
/// as [`skim`] reads them. Unlike a summary, opening a message lists the
/// files inside a forwarded message too, so this walks into them. A file's
/// size is counted from its encoded bytes without decoding it, wherever
/// those decode cleanly. Beside the parts come their spans, so one file
/// can be decoded later from the stored message without reading the rest.
/// `None` for bytes mail-parser cannot read at all.
pub fn outline(raw: &[u8]) -> Option<(Parts, Spans)> {
    let mut skim = Skim {
        forwarded: true,
        spans: Vec::new(),
    };
    let parts = skim.message(raw)?;
    Some((parts, Spans(skim.spans)))
}

/// A walk over a message's parts that decodes only their text.
#[derive(Default)]
struct Skim {
    /// Walk into forwarded messages, count every file's size and note
    /// where each part lies.
    forwarded: bool,
    spans: Vec<(String, Span)>,
}

impl Skim {
    fn message(&mut self, raw: &[u8]) -> Option<Parts> {
        let message = MessageParser::default().parse_headers(raw)?;
        let headers = message
            .headers_raw()
            .map(|(name, value)| (name.to_string(), unfold(value)))
            .collect();
        let root_path = match mime_type(message.root_part()).starts_with("multipart/") {
            true => String::new(),
            false => "1".to_string(),
        };
        Some(Parts {
            headers,
            root: self.part(raw, Some(0), root_path, 0),
            incomplete: false,
        })
    }

    /// One part from `bytes`, its headers and body, `depth` levels below
    /// the root. `at` is where `bytes` start in the raw message, `None`
    /// inside a forwarded message that had to be decoded first.
    fn part(&mut self, bytes: &[u8], at: Option<usize>, path: String, depth: usize) -> Part {
        let parser = MessageParser::default();
        // A headers-only parse puts the body offset at the end of its
        // input, so the stream says where the headers stop.
        let mut stream = MessageStream::new(bytes);
        stream.parse_headers(&parser, &mut Vec::new());
        let body_start = stream.offset().min(bytes.len());
        let body = &bytes[body_start..];
        let body_at = at.map(|at| at + body_start);
        let Some(message) = parser.parse_headers(bytes) else {
            // A part with no headers is plain text (RFC 2045 section 5.2).
            let text = bytes
                .strip_prefix(b"\r\n")
                .or_else(|| bytes.strip_prefix(b"\n"))
                .unwrap_or(bytes);
            let text_at = at.map(|at| at + bytes.len() - text.len());
            self.note(&path, text_at, text, Transfer::Plain, text.len() as u64);
            return Part {
                path,
                mime_type: "text/plain".into(),
                size: text.len() as i64,
                data: Some(text.to_vec()),
                ..Part::default()
            };
        };
        let part = message.root_part();
        let attribute = |name: &str| {
            part.content_type()
                .and_then(|ct| ct.attribute(name))
                .map(str::to_string)
        };
        let encoding = part.content_transfer_encoding();
        let mut skimmed = Part {
            mime_type: mime_type(part),
            charset: attribute("charset"),
            protocol: attribute("protocol"),
            smime_type: attribute("smime-type"),
            filename: part.attachment_name().map(str::to_string),
            content_id: part.content_id().map(content_id),
            attachment: part.content_disposition().is_some_and(|d| d.is_attachment()),
            ..Part::default()
        };
        if skimmed.mime_type.starts_with("multipart/") {
            if let (Some(boundary), true) = (attribute("boundary"), depth < MAX_DEPTH) {
                skimmed.children = self.children(body, body_at, boundary.as_bytes(), &path, depth);
            }
        } else if skimmed.mime_type == "message/rfc822" {
            if self.forwarded {
                skimmed.size = decoded_size(encoding, body) as i64;
                self.note(&path, body_at, body, Transfer::of(encoding), skimmed.size as u64);
                if depth < MAX_DEPTH {
                    self.forwarded_message(&mut skimmed, body, body_at, encoding, &path, depth);
                }
            }
        } else {
            let name = skimmed.filename.as_deref().unwrap_or_default();
            let wanted = (is_readable(&skimmed.mime_type) && !is_attachment(&skimmed))
                || is_calendar(&skimmed.mime_type, name);
            if wanted {
                skimmed.data = undo_transfer_encoding(encoding, body);
                skimmed.size = skimmed.data.as_ref().map_or(0, |d| d.len() as i64);
            } else if self.forwarded {
                skimmed.size = decoded_size(encoding, body) as i64;
            }
            self.note(&path, body_at, body, Transfer::of(encoding), skimmed.size as u64);
        }
        skimmed.path = path;
        skimmed
    }

    /// The parts a multipart `body`, at `at`, holds between its `boundary`
    /// lines. The line break before a boundary belongs to the boundary.
    fn children(
        &mut self,
        body: &[u8],
        at: Option<usize>,
        boundary: &[u8],
        path: &str,
        depth: usize,
    ) -> Vec<Part> {
        let mut stream = MessageStream::new(body);
        if !stream.seek_next_part(boundary) {
            return Vec::new();
        }
        stream.skip_crlf();
        let mut children = Vec::new();
        loop {
            let start = stream.offset().min(body.len());
            let (end, found) = stream.seek_part_end(Some(boundary));
            let child = body.get(start..end.min(body.len())).unwrap_or_default();
            let child_at = at.map(|at| at + start);
            let child_path = numbered(path, children.len());
            children.push(self.part(child, child_at, child_path, depth + 1));
            if !found || stream.is_multipart_end() {
                return children;
            }
        }
    }

    /// The parts of the message a `message/rfc822` part holds, numbered as
    /// IMAP numbers them (see [`nested_message`]), and its subject, which
    /// names its file. A forwarded message sent with a transfer encoding,
    /// which RFC 2045 forbids and senders use anyway, is decoded to read it.
    fn forwarded_message(
        &mut self,
        skimmed: &mut Part,
        body: &[u8],
        at: Option<usize>,
        encoding: Option<&str>,
        path: &str,
        depth: usize,
    ) {
        let decoded;
        let (inner, inner_at) = match Transfer::of(encoding) {
            Transfer::Plain => (body, at),
            _ => {
                decoded = undo_transfer_encoding(encoding, body).unwrap_or_default();
                (decoded.as_slice(), None)
            }
        };
        let Some(headers) = MessageParser::default().parse_headers(inner) else {
            return;
        };
        skimmed.subject = headers.subject().map(str::to_string);
        skimmed.children = match mime_type(headers.root_part()).starts_with("multipart/") {
            true => self.part(inner, inner_at, path.to_string(), depth + 1).children,
            false => vec![self.part(inner, inner_at, format!("{path}.1"), depth + 1)],
        };
    }

    /// Notes where part `path`'s `bytes` lie, when they lie in the raw
    /// message at `at` and spans are wanted.
    fn note(&mut self, path: &str, at: Option<usize>, bytes: &[u8], transfer: Transfer, size: u64) {
        if let (true, Some(at)) = (self.forwarded, at) {
            let span = Span {
                start: at as u64,
                end: (at + bytes.len()) as u64,
                transfer,
                size,
            };
            self.spans.push((path.to_string(), span));
        }
    }
}

/// The size of `bytes` once `encoding` is undone. Base64 that decodes as
/// one stream is counted without decoding it; anything else is decoded,
/// so the count matches what a full read gives.
fn decoded_size(encoding: Option<&str>, bytes: &[u8]) -> usize {
    match Transfer::of(encoding) {
        Transfer::Plain => bytes.len(),
        Transfer::Base64 => base64_size(bytes).unwrap_or_else(|| {
            undo_transfer_encoding(encoding, bytes).map_or(0, |d| d.len())
        }),
        Transfer::QuotedPrintable => undo_transfer_encoding(encoding, bytes).map_or(0, |d| d.len()),
    }
}

/// How many bytes `bytes` decode to as base64, when they are base64
/// letters with at most two `=` at the end and white space anywhere, the
/// shape the whole-body decode in [`base64_decoded`] takes. `None` for
/// any other shape.
fn base64_size(bytes: &[u8]) -> Option<usize> {
    let (mut letters, mut padding) = (0usize, 0usize);
    for &b in bytes {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'+' | b'/' if padding == 0 => letters += 1,
            b'=' => padding += 1,
            b if b.is_ascii_whitespace() => {}
            _ => return None,
        }
    }
    let whole = letters % 4 != 1 && padding <= 2 && (padding == 0 || (letters + padding) % 4 == 0);
    whole.then_some(letters * 3 / 4)
}

/// Drops a parsed message one level at a time. mail-parser parses a
/// forwarded message inside a forwarded message without recursing, but
/// holds each one inside the part above it, and the drop the compiler
/// writes for that recurses once per level: ten thousand levels, a few
/// hundred kilobytes of mail, overflow a worker thread's stack.
fn dismantle(message: Message<'_>) {
    let mut pending = vec![message];
    while let Some(mut message) = pending.pop() {
        for part in &mut message.parts {
            if let PartType::Message(inner) = std::mem::replace(&mut part.body, PartType::Text("".into())) {
                pending.push(inner);
            }
        }
    }
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

/// Part `id` and the parts under it, `depth` levels below the root.
fn convert(message: &Message, raw: &[u8], id: u32, path: String, depth: usize) -> Part {
    let Some(part) = message.part(id) else {
        return Part::default();
    };
    let below = depth < MAX_DEPTH;
    if let (Some(nested), true) = (part.message(), below) {
        return nested_message(part, nested, raw, path, depth);
    }
    let children = match below {
        true => part.sub_parts().unwrap_or_default(),
        false => &[],
    }
    .iter()
    .enumerate()
    .map(|(i, child)| convert(message, raw, *child, numbered(&path, i), depth + 1))
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
        content_id: part.content_id().map(content_id),
        subject: None,
        attachment: part.content_disposition().is_some_and(|d| d.is_attachment()),
        size: data.as_ref().map_or(0, |d| d.len() as i64),
        data,
        children,
    }
}

/// A `message/rfc822` part: a file of its own, whose bytes are the
/// forwarded message, and the parts inside it, walked into for their
/// own files. IMAP addresses what is inside it starting one level under
/// this part's own number: "4.1" for a single-part encapsulated message,
/// "4.1", "4.2" for a multipart one; "4" itself is the whole
/// encapsulated message.
///
/// RFC 2045 forbids a transfer encoding on `message/rfc822`, but a
/// sender out there sets one anyway. When `part` carries one mail-parser
/// decodes its body first and parses `nested` from that decoded copy,
/// so `nested`'s own parts carry offsets into it rather than into the
/// outer message; `nested.raw_message()` is that copy. Without one,
/// mail-parser parses `nested` by continuing the same scan over the
/// outer bytes, so its offsets stay relative to them, and
/// `nested.raw_message()` is a different thing here: a copy of the
/// nested message's own bytes built separately, indexed from zero,
/// that the child offsets do not match. Reading from the wrong one of
/// the two returns the wrong slice, or one out of range.
fn nested_message(
    part: &MessagePart,
    nested: &Message,
    raw: &[u8],
    path: String,
    depth: usize,
) -> Part {
    // The part's own bytes, the forwarded message as a file, come from
    // the outer message whatever its encoding says.
    let data = transfer_decoded(raw, part);
    let raw = match part.content_transfer_encoding() {
        Some(cte) if cte.eq_ignore_ascii_case("base64") || cte.eq_ignore_ascii_case("quoted-printable") => {
            nested.raw_message()
        }
        _ => raw,
    };
    let root = nested.root_part();
    let children = if root.is_multipart() {
        root.sub_parts()
            .unwrap_or_default()
            .iter()
            .enumerate()
            .map(|(i, child)| convert(nested, raw, *child, numbered(&path, i), depth + 1))
            .collect()
    } else {
        vec![convert(nested, raw, 0, format!("{path}.1"), depth + 1)]
    };
    Part {
        path,
        mime_type: mime_type(part),
        filename: part.attachment_name().map(str::to_string),
        content_id: part.content_id().map(content_id),
        subject: nested.subject().map(str::to_string),
        size: data.as_ref().map_or(0, |d| d.len() as i64),
        data,
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
    undo_transfer_encoding(part.content_transfer_encoding(), bytes)
}

/// `bytes` with `encoding`, a part's Content-Transfer-Encoding, undone,
/// and the charset left alone. `None` for base64 that does not decode at
/// all. An IMAP server sends a part fetched on its own still encoded.
pub fn undo_transfer_encoding(encoding: Option<&str>, bytes: &[u8]) -> Option<Vec<u8>> {
    match encoding {
        Some(cte) if cte.eq_ignore_ascii_case("base64") => base64_decoded(bytes),
        Some(cte) if cte.eq_ignore_ascii_case("quoted-printable") => Some(
            quoted_printable_decode(bytes).unwrap_or_else(|| quoted_printable_lenient(bytes)),
        ),
        _ => Some(bytes.to_vec()),
    }
}

/// Base64, undone over the whole body with the line breaks taken out.
/// Only a body that does not decode that way is read one line at a
/// time: decoding line by line first would turn a file wrapped at a
/// width that is not a multiple of four into wrong bytes without an
/// error.
fn base64_decoded(bytes: &[u8]) -> Option<Vec<u8>> {
    let clean: Vec<u8> = bytes.iter().copied().filter(|b| !b.is_ascii_whitespace()).collect();
    match BASE64.decode(clean) {
        Ok(decoded) => (!decoded.is_empty()).then_some(decoded),
        Err(_) => base64_by_line(bytes),
    }
}

/// Base64, undone one line at a time. A sender that pads every wrapped
/// line, not only the last, leaves a `=` in the middle of the stream
/// that a single whole-body decode refuses; each line still stands on
/// its own. Decoding stops at the first line that is not base64, rather
/// than losing the lines that came before it, the way a mailing list's
/// footer or a trailer after the encoded body would. `None` only when
/// nothing at all decoded, so genuinely corrupt data still gives no text.
fn base64_by_line(bytes: &[u8]) -> Option<Vec<u8>> {
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

/// A part's Content-Transfer-Encoding, as far as undoing it goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transfer {
    /// None, `7bit`, `8bit`, `binary` or a name nobody undoes.
    Plain,
    Base64,
    QuotedPrintable,
}

/// Where one part's encoded bytes lie in a raw message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub start: u64,
    pub end: u64,
    pub transfer: Transfer,
    /// The part's size once decoded.
    pub size: u64,
}

/// Where each part of a raw message lies, by part path. A part inside a
/// forwarded message that carries a transfer encoding has none: its bytes
/// exist only once that message is decoded.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Spans(Vec<(String, Span)>);

impl Spans {
    pub fn get(&self, path: &str) -> Option<&Span> {
        self.0.iter().find(|(p, _)| p == path).map(|(_, span)| span)
    }
}

impl Transfer {
    /// Compared without case, as mail-parser compares it.
    fn of(encoding: Option<&str>) -> Transfer {
        match encoding {
            Some(cte) if cte.eq_ignore_ascii_case("base64") => Transfer::Base64,
            Some(cte) if cte.eq_ignore_ascii_case("quoted-printable") => Transfer::QuotedPrintable,
            _ => Transfer::Plain,
        }
    }

    fn name(self) -> Option<&'static str> {
        match self {
            Transfer::Plain => None,
            Transfer::Base64 => Some("base64"),
            Transfer::QuotedPrintable => Some("quoted-printable"),
        }
    }
}

/// How many encoded bytes base64 decoding reads from its source at a time.
const CHUNK: usize = 64 << 10;

/// The bytes of the part at `span` in `source`, a raw message, with the
/// transfer encoding undone, as [`part`] gives them. Base64, which almost
/// every file travels in, is read and decoded a chunk at a time into a
/// buffer of the decoded size, so a file costs its own decoded bytes.
/// Other encodings read the span whole first.
pub fn decode_span<R: Read + Seek>(source: &mut R, span: &Span) -> io::Result<Option<Vec<u8>>> {
    if span.transfer == Transfer::Base64
        && let Some(decoded) = base64_streamed(source, span)?
    {
        return Ok((!decoded.is_empty()).then_some(decoded));
    }
    let encoded = read_span(source, span)?;
    Ok(match span.transfer {
        Transfer::Plain => Some(encoded),
        transfer => undo_transfer_encoding(transfer.name(), &encoded),
    })
}

/// The encoded bytes at `span`.
fn read_span<R: Read + Seek>(source: &mut R, span: &Span) -> io::Result<Vec<u8>> {
    let length = usize::try_from(span.end.saturating_sub(span.start))
        .map_err(|_| io::Error::other("a part longer than memory"))?;
    source.seek(SeekFrom::Start(span.start))?;
    let mut encoded = vec![0; length];
    source.read_exact(&mut encoded)?;
    Ok(encoded)
}

/// Base64 at `span` decoded a chunk at a time, or `None` when it does not
/// decode as one stream, for the caller to decode the way a full read
/// does. Whole groups of four letters decode as they arrive; padding may
/// only end the stream, as in the whole-body decode.
fn base64_streamed<R: Read + Seek>(source: &mut R, span: &Span) -> io::Result<Option<Vec<u8>>> {
    source.seek(SeekFrom::Start(span.start))?;
    let mut left = span.end.saturating_sub(span.start);
    // Room for the last group's estimate, which can run two bytes over.
    let mut decoded = Vec::with_capacity(usize::try_from(span.size).unwrap_or(0).saturating_add(3));
    let mut chunk = vec![0; CHUNK];
    let mut letters: Vec<u8> = Vec::with_capacity(CHUNK + 4);
    let mut padded = false;
    while left > 0 {
        let n = usize::try_from(left).unwrap_or(CHUNK).min(CHUNK);
        source.read_exact(&mut chunk[..n])?;
        left -= n as u64;
        for &b in &chunk[..n] {
            match b {
                b'=' => padded = true,
                b if b.is_ascii_whitespace() => continue,
                _ if padded => return Ok(None),
                _ => {}
            }
            letters.push(b);
        }
        if !padded {
            let whole = letters.len() / 4 * 4;
            if BASE64.decode_vec(&letters[..whole], &mut decoded).is_err() {
                return Ok(None);
            }
            letters.drain(..whole);
        }
    }
    Ok(BASE64.decode_vec(&letters, &mut decoded).ok().map(|()| decoded))
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;
    use crate::summary::tests::SHAPES;

    fn shown(raw: &[u8]) -> String {
        String::from_utf8_lossy(raw).into_owned()
    }

    #[test]
    fn an_outline_gives_the_body_a_full_read_gives() {
        for raw in SHAPES {
            let (parts, _) = outline(raw).expect("an outline");
            assert_eq!(body(&parts), read(raw), "{}", shown(raw));
        }
    }

    #[test]
    fn an_outline_decodes_no_file() {
        for raw in SHAPES {
            let (parts, _) = outline(raw).expect("an outline");
            for file in body(&parts).attachments {
                let part = parts.find(&file.part_id).expect("listed");
                let calendar = is_calendar(&part.mime_type, part.filename.as_deref().unwrap_or_default());
                assert!(part.data.is_none() || calendar, "{} in {}", file.part_id, shown(raw));
            }
        }
    }

    #[test]
    fn every_file_decodes_from_its_span_as_from_the_whole_message() {
        let mut spanned = 0;
        for raw in SHAPES {
            let (_, spans) = outline(raw).expect("an outline");
            for file in read(raw).attachments {
                let Some(span) = spans.get(&file.part_id) else {
                    continue;
                };
                spanned += 1;
                let decoded = decode_span(&mut Cursor::new(raw), span).unwrap();
                assert_eq!(decoded, part(raw, &file.part_id), "{} in {}", file.part_id, shown(raw));
                assert_eq!(span.size, file.size as u64, "{} in {}", file.part_id, shown(raw));
            }
        }
        assert!(spanned >= 12, "only {spanned} files had a span");
    }

    #[test]
    fn a_file_inside_an_encoded_forwarded_message_has_no_span() {
        let raw = SHAPES.iter().find(|raw| raw.starts_with(b"Subject: fwd b64")).expect("the shape");
        let (parts, spans) = outline(raw).expect("an outline");
        assert_eq!(parts.find("2.1").map(|p| p.mime_type.as_str()), Some("application/zip"));
        assert!(spans.get("2").is_some(), "the forwarded message itself lies in the raw bytes");
        assert!(spans.get("2.1").is_none());
    }

    /// Base64 decoded a chunk at a time must agree with the whole-body
    /// decode wherever a chunk boundary falls.
    #[test]
    fn a_long_base64_file_decodes_across_chunks() {
        let data: Vec<u8> = (0..200_000u32).map(|n| (n % 251) as u8).collect();
        let encoded = BASE64.encode(&data);
        let mut body = String::new();
        for line in encoded.as_bytes().chunks(76) {
            body.push_str(std::str::from_utf8(line).unwrap());
            body.push_str("\r\n");
        }
        let raw = format!(
            "Subject: big\r\nContent-Type: multipart/mixed; boundary=b\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\nHi.\r\n\
             --b\r\nContent-Type: application/octet-stream; name=d.bin\r\nContent-Transfer-Encoding: base64\r\n\r\n{body}--b--\r\n"
        );
        let (_, spans) = outline(raw.as_bytes()).expect("an outline");
        let span = spans.get("2").expect("a span");
        assert_eq!(span.size, data.len() as u64);
        assert_eq!(decode_span(&mut Cursor::new(raw.as_bytes()), span).unwrap(), Some(data));
    }
}
