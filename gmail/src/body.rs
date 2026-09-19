//! Turns a `format=full` MIME part tree into displayable text, HTML, and an
//! attachment list.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD_INDIFFERENT;
use mailrs_domain::{Attachment, MessageBody};

use crate::convert::find_header;
use crate::model::MessagePart;

/// Takes the first `text/html` and first `text/plain` part found in document
/// order. Any part with a filename counts as an attachment.
pub fn extract_body(payload: &MessagePart) -> MessageBody {
    let mut body = MessageBody::default();
    walk(payload, &mut body);
    body
}

fn walk(part: &MessagePart, body: &mut MessageBody) {
    if !part.filename.is_empty() {
        body.attachments.push(Attachment {
            part_id: part.part_id.clone(),
            filename: part.filename.clone(),
            mime_type: part.mime_type.clone(),
            size: part.body.size,
            attachment_id: part.body.attachment_id.clone(),
            content_id: find_header(part, "Content-ID").map(|v| {
                v.trim()
                    .trim_start_matches('<')
                    .trim_end_matches('>')
                    .to_string()
            }),
        });
        return;
    }
    if part.mime_type.eq_ignore_ascii_case("text/html") && body.html.is_none() {
        body.html = decode_text(part);
    } else if part.mime_type.eq_ignore_ascii_case("text/plain") && body.text.is_none() {
        body.text = decode_text(part);
    }
    for child in &part.parts {
        walk(child, body);
    }
}

fn decode_text(part: &MessagePart) -> Option<String> {
    let data = part.body.data.as_deref()?;
    let bytes = URL_SAFE_NO_PAD_INDIFFERENT.decode(data.trim()).ok()?;
    let charset = find_header(part, "Content-Type")
        .and_then(charset_param)
        .unwrap_or("utf-8");
    let encoding =
        encoding_rs::Encoding::for_label(charset.as_bytes()).unwrap_or(encoding_rs::UTF_8);
    let (text, _, _) = encoding.decode(&bytes);
    Some(text.into_owned())
}

/// The `charset` parameter of a `Content-Type` value, without quotes.
pub fn charset_param(content_type: &str) -> Option<&str> {
    content_type.split(';').skip(1).find_map(|param| {
        let (key, value) = param.split_once('=')?;
        key.trim()
            .eq_ignore_ascii_case("charset")
            .then(|| value.trim().trim_matches('"'))
    })
}
