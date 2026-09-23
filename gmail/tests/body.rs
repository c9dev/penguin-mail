use base64::Engine;
use base64::engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD};
use mailrs_domain::Protection;
use mailrs_gmail::body::{charset_param, extract_body};
use mailrs_gmail::model::MessagePart;
use serde_json::json;

fn part(value: serde_json::Value) -> MessagePart {
    serde_json::from_value(value).unwrap()
}

fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

#[test]
fn picks_html_and_text_from_an_alternative() {
    let payload = part(json!({
        "mimeType": "multipart/alternative",
        "parts": [
            {"partId": "0", "mimeType": "text/plain", "body": {"data": b64(b"Hello")}},
            {"partId": "1", "mimeType": "text/html", "body": {"data": b64(b"<p>Hello</p>")}}
        ]
    }));
    let body = extract_body(&payload);
    assert_eq!(body.text.as_deref(), Some("Hello"));
    assert_eq!(body.html.as_deref(), Some("<p>Hello</p>"));
    assert!(body.attachments.is_empty());
}

#[test]
fn decodes_non_utf8_charsets() {
    let payload = part(json!({
        "mimeType": "text/plain",
        "headers": [{"name": "Content-Type", "value": "text/plain; charset=\"ISO-8859-1\""}],
        "body": {"data": b64(&[0x63, 0x61, 0x66, 0xE9])}
    }));
    assert_eq!(extract_body(&payload).text.as_deref(), Some("café"));
}

#[test]
fn utf8_text_survives_a_mislabelled_charset() {
    for label in ["ISO-8859-1", "us-ascii", "windows-1252"] {
        let payload = part(json!({
            "mimeType": "text/plain",
            "headers": [
                {"name": "Content-Type", "value": format!("text/plain; charset=\"{label}\"")}
            ],
            "body": {"data": b64("Direção de Recuperação de Crédito".as_bytes())}
        }));
        assert_eq!(
            extract_body(&payload).text.as_deref(),
            Some("Direção de Recuperação de Crédito"),
            "a part labelled {label}"
        );
    }
}

#[test]
fn a_latin_1_part_labelled_utf8_still_reads() {
    let payload = part(json!({
        "mimeType": "text/plain",
        "headers": [{"name": "Content-Type", "value": "text/plain; charset=utf-8"}],
        "body": {"data": b64(&[0x63, 0x61, 0x66, 0xE9])}
    }));
    assert_eq!(extract_body(&payload).text.as_deref(), Some("café"));
}

#[test]
fn records_attachments_and_inline_content_ids() {
    let payload = part(json!({
        "mimeType": "multipart/mixed",
        "parts": [
            {"partId": "0", "mimeType": "text/plain", "body": {"data": b64(b"See logo")}},
            {"partId": "1", "mimeType": "image/png", "filename": "logo.png",
             "headers": [{"name": "Content-ID", "value": "<logo@example.com>"}],
             "body": {"attachmentId": "att1", "size": 1234}}
        ]
    }));
    let body = extract_body(&payload);
    assert_eq!(body.text.as_deref(), Some("See logo"));
    assert_eq!(body.attachments.len(), 1);
    let a = &body.attachments[0];
    assert_eq!(a.part_id, "1");
    assert_eq!(a.filename, "logo.png");
    assert_eq!(a.mime_type, "image/png");
    assert_eq!(a.size, 1234);
    assert_eq!(a.attachment_id.as_deref(), Some("att1"));
    assert_eq!(a.content_id.as_deref(), Some("logo@example.com"));
}

#[test]
fn accepts_padded_base64() {
    let payload = part(json!({"mimeType": "text/plain", "body": {"data": URL_SAFE.encode(b"Hi")}}));
    assert_eq!(extract_body(&payload).text.as_deref(), Some("Hi"));
}

#[test]
fn corrupt_data_is_skipped_rather_than_panicking() {
    let payload = part(json!({"mimeType": "text/plain", "body": {"data": "!!!not base64!!!"}}));
    assert_eq!(extract_body(&payload).text, None);
}

