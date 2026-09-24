//! BODYSTRUCTURE in the body reader's shape: `mailrs_mime`'s part tree,
//! numbered the way IMAP numbers sections, so the rules that read a raw
//! message read a large one fetched part by part.

use std::collections::BTreeMap;

use async_imap::imap_proto::{
    BodyContentCommon, BodyContentSinglePart, BodyParams, BodyStructure as Wire, ContentEncoding,
};
use mail_parser::MessageParser;
use mailrs_mime::charset::decode_charset;
use mailrs_mime::{MAX_DEPTH, Part, Parts, content_id, numbered};

/// A message's parts as the server described them, with no bytes yet.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BodyStructure {
    /// The part tree. A multipart root has the empty path; a message that
    /// is not multipart has its one body at "1".
    pub root: Part,
    /// Each part's Content-Transfer-Encoding by path, lower case. A part
    /// fetched with `BODY.PEEK[<path>]` arrives still encoded.
    pub encodings: BTreeMap<String, String>,
}

impl BodyStructure {
    /// The parts with the message's own headers read from `header`, the
    /// bytes of `BODY.PEEK[HEADER]`.
    pub fn parts(&self, header: &[u8]) -> Parts {
        Parts {
            headers: mailrs_mime::parts(header)
                .map(|p| p.headers)
                .unwrap_or_default(),
            root: self.root.clone(),
            incomplete: false,
        }
    }

    /// The bytes `BODY.PEEK[<path>]` returned for `path`, with the
    /// transfer encoding undone. `None` for base64 that does not decode.
    pub fn decode(&self, path: &str, bytes: &[u8]) -> Option<Vec<u8>> {
        mailrs_mime::undo_transfer_encoding(self.encodings.get(path).map(String::as_str), bytes)
    }

    /// The structure in async-imap's parsed form of a FETCH answer.
    pub fn from_wire(wire: &Wire<'_>) -> BodyStructure {
        let mut encodings = BTreeMap::new();
        let root_path = match wire {
            Wire::Multipart { .. } => String::new(),
            _ => "1".to_string(),
        };
        let root = convert(wire, root_path, 0, &mut encodings);
        BodyStructure { root, encodings }
    }
}

/// Part `wire` at `path`, `depth` levels below the root.
fn convert(
    wire: &Wire<'_>,
    path: String,
    depth: usize,
    encodings: &mut BTreeMap<String, String>,
) -> Part {
    match wire {
        Wire::Multipart { common, bodies, .. } => {
            let children = match depth < MAX_DEPTH {
                true => bodies
                    .iter()
                    .enumerate()
                    .map(|(i, body)| convert(body, numbered(&path, i), depth + 1, encodings))
                    .collect(),
                false => Vec::new(),
            };
            Part {
                path,
                mime_type: mime_type(common),
                protocol: parameter(&common.ty.params, "protocol"),
                children,
                ..Part::default()
            }
        }
        Wire::Text { common, other, .. } | Wire::Basic { common, other, .. } => {
            leaf(common, other, path, encodings)
        }
        Wire::Message {
            common,
            other,
            envelope,
            body,
            ..
        } => {
            // IMAP numbers what a forwarded message holds from one level
            // under its own number: "4.1" for a single body, "4.1", "4.2"
            // for a multipart's children.
            let children = match (depth < MAX_DEPTH, body.as_ref()) {
                (false, _) => Vec::new(),
                (true, Wire::Multipart { bodies, .. }) => bodies
                    .iter()
                    .enumerate()
                    .map(|(i, child)| convert(child, numbered(&path, i), depth + 1, encodings))
                    .collect(),
                (true, single) => vec![convert(single, format!("{path}.1"), depth + 1, encodings)],
            };
            let subject = envelope
                .subject
                .as_deref()
                .map(|s| encoded_words(&String::from_utf8_lossy(s)));
            Part {
                subject,
                children,
                ..leaf(common, other, path, encodings)
            }
        }
    }
}

/// A part that holds bytes rather than other parts.
fn leaf(
    common: &BodyContentCommon<'_>,
    other: &BodyContentSinglePart<'_>,
    path: String,
    encodings: &mut BTreeMap<String, String>,
) -> Part {
    let encoding = match &other.transfer_encoding {
        ContentEncoding::SevenBit => "7bit".to_string(),
        ContentEncoding::EightBit => "8bit".to_string(),
        ContentEncoding::Binary => "binary".to_string(),
        ContentEncoding::Base64 => "base64".to_string(),
        ContentEncoding::QuotedPrintable => "quoted-printable".to_string(),
        ContentEncoding::Other(other) => other.to_ascii_lowercase(),
    };
    // The server counts encoded octets; base64 carries three bytes in four.
    let size = match encoding.as_str() {
        "base64" => i64::from(other.octets) * 3 / 4,
        _ => i64::from(other.octets),
    };
    encodings.insert(path.clone(), encoding);
    let disposition = common.disposition.as_ref();
    Part {
        path,
        mime_type: mime_type(common),
        charset: parameter(&common.ty.params, "charset"),
        protocol: parameter(&common.ty.params, "protocol"),
        smime_type: parameter(&common.ty.params, "smime-type"),
        filename: disposition
            .and_then(|d| parameter(&d.params, "filename"))
            .or_else(|| parameter(&common.ty.params, "name")),
        content_id: other.id.as_deref().map(content_id),
        subject: None,
        attachment: disposition.is_some_and(|d| d.ty.eq_ignore_ascii_case("attachment")),
        size,
        data: None,
        children: Vec::new(),
    }
}

