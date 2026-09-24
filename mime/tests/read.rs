use base64::Engine;
use mailrs_domain::Protection;
use mailrs_mime::{Part, Parts, body, files, part, read};

/// A Google Calendar invitation as Gmail's `format=raw` hands it over: the
/// calendar inline in the alternative, and the same file again beside it.
/// Twin of the JSON payload in
/// `a_google_invitation_lists_its_ics_file_once` in gmail/tests/body.rs,
/// which builds the same invitation as a `format=full` part tree.
fn google_invitation() -> Vec<u8> {
    let ics = "BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:abc@google.com\r\nSUMMARY:Design crit\r\nDTSTART:20260310T090000Z\r\nDTEND:20260310T100000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    format!(
        "From: Ann <ann@example.com>\r\n\
         To: me@example.com\r\n\
         Subject: Invitation: Design crit\r\n\
         MIME-Version: 1.0\r\n\
         Content-Type: multipart/mixed; boundary=\"outer\"\r\n\
         \r\n\
         --outer\r\n\
         Content-Type: multipart/alternative; boundary=\"inner\"\r\n\
         \r\n\
         --inner\r\n\
         Content-Type: text/plain; charset=\"UTF-8\"\r\n\
         \r\n\
         You have been invited\r\n\
         --inner\r\n\
         Content-Type: text/html; charset=\"UTF-8\"\r\n\
         \r\n\
         <p>You have been invited</p>\r\n\
         --inner\r\n\
         Content-Type: text/calendar; charset=\"UTF-8\"; method=REQUEST\r\n\
         Content-Disposition: inline; filename=\"invite.ics\"\r\n\
         \r\n\
         {ics}\
         --inner--\r\n\
         --outer\r\n\
         Content-Type: application/ics; name=\"invite.ics\"\r\n\
         Content-Disposition: attachment; filename=\"invite.ics\"\r\n\
         Content-Transfer-Encoding: base64\r\n\
         \r\n\
         {b64}\r\n\
         --outer--\r\n",
        b64 = base64::engine::general_purpose::STANDARD.encode(ics),
    )
    .into_bytes()
}

#[test]
fn picks_html_and_text_from_an_alternative() {
    let raw = b"Content-Type: multipart/alternative; boundary=b\r\n\r\n\
--b\r\nContent-Type: text/plain\r\n\r\nHello\r\n\
--b\r\nContent-Type: text/html\r\n\r\n<p>Hello</p>\r\n--b--\r\n";
    let body = read(raw);
    assert_eq!(body.text.as_deref(), Some("Hello"));
    assert_eq!(body.html.as_deref(), Some("<p>Hello</p>"));
    assert!(body.attachments.is_empty());
}

#[test]
fn decodes_non_utf8_charsets() {
    let raw = [
        b"Content-Type: text/plain; charset=\"ISO-8859-1\"\r\nContent-Transfer-Encoding: 8bit\r\n\r\n".as_slice(),
        &[0x63, 0x61, 0x66, 0xE9],
    ]
    .concat();
    assert_eq!(read(&raw).text.as_deref(), Some("café"));
}

#[test]
fn utf8_text_survives_a_mislabelled_charset() {
    for label in ["ISO-8859-1", "us-ascii", "windows-1252"] {
        let raw = format!(
            "Content-Type: text/plain; charset=\"{label}\"\r\n\r\nDireção de Recuperação de Crédito"
        );
        assert_eq!(
            read(raw.as_bytes()).text.as_deref(),
            Some("Direção de Recuperação de Crédito"),
            "a part labelled {label}"
        );
    }
}

#[test]
fn a_latin_1_part_labelled_utf8_still_reads() {
    let raw = [
        b"Content-Type: text/plain; charset=utf-8\r\n\r\n".as_slice(),
        &[0x63, 0x61, 0x66, 0xE9],
    ]
    .concat();
    assert_eq!(read(&raw).text.as_deref(), Some("café"));
}

