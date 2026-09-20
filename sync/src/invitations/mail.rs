//! The mail an answer travels in.
//!
//! RFC 6047 says how an iTIP object rides in a message: the calendar part
//! carries the method it holds as a `Content-Type` parameter as well as a
//! property, so a mailer can tell a reply from an invitation without
//! reading the object. A line of prose goes beside it in
//! `multipart/alternative`, for the organizer whose mail client does not
//! read calendar parts at all.
//!
//! The part is base64, because the object means its own line endings:
//! quoted-printable would leave a mail server free to rewrite them, and an
//! object whose lines end in a bare newline is one Exchange turns down.

use mail_builder::MessageBuilder;
use mail_builder::headers::address::Address as MimeAddress;
use mail_builder::headers::content_type::ContentType;
use mail_builder::mime::MimePart;
use mailrs_domain::invitation::Answer;
use mailrs_domain::{Address, EpochMillis};

/// The RFC 822 bytes of one iTIP message: `object` to `to`, from `me`.
/// `method` is the iTIP method the object holds, which the calendar part
/// names again in its own `Content-Type`.
pub(super) fn itip(
    me: &Address,
    to: &Address,
    subject: &str,
    prose: &str,
    method: &str,
    object: &str,
    now: EpochMillis,
) -> Result<Vec<u8>, String> {
    let calendar = MimePart::new(
        ContentType::new("text/calendar")
            .attribute("method", method)
            .attribute("charset", "utf-8"),
        object.to_string(),
    )
    .transfer_encoding("base64");
    MessageBuilder::new()
        .from(address(me))
        .to(address(to))
        .subject(subject)
        .date(now / 1_000)
        .message_id(message_id(&me.email))
        .body(MimePart::new(
            "multipart/alternative",
            vec![MimePart::new("text/plain", prose.to_string()), calendar],
        ))
        .write_to_vec()
        .map_err(|err| err.to_string())
}

/// "Accepted: Q4 roadmap review", the subject line every mailer since
/// Outlook 97 has put an answer under.
pub(super) fn reply_subject(answer: Answer, summary: &str) -> String {
    let said = match answer {
        Answer::Yes => "Accepted",
        Answer::No => "Declined",
        Answer::Maybe => "Tentative",
    };
    format!("{said}: {}", titled(summary))
}

/// What the organizer reads when their mail client shows no calendar part.
pub(super) fn reply_prose(me: &Address, answer: Answer, summary: &str) -> String {
    let said = match answer {
        Answer::Yes => "accepted",
        Answer::No => "declined",
        Answer::Maybe => "tentatively accepted",
    };
    format!(
        "{} has {said} the invitation to {}.",
        me.display(),
        titled(summary)
    )
}

/// The title as a sentence names it, for an event the organizer left
/// untitled as well as for one they named.
pub(super) fn titled(summary: &str) -> &str {
    match summary.trim().is_empty() {
        true => "this meeting",
        false => summary.trim(),
    }
}

fn address(who: &Address) -> MimeAddress<'static> {
    match who.name.as_deref().filter(|name| !name.trim().is_empty()) {
        Some(name) => MimeAddress::from((name.to_string(), who.email.clone())),
        None => MimeAddress::from(who.email.clone()),
    }
}

/// A `Message-ID` in the sender's own domain, as the composer makes one.
fn message_id(from_email: &str) -> String {
    let domain = from_email
        .rsplit_once('@')
        .map_or("penguin-mail.local", |(_, domain)| domain);
    format!("{}.itip@{domain}", mailrs_gmail::random_token(12))
}
