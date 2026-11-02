//! The header block of a message and nothing else: the sender, the
//! recipients, the Message-ID, and a copy without the Bcc header. Sending
//! reads these from messages of tens of megabytes, so no part is decoded
//! and no body is copied.

use std::borrow::Cow;

use mail_parser::MessageParser;

use crate::address::parse_address_list;

/// The headers of a message, in order, with their folds undone.
#[derive(Debug, Default, Clone)]
pub struct Head {
    pub headers: Vec<(String, String)>,
}

/// Who a message goes from and to, read from its headers.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Envelope {
    /// The address in the first From header, if there is one.
    pub from: Option<String>,
    /// Every address in To, Cc and Bcc, sorted, once each.
    pub recipients: Vec<String>,
}

/// Where the header block of `raw` ends, after its blank line: the whole
/// of `raw` when it has none.
pub fn header_end(raw: &[u8]) -> usize {
    if raw.starts_with(b"\r\n") {
        return 2;
    }
    if raw.starts_with(b"\n") {
        return 1;
    }
    let mut at = 0;
    while let Some(nl) = raw[at..].iter().position(|b| *b == b'\n') {
        let line_end = at + nl + 1;
        let next = &raw[line_end..];
        if next.starts_with(b"\n") {
            return line_end + 1;
        }
        if next.starts_with(b"\r\n") {
            return line_end + 2;
        }
        at = line_end;
    }
    raw.len()
}