#[test]
fn records_attachments_and_inline_content_ids() {
    let raw = b"Content-Type: multipart/mixed; boundary=b\r\n\r\n\
--b\r\nContent-Type: text/plain\r\n\r\nSee logo\r\n\
--b\r\nContent-Type: image/png\r\nContent-Disposition: attachment; filename=\"logo.png\"\r\n\
Content-ID: <logo@example.com>\r\nContent-Transfer-Encoding: base64\r\n\r\n\
iVBORw0KGgo=\r\n--b--\r\n";
    let body = read(raw);
    assert_eq!(body.text.as_deref(), Some("See logo"));
    assert_eq!(body.attachments.len(), 1);
    let a = &body.attachments[0];
    assert_eq!(a.part_id, "2");
    assert_eq!(a.filename, "logo.png");
    assert_eq!(a.mime_type, "image/png");
    assert_eq!(a.size, 8, "the decoded length, not the base64 text's");
    assert_eq!(a.attachment_id.as_deref(), Some("2"));
    assert_eq!(a.content_id.as_deref(), Some("logo@example.com"));
}

#[test]
fn accepts_padded_base64() {
    let raw = b"Content-Type: text/plain\r\nContent-Transfer-Encoding: base64\r\n\r\nSGk=\r\n";
    assert_eq!(read(raw).text.as_deref(), Some("Hi"));
}

#[test]
fn corrupt_data_is_skipped_rather_than_panicking() {
    let raw = b"Content-Type: text/plain\r\nContent-Transfer-Encoding: base64\r\n\r\n!!!\r\n";
    assert_eq!(read(raw).text, None);
}

#[test]
fn a_nameless_inline_image_is_still_an_attachment() {
    // Apple Mail, Outlook and mail_builder all send an image the HTML
    // shows with a Content-ID and no filename. Passing over it leaves the
    // reader with a cid: it cannot resolve and no row to fall back on.
    let raw = b"Content-Type: multipart/related; boundary=b\r\n\r\n\
--b\r\nContent-Type: text/html\r\n\r\n<img src=\"cid:img1@mailrs\">\r\n\
--b\r\nContent-Type: image/png\r\nContent-ID: <img1@mailrs>\r\nContent-Disposition: inline\r\n\
Content-Transfer-Encoding: base64\r\n\r\niVBORw0KGgo=\r\n--b--\r\n";
    let body = read(raw);
    assert_eq!(body.attachments.len(), 1);
    let image = &body.attachments[0];
    assert_eq!(image.content_id.as_deref(), Some("img1@mailrs"));
    assert_eq!(image.filename, "image-img1.png");
}

#[test]
fn a_nameless_file_takes_a_name_from_its_type() {
    let raw = b"Content-Type: multipart/mixed; boundary=b\r\n\r\n\
--b\r\nContent-Type: text/plain\r\n\r\nSee attached\r\n\
--b\r\nContent-Type: application/pdf\r\nContent-Disposition: attachment\r\n\
Content-Transfer-Encoding: base64\r\n\r\nJVBER\r\n\
--b\r\nContent-Type: image/jpeg\r\nContent-Disposition: attachment\r\n\
Content-Transfer-Encoding: base64\r\n\r\n/9k=\r\n--b--\r\n";
    let body = read(raw);
    let names: Vec<&str> = body.attachments.iter().map(|a| a.filename.as_str()).collect();
    assert_eq!(names, ["attachment.pdf", "image.jpg"]);
}

#[test]
fn the_two_readable_parts_never_count_as_attachments() {
    let raw = b"Content-Type: multipart/alternative; boundary=b\r\n\r\n\
--b\r\nContent-Type: text/plain\r\n\r\nHello\r\n\
--b\r\nContent-Type: text/html\r\n\r\n<p>Hello</p>\r\n--b--\r\n";
    assert!(read(raw).attachments.is_empty());
}

#[test]
fn a_signed_message_says_which_wrapper_it_arrived_in() {
    let raw = b"Content-Type: multipart/signed; micalg=pgp-sha256; \
protocol=\"application/pgp-signature\"; boundary=b\r\n\r\n\
--b\r\nContent-Type: text/plain\r\n\r\nMeet at six.\r\n\
--b\r\nContent-Type: application/pgp-signature\r\nContent-Disposition: attachment; filename=\"signature.asc\"\r\n\r\n\
sig\r\n--b--\r\n";
    assert_eq!(read(raw).protection, Some(Protection::Signed));
}

