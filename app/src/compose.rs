//! What the composer sends: reply and forward drafts, Markdown rendering,
//! and MIME assembly. The `text/plain` part of every message is the
//! Markdown source, which is how a saved draft reopens as Markdown.

use mail_builder::MessageBuilder;
use mail_builder::headers::address::Address as MimeAddress;
use mailrs_domain::{AccountId, Address, MessageBody, MessageMeta};
use mailrs_gmail::address::parse_address_list_keeping_invalid;
use mailrs_gmail::convert::unescape_snippet;
use pulldown_cmark::{Event, Options, Parser, html};

use crate::format::full_date;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutgoingAttachment {
    pub filename: String,
    pub mime_type: String,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    pub account_id: AccountId,
    pub from: Address,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub subject: String,
    pub markdown: String,
    /// `Message-ID` of the message this replies to, with angle brackets.
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    pub thread_id: Option<String>,
    pub attachments: Vec<OutgoingAttachment>,
    /// The Gmail draft this composer saves into.
    pub draft_id: Option<String>,
}

impl Draft {
    pub fn new(account_id: AccountId, from: Address) -> Self {
        Draft {
            account_id,
            from,
            to: vec![],
            cc: vec![],
            subject: String::new(),
            markdown: String::new(),
            in_reply_to: None,
            references: vec![],
            thread_id: None,
            attachments: vec![],
            draft_id: None,
        }
    }

    /// Why this draft cannot be sent yet, if anything stops it.
    pub fn problem(&self) -> Option<String> {
        if self.to.is_empty() && self.cc.is_empty() {
            return Some("Add at least one recipient.".into());
        }
        self.to
            .iter()
            .chain(&self.cc)
            .find(|a| !looks_like_address(&a.email))
            .map(|a| format!("“{}” is not an email address.", a.email))
    }
}

