//! Getting a part into the shape that can be signed, and wrapping the
//! result in the bodies RFC 3156 describes.
//!
//! A signature covers bytes, and a mail part crosses machines that feel free
//! to rewrite some of them. Two rewrites matter: line endings, which the
//! next hop makes CRLF whatever it was given, and whitespace at the end of a
//! line, which servers strip. [`canonical`] does both changes first, while
//! the signature can still be made over the result. Skipping this produces
//! mail that looks signed and verifies nowhere.

use mail_builder::encoders::QuotedPrintableEncoder;

/// How far down a nested part this will rewrite. A message from the app's
/// own composer is two or three deep; anything past this is left alone
/// rather than followed.
const MAX_DEPTH: u8 = 8;

/// The part as it will be signed: CRLF throughout, and every byte a mail
/// server might rewrite put beyond its reach.
///
/// `part` is a whole MIME entity, its headers and a blank line and its body.
/// A part that is itself multipart is walked, so each part inside is
/// protected in its own right and the boundaries around them stay as they
/// were. A body that already arrived base64 or quoted-printable is left
/// alone, since nothing can be stripped out of either.
pub fn canonical(part: &[u8]) -> Vec<u8> {
    protect(part, 0)
}

fn protect(part: &[u8], depth: u8) -> Vec<u8> {
    let part = crlf(part);
    let Some(blank) = find(&part, b"\r\n\r\n") else {
        // Nothing here says where the headers stop, so the line endings are
        // all that can be put right.
        return part;
    };
    let (headers, rest) = part.split_at(blank);
    let body = &rest[4..];
    let content_type = header(headers, "content-type").unwrap_or_default();
    if content_type
        .trim_start()
        .to_ascii_lowercase()
        .starts_with("multipart/")
        && depth < MAX_DEPTH
        && let Some(boundary) = param(&content_type, "boundary")
    {
        let inside = subparts(body, boundary.as_bytes(), depth);
        let mut out = headers.to_vec();
        out.extend_from_slice(b"\r\n\r\n");
        out.extend_from_slice(&inside);
        return out;
    }
    let encoding = header(headers, "content-transfer-encoding")
        .unwrap_or_default()
        .to_ascii_lowercase();
    if matches!(encoding.as_str(), "base64" | "quoted-printable") || !at_risk(body) {
        return part;
    }
    let encoded = QuotedPrintableEncoder::new()
        .preserve_line_breaks()
        .encode(body)
        .expect("quoted-printable encoding writes to memory");
    let mut out = without_header(headers, "content-transfer-encoding");
    out.extend_from_slice(b"Content-Transfer-Encoding: quoted-printable\r\n\r\n");
    out.extend_from_slice(&encoded);
    out
}

/// Whether anything in `body` is worth encoding: whitespace at the end of a
/// line, which servers strip, or a byte above ASCII, which a seven-bit hop
/// would have to rewrite.
fn at_risk(body: &[u8]) -> bool {
    body.iter().enumerate().any(|(i, &byte)| match byte {
        b' ' | b'\t' => matches!(body.get(i + 1), None | Some(b'\r') | Some(b'\n')),
        _ => byte >= 0x80,
    })
}

/// The body of a multipart, with each part inside it protected. The
/// boundaries, the preamble, and the epilogue go back as they came.
fn subparts(body: &[u8], boundary: &[u8], depth: u8) -> Vec<u8> {
    let open = [b"--", boundary].concat();
    let close = [b"--", boundary, b"--"].concat();
    let mut out = Vec::with_capacity(body.len());
    let mut inside: Option<Vec<u8>> = None;
    let finish = |part: Vec<u8>, out: &mut Vec<u8>| {
        // The CRLF before a boundary belongs to the boundary, not to the
        // part, so it comes off before the part is rewritten and goes back
        // on after.
        let part = part.strip_suffix(b"\r\n").unwrap_or(&part).to_vec();
        out.extend_from_slice(&protect(&part, depth + 1));
        out.extend_from_slice(b"\r\n");
    };
    for line in lines(body) {
        let text = line.strip_suffix(b"\r\n").unwrap_or(line);
        if text == open || text == close {
            if let Some(part) = inside.take() {
                finish(part, &mut out);
            }
            out.extend_from_slice(line);
            inside = (text == open).then(Vec::new);
            continue;
        }
        match &mut inside {
            Some(part) => part.extend_from_slice(line),
            None => out.extend_from_slice(line),
        }
    }
    // A multipart whose closing boundary never arrived still holds a part.
    if let Some(part) = inside.take() {
        out.extend_from_slice(&protect(&part, depth + 1));
    }
    out
}

