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
    let mut body = MessageBody {
        list_unsubscribe: find_header(payload, "List-Unsubscribe").map(|v| v.trim().to_string()),
        one_click_unsubscribe: find_header(payload, "List-Unsubscribe-Post")
            .is_some_and(|v| v.contains("One-Click")),
        ..MessageBody::default()
    };
    walk(payload, &mut body);
    body
}

fn walk(part: &MessagePart, body: &mut MessageBody) {
    // An invitation arrives as a `text/calendar` part beside the text and
    // the HTML, and again as an `.ics` file. The part beside the text
    // carries its content inline, so it is the one worth reading; the file
    // still shows up in the attachment list below.
    if body.calendar.is_none() && is_calendar(part) {
        body.calendar = decode_text(part).filter(|ics| ics.contains("BEGIN:VCALENDAR"));
    }
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

/// Whether a part holds iCalendar text. Gmail labels the inline part
/// `text/calendar`; Outlook sends the file as `application/ics` and
/// sometimes as `application/octet-stream` with an `.ics` name.
fn is_calendar(part: &MessagePart) -> bool {
    part.mime_type.eq_ignore_ascii_case("text/calendar")
        || part.mime_type.eq_ignore_ascii_case("application/ics")
        || part
            .mime_type
            .to_ascii_lowercase()
            .starts_with("text/calendar;")
        || part.filename.to_ascii_lowercase().ends_with(".ics")
}

fn decode_text(part: &MessagePart) -> Option<String> {
    let data = part.body.data.as_deref()?;
    let bytes = URL_SAFE_NO_PAD_INDIFFERENT.decode(data.trim()).ok()?;
    let charset = find_header(part, "Content-Type").and_then(charset_param);
    Some(decode_charset(&bytes, charset))
}

/// Text from `bytes`, read in the charset the part declares. The bytes win
/// over the label when they are valid UTF-8 and hold a character above
/// ASCII: plenty of mailers send UTF-8 under `iso-8859-1` or `us-ascii`,
/// and obeying the label is what turns "Direção" into "DireÃ§Ã£o". A part
/// that says UTF-8 but is not falls the other way, to windows-1252, rather
/// than showing a row of replacement characters.
pub fn decode_charset(bytes: &[u8], charset: Option<&str>) -> String {
    let declared = charset
        .and_then(|label| encoding_rs::Encoding::for_label(label.as_bytes()))
        .unwrap_or(encoding_rs::UTF_8);
    if declared != encoding_rs::UTF_8
        && let Ok(text) = std::str::from_utf8(bytes)
        && !text.is_ascii()
    {
        return text.to_string();
    }
    let (text, _, replaced) = declared.decode(bytes);
    if replaced && declared == encoding_rs::UTF_8 {
        return encoding_rs::WINDOWS_1252.decode(bytes).0.into_owned();
    }
    text.into_owned()
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