fn looks_like_address(email: &str) -> bool {
    let mut parts = email.splitn(2, '@');
    let (local, domain) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !email.contains(char::is_whitespace)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyKind {
    Reply,
    ReplyAll,
    Forward,
}

/// A draft that answers or forwards `original`. `thread` is the whole
/// conversation, oldest first; `original_text` is the original's body as
/// plain text.
pub fn respond(
    kind: ReplyKind,
    account_id: AccountId,
    me: &Address,
    original: &MessageMeta,
    original_text: &str,
    thread: &[MessageMeta],
) -> Draft {
    let mut draft = Draft::new(account_id, me.clone());
    let sender = original.from.clone().unwrap_or_else(|| Address {
        name: None,
        email: String::new(),
    });
    match kind {
        ReplyKind::Reply | ReplyKind::ReplyAll => {
            let from_me = same_address(&sender, me);
            let mut to: Vec<Address> = if from_me {
                original.to.clone()
            } else {
                vec![sender.clone()]
            };
            let mut cc = Vec::new();
            if kind == ReplyKind::ReplyAll {
                to.extend(original.to.iter().cloned());
                cc.extend(original.cc.iter().cloned());
            }
            draft.to = dedupe(to, &[me]);
            let taken: Vec<&Address> = draft.to.iter().chain([me]).collect();
            draft.cc = dedupe(cc, &taken);
            draft.subject = prefixed("Re: ", &original.subject, &["re:"]);
            draft.markdown = format!(
                "\n\nOn {}, {} wrote:\n{}",
                full_date(original.date),
                sender.display(),
                quote(original_text)
            );
            draft.in_reply_to = original.rfc822_msgid.clone();
            draft.references = references(original, thread);
            draft.thread_id = Some(original.thread_id.clone());
        }
        ReplyKind::Forward => {
            draft.subject = prefixed("Fwd: ", &original.subject, &["fwd:", "fw:"]);
            draft.markdown = format!(
                "\n\n---------- Forwarded message ----------\nFrom: {}\nDate: {}\nSubject: {}\nTo: {}\n\n{}",
                format_recipients(std::slice::from_ref(&sender)),
                full_date(original.date),
                original.subject,
                format_recipients(&original.to),
                original_text.trim_end()
            );
        }
    }
    draft
}

fn same_address(a: &Address, b: &Address) -> bool {
    a.email.eq_ignore_ascii_case(&b.email)
}

/// Keeps the first occurrence of each address and drops any in `exclude`.
fn dedupe(list: Vec<Address>, exclude: &[&Address]) -> Vec<Address> {
    let mut out: Vec<Address> = Vec::new();
    for address in list {
        if address.email.is_empty()
            || exclude.iter().any(|e| same_address(e, &address))
            || out.iter().any(|o| same_address(o, &address))
        {
            continue;
        }
        out.push(address);
    }
    out
}

fn prefixed(prefix: &str, subject: &str, existing: &[&str]) -> String {
    let lower = subject.trim_start().to_lowercase();
    if existing.iter().any(|p| lower.starts_with(p)) {
        subject.trim().to_string()
    } else {
        format!("{prefix}{}", subject.trim())
    }
}

fn quote(text: &str) -> String {
    text.trim_end()
        .lines()
        .map(|line| {
            if line.is_empty() {
                ">".to_string()
            } else {
                format!("> {line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Message ids of the thread up to and including `original`.
fn references(original: &MessageMeta, thread: &[MessageMeta]) -> Vec<String> {
    let mut ids: Vec<String> = thread
        .iter()
        .take_while(|m| m.id != original.id)
        .filter_map(|m| m.rfc822_msgid.clone())
        .collect();
    ids.extend(original.rfc822_msgid.clone());
    ids.dedup();
    ids
}

/// Puts `signature` below where the user writes: first in a new message,
/// and above the quote in a reply.
pub fn with_signature(markdown: &str, signature: &str) -> String {
    let signature = signature.trim_end();
    if signature.trim().is_empty() {
        return markdown.to_string();
    }
    format!("\n\n-- \n{signature}{markdown}")
}

/// A message body as plain text, for quoting and for reopening drafts.
pub fn body_text(body: &MessageBody) -> String {
    match (&body.text, &body.html) {
        (Some(text), _) if !text.trim().is_empty() => text.replace("\r\n", "\n"),
        (_, Some(html)) => html_to_text(html),
        _ => String::new(),
    }
}

/// Rough HTML to text: drops styles, scripts, and the head, turns block
/// ends into line breaks, and strips the remaining tags.
pub fn html_to_text(html: &str) -> String {
    const BLOCKS: [&str; 13] = [
        "p",
        "div",
        "tr",
        "li",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "blockquote",
        "table",
        "ul",
    ];
    // ASCII lowercasing keeps byte offsets, so indexes into `lower` fit `html`.
    let lower = html.to_ascii_lowercase();
    let mut text = String::with_capacity(html.len() / 2);
    let mut i = 0;
    while i < html.len() {
        let Some(offset) = html[i..].find('<') else {
            text.push_str(&html[i..]);
            break;
        };
        text.push_str(&html[i..i + offset]);
        let start = i + offset;
        let end = html[start..]
            .find('>')
            .map_or(html.len(), |e| start + e + 1);
        let closing = lower[start..].starts_with("</");
        let name: String = lower[start + 1..end]
            .trim_start_matches('/')
            .chars()
            .take_while(char::is_ascii_alphanumeric)
            .collect();
        if !closing && matches!(name.as_str(), "style" | "script" | "head" | "title") {
            let close = lower[end..]
                .find(&format!("</{name}"))
                .map_or(html.len(), |c| end + c);
            i = html[close..]
                .find('>')
                .map_or(html.len(), |e| close + e + 1);
            continue;
        }
        if name == "br" || (closing && BLOCKS.contains(&name.as_str())) {
            text.push('\n');
        }
        i = end;
    }
    let decoded = unescape_snippet(&text);
    let mut out = String::new();
    let mut blank = 0;
    for line in decoded
        .lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
    {
        if line.is_empty() {
            blank += 1;
            if blank > 1 || out.is_empty() {
                continue;
            }
        } else {
            blank = 0;
        }
        out.push_str(&line);
        out.push('\n');
    }
    out.trim_end().to_string()
}

/// Markdown to email HTML. A single line break stays a line break, as it
/// would in any other mail client, and styles are inline because many mail
/// clients drop `<style>` blocks.
pub fn markdown_to_html(markdown: &str) -> String {
    let options = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH;
    let events = Parser::new_ext(markdown, options).map(|event| match event {
        Event::SoftBreak => Event::HardBreak,
        other => other,
    });
    let mut body = String::new();
    html::push_html(&mut body, events);
    let body = body
        .replace("<blockquote>", "<blockquote style=\"margin:0 0 0 0.8ex;border-left:2px solid #ccc;padding-left:1ex;color:#555\">")
        .replace("<pre>", "<pre style=\"background:#f6f6f8;padding:10px;border-radius:6px;overflow:auto\">")
        .replace("<p>", "<p style=\"margin:0 0 1em\">");
    format!(
        "<div style=\"font-family:-apple-system,'Segoe UI',Roboto,Helvetica,Arial,sans-serif;font-size:14px;line-height:1.5\">{body}</div>"
    )
}

/// Recipients as the composer shows them: `Name <email>`, comma separated,
/// with names that contain commas quoted.
pub fn format_recipients(list: &[Address]) -> String {
    list.iter()
        .map(|a| match a.name.as_deref().filter(|n| !n.is_empty()) {
            Some(name) if name.contains([',', '"', '<', '>', ';']) => {
                format!("\"{}\" <{}>", name.replace('"', "\\\""), a.email)
            }
            Some(name) => format!("{name} <{}>", a.email),
            None => a.email.clone(),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Parses what the user typed into a recipient field. Semicolons work as
/// separators too.
pub fn parse_recipients(text: &str) -> Vec<Address> {
    parse_address_list_keeping_invalid(&text.replace(';', ","))
}

/// A fresh `Message-ID`, without angle brackets, in the sender's domain.
pub fn new_message_id(from_email: &str) -> String {
    let domain = from_email
        .rsplit_once('@')
        .map_or("mailrs.local", |(_, d)| d);
    format!(
        "{}.{}@{domain}",
        mailrs_gmail::random_token(12),
        std::process::id()
    )
}

fn mime_address(address: &Address) -> MimeAddress<'static> {
    match address.name.as_deref().filter(|n| !n.trim().is_empty()) {
        Some(name) => MimeAddress::from((name.to_string(), address.email.clone())),
        None => MimeAddress::from(address.email.clone()),
    }
}

fn bare_id(id: &str) -> String {
    id.trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .to_string()
}

/// The RFC 822 bytes for `draft`: `multipart/alternative` with the Markdown
/// source as text and its rendering as HTML, plus any attachments.
pub fn build_mime(draft: &Draft, date_secs: i64, message_id: &str) -> Result<Vec<u8>, String> {
    let mut builder = MessageBuilder::new()
        .from(mime_address(&draft.from))
        .subject(draft.subject.trim().to_string())
        .date(date_secs)
        .message_id(bare_id(message_id))
        .text_body(draft.markdown.clone())
        .html_body(markdown_to_html(&draft.markdown));
    if !draft.to.is_empty() {
        builder = builder.to(draft.to.iter().map(mime_address).collect::<Vec<_>>());
    }
    if !draft.cc.is_empty() {
        builder = builder.cc(draft.cc.iter().map(mime_address).collect::<Vec<_>>());
    }
    if let Some(parent) = &draft.in_reply_to {
        builder = builder.in_reply_to(bare_id(parent));
    }
    if !draft.references.is_empty() {
        builder = builder.references(
            draft
                .references
                .iter()
                .map(|r| bare_id(r))
                .collect::<Vec<_>>(),
        );
    }
    for attachment in &draft.attachments {
        builder = builder.attachment(
            attachment.mime_type.clone(),
            attachment.filename.clone(),
            attachment.data.clone(),
        );
    }
    builder.write_to_vec().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use mail_parser::{MessageParser, MimeHeaders};

    use super::*;

    fn addr(name: Option<&str>, email: &str) -> Address {
        Address {
            name: name.map(str::to_string),
            email: email.into(),
        }
    }

    fn me() -> Address {
        addr(Some("Dana Reyes"), "dana@example.com")
    }

    fn message(id: &str, from: Address, to: Vec<Address>, cc: Vec<Address>) -> MessageMeta {
        MessageMeta {
            account_id: 1,
            id: id.into(),
            thread_id: "t1".into(),
            rfc822_msgid: Some(format!("<{id}@mail.example.com>")),
            from: Some(from),
            to,
            cc,
            subject: "Lunch plans".into(),
            date: 1_757_000_000_000,
            snippet: String::new(),
            size: 0,
            has_attachments: false,
            label_ids: vec![],
        }
    }

    #[test]
    fn a_reply_goes_to_the_sender_and_quotes_them() {
        let first = message(
            "m1",
            me(),
            vec![addr(Some("Ann"), "ann@example.com")],
            vec![],
        );
        let second = message(
            "m2",
            addr(Some("Ann"), "ann@example.com"),
            vec![me()],
            vec![],
        );
        let thread = [first, second.clone()];
        let draft = respond(
            ReplyKind::Reply,
            1,
            &me(),
            &second,
            "Noon works.\n\nSee you",
            &thread,
        );
        assert_eq!(draft.to, vec![addr(Some("Ann"), "ann@example.com")]);
        assert!(draft.cc.is_empty());
        assert_eq!(draft.subject, "Re: Lunch plans");
        assert!(
            draft
                .markdown
                .contains("Ann wrote:\n> Noon works.\n>\n> See you"),
            "{}",
            draft.markdown
        );
        assert_eq!(draft.in_reply_to.as_deref(), Some("<m2@mail.example.com>"));
        assert_eq!(
            draft.references,
            ["<m1@mail.example.com>", "<m2@mail.example.com>"]
        );
        assert_eq!(draft.thread_id.as_deref(), Some("t1"));
    }

    #[test]
    fn replying_to_your_own_message_goes_to_its_recipients() {
        let sent = message("m1", me(), vec![addr(None, "bob@example.com")], vec![]);
        let draft = respond(
            ReplyKind::Reply,
            1,
            &me(),
            &sent,
            "hi",
            std::slice::from_ref(&sent),
        );
        assert_eq!(draft.to, vec![addr(None, "bob@example.com")]);
    }

    #[test]
    fn reply_all_drops_you_and_duplicates() {
        let original = message(
            "m1",
            addr(Some("Ann"), "ann@example.com"),
            vec![
                addr(None, "DANA@example.com"),
                addr(Some("Bob"), "bob@example.com"),
            ],
            vec![
                addr(None, "ann@example.com"),
                addr(Some("Cy"), "cy@example.com"),
            ],
        );
        let draft = respond(
            ReplyKind::ReplyAll,
            1,
            &me(),
            &original,
            "x",
            std::slice::from_ref(&original),
        );
        assert_eq!(
            draft.to,
            vec![
                addr(Some("Ann"), "ann@example.com"),
                addr(Some("Bob"), "bob@example.com")
            ]
        );
        assert_eq!(draft.cc, vec![addr(Some("Cy"), "cy@example.com")]);
    }

    #[test]
    fn prefixes_are_not_doubled() {
        let mut original = message("m1", addr(None, "a@example.com"), vec![], vec![]);
        original.subject = "RE: Budget".into();
        assert_eq!(
            respond(ReplyKind::Reply, 1, &me(), &original, "", &[]).subject,
            "RE: Budget"
        );
        original.subject = "Fw: Budget".into();
        assert_eq!(
            respond(ReplyKind::Forward, 1, &me(), &original, "", &[]).subject,
            "Fw: Budget"
        );
    }

    #[test]
    fn a_forward_starts_a_new_thread_with_the_original_inline() {
        let original = message(
            "m1",
            addr(Some("Ann"), "ann@example.com"),
            vec![me()],
            vec![],
        );
        let draft = respond(
            ReplyKind::Forward,
            1,
            &me(),
            &original,
            "Menu attached.",
            &[],
        );
        assert!(draft.to.is_empty());
        assert_eq!(draft.subject, "Fwd: Lunch plans");
        assert!(draft.markdown.contains("From: Ann <ann@example.com>\n"));
        assert!(draft.markdown.ends_with("Menu attached."));
        assert!(draft.in_reply_to.is_none() && draft.thread_id.is_none());
    }

    #[test]
    fn markdown_keeps_single_line_breaks() {
        let html = markdown_to_html("Hi Bob,\nThanks for **this**.\n\n> quoted");
        assert!(html.contains("Hi Bob,<br />"), "{html}");
        assert!(html.contains("<strong>this</strong>"));
        assert!(html.contains("<blockquote style="));
    }

    #[test]
    fn built_messages_parse_back() {
        let mut draft = Draft::new(1, me());
        draft.to = vec![addr(Some("Lee, Ann"), "ann@example.com")];
        draft.cc = vec![addr(None, "bob@example.com")];
        draft.subject = "Plans".into();
        draft.markdown = "Hello **Ann**\nSee you".into();
        draft.in_reply_to = Some("<m2@mail.example.com>".into());
        draft.references = vec![
            "<m1@mail.example.com>".into(),
            "<m2@mail.example.com>".into(),
        ];
        draft.attachments = vec![OutgoingAttachment {
            filename: "menu.pdf".into(),
            mime_type: "application/pdf".into(),
            data: b"%PDF-1.7".to_vec(),
        }];
        let raw = build_mime(&draft, 1_757_000_000, "<new@example.com>").unwrap();
        let parsed = MessageParser::default().parse(&raw).unwrap();
        let from = parsed.from().unwrap().first().unwrap();
        assert_eq!(
            (from.name(), from.address()),
            (Some("Dana Reyes"), Some("dana@example.com"))
        );
        assert_eq!(
            parsed.to().unwrap().first().unwrap().name(),
            Some("Lee, Ann")
        );
        assert_eq!(
            parsed.cc().unwrap().first().unwrap().address(),
            Some("bob@example.com")
        );
        assert_eq!(parsed.subject(), Some("Plans"));
        assert_eq!(parsed.message_id(), Some("new@example.com"));
        assert_eq!(parsed.in_reply_to().as_text(), Some("m2@mail.example.com"));
        assert_eq!(
            parsed.references().as_text_list().unwrap(),
            ["m1@mail.example.com", "m2@mail.example.com"]
        );
        assert_eq!(
            parsed
                .body_text(0)
                .unwrap()
                .replace("\r\n", "\n")
                .trim_end(),
            "Hello **Ann**\nSee you"
        );
        assert!(
            parsed
                .body_html(0)
                .unwrap()
                .contains("<strong>Ann</strong>")
        );
        assert_eq!(parsed.attachment_count(), 1);
        assert_eq!(
            parsed.attachments().next().unwrap().attachment_name(),
            Some("menu.pdf")
        );
    }

    #[test]
    fn drafts_without_recipients_or_with_bad_ones_cannot_be_sent() {
        let mut draft = Draft::new(1, me());
        assert!(draft.problem().is_some());
        draft.to = parse_recipients("ann@example.com; not-an-address");
        assert!(draft.problem().unwrap().contains("not-an-address"));
        draft.to = parse_recipients("ann@example.com");
        assert!(draft.problem().is_none());
    }

    #[test]
    fn recipients_round_trip_through_the_text_field() {
        let list = vec![
            addr(Some("Lee, Ann"), "ann@example.com"),
            addr(None, "bob@example.com"),
            addr(Some("Cy"), "cy@example.com"),
        ];
        let text = format_recipients(&list);
        assert_eq!(
            text,
            "\"Lee, Ann\" <ann@example.com>, bob@example.com, Cy <cy@example.com>"
        );
        assert_eq!(parse_recipients(&text), list);
    }

    #[test]
    fn html_bodies_become_readable_text() {
        let text = html_to_text(
            "<html><head><style>p{color:red}</style></head><body><p>Hello&nbsp;there</p><div>Line <b>two</b><br>three</div>\
             <script>x()</script><p></p><p>&amp; four</p></body></html>",
        );
        assert_eq!(text, "Hello there\nLine two\nthree\n\n& four");
    }

    #[test]
    fn signatures_sit_above_the_quote() {
        assert_eq!(with_signature("", "Dana\n"), "\n\n-- \nDana");
        assert_eq!(
            with_signature("\n\nOn Monday, Ann wrote:\n> hi", "Dana"),
            "\n\n-- \nDana\n\nOn Monday, Ann wrote:\n> hi"
        );
        assert_eq!(with_signature("body", "  "), "body");
    }

    #[test]
    fn drafts_reopen_from_their_text_part() {
        let body = MessageBody {
            text: Some("# Title\r\nBody".into()),
            html: Some("<h1>Title</h1>".into()),
            attachments: vec![],
        };
        assert_eq!(body_text(&body), "# Title\nBody");
    }
}