fn mime_type(common: &BodyContentCommon<'_>) -> String {
    format!("{}/{}", common.ty.ty, common.ty.subtype).to_ascii_lowercase()
}

/// Parameter `name`, read in whichever form the sender wrote it: RFC
/// 2231's `name*` (encoded) and `name*0`, `name*1*` (split), or a plain
/// `name`, whose RFC 2047 encoded words are decoded too.
fn parameter(params: &BodyParams<'_>, name: &str) -> Option<String> {
    let params = params.as_deref()?;
    let key = |k: &str| k.to_ascii_lowercase();
    let name = name.to_ascii_lowercase();
    let encoded = format!("{name}*");
    if let Some((_, value)) = params.iter().find(|(k, _)| key(k) == encoded) {
        return Some(rfc2231(&[(value.as_ref(), true)]));
    }
    let mut pieces: Vec<(usize, &str, bool)> = params
        .iter()
        .filter_map(|(k, v)| {
            let k = key(k);
            let rest = k.strip_prefix(&encoded)?;
            let (index, is_encoded) = match rest.strip_suffix('*') {
                Some(index) => (index, true),
                None => (rest, false),
            };
            Some((index.parse().ok()?, v.as_ref(), is_encoded))
        })
        .collect();
    if !pieces.is_empty() {
        pieces.sort_by_key(|(index, _, _)| *index);
        let pieces: Vec<(&str, bool)> = pieces.iter().map(|(_, v, e)| (*v, *e)).collect();
        return Some(rfc2231(&pieces));
    }
    params
        .iter()
        .find(|(k, _)| key(k) == name)
        .map(|(_, value)| encoded_words(value))
}

/// An RFC 2231 value from its pieces in order, each marked encoded or
/// not. The first encoded piece starts with `charset'language'`.
fn rfc2231(pieces: &[(&str, bool)]) -> String {
    let mut charset = None;
    let mut bytes = Vec::new();
    for (i, (piece, is_encoded)) in pieces.iter().enumerate() {
        if !is_encoded {
            bytes.extend_from_slice(piece.as_bytes());
            continue;
        }
        let mut text = *piece;
        if i == 0 {
            let mut parts = piece.splitn(3, '\'');
            if let (Some(set), Some(_language), Some(rest)) =
                (parts.next(), parts.next(), parts.next())
            {
                charset = Some(set.to_string()).filter(|s| !s.is_empty());
                text = rest;
            }
        }
        bytes.extend(percent_decoded(text));
    }
    decode_charset(&bytes, Some(charset.as_deref().unwrap_or("utf-8")))
}

fn percent_decoded(text: &str) -> Vec<u8> {
    let raw = text.as_bytes();
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        let hex = raw
            .get(i + 1..i + 3)
            .and_then(|pair| std::str::from_utf8(pair).ok())
            .and_then(|pair| u8::from_str_radix(pair, 16).ok());
        match (raw[i], hex) {
            (b'%', Some(byte)) => {
                out.push(byte);
                i += 3;
            }
            (byte, _) => {
                out.push(byte);
                i += 1;
            }
        }
    }
    out
}

