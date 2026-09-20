//! Turns a `format=full` MIME part tree into displayable text, HTML, and an
//! attachment list.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD_INDIFFERENT;
use mailrs_domain::{Attachment, MessageBody, Protection};

use crate::convert::find_header;
use crate::model::MessagePart;

/// Takes the first `text/html` and first `text/plain` part found in document
/// order. Any part the reader has to fetch separately counts as an
/// attachment.
pub fn extract_body(payload: &MessagePart) -> MessageBody {
    let mut body = MessageBody {
        list_unsubscribe: find_header(payload, "List-Unsubscribe").map(|v| v.trim().to_string()),
        one_click_unsubscribe: find_header(payload, "List-Unsubscribe-Post")
            .is_some_and(|v| v.contains("One-Click")),
        protection: protection(payload),
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
    if is_attachment(part) {
        let content_id = find_header(part, "Content-ID").map(|v| {
            v.trim()
                .trim_start_matches('<')
                .trim_end_matches('>')
                .to_string()
        });
        body.attachments.push(Attachment {
            part_id: part.part_id.clone(),
            filename: attachment_name(part, content_id.as_deref()),
            mime_type: part.mime_type.clone(),
            size: part.body.size,
            attachment_id: part.body.attachment_id.clone(),
            content_id,
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

/// Whether the reader has to fetch this part on its own. A filename says
/// so outright. So does a `Content-ID`, which an image the HTML shows
/// carries and a filename often does not: `mail_builder` wrote one that
/// way until we made it name its parts, and Apple Mail and Outlook still
/// do. A part Gmail hands back with an `attachmentId` and no text is one
/// more, which is how a nameless PDF still reaches the attachment list.
fn is_attachment(part: &MessagePart) -> bool {
    if part
        .mime_type
        .to_ascii_lowercase()
        .starts_with("multipart/")
    {
        return false;
    }
    if !part.filename.is_empty() || find_header(part, "Content-ID").is_some() {
        return true;
    }
    let disposition = find_header(part, "Content-Disposition")
        .unwrap_or("")
        .trim_start()
        .to_ascii_lowercase();
    if disposition.starts_with("attachment") {
        return true;
    }
    part.body.attachment_id.is_some() && !is_text(part)
}

/// Whether a part is one of the two ways of reading the message itself.
fn is_text(part: &MessagePart) -> bool {
    let mime = part.mime_type.to_ascii_lowercase();
    mime.starts_with("text/plain") || mime.starts_with("text/html")
}

/// What to call a part that arrived without a name. The extension follows
/// the media type, so a saved file opens in the right program and the
/// attachment row reads as something rather than as a blank.
fn attachment_name(part: &MessagePart, content_id: Option<&str>) -> String {
    if !part.filename.is_empty() {
        return part.filename.clone();
    }
    let mime = part.mime_type.to_ascii_lowercase();
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

/// Which OpenPGP wrapper the message arrived in, read off the top-level
/// part alone. A signed part further down belongs to a message somebody
/// forwarded, and whatever it was signed over is not this message.
fn protection(payload: &MessagePart) -> Option<Protection> {
    let mime = payload.mime_type.to_ascii_lowercase();
    let (wrapper, protocol) = match mime.split(';').next().unwrap_or_default().trim() {
        "multipart/signed" => (Protection::Signed, "application/pgp-signature"),
        "multipart/encrypted" => (Protection::Encrypted, "application/pgp-encrypted"),
        _ => return None,
    };
    // S/MIME uses the same two media types, so the protocol parameter is
    // what tells the two apart. A sender who left it out is judged by the
    // part that carries the OpenPGP instead.
    match find_header(payload, "Content-Type").and_then(|value| param(value, "protocol")) {
        Some(declared) => declared.eq_ignore_ascii_case(protocol).then_some(wrapper),
        None => payload
            .parts
            .iter()
            .any(|part| part.mime_type.eq_ignore_ascii_case(protocol))
            .then_some(wrapper),
    }
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
    param(content_type, "charset")
}

/// One parameter of a header value, without its quotes.
fn param<'a>(value: &'a str, name: &str) -> Option<&'a str> {
    value.split(';').skip(1).find_map(|parameter| {
        let (key, value) = parameter.split_once('=')?;
        key.trim()
            .eq_ignore_ascii_case(name)
            .then(|| value.trim().trim_matches('"'))
    })
}