#[test]
fn an_encrypted_message_says_which_wrapper_it_arrived_in() {
    let raw = b"Content-Type: multipart/encrypted; protocol=\"application/pgp-encrypted\"; boundary=b\r\n\r\n\
--b\r\nContent-Type: application/pgp-encrypted\r\n\r\nVersion: 1\r\n\
--b\r\nContent-Type: application/octet-stream\r\nContent-Disposition: attachment; filename=\"encrypted.asc\"\r\n\r\n\
data\r\n--b--\r\n";
    let body = read(raw);
    assert_eq!(body.protection, Some(Protection::Encrypted));
    // The control part that carries the version string is structural,
    // not a file: only the ciphertext is something to save.
    let names: Vec<&str> = body.attachments.iter().map(|a| a.filename.as_str()).collect();
    assert_eq!(names, ["encrypted.asc"]);
}

#[test]
fn a_signature_that_is_smime_says_so_rather_than_openpgp() {
    let raw = b"Content-Type: multipart/signed; protocol=\"application/pkcs7-signature\"; boundary=b\r\n\r\n\
--b\r\nContent-Type: text/plain\r\n\r\nMeet at six.\r\n\
--b\r\nContent-Type: application/pkcs7-signature\r\nContent-Disposition: attachment; filename=\"smime.p7s\"\r\n\r\n\
sig\r\n--b--\r\n";
    assert_eq!(read(raw).protection, Some(Protection::SmimeSigned));
}

#[test]
fn a_message_inside_its_own_signature_says_which_shape_it_is() {
    let raw = b"Content-Type: application/pkcs7-mime; smime-type=signed-data; name=\"smime.p7m\"\r\n\
Content-Disposition: attachment; filename=\"smime.p7m\"\r\n\r\ndata\r\n";
    assert_eq!(read(raw).protection, Some(Protection::SmimeOpaque));
}

#[test]
fn an_enveloped_message_says_which_wrapper_it_arrived_in() {
    let raw = b"Content-Type: application/pkcs7-mime; smime-type=enveloped-data; name=\"smime.p7m\"\r\n\
Content-Disposition: attachment; filename=\"smime.p7m\"\r\n\r\ndata\r\n";
    assert_eq!(read(raw).protection, Some(Protection::SmimeEnveloped));
}

#[test]
fn the_older_names_for_the_smime_parts_count_too() {
    let raw = b"Content-Type: application/x-pkcs7-mime; smime-type=enveloped-data\r\n\
Content-Disposition: attachment; filename=\"smime.p7m\"\r\n\r\ndata\r\n";
    assert_eq!(read(raw).protection, Some(Protection::SmimeEnveloped));
}

#[test]
fn a_pkcs7_part_that_says_nothing_about_its_kind_is_left_alone() {
    // Certificates travel this way too, and a blob that names neither a
    // signature nor an envelope is nothing to open.
    let raw = b"Content-Type: application/pkcs7-mime; smime-type=certs-only\r\n\
Content-Disposition: attachment; filename=\"smime.p7c\"\r\n\r\ndata\r\n";
    assert_eq!(read(raw).protection, None);
}

#[test]
fn a_sender_who_left_the_protocol_out_is_judged_by_its_parts() {
    let raw = b"Content-Type: multipart/signed; boundary=b\r\n\r\n\
--b\r\nContent-Type: text/plain\r\n\r\nMeet at six.\r\n\
--b\r\nContent-Type: application/pgp-signature\r\nContent-Disposition: attachment; filename=\"signature.asc\"\r\n\r\n\
sig\r\n--b--\r\n";
    assert_eq!(read(raw).protection, Some(Protection::Signed));

    let raw = b"Content-Type: multipart/signed; boundary=b\r\n\r\n\
--b\r\nContent-Type: text/plain\r\n\r\nMeet at six.\r\n\
--b\r\nContent-Type: application/pkcs7-signature\r\nContent-Disposition: attachment; filename=\"smime.p7s\"\r\n\r\n\
sig\r\n--b--\r\n";
    assert_eq!(read(raw).protection, Some(Protection::SmimeSigned));
}