/// The headers of `raw`. Only the header block is parsed.
pub fn head(raw: &[u8]) -> Head {
    let block = &raw[..header_end(raw)];
    let headers = MessageParser::default()
        .parse(block)
        .map(|message| {
            message
                .headers()
                .iter()
                .map(|header| {
                    (
                        header.raw_name().to_string(),
                        unfold(&String::from_utf8_lossy(header.raw_value())),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    Head { headers }
}

fn unfold(value: &str) -> String {
    value
        .replace("\r\n", "")
        .replace('\n', "")
        .trim()
        .to_string()
}

impl Head {
    /// The first value of the header `name`, compared without case.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// The Message-ID without its angle brackets, `None` when absent or empty.
    pub fn message_id(&self) -> Option<String> {
        let id = self.header("Message-ID")?.trim_matches(['<', '>']).trim();
        (!id.is_empty()).then(|| id.to_string())
    }

    /// The sender and the recipients.
    pub fn envelope(&self) -> Envelope {
        let from = self
            .header("From")
            .map(parse_address_list)
            .unwrap_or_default()
            .into_iter()
            .next()
            .map(|a| a.email);
        let mut recipients: Vec<String> = self
            .headers
            .iter()
            .filter(|(name, _)| {
                ["To", "Cc", "Bcc"]
                    .iter()
                    .any(|h| name.eq_ignore_ascii_case(h))
            })
            .flat_map(|(_, value)| parse_address_list(value))
            .map(|a| a.email)
            .collect();
        recipients.sort();
        recipients.dedup();
        Envelope { from, recipients }
    }
}

/// The Message-ID of `raw` without its angle brackets.
pub fn message_id(raw: &[u8]) -> Option<String> {
    head(raw).message_id()
}

/// `raw` without the header `name` and the lines folded under it. Borrowed
/// as it is when the header is absent; otherwise one copy, with the body
/// appended once.
pub fn without_header<'a>(raw: &'a [u8], name: &str) -> Cow<'a, [u8]> {
    let (block, body) = raw.split_at(header_end(raw));
    // Obsolete syntax (RFC 5322 section 4.2) allows blanks before the colon.
    let named = |line: &[u8]| {
        line.len() > name.len()
            && line[..name.len()].eq_ignore_ascii_case(name.as_bytes())
            && line[name.len()..]
                .iter()
                .find(|b| !matches!(b, b' ' | b'\t'))
                == Some(&b':')
    };
    let mut skipping = false;
    let mut out: Option<Vec<u8>> = None;
    for line in block.split_inclusive(|b| *b == b'\n') {
        if !matches!(line.first(), Some(b' ' | b'\t')) {
            skipping = named(line);
        }
        match (skipping, out.as_mut()) {
            (true, None) => {
                let at = line.as_ptr() as usize - raw.as_ptr() as usize;
                let mut copy = Vec::with_capacity(raw.len() - line.len());
                copy.extend_from_slice(&raw[..at]);
                out = Some(copy);
            }
            (false, Some(copy)) => copy.extend_from_slice(line),
            _ => {}
        }
    }
    match out {
        None => Cow::Borrowed(raw),
        Some(mut copy) => {
            copy.extend_from_slice(body);
            Cow::Owned(copy)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MESSAGES: &[&[u8]] = &[
        b"From: Ann <ann@example.com>\r\nTo: bob@example.com, Cy <cy@example.com>\r\nMessage-ID: <a@x>\r\nSubject: Hi\r\n\r\nBody\r\n",
        b"from: ann@example.com\r\nTO: bob@example.com,\r\n cy@example.com\r\nCc: bob@example.com\r\nBcc: dee@example.com,\r\n\teve@example.com\r\nMessage-Id:\r\n <folded@x>\r\n\r\nBody\r\n",
        b"From: =?UTF-8?Q?Andr=C3=A9?= <andre@example.com>\r\nTo: =?UTF-8?B?w4lsaQ==?= <eli@example.com>\r\nSubject: =?UTF-8?Q?caf=C3=A9?=\r\n\r\nx",
        b"From: ann@example.com\r\nTo: team: bob@example.com, cy@example.com;, dee@example.com\r\n\r\nGroup",
        b"From: ann@example.com\nTo: bob@example.com\nBcc: z@example.com\n\nUnix line ends\n",
        b"To: bob@example.com\r\nSubject: no sender, no id\r\n\r\nbody",
        b"Subject: only headers, no blank line\r\nTo: a@example.com",
        b"",
    ];

    #[test]
    fn the_head_reads_what_the_full_parse_reads() {
        for raw in MESSAGES {
            let full = crate::parts(raw).unwrap_or_default();
            let head = head(raw);
            for name in ["From", "To", "Cc", "Bcc", "Message-ID", "Subject"] {
                assert_eq!(
                    head.header(name),
                    full.header(name),
                    "{name} in {:?}",
                    String::from_utf8_lossy(raw)
                );
            }
        }
    }

    #[test]
    fn the_envelope_is_the_sender_and_every_recipient_once() {
        let env = head(MESSAGES[1]).envelope();
        assert_eq!(env.from.as_deref(), Some("ann@example.com"));
        assert_eq!(
            env.recipients,
            [
                "bob@example.com",
                "cy@example.com",
                "dee@example.com",
                "eve@example.com"
            ]
        );
        assert_eq!(head(MESSAGES[5]).envelope().from, None);
        assert_eq!(head(MESSAGES[7]).envelope(), Envelope::default());
    }

    #[test]
    fn encoded_names_and_groups_give_their_addresses() {
        let env = head(MESSAGES[2]).envelope();
        assert_eq!(env.from.as_deref(), Some("andre@example.com"));
        assert_eq!(env.recipients, ["eli@example.com"]);
        let group = head(MESSAGES[3]).envelope();
        // Groups read as parse_address_list reads them, as before this module.
        let old = crate::parts(MESSAGES[3]).unwrap();
        let mut expected: Vec<String> = parse_address_list(old.header("To").unwrap())
            .into_iter()
            .map(|a| a.email)
            .collect();
        expected.sort();
        assert_eq!(group.recipients, expected);
    }

    #[test]
    fn the_message_id_loses_its_brackets_and_folds() {
        assert_eq!(message_id(MESSAGES[0]).as_deref(), Some("a@x"));
        assert_eq!(message_id(MESSAGES[1]).as_deref(), Some("folded@x"));
        assert_eq!(message_id(MESSAGES[5]), None);
        assert_eq!(message_id(b"Subject: Hi\r\n\r\nMessage-ID: <body@x>"), None);
    }

    #[test]
    fn removing_bcc_leaves_every_other_byte() {
        let raw = MESSAGES[1];
        let kept = without_header(raw, "Bcc");
        let text = String::from_utf8(kept.to_vec()).unwrap();
        assert!(!text.to_ascii_lowercase().contains("bcc"));
        assert!(!text.contains("eve@example.com"));
        let expected = String::from_utf8(raw.to_vec())
            .unwrap()
            .replace("Bcc: dee@example.com,\r\n\teve@example.com\r\n", "");
        assert_eq!(text, expected);
    }

    #[test]
    fn a_message_without_the_header_is_borrowed() {
        assert!(matches!(
            without_header(MESSAGES[0], "Bcc"),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn a_body_line_that_says_bcc_stays() {
        let raw = b"To: a@example.com\r\nBcc: b@example.com\r\n\r\nBcc: not a header\r\n";
        assert_eq!(
            &*without_header(raw, "bcc"),
            b"To: a@example.com\r\n\r\nBcc: not a header\r\n"
        );
    }

    #[test]
    fn the_first_and_last_header_lines_can_be_the_bcc() {
        let raw = b"Bcc: b@example.com\nTo: a@example.com\nBcc: c@example.com\n\nx";
        assert_eq!(&*without_header(raw, "Bcc"), b"To: a@example.com\n\nx");
    }

    #[test]
    fn blanks_before_the_colon_still_name_the_header() {
        let raw = b"To: a@example.com\r\nBcc : b@example.com\r\nBcc\t:c@example.com\r\nBcc \t : d@example.com\r\nSubject: s\r\n\r\nx";
        assert_eq!(
            &*without_header(raw, "Bcc"),
            b"To: a@example.com\r\nSubject: s\r\n\r\nx"
        );
    }

    #[test]
    fn a_longer_header_name_is_not_the_bcc() {
        let raw = b"Bccx: b@example.com\r\nBcc-list : c@example.com\r\n\r\nx";
        assert!(matches!(without_header(raw, "Bcc"), Cow::Borrowed(_)));
    }
}
