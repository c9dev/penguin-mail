//! A message's parts, in words every source shares, and the one set of
//! rules that reads a body out of them. A raw message gives every part
//! with its bytes; a server's structure gives every part and the bytes of
//! its text parts alone. The rules treat both alike, so a body read either
//! way lists the same texts, files and part paths.

use mailrs_domain::{Attachment, MessageBody, Protection};

use crate::charset::decode_charset;

/// A message's top-level headers and its tree of parts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Parts {
    /// Name and value, unfolded, in the order the message carries them.
    pub headers: Vec<(String, String)>,
    pub root: Part,
}

/// One MIME part.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Part {
    /// IMAP's section number: "1", "2.1". Empty for a multipart root,
    /// which IMAP does not number.
    pub path: String,
    /// Lower case, without parameters: `text/plain`.
    pub mime_type: String,
    pub charset: Option<String>,
    /// The `protocol` parameter of a `multipart/signed` or `encrypted`.
    pub protocol: Option<String>,
    /// The `smime-type` parameter of an `application/pkcs7-mime`.
    pub smime_type: Option<String>,
    /// From `Content-Disposition`'s filename or `Content-Type`'s name.
    pub filename: Option<String>,
    /// Without angle brackets.
    pub content_id: Option<String>,
    /// `Content-Disposition: attachment`.
    pub attachment: bool,
    /// Decoded bytes, as the source counts them.
    pub size: i64,
    /// The bytes with the transfer encoding undone and the charset left
    /// alone. `None` when the source sent none, as a server's structure
    /// does for a file.
    pub data: Option<Vec<u8>>,
    pub children: Vec<Part>,
}

impl Parts {
    /// The first value of the header `name`, compared without case.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// The part at `path`.
    pub fn find(&self, path: &str) -> Option<&Part> {
        fn search<'a>(part: &'a Part, path: &str) -> Option<&'a Part> {
            if part.path == path {
                return Some(part);
            }
            part.children.iter().find_map(|c| search(c, path))
        }
        search(&self.root, path)
    }

    /// Gives the part at `path` bytes fetched after its structure, as a
    /// text part a server sent by reference.
    pub fn set_data(&mut self, path: &str, bytes: Vec<u8>) {
        fn search<'a>(part: &'a mut Part, path: &str) -> Option<&'a mut Part> {
            if part.path == path {
                return Some(part);
            }
            part.children.iter_mut().find_map(|c| search(c, path))
        }
        if let Some(part) = search(&mut self.root, path) {
            part.size = bytes.len() as i64;
            part.data = Some(bytes);
        }
    }
}

/// The body the window shows for a message with these parts.
pub fn body(parts: &Parts) -> MessageBody {
    let mut body = MessageBody {
        list_unsubscribe: parts.header("List-Unsubscribe").map(|v| v.trim().to_string()),
        one_click_unsubscribe: parts
            .header("List-Unsubscribe-Post")
            .is_some_and(|v| v.contains("One-Click")),
        protection: protection(&parts.root),
        provenance: crate::provenance::provenance(|name| parts.header(name).map(str::to_string)),
        ..MessageBody::default()
    };
    let start = match body.protection {
        // What the signature covers is the first part; the second is the
        // signature itself.
        Some(Protection::Signed | Protection::SmimeSigned) => parts.root.children.first(),
        _ => Some(&parts.root),
    };
    if let Some(start) = start {
        walk(start, &mut body);
    }
    drop_calendar_twin(&mut body);
    body
}

fn walk(part: &Part, body: &mut MessageBody) {
    // A nested message walks into its own parts, the way Gmail expands a
    // forwarded message rather than attaching it whole.
    if part.mime_type.starts_with("multipart/") || part.mime_type == "message/rfc822" {
        for child in &part.children {
            walk(child, body);
        }
        return;
    }
    let name = part.filename.as_deref().unwrap_or_default();
    if body.calendar.is_none() && is_calendar(&part.mime_type, name) {
        body.calendar = text(part).filter(|ics| ics.contains("BEGIN:VCALENDAR"));
    }
    if is_attachment(part) {
        body.attachments.push(Attachment {
            part_id: part.path.clone(),
            filename: attachment_name(name, &part.mime_type, part.content_id.as_deref()),
            mime_type: part.mime_type.clone(),
            size: part.size,
            attachment_id: Some(part.path.clone()),
            content_id: part.content_id.clone(),
        });
        return;
    }
    match part.mime_type.as_str() {
        "text/html" if body.html.is_none() => body.html = text(part),
        "text/plain" if body.text.is_none() => body.text = text(part),
        _ => {}
    }
}

/// A text part as text, in the charset its bytes show.
fn text(part: &Part) -> Option<String> {
    part.data
        .as_deref()
        .map(|bytes| decode_charset(bytes, part.charset.as_deref()))
}

fn is_readable(mime: &str) -> bool {
    mime == "text/plain" || mime == "text/html"
}

/// A part that carries a protocol's own plumbing, not something anybody
/// sent: the version string in front of PGP/MIME's ciphertext, a
/// signature by itself, a bounce's machine-readable status, or the
/// headers of a message a mail server refused. Nameless and without a
/// disposition, these would otherwise fall out of `is_attachment`'s
/// catch-all for anything that is not text.
const STRUCTURAL: &[&str] = &[
    "application/pgp-encrypted",
    "application/pgp-signature",
    "application/pkcs7-signature",
    "application/x-pkcs7-signature",
    "message/delivery-status",
    "message/disposition-notification",
    "text/rfc822-headers",
];

