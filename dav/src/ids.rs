//! Penguin Mail's id for a calendar resource, and back. An event's id is
//! its resource's name without `.ics`, which is what the copy and an
//! occurrence id (`<id>_<start>`) carry; a name without `.ics` keeps its
//! whole self behind a `~`, so the way back is never a guess. A name that
//! ends in `_` and eight or sixteen digits would read as an occurrence
//! id; servers name resources by UUID or UID, which never do.

/// The id of the resource at `href`.
pub fn resource_id(href: &str) -> String {
    let segment = path_of(href).trim_end_matches('/').rsplit('/').next().unwrap_or_default().to_string();
    match segment.strip_suffix(".ics") {
        Some(stem) if !stem.is_empty() && !stem.starts_with('~') => stem.to_string(),
        _ => format!("~{segment}"),
    }
}

/// The href of resource `id` in `collection`.
pub fn resource_href(collection: &str, id: &str) -> String {
    match id.strip_prefix('~') {
        Some(segment) => join(collection, segment),
        None => join(collection, &format!("{id}.ics")),
    }
}

/// `path` with every percent escape decoded, except `%2F`: a slash in a
/// name would split it into two segments. The server's `/me%40x.test/` and
/// the app's `/me@x.test/` both come out as the second, so the two compare
/// equal. Bytes that are not UTF-8 stay as the server wrote them.
pub fn decode_path(path: &str) -> String {
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let escaped = match bytes.get(at..at + 3) {
            Some([b'%', hi, lo]) => (hi.is_ascii_hexdigit() && lo.is_ascii_hexdigit()).then_some((*hi, *lo)),
            _ => None,
        };
        match escaped {
            Some((b'2', lo)) if lo.eq_ignore_ascii_case(&b'F') => {
                out.extend_from_slice(b"%2F");
                at += 3;
            }
            Some((hi, lo)) => {
                out.push(hex(hi) << 4 | hex(lo));
                at += 3;
            }
            None => {
                out.push(bytes[at]);
                at += 1;
            }
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| path.to_string())
}

fn hex(digit: u8) -> u8 {
    (digit as char).to_digit(16).unwrap_or(0) as u8
}

/// The canonical path as it goes on the wire: each character outside
/// RFC 3986's `pchar` encoded, and a kept `%2F` left alone.
pub fn encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut chars = path.char_indices();
    while let Some((at, c)) = chars.next() {
        if path[at..].starts_with("%2F") {
            out.push_str("%2F");
            chars.nth(1);
        } else if c.is_ascii_alphanumeric() || "-._~!$&'()*+,;=:@/".contains(c) {
            out.push(c);
        } else {
            let mut buf = [0; 4];
            for byte in c.encode_utf8(&mut buf).bytes() {
                out.push_str(&format!("%{byte:02X}"));
            }
        }
    }
    out
}

/// `href` as a canonical path: a server may answer a full URL where it was
/// asked a path, and both name the same resource, whichever way each
/// spells its escapes.
pub fn path_of(href: &str) -> String {
    match href.split_once("://") {
        Some((_, rest)) => match rest.find('/') {
            Some(at) => decode_path(&rest[at..]),
            None => "/".to_string(),
        },
        None => decode_path(href),
    }
}

/// `href` with the path decoded and the scheme, host and port left as
/// they were; a path-only href stays one.
pub fn canonical_href(href: &str) -> String {
    match href.split_once("://") {
        Some((scheme, rest)) => match rest.find('/') {
            Some(at) => format!("{scheme}://{}{}", &rest[..at], decode_path(&rest[at..])),
            None => href.to_string(),
        },
        None => decode_path(href),
    }
}

/// `href` as it goes on the wire: the path decoded, so either spelling
/// reads the same, then encoded again; the origin as it was.
pub fn wire_href(href: &str) -> String {
    match href.split_once("://") {
        Some((scheme, rest)) => match rest.find('/') {
            Some(at) => format!("{scheme}://{}{}", &rest[..at], encode_path(&decode_path(&rest[at..]))),
            None => href.to_string(),
        },
        None => encode_path(&decode_path(href)),
    }
}

/// `segment` under `collection`, with one slash between.
pub fn join(collection: &str, segment: &str) -> String {
    format!("{}/{}", collection.trim_end_matches('/'), segment.trim_start_matches('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ics_resource_is_named_by_its_stem() {
        assert_eq!(resource_id("/dav/cal/work/abc-123.ics"), "abc-123");
        assert_eq!(resource_href("/dav/cal/work/", "abc-123"), "/dav/cal/work/abc-123.ics");
    }

    #[test]
    fn a_resource_without_ics_keeps_its_whole_name_behind_a_tilde() {
        assert_eq!(resource_id("/dav/cal/work/abc"), "~abc");
        assert_eq!(resource_href("/dav/cal/work", "~abc"), "/dav/cal/work/abc");
    }

    #[test]
    fn a_full_url_and_its_path_name_one_resource() {
        assert_eq!(path_of("https://p12-caldav.icloud.com/1234/calendars/home/"), "/1234/calendars/home/");
        assert_eq!(path_of("/1234/calendars/home/"), "/1234/calendars/home/");
    }

    #[test]
    fn an_encoded_and_a_decoded_href_have_one_path() {
        assert_eq!(path_of("/me%40example.test/work/"), "/me@example.test/work/");
        assert_eq!(path_of("http://h:80/a%20b/%C3%A9.ics"), "/a b/\u{e9}.ics");
        assert_eq!(resource_id("/me%40x.test/work/e%201.ics"), "e 1");
    }

    #[test]
    fn a_slash_or_a_stray_percent_keeps_its_form() {
        assert_eq!(decode_path("/a%2fb%/c%zz"), "/a%2Fb%/c%zz");
    }

    #[test]
    fn the_wire_form_encodes_what_a_path_may_not_hold() {
        assert_eq!(encode_path("/me@example.test/a b/c%d?e#f.ics"), "/me@example.test/a%20b/c%25d%3Fe%23f.ics");
        assert_eq!(encode_path("/a%2Fb/\u{e9}"), "/a%2Fb/%C3%A9");
    }

    #[test]
    fn a_canonical_href_keeps_the_origin() {
        assert_eq!(canonical_href("https://h:443/me%40x/"), "https://h:443/me@x/");
        assert_eq!(canonical_href("/me%40x/"), "/me@x/");
    }
}