#[test]
fn a_signed_message_somebody_forwarded_is_not_this_message() {
    let raw = b"Content-Type: multipart/mixed; boundary=b\r\n\r\n\
--b\r\nContent-Type: text/plain\r\n\r\nLook at this\r\n\
--b\r\nContent-Type: multipart/signed; protocol=\"application/pgp-signature\"; boundary=c\r\n\r\n\
--c\r\nContent-Type: text/plain\r\n\r\nMeet at six.\r\n\
--c\r\nContent-Type: application/pgp-signature\r\nContent-Disposition: attachment; filename=\"signature.asc\"\r\n\r\n\
sig\r\n--c--\r\n\
--b--\r\n";
    assert_eq!(read(raw).protection, None);
}

/// LinkedIn gives the text and the HTML of its mail a Content-ID each and
/// no name. They are the message, not files beside it.
#[test]
fn text_parts_with_a_content_id_and_no_name_are_the_body() {
    let raw = b"Content-Type: multipart/alternative; boundary=b\r\n\r\n\
--b\r\nContent-Type: text/plain\r\nContent-ID: <text-body>\r\n\r\nYou have new invitations\r\n\
--b\r\nContent-Type: text/html\r\nContent-ID: <html-body>\r\n\r\n<p>You have new invitations</p>\r\n--b--\r\n";
    let body = read(raw);
    assert_eq!(body.html.as_deref(), Some("<p>You have new invitations</p>"));
    assert_eq!(body.text.as_deref(), Some("You have new invitations"));
    assert!(body.attachments.is_empty());
}

#[test]
fn a_named_text_file_with_a_content_id_stays_an_attachment() {
    let raw = b"Content-Type: multipart/mixed; boundary=b\r\n\r\n\
--b\r\nContent-Type: text/plain\r\n\r\nNotes attached\r\n\
--b\r\nContent-Type: text/plain\r\nContent-Disposition: attachment; filename=\"notes.txt\"\r\n\
Content-ID: <notes>\r\n\r\nthe notes\r\n--b--\r\n";
    let body = read(raw);
    assert_eq!(body.text.as_deref(), Some("Notes attached"));
    assert_eq!(body.attachments.len(), 1);
}

/// RFC 3156 puts two parts in a `multipart/signed`: the one that was
/// signed and the signature over it. A third part is something nobody
/// signed, and drawing it under the signature card would lend it the
/// signer's name.
#[test]
fn only_the_signed_part_of_a_signed_message_is_read() {
    let raw = b"Content-Type: multipart/signed; micalg=pgp-sha256; \
protocol=\"application/pgp-signature\"; boundary=b\r\n\r\n\
--b\r\nContent-Type: text/plain\r\n\r\nMeet at six.\r\n\
--b\r\nContent-Type: application/pgp-signature\r\nContent-Disposition: attachment; filename=\"signature.asc\"\r\n\r\n\
sig\r\n\
--b\r\nContent-Type: text/html\r\n\r\n<p>Pay Mallory.</p>\r\n\
--b\r\nContent-Type: application/pdf\r\nContent-Disposition: attachment; filename=\"invoice.pdf\"\r\n\
Content-Transfer-Encoding: base64\r\n\r\nJVBER\r\n\
--b--\r\n";
    let body = read(raw);
    assert_eq!(body.protection, Some(Protection::Signed));
    assert_eq!(body.text.as_deref(), Some("Meet at six."));
    assert_eq!(body.html, None, "the unsigned HTML stays out");
    assert!(body.attachments.is_empty(), "{:?}", body.attachments);
}

#[test]
fn a_google_invitation_lists_its_ics_file_once() {
    let body = read(&google_invitation());
    let files: Vec<(&str, &str)> = body
        .attachments
        .iter()
        .map(|a| (a.filename.as_str(), a.mime_type.as_str()))
        .collect();
    assert_eq!(files, [("invite.ics", "application/ics")]);
    assert!(body.calendar.as_deref().is_some_and(|c| c.contains("UID:abc@google.com")));
    assert_eq!(body.text.as_deref(), Some("You have been invited"));
}