/// Whether a part is a file rather than text to show. A name, a
/// Content-ID on anything but readable text, or an attachment disposition
/// make it one. So does any part that is not text and not structural:
/// Gmail sent those by reference, which is how the app listed them
/// before it read raw mail.
fn is_attachment(part: &Part) -> bool {
    part.filename.as_deref().is_some_and(|n| !n.is_empty())
        || (part.content_id.is_some() && !is_readable(&part.mime_type))
        || part.attachment
        || (!part.mime_type.starts_with("text/") && !STRUCTURAL.contains(&part.mime_type.as_str()))
}

/// What to call a part that arrived without a name. The extension follows
/// the media type, so a saved file opens in the right program and the
/// attachment row reads as something rather than as a blank.
fn attachment_name(name: &str, mime: &str, content_id: Option<&str>) -> String {
    if !name.is_empty() {
        return name.to_string();
    }
    let mime = mime.split([';', ' ']).next().unwrap_or("").trim();
    let (kind, subtype) = mime.split_once('/').unwrap_or(("application", "dat"));
    let extension = match subtype {
        "jpeg" => "jpg",
        "svg+xml" => "svg",
        "plain" => "txt",
        "msword" => "doc",
        other => other.split('+').next().unwrap_or("dat"),
    };
    let stem = match kind {
        "image" | "audio" | "video" | "text" => kind,
        _ => "attachment",
    };
    // A Content-ID is unique within the message, so two nameless images
    // do not both come out as "image.png".
    match content_id
        .map(|id| id.split('@').next().unwrap_or(id))
        .filter(|id| {
            !id.is_empty()
                && id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        }) {
        Some(id) => format!("{stem}-{id}.{extension}"),
        None => format!("{stem}.{extension}"),
    }
}

/// Whether a part holds iCalendar text. Gmail labels the inline part
/// `text/calendar`; Outlook sends the file as `application/ics` and
/// sometimes as `application/octet-stream` with an `.ics` name. Shared
/// with the structure path (`mailrs_gmail::structure::text_by_reference`),
/// which needs the same rule to know which by-reference part to fetch.
pub fn is_calendar(mime: &str, filename: &str) -> bool {
    mime == "text/calendar" || mime == "application/ics" || filename.to_ascii_lowercase().ends_with(".ics")
}

/// Which wrapper the message arrived in and which standard wrote it, read
/// off the top-level part alone. A signed part further down belongs to a
/// message somebody forwarded.
fn protection(top: &Part) -> Option<Protection> {
    let children: Vec<&str> = top.children.iter().map(|c| c.mime_type.as_str()).collect();
    let protocol = top.protocol.as_deref();
    match top.mime_type.as_str() {
        "multipart/signed" => wrapped(protocol, &children, Protection::Signed, PGP_SIGNATURE)
            .or_else(|| wrapped(protocol, &children, Protection::SmimeSigned, PKCS7_SIGNATURE)),
        "multipart/encrypted" => {
            wrapped(protocol, &children, Protection::Encrypted, PGP_ENCRYPTED)
        }
        // The blob shapes of S/MIME. The `x-` names are the ones mail
        // clients sent before the media types were registered.
        "application/pkcs7-mime" | "application/x-pkcs7-mime" => {
            match top.smime_type.as_deref()? {
                kind if kind.eq_ignore_ascii_case("signed-data") => Some(Protection::SmimeOpaque),
                kind if kind.eq_ignore_ascii_case("enveloped-data") => {
                    Some(Protection::SmimeEnveloped)
                }
                // Certificates travel this way too, and nothing here opens
                // one.
                _ => None,
            }
        }
        _ => None,
    }
}

const PGP_SIGNATURE: &str = "application/pgp-signature";
const PGP_ENCRYPTED: &str = "application/pgp-encrypted";
const PKCS7_SIGNATURE: &str = "application/pkcs7-signature";

/// `wrapper` when the part declares `wanted` as its protocol, or when a
/// sender who left the parameter out carries a part of that type instead.
fn wrapped(
    protocol: Option<&str>,
    children: &[&str],
    wrapper: Protection,
    wanted: &str,
) -> Option<Protection> {
    match protocol {
        Some(declared) => names(declared, wanted).then_some(wrapper),
        None => children.iter().any(|c| names(c, wanted)).then_some(wrapper),
    }
}

/// Whether a media type is the one wanted, under the registered name or
/// under the `x-` one that came before it.
fn names(declared: &str, wanted: &str) -> bool {
    let plain = |name: &str| name.trim().to_ascii_lowercase().replace("/x-", "/");
    plain(declared) == plain(wanted)
}

/// Drops the calendar part that sits beside the text when the same file
/// comes again as an attachment of its own, as Google Calendar sends it.
/// The list would otherwise show `invite.ics` twice.
fn drop_calendar_twin(body: &mut MessageBody) {
    let twin = |i: usize| {
        let a = &body.attachments[i];
        a.mime_type.starts_with("text/calendar")
            && body.attachments.iter().enumerate().any(|(j, other)| {
                j != i
                    && other.filename == a.filename
                    && !other.mime_type.starts_with("text/calendar")
                    && is_calendar(&other.mime_type, &other.filename)
            })
    };
    let drop: Vec<usize> = (0..body.attachments.len()).filter(|&i| twin(i)).collect();
    for i in drop.into_iter().rev() {
        body.attachments.remove(i);
    }
}
