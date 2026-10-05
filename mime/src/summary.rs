//! The headers and preview of a message read from its whole bytes, for
//! mail that arrives whole: a POP3 download, or a copy filed on this
//! computer. Header values keep the forms `mailrs_imap::Fetched` keeps,
//! angle brackets included, so local threading reads them alike.

use mail_parser::MessageParser;
use mailrs_domain::{Address, EpochMillis};

/// The most characters of text a preview holds.
const SNIPPET_CHARS: usize = 140;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Summary {
    /// The `Message-ID` header as sent, angle brackets included.
    pub message_id: Option<String>,
    pub from: Option<Address>,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    /// Decoded from any RFC 2047 encoded words.
    pub subject: String,
    /// The `Date` header, which the sender's clock wrote.
    pub date: Option<EpochMillis>,
    pub in_reply_to: Option<String>,
    /// Oldest first, each in angle brackets.
    pub references: Vec<String>,
    pub list_unsubscribe: Option<String>,
    pub list_unsubscribe_post: Option<String>,
    pub has_files: bool,
    /// The start of the text, its white space folded to single spaces.
    pub snippet: String,
}

/// What `raw` says about itself. Bytes mail-parser cannot read give an
/// empty summary.
pub fn summary(raw: &[u8]) -> Summary {
    let Some(message) = MessageParser::default().parse_headers(raw) else {
        return Summary::default();
    };
    let addresses = |list: Option<&mail_parser::Address>| -> Vec<Address> {
        list.map(|list| {
            list.iter()
                .filter_map(|a| {
                    let email = a.address()?.trim();
                    (!email.is_empty()).then(|| Address {
                        name: a.name().map(str::to_string),
                        email: email.to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
    };
    let header = |name: &str| {
        message
            .header_raw(name)
            .map(|value| value.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|value| !value.is_empty())
    };
    let body = crate::read(raw);
    let snippet = body
        .text
        .as_deref()
        .unwrap_or_default()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(SNIPPET_CHARS)
        .collect();
    Summary {
        message_id: header("Message-ID"),
        from: addresses(message.from()).into_iter().next(),
        to: addresses(message.to()),
        cc: addresses(message.cc()),
        subject: message.subject().unwrap_or_default().to_string(),
        date: message.date().map(|d| d.to_timestamp() * 1000),
        in_reply_to: header("In-Reply-To"),
        references: header("References")
            .map(|value| {
                value
                    .split([' ', ','])
                    .filter(|id| !id.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
        list_unsubscribe: header("List-Unsubscribe"),
        list_unsubscribe_post: header("List-Unsubscribe-Post"),
        has_files: !body.attachments.is_empty(),
        snippet,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAIL: &[u8] = b"From: Ana Lima <ana@example.org>\r\n\
To: me@example.org, Bo <bo@example.org>\r\n\
Cc: cy@example.org\r\n\
Subject: =?UTF-8?Q?Ol=C3=A1?=\r\n\
Date: Mon, 5 Oct 2026 09:00:00 +0000\r\n\
Message-ID: <m2@example.org>\r\n\
In-Reply-To: <m1@example.org>\r\n\
References: <m0@example.org>\r\n <m1@example.org>\r\n\
List-Unsubscribe: <https://example.org/u>\r\n\
List-Unsubscribe-Post: List-Unsubscribe=One-Click\r\n\
\r\n\
First line of the body.\r\nSecond line.\r\n";

    #[test]
    fn a_message_reads_as_its_headers_and_a_preview() {
        let s = summary(MAIL);
        assert_eq!(
            s.from,
            Some(Address {
                name: Some("Ana Lima".into()),
                email: "ana@example.org".into()
            })
        );
        assert_eq!(s.to.len(), 2);
        assert_eq!(s.cc[0].email, "cy@example.org");
        assert_eq!(s.subject, "Olá");
        assert_eq!(s.date, Some(1_791_190_800_000));
        assert_eq!(s.message_id.as_deref(), Some("<m2@example.org>"));
        assert_eq!(s.in_reply_to.as_deref(), Some("<m1@example.org>"));
        assert_eq!(s.references, ["<m0@example.org>", "<m1@example.org>"]);
        assert_eq!(
            s.list_unsubscribe.as_deref(),
            Some("<https://example.org/u>")
        );
        assert!(
            s.list_unsubscribe_post
                .as_deref()
                .is_some_and(|v| v.contains("One-Click"))
        );
        assert_eq!(s.snippet, "First line of the body. Second line.");
        assert!(!s.has_files);
    }

    #[test]
    fn a_message_with_an_attachment_has_files() {
        let raw = b"From: a@example.org\r\nSubject: file\r\nMIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=b\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\nSee attached.\r\n\
--b\r\nContent-Type: application/pdf\r\nContent-Disposition: attachment; filename=a.pdf\r\n\
Content-Transfer-Encoding: base64\r\n\r\nJVBERg==\r\n--b--\r\n";
        let s = summary(raw);
        assert!(s.has_files);
        assert_eq!(s.snippet, "See attached.");
    }

    #[test]
    fn bytes_that_are_no_mail_give_an_empty_summary() {
        assert_eq!(summary(b""), Summary::default());
    }
}