#[test]
fn a_calendar_part_arrives_inline_in_a_raw_message() {
    let raw = b"Content-Type: multipart/alternative; boundary=b\r\n\r\n\
--b\r\nContent-Type: text/plain\r\n\r\nInvitation\r\n\
--b\r\nContent-Type: text/calendar\r\nContent-Disposition: attachment; filename=\"invite.ics\"\r\n\r\n\
BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n--b--\r\n";
    let body = read(raw);
    assert!(body.calendar.as_deref().is_some_and(|c| c.contains("BEGIN:VCALENDAR")));
    let names: Vec<&str> = body.attachments.iter().map(|a| a.filename.as_str()).collect();
    assert_eq!(names, ["invite.ics"]);
}

#[test]
fn files_are_named_by_their_part_paths() {
    let body = read(&google_invitation());
    assert_eq!(body.attachments[0].part_id, "2");
    assert_eq!(body.attachments[0].attachment_id.as_deref(), Some("2"));
    let ics = part(&google_invitation(), "2").unwrap();
    assert!(String::from_utf8(ics).unwrap().starts_with("BEGIN:VCALENDAR"));
    assert_eq!(part(&google_invitation(), "1.3").map(|b| b.starts_with(b"BEGIN:VCALENDAR")), Some(true));
    assert_eq!(part(&google_invitation(), "9"), None);
}

#[test]
fn a_message_that_is_not_multipart_has_its_body_at_one() {
    let raw = b"Subject: Hi\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nHello";
    assert_eq!(read(raw).text.as_deref(), Some("Hello"));
    assert_eq!(part(raw, "1").as_deref(), Some(&b"Hello"[..]));
}

#[test]
fn the_line_break_before_a_boundary_is_not_text() {
    let raw = b"Content-Type: multipart/alternative; boundary=b\r\n\r\n\
--b\r\nContent-Type: text/plain\r\n\r\nHello\r\n--b\r\nContent-Type: text/html\r\n\r\n<p>Hello</p>\r\n--b--\r\n";
    let body = read(raw);
    assert_eq!(body.text.as_deref(), Some("Hello"));
    assert_eq!(body.html.as_deref(), Some("<p>Hello</p>"));
}

#[test]
fn files_come_in_the_order_they_are_listed() {
    let body = read(&google_invitation());
    let bytes = files(&google_invitation());
    assert_eq!(bytes.len(), body.attachments.len());
    assert_eq!(bytes[0].len() as i64, body.attachments[0].size);
}

#[test]
fn an_unnamed_calendar_part_is_read_and_not_listed() {
    let raw = b"Content-Type: multipart/alternative; boundary=b\r\n\r\n\
--b\r\nContent-Type: text/plain\r\n\r\nMeet?\r\n\
--b\r\nContent-Type: text/calendar; method=REQUEST\r\n\r\nBEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n--b--\r\n";
    let body = read(raw);
    assert!(body.calendar.is_some());
    assert!(body.attachments.is_empty());
}

#[test]
fn unsubscribe_headers_are_unfolded_and_trimmed() {
    let raw = b"List-Unsubscribe: <mailto:off@example.com>,\r\n <https://example.com/off>\r\nList-Unsubscribe-Post: List-Unsubscribe=One-Click\r\n\r\nbody";
    let body = read(raw);
    assert_eq!(
        body.list_unsubscribe.as_deref(),
        Some("<mailto:off@example.com>, <https://example.com/off>")
    );
    assert!(body.one_click_unsubscribe);
}

#[test]
fn a_multibyte_charset_reads() {
    // "日本" in Shift_JIS.
    let raw = b"Content-Type: text/plain; charset=Shift_JIS\r\n\r\n\x93\xfa\x96\x7b";
    assert_eq!(read(raw).text.as_deref(), Some("日本"));
}

