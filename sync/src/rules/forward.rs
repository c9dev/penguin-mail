//! A local rule's forward: a new message from the account's own address,
//! "Fwd: " and the subject, with the original attached whole as
//! message/rfc822. Resending the original with its own From would be
//! refused by many SMTP servers and fail the sender's DMARC.

use mail_builder::MessageBuilder;
use mail_builder::headers::address::Address;
use mailrs_domain::EpochMillis;
use mailrs_domain::translate::{fill, gettext};

use crate::SyncError;

pub fn forwarded(raw: &[u8], from: &str, to: &str, now: EpochMillis) -> Result<Vec<u8>, SyncError> {
    let subject = mailrs_mime::parts(raw)
        .and_then(|p| p.header("Subject").map(str::to_string))
        .unwrap_or_default();
    let note = fill(
        &gettext("A rule on this computer forwarded the attached message from {address}."),
        &[("address", from)],
    );
    MessageBuilder::new()
        .from(Address::new_address(None::<&str>, from))
        .to(Address::new_address(None::<&str>, to))
        .subject(format!("Fwd: {subject}"))
        .date(now / 1000)
        .text_body(note)
        .attachment("message/rfc822", "forwarded.eml", raw.to_vec())
        .write_to_vec()
        .map_err(|err| SyncError::Mime(err.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_forward_is_a_new_message_that_carries_the_original_whole() {
        let original = b"From: news@example.com\r\nTo: me@example.com\r\nSubject: Hello\r\n\r\nBody.\r\n";
        let sent = forwarded(original, "me@example.com", "you@example.org", 1_700_000_000_000).unwrap();
        let parsed = mail_parser::MessageParser::default().parse(&sent).unwrap();
        assert_eq!(parsed.subject(), Some("Fwd: Hello"));
        assert_eq!(parsed.from().and_then(|f| f.first()).and_then(|a| a.address()), Some("me@example.com"));
        assert_eq!(parsed.to().and_then(|f| f.first()).and_then(|a| a.address()), Some("you@example.org"));
        assert!(String::from_utf8_lossy(&sent).contains("message/rfc822"));
    }
}
