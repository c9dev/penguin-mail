use mailrs_pgp::mime::{canonical, multipart_encrypted, multipart_signed};

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).expect("utf-8")
}

#[test]
fn every_line_ending_becomes_crlf() {
    let part = b"Content-Type: text/plain\n\nMeet at six.\nBring tea.\n";
    assert_eq!(
        text(&canonical(part)),
        "Content-Type: text/plain\r\n\r\nMeet at six.\r\nBring tea.\r\n"
    );
}

#[test]
fn a_lone_carriage_return_becomes_crlf_too() {
    assert_eq!(
        text(&canonical(b"Content-Type: text/plain\r\rone\rtwo\r")),
        "Content-Type: text/plain\r\n\r\none\r\ntwo\r\n"
    );
}

#[test]
fn a_line_ending_in_a_space_is_encoded_so_it_survives_the_trip() {
    // Trailing whitespace is what a mail server is most likely to change on
    // the way, and changing one byte is the difference between a signature
    // that verifies and one that does not.
    let part = b"Content-Type: text/plain\r\n\r\nMeet at six. \r\nBring tea.\r\n";
    let out = text(&canonical(part));
    assert!(
        out.contains("Content-Transfer-Encoding: quoted-printable"),
        "{out}"
    );
    assert!(out.contains("Meet at six.=20\r\n"), "{out}");
    assert!(out.ends_with("Bring tea.\r\n"), "{out}");
}

#[test]
fn a_tab_at_the_end_of_the_last_line_is_encoded_as_well() {
    let part = b"Content-Type: text/plain\r\n\r\nMeet at six.\t";
    let out = text(&canonical(part));
    assert!(out.ends_with("Meet at six.=09"), "{out}");
}

#[test]
fn text_above_ascii_is_encoded_so_a_seven_bit_hop_cannot_mangle_it() {
    let part = "Content-Type: text/plain; charset=utf-8\r\n\r\nDireção\r\n".as_bytes();
    let out = text(&canonical(part));
    assert!(
        out.contains("Content-Transfer-Encoding: quoted-printable"),
        "{out}"
    );
    assert!(out.contains("Dire=C3=A7=C3=A3o"), "{out}");
}

#[test]
fn a_body_that_is_already_encoded_is_left_as_it_is() {
    let part = b"Content-Type: image/png\r\n\
        Content-Transfer-Encoding: base64\r\n\r\niVBORw0KGgo=\r\n";
    let out = text(&canonical(part));
    assert_eq!(out.matches("Content-Transfer-Encoding").count(), 1, "{out}");
    assert!(out.ends_with("iVBORw0KGgo=\r\n"), "{out}");
}

#[test]
fn plain_text_with_nothing_to_protect_keeps_its_bytes() {
    let part = b"Content-Type: text/plain\r\n\r\nMeet at six.\r\n";
    assert_eq!(canonical(part), part);
}

#[test]
fn each_part_inside_a_multipart_is_protected_on_its_own() {
    let part = b"Content-Type: multipart/alternative; boundary=\"inner\"\r\n\
        \r\n\
        --inner\r\n\
        Content-Type: text/plain\r\n\
        \r\n\
        Meet at six. \r\n\
        --inner\r\n\
        Content-Type: text/html\r\n\
        \r\n\
        <p>Meet at six.</p>\r\n\
        --inner--\r\n";
    let out = text(&canonical(part));
    assert_eq!(out.matches("--inner\r\n").count(), 2, "{out}");
    assert!(out.ends_with("--inner--\r\n"), "{out}");
    assert!(out.contains("Meet at six.=20"), "{out}");
    assert!(
        out.contains("Content-Transfer-Encoding: quoted-printable"),
        "{out}"
    );
    // The part that needed nothing is untouched.
    assert!(out.contains("<p>Meet at six.</p>\r\n"), "{out}");
}

#[test]
fn an_old_content_transfer_encoding_header_is_replaced_not_doubled() {
    let part = b"Content-Type: text/plain\r\n\
        Content-Transfer-Encoding: 8bit\r\n\r\nMeet at six. \r\n";
    let out = text(&canonical(part));
    assert_eq!(out.matches("Content-Transfer-Encoding").count(), 1, "{out}");
    assert!(out.contains("Content-Transfer-Encoding: quoted-printable"));
}

#[test]
fn a_signed_body_names_the_protocol_and_holds_both_parts() {
    let part = b"Content-Type: text/plain\r\n\r\nMeet at six.";
    let body = text(&multipart_signed(
        part,
        b"-----BEGIN PGP SIGNATURE-----\r\n",
        Some("sha512"),
    ));

    let boundary = body
        .split("boundary=\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("a boundary")
        .to_string();
    assert!(
        body.starts_with("Content-Type: multipart/signed;"),
        "{body}"
    );
    assert!(body.contains("micalg=pgp-sha512"), "{body}");
    assert!(
        body.contains("protocol=\"application/pgp-signature\""),
        "{body}"
    );
    assert!(
        body.contains("Content-Type: application/pgp-signature"),
        "{body}"
    );
    // The signed bytes sit between the first two boundaries, untouched.
    let between = body
        .split(&format!("--{boundary}\r\n"))
        .nth(1)
        .expect("a first part");
    assert_eq!(between, "Content-Type: text/plain\r\n\r\nMeet at six.\r\n");
    assert!(body.ends_with(&format!("--{boundary}--\r\n")), "{body}");
}

#[test]
fn a_signed_body_leaves_micalg_out_when_the_digest_has_no_name() {
    let body = text(&multipart_signed(b"x", b"sig", None));
    assert!(!body.contains("micalg"), "{body}");
}

#[test]
fn an_encrypted_body_carries_the_version_part_and_the_ciphertext() {
    let body = text(&multipart_encrypted(
        b"-----BEGIN PGP MESSAGE-----\r\nabc\r\n",
    ));
    assert!(
        body.starts_with("Content-Type: multipart/encrypted;"),
        "{body}"
    );
    assert!(
        body.contains("protocol=\"application/pgp-encrypted\""),
        "{body}"
    );
    assert!(body.contains("Content-Type: application/pgp-encrypted\r\n"));
    assert!(body.contains("\r\nVersion: 1\r\n"), "{body}");
    assert!(
        body.contains("Content-Type: application/octet-stream"),
        "{body}"
    );
    assert!(
        body.contains("-----BEGIN PGP MESSAGE-----\r\nabc\r\n"),
        "{body}"
    );
}