/// On the structure path a file arrives without its bytes. It is listed
/// all the same, with the size its source gave and its part path.
#[test]
fn a_file_without_its_bytes_is_still_listed() {
    let leaf = |path: &str, mime: &str| Part {
        path: path.into(),
        mime_type: mime.into(),
        charset: None,
        protocol: None,
        smime_type: None,
        filename: None,
        content_id: None,
        attachment: false,
        size: 0,
        data: None,
        children: vec![],
    };
    let text = Part {
        charset: Some("utf-8".into()),
        size: 5,
        data: Some(b"Hello".to_vec()),
        ..leaf("1", "text/plain")
    };
    let pdf = Part {
        filename: Some("plan.pdf".into()),
        attachment: true,
        size: 25 << 20,
        ..leaf("2", "application/pdf")
    };
    let parts = Parts {
        headers: vec![],
        root: Part {
            children: vec![text, pdf],
            ..leaf("", "multipart/mixed")
        },
    };
    let body = body(&parts);
    assert_eq!(body.text.as_deref(), Some("Hello"));
    assert_eq!(body.attachments.len(), 1);
    assert_eq!(body.attachments[0].part_id, "2");
    assert_eq!(body.attachments[0].size, 25 << 20);
}

/// Gmail's own structural parts, and a bounce's, never show up as files:
/// the PGP control part that names the version, and the delivery-status
/// part a bounce carries, are not something to save. The returned
/// message the bounce quotes arrives as `message/rfc822`, walked into
/// for its own file rather than listed whole, the way Gmail expands a
/// forwarded message.
#[test]
fn a_bounce_lists_only_the_file_inside_the_returned_message() {
    let raw = b"Content-Type: multipart/report; report-type=delivery-status; boundary=r\r\n\r\n\
--r\r\nContent-Type: text/plain\r\n\r\nYour message could not be delivered.\r\n\
--r\r\nContent-Type: message/delivery-status\r\n\r\nReporting-MTA: dns; mail.example.com\r\nAction: failed\r\n\
--r\r\nContent-Type: message/rfc822\r\n\r\n\
Content-Type: application/pdf\r\nContent-Disposition: attachment; filename=\"report.pdf\"\r\nContent-Transfer-Encoding: base64\r\n\r\nJVBER\r\n\
--r--\r\n";
    let body = read(raw);
    let names: Vec<(&str, &str)> = body
        .attachments
        .iter()
        .map(|a| (a.filename.as_str(), a.part_id.as_str()))
        .collect();
    assert_eq!(names, [("report.pdf", "3.1")]);
}

/// RFC 2045 forbids a transfer encoding on `message/rfc822`, but a sender
/// out there sets one anyway. mail-parser then decodes the part's body
/// before parsing the nested message, so that nested message's own part
/// offsets point into the decoded bytes, not into the outer raw message.
/// Reading them from the outer bytes would return the wrong slice, or an
/// out-of-range one.
#[test]
fn a_message_rfc822_part_with_a_transfer_encoding_still_reads_its_file() {
    let nested = b"Content-Type: application/pdf\r\nContent-Disposition: attachment; filename=\"report.pdf\"\r\nContent-Transfer-Encoding: base64\r\n\r\nUERGLUJZVEVT\r\n";
    let raw = format!(
        "Content-Type: multipart/mixed; boundary=r\r\n\r\n\
         --r\r\nContent-Type: text/plain\r\n\r\nSee the attached report.\r\n\
         --r\r\nContent-Type: message/rfc822\r\nContent-Transfer-Encoding: base64\r\n\r\n\
         {b64}\r\n--r--\r\n",
        b64 = base64::engine::general_purpose::STANDARD.encode(nested),
    )
    .into_bytes();
    let body = read(&raw);
    assert_eq!(body.text.as_deref(), Some("See the attached report."));
    let names: Vec<(&str, &str)> = body
        .attachments
        .iter()
        .map(|a| (a.filename.as_str(), a.part_id.as_str()))
        .collect();
    assert_eq!(names, [("report.pdf", "2.1")]);
    assert_eq!(part(&raw, "2.1").as_deref(), Some(b"PDF-BYTES".as_slice()));
}