#[test]
fn charset_parameter_parsing() {
    assert_eq!(
        charset_param(r#"text/plain; format=flowed; charset="utf-8""#),
        Some("utf-8")
    );
    assert_eq!(
        charset_param("text/plain; CHARSET=windows-1252"),
        Some("windows-1252")
    );
    assert_eq!(charset_param("text/plain"), None);
}

#[test]
fn a_nameless_inline_image_is_still_an_attachment() {
    // Apple Mail, Outlook and mail_builder all send an image the HTML
    // shows with a Content-ID and no filename. Passing over it leaves the
    // reader with a cid: it cannot resolve and no row to fall back on.
    let payload = part(json!({
        "mimeType": "multipart/related",
        "parts": [
            {"partId": "0", "mimeType": "text/html",
             "body": {"data": b64(b"<img src=\"cid:img1@mailrs\">")}},
            {"partId": "1", "mimeType": "image/png", "filename": "",
             "headers": [
                 {"name": "Content-ID", "value": "<img1@mailrs>"},
                 {"name": "Content-Disposition", "value": "inline"}
             ],
             "body": {"attachmentId": "att-1", "size": 4096}}
        ]
    }));
    let body = extract_body(&payload);
    assert_eq!(body.attachments.len(), 1);
    let image = &body.attachments[0];
    assert_eq!(image.content_id.as_deref(), Some("img1@mailrs"));
    assert_eq!(image.attachment_id.as_deref(), Some("att-1"));
    assert_eq!(image.filename, "image-img1.png");
}

#[test]
fn a_nameless_file_takes_a_name_from_its_type() {
    let payload = part(json!({
        "mimeType": "multipart/mixed",
        "parts": [
            {"partId": "0", "mimeType": "text/plain", "body": {"data": b64(b"See attached")}},
            {"partId": "1", "mimeType": "application/pdf", "filename": "",
             "headers": [{"name": "Content-Disposition", "value": "attachment"}],
             "body": {"attachmentId": "att-1", "size": 120}},
            {"partId": "2", "mimeType": "image/jpeg", "filename": "",
             "body": {"attachmentId": "att-2", "size": 240}}
        ]
    }));
    let body = extract_body(&payload);
    let names: Vec<&str> = body
        .attachments
        .iter()
        .map(|a| a.filename.as_str())
        .collect();
    assert_eq!(names, ["attachment.pdf", "image.jpg"]);
}

#[test]
fn the_two_readable_parts_never_count_as_attachments() {
    // Gmail hands back an attachmentId for a large text part. That is the
    // message, not a file.
    let payload = part(json!({
        "mimeType": "multipart/alternative",
        "parts": [
            {"partId": "0", "mimeType": "text/plain",
             "body": {"attachmentId": "att-1", "size": 900000}},
            {"partId": "1", "mimeType": "text/html",
             "body": {"data": b64(b"<p>Hello</p>")}}
        ]
    }));
    assert!(extract_body(&payload).attachments.is_empty());
}

#[test]
fn a_signed_message_says_which_wrapper_it_arrived_in() {
    let payload = part(json!({
        "mimeType": "multipart/signed",
        "headers": [{"name": "Content-Type", "value":
            "multipart/signed; micalg=pgp-sha256; protocol=\"application/pgp-signature\"; boundary=b"}],
        "parts": [
            {"partId": "0", "mimeType": "text/plain", "body": {"data": b64(b"Meet at six.")}},
            {"partId": "1", "mimeType": "application/pgp-signature", "filename": "signature.asc",
             "body": {"attachmentId": "att-1", "size": 480}}
        ]
    }));
    assert_eq!(extract_body(&payload).protection, Some(Protection::Signed));
}

#[test]
fn an_encrypted_message_says_which_wrapper_it_arrived_in() {
    let payload = part(json!({
        "mimeType": "multipart/encrypted",
        "headers": [{"name": "Content-Type", "value":
            "multipart/encrypted; protocol=\"application/pgp-encrypted\"; boundary=b"}],
        "parts": [
            {"partId": "0", "mimeType": "application/pgp-encrypted",
             "body": {"data": b64(b"Version: 1")}},
            {"partId": "1", "mimeType": "application/octet-stream", "filename": "encrypted.asc",
             "body": {"attachmentId": "att-1", "size": 900}}
        ]
    }));
    assert_eq!(
        extract_body(&payload).protection,
        Some(Protection::Encrypted)
    );
}

#[test]
fn a_signature_that_is_smime_says_so_rather_than_openpgp() {
    let payload = part(json!({
        "mimeType": "multipart/signed",
        "headers": [{"name": "Content-Type", "value":
            "multipart/signed; protocol=\"application/pkcs7-signature\"; boundary=b"}],
        "parts": [
            {"partId": "0", "mimeType": "text/plain", "body": {"data": b64(b"Meet at six.")}},
            {"partId": "1", "mimeType": "application/pkcs7-signature", "filename": "smime.p7s",
             "body": {"attachmentId": "att-1", "size": 480}}
        ]
    }));
    assert_eq!(
        extract_body(&payload).protection,
        Some(Protection::SmimeSigned)
    );
}

#[test]
fn a_message_inside_its_own_signature_says_which_shape_it_is() {
    let payload = part(json!({
        "mimeType": "application/pkcs7-mime",
        "headers": [{"name": "Content-Type", "value":
            "application/pkcs7-mime; smime-type=signed-data; name=\"smime.p7m\""}],
        "filename": "smime.p7m",
        "body": {"attachmentId": "att-1", "size": 4800}
    }));
    assert_eq!(
        extract_body(&payload).protection,
        Some(Protection::SmimeOpaque)
    );
}

#[test]
fn an_enveloped_message_says_which_wrapper_it_arrived_in() {
    let payload = part(json!({
        "mimeType": "application/pkcs7-mime",
        "headers": [{"name": "Content-Type", "value":
            "application/pkcs7-mime; smime-type=enveloped-data; name=\"smime.p7m\""}],
        "filename": "smime.p7m",
        "body": {"attachmentId": "att-1", "size": 4800}
    }));
    assert_eq!(
        extract_body(&payload).protection,
        Some(Protection::SmimeEnveloped)
    );
}

#[test]
fn the_older_names_for_the_smime_parts_count_too() {
    let payload = part(json!({
        "mimeType": "application/x-pkcs7-mime",
        "headers": [{"name": "Content-Type", "value":
            "application/x-pkcs7-mime; smime-type=enveloped-data"}],
        "filename": "smime.p7m",
        "body": {"attachmentId": "att-1", "size": 4800}
    }));
    assert_eq!(
        extract_body(&payload).protection,
        Some(Protection::SmimeEnveloped)
    );
}

#[test]
fn a_pkcs7_part_that_says_nothing_about_its_kind_is_left_alone() {
    // Certificates travel this way too, and a blob that names neither a
    // signature nor an envelope is nothing to open.
    let payload = part(json!({
        "mimeType": "application/pkcs7-mime",
        "headers": [{"name": "Content-Type", "value":
            "application/pkcs7-mime; smime-type=certs-only"}],
        "filename": "smime.p7c",
        "body": {"attachmentId": "att-1", "size": 480}
    }));
    assert_eq!(extract_body(&payload).protection, None);
}

#[test]
fn a_sender_who_left_the_protocol_out_is_judged_by_its_parts() {
    let payload = part(json!({
        "mimeType": "multipart/signed",
        "headers": [{"name": "Content-Type", "value": "multipart/signed; boundary=b"}],
        "parts": [
            {"partId": "0", "mimeType": "text/plain", "body": {"data": b64(b"Meet at six.")}},
            {"partId": "1", "mimeType": "application/pgp-signature", "filename": "signature.asc",
             "body": {"attachmentId": "att-1", "size": 480}}
        ]
    }));
    assert_eq!(extract_body(&payload).protection, Some(Protection::Signed));

    let smime = part(json!({
        "mimeType": "multipart/signed",
        "headers": [{"name": "Content-Type", "value": "multipart/signed; boundary=b"}],
        "parts": [
            {"partId": "0", "mimeType": "text/plain", "body": {"data": b64(b"Meet at six.")}},
            {"partId": "1", "mimeType": "application/pkcs7-signature", "filename": "smime.p7s",
             "body": {"attachmentId": "att-1", "size": 480}}
        ]
    }));
    assert_eq!(
        extract_body(&smime).protection,
        Some(Protection::SmimeSigned)
    );
}

#[test]
fn a_signed_message_somebody_forwarded_is_not_this_message() {
    let payload = part(json!({
        "mimeType": "multipart/mixed",
        "parts": [
            {"partId": "0", "mimeType": "text/plain", "body": {"data": b64(b"Look at this")}},
            {"partId": "1", "mimeType": "multipart/signed",
             "headers": [{"name": "Content-Type", "value":
                "multipart/signed; protocol=\"application/pgp-signature\"; boundary=c"}],
             "parts": [
                {"partId": "1.0", "mimeType": "text/plain", "body": {"data": b64(b"Meet at six.")}},
                {"partId": "1.1", "mimeType": "application/pgp-signature",
                 "filename": "signature.asc", "body": {"attachmentId": "att-1", "size": 480}}
             ]}
        ]
    }));
    assert_eq!(extract_body(&payload).protection, None);
}

/// LinkedIn gives the text and the HTML of its mail a Content-ID each and
/// no name. They are the message, not files beside it.
#[test]
fn text_parts_with_a_content_id_and_no_name_are_the_body() {
    let payload = part(json!({
        "mimeType": "multipart/alternative",
        "parts": [
            {"partId": "0", "mimeType": "text/plain",
             "headers": [{"name": "Content-ID", "value": "<text-body>"}],
             "body": {"data": b64(b"You have new invitations")}},
            {"partId": "1", "mimeType": "text/html",
             "headers": [{"name": "Content-ID", "value": "<html-body>"}],
             "body": {"data": b64(b"<p>You have new invitations</p>")}}
        ]
    }));
    let body = extract_body(&payload);
    assert_eq!(body.html.as_deref(), Some("<p>You have new invitations</p>"));
    assert_eq!(body.text.as_deref(), Some("You have new invitations"));
    assert!(body.attachments.is_empty());
}

#[test]
fn a_named_text_file_with_a_content_id_stays_an_attachment() {
    let payload = part(json!({
        "mimeType": "multipart/mixed",
        "parts": [
            {"partId": "0", "mimeType": "text/plain", "body": {"data": b64(b"Notes attached")}},
            {"partId": "1", "mimeType": "text/plain", "filename": "notes.txt",
             "headers": [{"name": "Content-ID", "value": "<notes>"}],
             "body": {"data": b64(b"the notes")}}
        ]
    }));
    let body = extract_body(&payload);
    assert_eq!(body.text.as_deref(), Some("Notes attached"));
    assert_eq!(body.attachments.len(), 1);
}

/// RFC 3156 puts two parts in a `multipart/signed`: the one that was
/// signed and the signature over it. A third part is something nobody
/// signed, and drawing it under the signature card would lend it the
/// signer's name.
#[test]
fn only_the_signed_part_of_a_signed_message_is_read() {
    let payload = part(json!({
        "mimeType": "multipart/signed",
        "headers": [{"name": "Content-Type", "value":
            "multipart/signed; micalg=pgp-sha256; protocol=\"application/pgp-signature\"; boundary=b"}],
        "parts": [
            {"partId": "0", "mimeType": "text/plain", "body": {"data": b64(b"Meet at six.")}},
            {"partId": "1", "mimeType": "application/pgp-signature", "filename": "signature.asc",
             "body": {"attachmentId": "att-1", "size": 480}},
            {"partId": "2", "mimeType": "text/html", "body": {"data": b64(b"<p>Pay Mallory.</p>")}},
            {"partId": "3", "mimeType": "application/pdf", "filename": "invoice.pdf",
             "body": {"attachmentId": "att-2", "size": 900}}
        ]
    }));
    let body = extract_body(&payload);
    assert_eq!(body.protection, Some(Protection::Signed));
    assert_eq!(body.text.as_deref(), Some("Meet at six."));
    assert_eq!(body.html, None, "the unsigned HTML stays out");
    assert!(body.attachments.is_empty(), "{:?}", body.attachments);
}
