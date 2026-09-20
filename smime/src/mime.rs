//! Wrapping what gpgsm made of a part in the bodies RFC 8551 describes.
//!
//! What goes into a signature is the part in canonical form: CRLF
//! throughout, and every byte a mail server might rewrite put beyond its
//! reach. That is the same job under both standards, so
//! [`mailrs_pgp::mime::canonical`] does it here too rather than a second
//! copy that could drift from the one the OpenPGP path signs.
//!
//! CMS travels as bytes rather than as armor, so every part built here is
//! base64 and says so.

use mail_builder::encoders::Base64Encoder;

/// The `multipart/signed` body of RFC 8551: the part that was signed, then
/// the signature over it.
///
/// `part` goes in byte for byte, so it is the output of
/// [`mailrs_pgp::mime::canonical`] and the exact bytes the signature was
/// made over. `micalg` names the digest gpgsm used; `None` leaves the
/// parameter out, which beats naming the wrong one.
pub fn multipart_signed(part: &[u8], signature: &[u8], micalg: Option<&str>) -> Vec<u8> {
    let boundary = boundary();
    let micalg = match micalg {
        Some(name) => format!("micalg={name}; "),
        None => String::new(),
    };
    let mut out = format!(
        "Content-Type: multipart/signed; {micalg}\
         protocol=\"application/pkcs7-signature\";\r\n \
         boundary=\"{boundary}\"\r\n\r\n\
         --{boundary}\r\n"
    )
    .into_bytes();
    out.extend_from_slice(part);
    out.extend_from_slice(
        format!(
            "\r\n--{boundary}\r\n\
             Content-Type: application/pkcs7-signature; name=\"smime.p7s\"\r\n\
             Content-Transfer-Encoding: base64\r\n\
             Content-Disposition: attachment; filename=\"smime.p7s\"\r\n\r\n"
        )
        .as_bytes(),
    );
    out.extend_from_slice(&base64(signature));
    out.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    out
}

/// The enveloped body of RFC 8551: one part holding the ciphertext, with
/// nothing beside it. Whatever was encrypted, a signed entity included,
/// is inside those bytes.
pub fn enveloped(ciphertext: &[u8]) -> Vec<u8> {
    let mut out = b"Content-Type: application/pkcs7-mime; smime-type=enveloped-data;\r\n \
         name=\"smime.p7m\"\r\n\
         Content-Transfer-Encoding: base64\r\n\
         Content-Disposition: attachment; filename=\"smime.p7m\"\r\n\r\n"
        .to_vec();
    out.extend_from_slice(&base64(ciphertext));
    out
}

/// `bytes` as the body of a base64 part: lines of 76 characters ending in
/// CRLF, and a CRLF after the last of them.
pub fn base64(bytes: &[u8]) -> Vec<u8> {
    let mut out = Base64Encoder::new()
        .wrap_lines()
        .encode(bytes)
        .expect("base64 encoding writes to memory");
    if !out.ends_with(b"\r\n") {
        out.extend_from_slice(b"\r\n");
    }
    out
}

/// A boundary no part can hold by accident.
fn boundary() -> String {
    let mut bytes = [0u8; 12];
    rand::fill(&mut bytes);
    let mut out = String::from("=-=-smime");
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}
