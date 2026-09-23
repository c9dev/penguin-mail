//! The copy of a sent message that Gmail files under Sent, read from the
//! message's bytes. The demo and the assistant's tests both send through
//! the fake, and both want to find what they sent afterwards.

use mailrs_domain::{AccountId, Address, Attachment, MessageBody, MessageMeta};

use super::SentCopy;

/// Reads a sent message into the copy Gmail files under Sent, so it turns
/// up in Sent and in its conversation after the next sync.
pub fn read_sent(raw: &[u8], account_id: AccountId) -> Option<SentCopy> {
    use mail_parser::{MessageParser, MimeHeaders};
    let parsed = MessageParser::default().parse(raw)?;
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
    let text = parsed.body_text(0).map(|t| t.into_owned());
    // A plain-text message has no HTML part of its own, and mail-parser
    // would otherwise convert its text into one.
    let html = parsed
        .html_body
        .first()
        .filter(|part| !parsed.text_body.contains(part))
        .and_then(|_| parsed.body_html(0))
        .map(|h| h.into_owned());
    let mut files = Vec::new();
    let attachments = parsed
        .attachments()
        .enumerate()
        .map(|(i, part)| {
            let attachment_id = format!("sent-att-{i}");
            files.push((attachment_id.clone(), part.contents().to_vec()));
            Attachment {
                part_id: (i + 1).to_string(),
                filename: part.attachment_name().unwrap_or_default().into(),
                mime_type: part
                    .content_type()
                    .map(|t| match t.subtype() {
                        Some(sub) => format!("{}/{sub}", t.ctype()),
                        None => t.ctype().to_string(),
                    })
                    .unwrap_or_else(|| "application/octet-stream".into()),
                size: part.contents().len() as i64,
                attachment_id: Some(attachment_id),
                content_id: None,
            }
        })
        .collect::<Vec<_>>();
    let references = [parsed.in_reply_to(), parsed.references()]
        .into_iter()
        .filter_map(|header| header.as_text_list())
        .flatten()
        .map(|id| id.to_string())
        .collect();
    let snippet = text
        .as_deref()
        .unwrap_or_default()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(140)
        .collect();
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
        has_attachments: !attachments.is_empty(),
        label_ids: Vec::new(),
        // A message the demo sent itself belongs to no mailing list.
        list_unsubscribe: None,
        one_click: false,
    };
    let body = MessageBody {
        text,
        html,
        attachments,
        ..MessageBody::default()
    };
    Some(SentCopy {
        meta,
        body,
        references,
        files,
    })
}
