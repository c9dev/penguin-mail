//! The copy of a sent message that Gmail files under Sent, read from the
//! message's bytes. The demo and the assistant's tests both send through
//! the fake, and both want to find what they sent afterwards.

use mail_parser::MessageParser;
use mailrs_domain::{AccountId, Address, Memberships, MessageMeta};

use super::SentCopy;

/// Reads a sent message into the copy Gmail files under Sent, so it turns
/// up in Sent and in its conversation after the next sync. The envelope
/// still comes from mail-parser's headers; the body is the same
/// `mailrs_mime::read` every other reader uses, so a sent copy's text,
/// HTML and attachments match what the window would show for the same
/// bytes fetched back from Gmail. Each attachment's handle is its part
/// path, which is how a caller fetches it now.
pub fn read_sent(raw: &[u8], account_id: AccountId) -> Option<SentCopy> {
    let parsed = MessageParser::default().parse_headers(raw)?;
    let addresses = |list: Option<&mail_parser::Address>| -> Vec<Address> {
        list.map(|list| {
            list.iter()
                .filter_map(|a| {
                    Some(Address {
                        name: a.name().map(str::to_string),
                        email: a.address()?.to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
    };
    let body = mailrs_mime::read(raw);
    let files: Vec<(String, Vec<u8>)> = body
        .attachments
        .iter()
        .zip(mailrs_mime::files(raw))
        .map(|(attachment, bytes)| (attachment.part_id.clone(), bytes))
        .collect();
    let references = [parsed.in_reply_to(), parsed.references()]
        .into_iter()
        .filter_map(|header| header.as_text_list())
        .flatten()
        .map(|id| id.to_string())
        .collect();
    let snippet = body
        .text
        .as_deref()
        .unwrap_or_default()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(140)
        .collect();
    let has_attachments = !body.attachments.is_empty();
    let meta = MessageMeta {
        account_id,
        id: String::new(),
        thread_id: String::new(),
        rfc822_msgid: parsed.message_id().map(|id| format!("<{id}>")),
        from: addresses(parsed.from()).into_iter().next(),
        to: addresses(parsed.to()),
        cc: addresses(parsed.cc()),
        subject: parsed.subject().unwrap_or_default().into(),
        date: parsed
            .date()
            .map(|d| d.to_timestamp() * 1000)
            .unwrap_or_else(crate::now_millis),
        snippet,
        size: raw.len() as i64,
        has_attachments,
        // Read and in no mailbox until the fake files it.
        held: Memberships::read(),
        roles: vec![],
        // A message the demo sent itself belongs to no mailing list.
        list_unsubscribe: None,
        one_click: false,
    };
    Some(SentCopy {
        meta,
        body,
        references,
        files,
    })
}