/// mailers wrap base64 at some fixed width, and a broken one pads every
/// wrapped line instead of only the last. Each line still decodes on its
/// own; concatenating them is what a real reader gets right.
#[test]
fn base64_padded_line_by_line_still_decodes() {
    let raw = b"Content-Type: text/plain\r\nContent-Transfer-Encoding: base64\r\n\r\nSGk=\r\nSGk=\r\n";
    assert_eq!(read(raw).text.as_deref(), Some("HiHi"));
}

/// The last sextet of a padded group can carry bits beyond what two
/// bytes need; RFC 4648 calls them undefined, and some encoders leave
/// them non-zero. A strict decoder throws the whole part away over it.
#[test]
fn base64_with_non_zero_trailing_bits_still_decodes() {
    let raw = b"Content-Type: text/plain\r\nContent-Transfer-Encoding: base64\r\n\r\nSGl=\r\n";
    assert_eq!(read(raw).text.as_deref(), Some("Hi"));
}

/// A mailing list appends an unsubscribe line after the base64 body
/// without re-encoding it. The body up to that line is still good.
#[test]
fn a_footer_after_base64_does_not_lose_the_body() {
    let raw = b"Content-Type: text/plain\r\nContent-Transfer-Encoding: base64\r\n\r\n\
SGVsbG8=\r\nUnsubscribe: http://example.com/off\r\n";
    assert_eq!(read(raw).text.as_deref(), Some("Hello"));
}

/// A stray `=` not followed by two hex digits used to fail the whole
/// part, showing the raw quoted-printable text instead of the message.
/// The bad escape shows through as written; everything around it still
/// decodes.
#[test]
fn quoted_printable_with_a_bad_escape_decodes_the_rest() {
    let raw = b"Content-Type: text/plain\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\n\
Caf=E9 today =ZZ nice";
    let decoded = part(raw, "1").unwrap();
    assert_eq!(decoded, [b"Caf".as_slice(), &[0xE9], b" today =ZZ nice"].concat());
}

/// mail-parser accepts multiparts nested to any depth. A message nested
/// ten thousand levels deep must still read on a worker thread's 2 MiB
/// stack rather than overflow it and abort the app.
#[test]
fn a_deeply_nested_message_reads_on_a_small_stack() {
    let depth = 10_000;
    let mut raw = String::from("Subject: Deep\r\nContent-Type: multipart/mixed; boundary=\"b0\"\r\n\r\n");
    for level in 1..depth {
        raw.push_str(&format!("--b{}\r\nContent-Type: multipart/mixed; boundary=\"b{level}\"\r\n\r\n", level - 1));
    }
    raw.push_str(&format!("--b{}\r\nContent-Type: text/plain\r\n\r\nBottom\r\n", depth - 1));
    for level in (0..depth).rev() {
        raw.push_str(&format!("--b{level}--\r\n"));
    }
    let reader = std::thread::Builder::new()
        .stack_size(2 << 20)
        .spawn(move || {
            let body = read(raw.as_bytes());
            let parts = mailrs_mime::parts(raw.as_bytes()).unwrap();
            drop(parts);
            body
        })
        .unwrap();
    assert!(reader.join().is_ok(), "reading the message overflowed the stack");
}

/// A sender may wrap base64 at a width that is not a multiple of four,
/// so a line on its own does not decode to the bytes it carries. A file
/// wrapped at 70 columns must still come back byte for byte.
#[test]
fn base64_wrapped_at_seventy_columns_round_trips() {
    let pdf: Vec<u8> = (0..3000u32).map(|i| (i * 7 + i / 13) as u8).collect();
    let encoded = base64::engine::general_purpose::STANDARD.encode(&pdf);
    let wrapped: Vec<&str> = encoded
        .as_bytes()
        .chunks(70)
        .map(|line| std::str::from_utf8(line).unwrap())
        .collect();
    let raw = format!(
        "Content-Type: multipart/mixed; boundary=b\r\n\r\n\
         --b\r\nContent-Type: text/plain\r\n\r\nThe plan.\r\n\
         --b\r\nContent-Type: application/pdf; name=\"plan.pdf\"\r\n\
         Content-Transfer-Encoding: base64\r\n\r\n{}\r\n--b--\r\n",
        wrapped.join("\r\n"),
    );
    assert_eq!(part(raw.as_bytes(), "2"), Some(pdf));
}
