use mailrs_smime::mime::{base64, enveloped, multipart_signed};

#[test]
fn a_signed_body_names_the_protocol_and_keeps_the_part_as_it_was() {
    let part = b"Content-Type: text/plain\r\n\r\nMeet at six.";
    let body = multipart_signed(part, b"\x30\x82signature", Some("sha-256"));
    let text = String::from_utf8_lossy(&body).into_owned();

    assert!(
        text.starts_with("Content-Type: multipart/signed; micalg=sha-256; "),
        "{text}"
    );
    assert!(
        text.contains("protocol=\"application/pkcs7-signature\""),
        "{text}"
    );
    assert!(
        text.contains("Content-Type: text/plain\r\n\r\nMeet at six.\r\n--"),
        "{text}"
    );
    assert!(
        text.contains("Content-Type: application/pkcs7-signature; name=\"smime.p7s\""),
        "{text}"
    );
    assert!(text.contains("Content-Transfer-Encoding: base64"), "{text}");
    assert!(text.ends_with("--\r\n"), "{text}");
}

#[test]
fn a_digest_with_no_name_leaves_the_parameter_out() {
    let body = multipart_signed(b"Content-Type: text/plain\r\n\r\nHello.", b"sig", None);
    let text = String::from_utf8_lossy(&body).into_owned();
    assert!(
        text.starts_with("Content-Type: multipart/signed; protocol="),
        "{text}"
    );
    assert!(!text.contains("micalg"), "{text}");
}

#[test]
fn an_enveloped_body_is_one_part_that_says_what_is_in_it() {
    let body = enveloped(b"\x30\x82ciphertext");
    let text = String::from_utf8_lossy(&body).into_owned();

    assert!(
        text.starts_with("Content-Type: application/pkcs7-mime; smime-type=enveloped-data;"),
        "{text}"
    );
    assert!(text.contains("name=\"smime.p7m\""), "{text}");
    assert!(
        text.contains("Content-Transfer-Encoding: base64\r\n"),
        "{text}"
    );
    assert!(text.contains("\r\n\r\nMIJjaXBoZXJ0ZXh0\r\n"), "{text}");
}

#[test]
fn base64_wraps_its_lines_and_ends_in_one_break() {
    let encoded = base64(&[b'a'; 200]);
    let text = String::from_utf8(encoded).expect("base64 is ascii");

    assert!(text.ends_with("\r\n"), "{text}");
    assert!(!text.ends_with("\r\n\r\n"), "{text}");
    for line in text.trim_end().split("\r\n") {
        assert!(line.len() <= 76, "a line of {} characters", line.len());
    }
}
