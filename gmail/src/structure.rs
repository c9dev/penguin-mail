//! A message's parts as Gmail's `format=full` lists them, for a message
//! too large to fetch raw. Gmail sends a part's bytes inline or, for any
//! part with a file name and for large parts, by an attachment handle to
//! fetch with `attachments.get`. The parts come out in `mailrs-mime`'s
//! words, numbered as IMAP numbers them, so the same rules read them as
//! read a raw message.

use base64::Engine;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use mailrs_mime::{Part, Parts};

use crate::convert::find_header;
use crate::model::MessagePart;

const URL_SAFE: GeneralPurpose = GeneralPurpose::new(
    &base64::alphabet::URL_SAFE,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

/// IMAP's part number for Gmail's `partId`. Gmail numbers children from
/// 0 ("0.2"); IMAP from 1 ("1.3"). Gmail gives the root an empty id,
/// which IMAP calls "1" when the root is the message's one body.
pub fn path_of(part_id: &str, root_is_multipart: bool) -> String {
    if part_id.is_empty() {
        return match root_is_multipart {
            true => String::new(),
            false => "1".to_string(),
        };
    }
    part_id
        .split('.')
        .map(|n| n.parse::<usize>().map_or_else(|_| n.to_string(), |i| (i + 1).to_string()))
        .collect::<Vec<_>>()
        .join(".")
}

/// The message's parts, with the bytes Gmail sent inline.
pub fn parts_of(payload: &MessagePart) -> Parts {
    let multipart = is_multipart(payload);
    Parts {
        headers: payload
            .headers
            .iter()
            .map(|h| (h.name.clone(), h.value.clone()))
            .collect(),
        root: convert(payload, multipart),
    }
}

/// The text parts the body needs that Gmail sent by reference: the
/// calendar, which Google Calendar always names `invite.ics`, and a plain
/// or HTML body too large to send inline. A text file someone attached
/// stays a file and is fetched when opened.
pub fn text_by_reference(payload: &MessagePart) -> Vec<(String, String)> {
    let multipart = is_multipart(payload);
    let mut found = Vec::new();
    every(payload, &mut |part| {
        let mime = part.mime_type.to_ascii_lowercase();
        let is_file = !part.filename.is_empty()
            || find_header(part, "Content-Disposition")
                .is_some_and(|d| d.trim_start().to_ascii_lowercase().starts_with("attachment"));
        let wanted = mime == "text/calendar"
            || ((mime == "text/plain" || mime == "text/html") && !is_file);
        if let (true, None, Some(handle)) = (wanted, &part.body.data, &part.body.attachment_id) {
            found.push((path_of(&part.part_id, multipart), handle.clone()));
        }
    });
    found
}

/// The attachment handle of every part Gmail sent by reference, by path.
pub fn handles(payload: &MessagePart) -> Vec<(String, String)> {
    let multipart = is_multipart(payload);
    let mut found = Vec::new();
    every(payload, &mut |part| {
        if let Some(handle) = &part.body.attachment_id {
            found.push((path_of(&part.part_id, multipart), handle.clone()));
        }
    });
    found
}

fn is_multipart(part: &MessagePart) -> bool {
    part.mime_type.to_ascii_lowercase().starts_with("multipart/")
}

/// Calls `visit` on every part that is not a multipart, in order. A
/// nested message is one part, as the raw reader counts it.
fn every(part: &MessagePart, visit: &mut impl FnMut(&MessagePart)) {
    if is_multipart(part) {
        for child in &part.parts {
            every(child, visit);
        }
    } else {
        visit(part);
    }
}

fn convert(part: &MessagePart, root_multipart: bool) -> Part {
    let content_type = find_header(part, "Content-Type").unwrap_or_default();
    let data = part
        .body
        .data
        .as_deref()
        .and_then(|d| URL_SAFE.decode(d.trim()).ok());
    Part {
        path: path_of(&part.part_id, root_multipart),
        mime_type: part.mime_type.to_ascii_lowercase(),
        charset: param(content_type, "charset").map(str::to_string),
        protocol: param(content_type, "protocol").map(str::to_string),
        smime_type: param(content_type, "smime-type").map(str::to_string),
        filename: Some(part.filename.clone()).filter(|f| !f.is_empty()),
        content_id: find_header(part, "Content-ID")
            .map(|v| v.trim().trim_start_matches('<').trim_end_matches('>').to_string()),
        attachment: find_header(part, "Content-Disposition")
            .is_some_and(|d| d.trim_start().to_ascii_lowercase().starts_with("attachment")),
        size: data.as_ref().map_or(part.body.size, |d| d.len() as i64),
        data,
        children: match is_multipart(part) {
            true => part.parts.iter().map(|c| convert(c, root_multipart)).collect(),
            false => Vec::new(),
        },
    }
}

/// One parameter of a header value, without its quotes.
pub(crate) fn param<'a>(value: &'a str, name: &str) -> Option<&'a str> {
    value.split(';').skip(1).find_map(|parameter| {
        let (key, value) = parameter.split_once('=')?;
        key.trim()
            .eq_ignore_ascii_case(name)
            .then(|| value.trim().trim_matches('"'))
    })
}
