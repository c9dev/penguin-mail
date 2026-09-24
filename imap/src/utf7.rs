//! IMAP's modified UTF-7 (RFC 3501 section 5.1.3), the form mailbox names
//! travel in. `Envoy&AOk-s` is how a server names "Envoyés".
//!
//! Written here rather than taken from a crate: the one crate for it,
//! `utf7-imap`, has had no release since 2022.

use base64::Engine;
use base64::alphabet::Alphabet;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};

/// Base64 with `,` in place of `/` and no padding, as RFC 3501 asks.
const ALPHABET: Alphabet =
    match Alphabet::new("ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+,") {
        Ok(alphabet) => alphabet,
        Err(_) => panic!("the modified base64 alphabet is malformed"),
    };

const MODIFIED_BASE64: GeneralPurpose = GeneralPurpose::new(
    &ALPHABET,
    GeneralPurposeConfig::new()
        .with_encode_padding(false)
        .with_decode_padding_mode(DecodePaddingMode::RequireNone),
);

/// The name a person reads for the server's `name`. A name that is not
/// valid modified UTF-7 comes back as the server sent it, since some
/// servers send raw UTF-8 and the person should still see something.
pub fn decode(name: &str) -> String {
    try_decode(name).unwrap_or_else(|| name.to_string())
}

fn try_decode(name: &str) -> Option<String> {
    let mut out = String::with_capacity(name.len());
    let mut rest = name;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let end = after.find('-')?;
        let encoded = &after[..end];
        if encoded.is_empty() {
            out.push('&');
        } else {
            let bytes = MODIFIED_BASE64.decode(encoded).ok()?;
            if bytes.len() % 2 != 0 {
                return None;
            }
            let units: Vec<u16> = bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| u16::from_be_bytes(*pair))
                .collect();
            out.push_str(&String::from_utf16(&units).ok()?);
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Some(out)
}

/// The server's form of `name`, for a mailbox the person names.
pub fn encode(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut pending: Vec<u16> = Vec::new();
    for c in name.chars() {
        if (' '..='~').contains(&c) {
            flush(&mut out, &mut pending);
            match c {
                '&' => out.push_str("&-"),
                c => out.push(c),
            }
        } else {
            let mut units = [0u16; 2];
            pending.extend_from_slice(c.encode_utf16(&mut units));
        }
    }
    flush(&mut out, &mut pending);
    out
}

/// Writes the characters waiting in `pending` as one `&...-` run.
fn flush(out: &mut String, pending: &mut Vec<u16>) {
    if pending.is_empty() {
        return;
    }
    let bytes: Vec<u8> = pending.iter().flat_map(|u| u.to_be_bytes()).collect();
    out.push('&');
    out.push_str(&MODIFIED_BASE64.encode(bytes));
    out.push('-');
    pending.clear();
}

#[cfg(test)]
mod tests {
    use super::{decode, encode};

    #[test]
    fn accented_names_decode() {
        assert_eq!(decode("Envoy&AOk-s"), "Envoyés");
        assert_eq!(decode("&AMk-l&AOk-ments"), "Éléments");
        assert_eq!(decode("&BB4EQgQ,BEAEMAQyBDsENQQ9BD0ESwQ1-"), "Отправленные");
    }

    #[test]
    fn an_ampersand_travels_as_ampersand_dash() {
        assert_eq!(decode("Tom &- Jerry"), "Tom & Jerry");
        assert_eq!(encode("Tom & Jerry"), "Tom &- Jerry");
    }

    #[test]
    fn encoding_then_decoding_gives_the_name_back() {
        for name in [
            "Wysłane",
            "Gesendete Objekte",
            "送信済み",
            "a&b",
            "📁 Files",
            "Inbox",
        ] {
            assert_eq!(decode(&encode(name)), name, "{name}");
        }
    }

    #[test]
    fn encoding_matches_the_rfc_example() {
        // RFC 3501 section 5.1.3.
        assert_eq!(
            encode("~peter/mail/台北/日本語"),
            "~peter/mail/&U,BTFw-/&ZeVnLIqe-"
        );
    }

    #[test]
    fn a_name_that_is_not_modified_utf7_comes_back_whole() {
        assert_eq!(decode("Broken &AOk"), "Broken &AOk");
        assert_eq!(decode("Já em UTF-8"), "Já em UTF-8");
    }
}
