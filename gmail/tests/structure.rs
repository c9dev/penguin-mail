use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use mailrs_gmail::model::MessagePart;
use mailrs_gmail::structure::{handles, parts_of, path_of, text_by_reference};
use serde_json::json;

fn part(value: serde_json::Value) -> MessagePart {
    serde_json::from_value(value).unwrap()
}

fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

#[test]
fn gmail_part_ids_become_imap_paths() {
    assert_eq!(path_of("", true), "");
    assert_eq!(path_of("", false), "1");
    assert_eq!(path_of("0", true), "1");
    assert_eq!(path_of("1", true), "2");
    assert_eq!(path_of("0.2", true), "1.3");
}

/// Google Calendar's invitation as `format=full` sends it: every named
/// part by reference, the calendar text among them.
fn google_invitation() -> MessagePart {
    part(json!({
        "partId": "",
        "mimeType": "multipart/mixed",
        "headers": [{"name": "Subject", "value": "Invitation: Design crit"}],
        "parts": [
            {"partId": "0", "mimeType": "multipart/alternative", "parts": [
                {"partId": "0.0", "mimeType": "text/plain",
                 "headers": [{"name": "Content-Type", "value": "text/plain; charset=\"UTF-8\""}],
                 "body": {"size": 21, "data": b64(b"You have been invited")}},
                {"partId": "0.1", "mimeType": "text/html",
                 "headers": [{"name": "Content-Type", "value": "text/html; charset=\"UTF-8\""}],
                 "body": {"size": 28, "data": b64(b"<p>You have been invited</p>")}},
                {"partId": "0.2", "mimeType": "text/calendar", "filename": "invite.ics",
                 "headers": [{"name": "Content-Type", "value": "text/calendar; charset=\"UTF-8\"; method=REQUEST"}],
                 "body": {"size": 300, "attachmentId": "ref-cal"}}
            ]},
            {"partId": "1", "mimeType": "application/ics", "filename": "invite.ics",
             "headers": [{"name": "Content-Disposition", "value": "attachment; filename=\"invite.ics\""}],
             "body": {"size": 300, "attachmentId": "ref-ics"}}
        ]
    }))
}

#[test]
fn the_part_tree_keeps_inline_text_and_names_every_part_by_path() {
    let parts = parts_of(&google_invitation());
    assert_eq!(parts.header("subject"), Some("Invitation: Design crit"));
    let text = parts.find("1.1").unwrap();
    assert_eq!(text.mime_type, "text/plain");
    assert_eq!(text.charset.as_deref(), Some("UTF-8"));
    assert_eq!(text.data.as_deref(), Some(&b"You have been invited"[..]));
    let file = parts.find("2").unwrap();
    assert_eq!(file.filename.as_deref(), Some("invite.ics"));
    assert!(file.attachment);
    assert_eq!(file.size, 300);
    assert_eq!(file.data, None);
}

#[test]
fn only_the_calendar_text_is_fetched_by_reference() {
    let payload = google_invitation();
    assert_eq!(text_by_reference(&payload), [("1.3".to_string(), "ref-cal".to_string())]);
    assert_eq!(
        handles(&payload),
        [
            ("1.3".to_string(), "ref-cal".to_string()),
            ("2".to_string(), "ref-ics".to_string()),
        ]
    );
}

#[test]
fn a_body_text_sent_by_reference_is_fetched_but_a_named_text_file_is_not() {
    let payload = part(json!({
        "partId": "", "mimeType": "multipart/mixed", "parts": [
            {"partId": "0", "mimeType": "text/html", "body": {"size": 90000, "attachmentId": "ref-html"}},
            {"partId": "1", "mimeType": "text/plain", "filename": "notes.txt",
             "body": {"size": 5000000, "attachmentId": "ref-notes"}}
        ]
    }));
    assert_eq!(text_by_reference(&payload), [("1".to_string(), "ref-html".to_string())]);
}
