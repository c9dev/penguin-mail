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
    // Only the text parts are decoded: a full read would decode every
    // file in the message, twice over, to learn whether there is one.
    let body = crate::read::skim(raw)
        .map(|parts| crate::body(&parts))
        .unwrap_or_default();
    let snippet = preview(body.text.as_deref());
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

/// The start of `text`, its white space folded to single spaces.
fn preview(text: Option<&str>) -> String {
    text.unwrap_or_default()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(SNIPPET_CHARS)
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
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

    const MIXED: &[u8] = b"From: a@example.org\r\nSubject: file\r\nMIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=b\r\n\r\npreamble\r\n--b\r\nContent-Type: text/plain\r\n\r\nSee attached.\r\n\
--b\r\nContent-Type: application/pdf\r\nContent-Disposition: attachment; filename=a.pdf\r\n\
Content-Transfer-Encoding: base64\r\n\r\nJVBERg==\r\n--b--\r\nepilogue\r\n";

    /// Message shapes a reader meets: alternatives, charsets, transfer
    /// encodings, nesting, signatures, invitations, forwarded mail with and
    /// without files and a transfer encoding, a part without headers, a
    /// boundary never closed, base64 padded on every line.
    pub(crate) const SHAPES: &[&[u8]] = &[
        MAIL,
        MIXED,
        b"Subject: alt\r\nContent-Type: multipart/alternative; boundary=\"x y\"\r\n\r\n--x y\r\n\
Content-Type: text/plain; charset=iso-8859-1\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\n\
Ol=E1, at=E9 j=E1.\r\n--x y\r\nContent-Type: text/html\r\n\r\n<p>Ol\xc3\xa1</p>\r\n--x y--\r\n",
        b"Subject: nest\r\nContent-Type: multipart/mixed; boundary=outer\r\n\r\n--outer\r\n\
Content-Type: multipart/related; boundary=inner\r\n\r\n--inner\r\nContent-Type: text/html\r\n\r\n\
<img src=cid:logo>\r\n--inner\r\nContent-Type: image/png\r\nContent-ID: <logo>\r\n\
Content-Transfer-Encoding: base64\r\n\r\niVBORw0KGgo=\r\n--inner--\r\n--outer\r\nContent-Type: text/plain\r\n\r\n\
Second text.\r\n--outer--\r\n",
        b"Subject: signed\r\nContent-Type: multipart/signed; protocol=\"application/pgp-signature\"; boundary=s\r\n\r\n\
--s\r\nContent-Type: text/plain\r\n\r\nSigned words.\r\n--s\r\nContent-Type: application/pgp-signature\r\n\r\n\
-----BEGIN PGP SIGNATURE-----\r\nxyz\r\n-----END PGP SIGNATURE-----\r\n--s--\r\n",
        b"Subject: invite\r\nContent-Type: multipart/mixed; boundary=m\r\n\r\n--m\r\n\
Content-Type: multipart/alternative; boundary=a\r\n\r\n--a\r\nContent-Type: text/plain\r\n\r\nLunch?\r\n--a\r\n\
Content-Type: text/calendar; method=REQUEST\r\n\r\nBEGIN:VCALENDAR\r\nUID:1\r\nEND:VCALENDAR\r\n--a--\r\n--m\r\n\
Content-Type: application/ics; name=invite.ics\r\nContent-Disposition: attachment; filename=invite.ics\r\n\r\n\
BEGIN:VCALENDAR\r\nUID:1\r\nEND:VCALENDAR\r\n--m--\r\n",
        b"Subject: fwd\r\nContent-Type: multipart/mixed; boundary=f\r\n\r\n--f\r\nContent-Type: text/plain\r\n\r\n\
See below.\r\n--f\r\nContent-Type: message/rfc822\r\n\r\nSubject: inner\r\nFrom: b@example.org\r\n\r\n\
Inner body.\r\n--f--\r\n",
        b"Subject: bare\r\nContent-Type: multipart/mixed; boundary=n\r\n\r\n--n\r\n\r\nNo headers here.\r\n--n--\r\n",
        b"Subject: open\r\nContent-Type: multipart/mixed; boundary=o\r\n\r\n--o\r\nContent-Type: text/plain\r\n\r\n\
Never closed.\r\n--o\r\nContent-Type: image/gif\r\nContent-Transfer-Encoding: base64\r\n\r\nR0lGODlh\r\n",
        b"Subject: b64\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\n\r\n\
T2zDoSwgbXVuZG8hCg==\r\n",
        b"Subject: html\r\nContent-Type: text/html\r\n\r\n<p>Only HTML.</p>\r\n",
        b"Subject: lf\nContent-Type: multipart/mixed; boundary=l\n\n--l\nContent-Type: text/plain\n\nBare line feeds.\n\
--l\nContent-Type: application/zip\n\nPK\n--l--\n",
        b"Subject: none\r\nContent-Type: multipart/mixed\r\n\r\nA multipart with no boundary.\r\n",
        b"Subject: fwd file\r\nContent-Type: multipart/mixed; boundary=f\r\n\r\n--f\r\nContent-Type: text/plain\r\n\r\n\
See below.\r\n--f\r\nContent-Type: message/rfc822\r\n\r\nSubject: inner\r\n\
Content-Type: multipart/mixed; boundary=g\r\n\r\n--g\r\nContent-Type: text/plain\r\n\r\nInner body.\r\n\
--g\r\nContent-Type: application/pdf; name=in.pdf\r\nContent-Transfer-Encoding: base64\r\n\r\nSU5ORVI=\r\n--g--\r\n\
--f--\r\n",
        b"Subject: fwd b64\r\nContent-Type: multipart/mixed; boundary=f\r\n\r\n--f\r\nContent-Type: text/plain\r\n\r\n\
See below.\r\n--f\r\nContent-Type: message/rfc822\r\nContent-Transfer-Encoding: base64\r\n\r\n\
U3ViamVjdDogaW5uZXINCkNvbnRlbnQtVHlwZTogYXBwbGljYXRpb24vemlwOyBuYW1lPWEuemlwDQoNClBLAwQ=\r\n--f--\r\n",
        b"Subject: qp file\r\nContent-Type: multipart/mixed; boundary=q\r\n\r\n--q\r\nContent-Type: text/plain\r\n\r\nHi.\r\n\
--q\r\nContent-Type: text/csv; name=a.csv\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\n\
a,b=3Dc\r\n1,2=\r\n3\r\n--q--\r\n",
        b"Subject: padded\r\nContent-Type: multipart/mixed; boundary=p\r\n\r\n--p\r\nContent-Type: text/plain\r\n\r\nHi.\r\n\
--p\r\nContent-Type: image/png; name=p.png\r\nContent-Transfer-Encoding: base64\r\n\r\nQUI=\r\nQ0Q=\r\nRQ==\r\n--p--\r\n",
        b"Subject: wrapped\r\nContent-Type: multipart/mixed; boundary=w\r\n\r\n--w\r\nContent-Type: text/plain\r\n\r\nHi.\r\n\
--w\r\nContent-Type: image/gif; name=w.gif\r\nContent-Transfer-Encoding: base64\r\n\r\nR0lGO\r\nDlhAQ\r\nABAA\r\n--w--\r\n",
    ];

    /// What a summary's snippet and files were before it skimmed: from a
    /// full read, which decodes every part.
    fn by_full_read(raw: &[u8]) -> (String, bool) {
        let body = crate::read(raw);
        (preview(body.text.as_deref()), !body.attachments.is_empty())
    }

    #[test]
    fn skimming_gives_the_snippet_and_files_a_full_read_gives() {
        for raw in SHAPES {
            let s = summary(raw);
            assert_eq!(
                (s.snippet, s.has_files),
                by_full_read(raw),
                "{}",
                String::from_utf8_lossy(raw)
            );
        }
    }

    #[test]
    fn a_file_is_never_decoded_to_summarize_its_message() {
        let parts = crate::read::skim(MIXED).expect("parts");
        assert_eq!(
            parts.find("1").and_then(|p| p.data.as_deref()),
            Some(&b"See attached."[..])
        );
        assert_eq!(
            parts.find("2").map(|p| p.data.is_none()),
            Some(true),
            "the PDF stays encoded and unread"
        );
    }
}