/// `value` with any RFC 2047 encoded words decoded. mail-parser decodes
/// them in a Subject header, so the value goes through as one.
fn encoded_words(value: &str) -> String {
    if !value.contains("=?") {
        return value.to_string();
    }
    let header = format!("Subject: {value}\r\n\r\n");
    MessageParser::default()
        .parse_headers(header.as_bytes())
        .and_then(|m| m.subject().map(str::to_string))
        .unwrap_or_else(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use async_imap::imap_proto::{AttributeValue, Response};

    use super::BodyStructure;

    /// The structure in a FETCH answer written by hand.
    fn structure(line: &str) -> BodyStructure {
        let (_, response) = Response::from_bytes(line.as_bytes()).expect("parses");
        let Response::Fetch(_, attributes) = response else {
            panic!("not a FETCH")
        };
        attributes
            .iter()
            .find_map(|a| match a {
                AttributeValue::BodyStructure(wire) => Some(BodyStructure::from_wire(wire)),
                _ => None,
            })
            .expect("has a BODYSTRUCTURE")
    }

    const MIXED: &str = "* 1 FETCH (UID 9 BODYSTRUCTURE ((\"text\" \"plain\" (\"charset\" \"utf-8\") NIL NIL \"quoted-printable\" 120 4 NIL NIL NIL NIL)(\"application\" \"pdf\" (\"name\" \"=?UTF-8?Q?r=C3=A9sum=C3=A9.pdf?=\") \"<cv@x>\" NIL \"base64\" 4000 NIL (\"attachment\" (\"filename*\" \"utf-8''r%C3%A9sum%C3%A9.pdf\")) NIL NIL) \"mixed\" (\"boundary\" \"x\") NIL NIL NIL))\r\n";

    #[test]
    fn a_mixed_message_numbers_its_parts_from_one() {
        let s = structure(MIXED);
        assert_eq!(s.root.path, "");
        assert_eq!(s.root.mime_type, "multipart/mixed");
        let paths: Vec<&str> = s.root.children.iter().map(|p| p.path.as_str()).collect();
        assert_eq!(paths, ["1", "2"]);
        assert_eq!(s.root.children[0].charset.as_deref(), Some("utf-8"));
        assert_eq!(
            s.encodings.get("1").map(String::as_str),
            Some("quoted-printable")
        );
    }

    #[test]
    fn an_attachment_keeps_its_name_size_and_content_id() {
        let s = structure(MIXED);
        let pdf = &s.root.children[1];
        assert_eq!(pdf.mime_type, "application/pdf");
        assert_eq!(pdf.filename.as_deref(), Some("résumé.pdf"));
        assert!(pdf.attachment);
        assert_eq!(pdf.size, 3000);
        assert_eq!(pdf.content_id.as_deref(), Some("cv@x"));
        assert_eq!(pdf.data, None);
    }

    #[test]
    fn a_single_part_message_has_its_body_at_one() {
        let s = structure(
            "* 1 FETCH (UID 9 BODYSTRUCTURE (\"text\" \"plain\" (\"charset\" \"us-ascii\") NIL NIL \"7bit\" 12 1 NIL NIL NIL NIL))\r\n",
        );
        assert_eq!(s.root.path, "1");
        assert_eq!(s.root.mime_type, "text/plain");
        assert!(s.root.children.is_empty());
    }

    #[test]
    fn a_forwarded_message_numbers_its_parts_under_its_own() {
        let s = structure(
            "* 1 FETCH (UID 9 BODYSTRUCTURE ((\"text\" \"plain\" NIL NIL NIL \"7bit\" 12 1 NIL NIL NIL NIL)(\"message\" \"rfc822\" NIL NIL NIL \"7bit\" 300 (NIL \"=?UTF-8?Q?Reuni=C3=A3o?=\" NIL NIL NIL NIL NIL NIL NIL NIL) ((\"text\" \"plain\" NIL NIL NIL \"7bit\" 3 1 NIL NIL NIL NIL)(\"text\" \"html\" NIL NIL NIL \"7bit\" 3 1 NIL NIL NIL NIL) \"alternative\" (\"boundary\" \"y\") NIL NIL NIL) 10 NIL NIL NIL NIL) \"mixed\" (\"boundary\" \"x\") NIL NIL NIL))\r\n",
        );
        let forwarded = &s.root.children[1];
        assert_eq!(forwarded.path, "2");
        assert_eq!(forwarded.mime_type, "message/rfc822");
        assert_eq!(forwarded.subject.as_deref(), Some("Reunião"));
        let inner: Vec<&str> = forwarded.children.iter().map(|p| p.path.as_str()).collect();
        assert_eq!(inner, ["2.1", "2.2"]);
    }

    #[test]
    fn a_split_encoded_file_name_joins_its_pieces() {
        let s = structure(
            "* 1 FETCH (UID 9 BODYSTRUCTURE (\"application\" \"octet-stream\" NIL NIL NIL \"base64\" 8 NIL (\"attachment\" (\"filename*0*\" \"utf-8''r%C3%A9\" \"filename*1\" \"sum\" \"filename*2*\" \"%C3%A9.txt\")) NIL NIL))\r\n",
        );
        assert_eq!(s.root.filename.as_deref(), Some("résumé.txt"));
    }

    #[test]
    fn a_part_decodes_by_its_own_transfer_encoding() {
        let s = structure(MIXED);
        assert_eq!(
            s.decode("1", b"caf=C3=A9").as_deref(),
            Some("café".as_bytes())
        );
        assert_eq!(s.decode("2", b"aGk=").as_deref(), Some(&b"hi"[..]));
    }

    #[test]
    fn parts_carry_the_headers_fetched_beside_the_structure() {
        let parts =
            structure(MIXED).parts(b"Subject: Hi\r\nList-Unsubscribe: <mailto:u@x>\r\n\r\n");
        assert_eq!(parts.header("list-unsubscribe"), Some("<mailto:u@x>"));
        assert_eq!(parts.root.children.len(), 2);
    }

    #[test]
    fn nesting_past_the_limit_keeps_no_children() {
        let mut body = "(\"text\" \"plain\" NIL NIL NIL \"7bit\" 1 1 NIL NIL NIL NIL)".to_string();
        for _ in 0..70 {
            body = format!("({body} \"mixed\" (\"boundary\" \"b\") NIL NIL NIL)");
        }
        let s = structure(&format!("* 1 FETCH (UID 9 BODYSTRUCTURE {body})\r\n"));
        let mut depth = 0;
        let mut part = &s.root;
        while let Some(child) = part.children.first() {
            part = child;
            depth += 1;
        }
        assert_eq!(depth, 64);
    }
}