/// The `multipart/signed` body of RFC 3156: the part that was signed, then
/// the signature over it.
///
/// `part` goes in byte for byte, so it is the output of [`canonical`] and
/// the exact bytes the signature was made over. `micalg` names the digest
/// gpg used, without its `pgp-` prefix; `None` leaves the parameter out,
/// which beats naming the wrong one.
pub fn multipart_signed(part: &[u8], signature: &[u8], micalg: Option<&str>) -> Vec<u8> {
    let boundary = boundary();
    let micalg = match micalg {
        Some(name) => format!("micalg=pgp-{name}; "),
        None => String::new(),
    };
    let mut out = format!(
        "Content-Type: multipart/signed; {micalg}\
         protocol=\"application/pgp-signature\";\r\n \
         boundary=\"{boundary}\"\r\n\r\n\
         --{boundary}\r\n"
    )
    .into_bytes();
    out.extend_from_slice(part);
    out.extend_from_slice(
        format!(
            "\r\n--{boundary}\r\n\
             Content-Type: application/pgp-signature; name=\"signature.asc\"\r\n\
             Content-Description: OpenPGP digital signature\r\n\r\n"
        )
        .as_bytes(),
    );
    out.extend_from_slice(&ending_in_crlf(signature));
    out.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    out
}

/// The `multipart/encrypted` body of RFC 3156: the version part that says
/// which OpenPGP this is, then the ciphertext.
pub fn multipart_encrypted(ciphertext: &[u8]) -> Vec<u8> {
    let boundary = boundary();
    let mut out = format!(
        "Content-Type: multipart/encrypted;\r\n \
         protocol=\"application/pgp-encrypted\"; boundary=\"{boundary}\"\r\n\r\n\
         --{boundary}\r\n\
         Content-Type: application/pgp-encrypted\r\n\
         Content-Description: PGP/MIME version identification\r\n\r\n\
         Version: 1\r\n\
         --{boundary}\r\n\
         Content-Type: application/octet-stream; name=\"encrypted.asc\"\r\n\
         Content-Disposition: inline; filename=\"encrypted.asc\"\r\n\r\n"
    )
    .into_bytes();
    out.extend_from_slice(&ending_in_crlf(ciphertext));
    out.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    out
}

fn ending_in_crlf(bytes: &[u8]) -> Vec<u8> {
    let mut out = crlf(bytes);
    if !out.ends_with(b"\r\n") {
        out.extend_from_slice(b"\r\n");
    }
    out
}

/// A boundary no part can hold by accident.
fn boundary() -> String {
    let mut bytes = [0u8; 12];
    rand::fill(&mut bytes);
    let mut out = String::from("=-=-pgp");
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// `bytes` with every line ending made CRLF, whichever of the three it was.
fn crlf(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() + bytes.len() / 16);
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\r' => {
                out.extend_from_slice(b"\r\n");
                if bytes.get(i + 1) == Some(&b'\n') {
                    i += 1;
                }
            }
            b'\n' => out.extend_from_slice(b"\r\n"),
            byte => out.push(byte),
        }
        i += 1;
    }
    out
}

/// The value of one header, folded lines joined back together. The value is
/// only ever read, never written back, so bytes that are not UTF-8 lose
/// nothing by being replaced here.
fn header(headers: &[u8], name: &str) -> Option<String> {
    let text = String::from_utf8_lossy(headers)
        .replace("\r\n ", " ")
        .replace("\r\n\t", " ");
    text.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.trim()
            .eq_ignore_ascii_case(name)
            .then(|| value.trim().to_string())
    })
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

/// The headers with one of them, and the lines it folds onto, taken out.
fn without_header(headers: &[u8], name: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(headers.len());
    let mut dropping = false;
    for line in lines(headers) {
        if line
            .first()
            .is_some_and(|byte| *byte == b' ' || *byte == b'\t')
        {
            if dropping {
                continue;
            }
        } else {
            dropping = String::from_utf8_lossy(line)
                .split_once(':')
                .is_some_and(|(key, _)| key.trim().eq_ignore_ascii_case(name));
            if dropping {
                continue;
            }
        }
        out.extend_from_slice(line);
        if !out.ends_with(b"\r\n") {
            out.extend_from_slice(b"\r\n");
        }
    }
    out
}

/// The lines of `bytes`, each keeping the CRLF that ends it.
fn lines(bytes: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\n' {
            out.push(&bytes[start..=i]);
            start = i + 1;
        }
        i += 1;
    }
    if start < bytes.len() {
        out.push(&bytes[start..]);
    }
    out
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}
