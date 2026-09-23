//! What the composer sends: reply and forward drafts, Markdown rendering,
//! and MIME assembly.
//!
//! A draft carries its body twice over. `markdown` is the source every
//! caller writes, from a reply's quote to the assistant's `draft_email`,
//! and it is the `text/plain` part of a message written as Markdown, which
//! is how such a draft reopens. `rich` is set when the writer used rich
//! text: it then decides both parts, HTML from the styled blocks and plain
//! text stripped from the same blocks.

use mail_builder::MessageBuilder;
use mail_builder::headers::address::Address as MimeAddress;
use mail_builder::headers::content_type::ContentType;
use mail_builder::headers::raw::Raw;
use mail_builder::mime::MimePart;
use mailrs_domain::{AccountId, Address, EpochMillis, MessageBody, MessageMeta};
use mailrs_gmail::address::parse_address_list_keeping_invalid;
use pulldown_cmark::{Event, Options, Parser, html};
use serde::{Deserialize, Serialize};

use crate::attachcheck::{self, Promise};
use crate::format::full_date;
use crate::protection::Standard;
use crate::richtext::{self, RichBody};
use mailrs_domain::translate::{fill, gettext};

/// One address an account may send mail as, as Gmail last reported it: the
/// account's own address, or an alias Gmail has verified. Gmail keeps a
/// display name and a signature per address.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SendAsAddress {
    pub email: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Gmail's signature for this address as plain text, empty when it has none.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub signature: String,
    /// The address Gmail sends from when the writer picks none.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub default: bool,
}

/// An address the From row offers: an account's own address, or one of its
/// send-as addresses. The display name and the signature come with it, so
/// picking a row in the composer picks all three.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub account_id: AccountId,
    /// The account this address belongs to, which groups the From row.
    pub account_email: String,
    pub address: Address,
    /// What goes below the message, as Markdown. Empty when there is none.
    pub signature: String,
    /// Gmail's own choice for the account, used when nothing else points
    /// at an address.
    pub default: bool,
}

/// The address a reply to `original` comes from: whichever of `mine` the
/// message was written to. Someone who wrote to `sales@` expects the answer
/// to come back from `sales@`. `None` when the message reached none of the
/// account's addresses, as happens through a mailing list.
pub fn reply_from<'a>(mine: &'a [Address], original: &MessageMeta) -> Option<&'a Address> {
    original
        .to
        .iter()
        .chain(&original.cc)
        .find_map(|wrote_to| mine.iter().find(|a| same_address(a, wrote_to)))
}

/// Which row the From dropdown starts on. The draft's own address wins,
/// since a reply already carries the address it was written to; then the
/// address this account last sent from; then Gmail's default.
pub fn opening_identity(
    identities: &[Identity],
    account_id: AccountId,
    from: &Address,
    last_used: Option<&str>,
) -> Option<usize> {
    let find = |email: &str| {
        identities
            .iter()
            .position(|i| i.account_id == account_id && i.address.email.eq_ignore_ascii_case(email))
    };
    find(&from.email)
        .or_else(|| last_used.and_then(find))
        .or_else(|| {
            identities
                .iter()
                .position(|i| i.account_id == account_id && i.default)
        })
        .or_else(|| identities.iter().position(|i| i.account_id == account_id))
}

/// An attachment's bytes as base64 while a draft waits in the outbox.
/// JSON has no bytes of its own, and a list of numbers runs to four times
/// the size.
mod attachment_bytes {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(data: &[u8], out: S) -> Result<S::Ok, S::Error> {
        out.serialize_str(&STANDARD.encode(data))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(input: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(input)?;
        STANDARD.decode(text).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutgoingAttachment {
    pub filename: String,
    pub mime_type: String,
    #[serde(with = "attachment_bytes")]
    pub data: Vec<u8>,
    /// Set for images shown in the text, which refers to them as `cid:`.
    pub content_id: Option<String>,
}

/// A Markdown prefix the toolbar adds to or removes from whole lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinePrefix {
    Bullet,
    Numbered,
    Quote,
}

impl LinePrefix {
    /// The prefix on this line, if it has one of this kind.
    fn strip(self, line: &str) -> Option<&str> {
        match self {
            LinePrefix::Bullet => line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")),
            LinePrefix::Quote => line.strip_prefix("> ").or_else(|| line.strip_prefix('>')),
            LinePrefix::Numbered => {
                let digits = line.chars().take_while(char::is_ascii_digit).count();
                (digits > 0)
                    .then(|| line[digits..].strip_prefix(". "))
                    .flatten()
            }
        }
    }
}

/// Adds `prefix` to every non-blank line of `text`, or removes it when all
/// of them already have it. Numbered lists count from 1.
pub fn toggle_prefix(text: &str, prefix: LinePrefix) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let filled = || lines.iter().filter(|l| !l.trim().is_empty());
    let all_have = filled().count() > 0 && filled().all(|l| prefix.strip(l).is_some());
    let mut number = 0;
    lines
        .iter()
        .map(|line| {
            if line.trim().is_empty() {
                return line.to_string();
            }
            if all_have {
                return prefix.strip(line).unwrap_or(line).to_string();
            }
            let bare = [LinePrefix::Bullet, LinePrefix::Numbered]
                .iter()
                .find_map(|p| p.strip(line))
                .filter(|_| prefix != LinePrefix::Quote)
                .unwrap_or(line);
            number += 1;
            match prefix {
                LinePrefix::Bullet => format!("- {bare}"),
                LinePrefix::Numbered => format!("{number}. {bare}"),
                LinePrefix::Quote => format!("> {bare}"),
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The message a forward carries, kept as it arrived.
///
/// A forwarded newsletter read as plain text and written back out as
/// Markdown is no longer the message anyone sent: its tables collapse, its
/// links come apart, and every line runs into the next. So the original
/// travels beside the writer's own words rather than through them, and
/// `build_mime` puts it back whole under the header block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Forwarded {
    pub from: String,
    pub date: String,
    pub subject: String,
    pub to: String,
    pub cc: String,
    /// The original's HTML as it arrived, or None when it had only text.
    pub html: Option<String>,
    /// The original as plain text, for the `text/plain` part.
    pub text: String,
    /// Set when `html` and `text` already carry the header block, because
    /// they came back out of a draft this composer saved.
    pub whole: bool,
}

/// The marker around a forwarded message in a saved draft, so reopening
/// one lifts the original back out instead of reading it as prose.
const FORWARD_MARK: &str = "mailrs-forwarded";

impl Forwarded {
    /// The header block above the original, as the writer reads it.
    fn header_lines(&self) -> Vec<(&'static str, &str)> {
        let mut lines = vec![
            ("From", self.from.as_str()),
            ("Date", self.date.as_str()),
            ("Subject", self.subject.as_str()),
            ("To", self.to.as_str()),
        ];
        if !self.cc.is_empty() {
            lines.push(("Cc", self.cc.as_str()));
        }
        lines
    }

    /// The whole forwarded part as plain text.
    pub fn to_plain(&self) -> String {
        if self.whole {
            return self.text.clone();
        }
        let mut out = String::from("\n\n---------- Forwarded message ----------\n");
        for (name, value) in self.header_lines() {
            out.push_str(&format!("{name}: {value}\n"));
        }
        out.push('\n');
        out.push_str(self.text.trim_end());
        out
    }

    /// The whole forwarded part as HTML, with the original untouched
    /// inside it.
    pub fn to_html(&self) -> String {
        if self.whole {
            return self.html.clone().unwrap_or_default();
        }
        let mut out = format!(
            "<div class=\"{FORWARD_MARK}\"><br><div style=\"border-top:1px solid #d4d4d4;padding-top:12px\">\
             <div style=\"color:#5f6368;font-size:13px;margin-bottom:12px\">---------- Forwarded message ----------<br>"
        );
        for (name, value) in self.header_lines() {
            out.push_str(&format!(
                "<b>{}:</b> {}<br>",
                richtext::escape(name),
                richtext::escape(value)
            ));
        }
        out.push_str("</div>");
        match self.html.as_deref().filter(|h| !h.trim().is_empty()) {
            Some(html) => out.push_str(html),
            None => out.push_str(&markdown_to_html(&plain_as_markdown(&self.text))),
        }
        out.push_str("</div></div>");
        out
    }
}

/// Splits a saved draft's HTML at the forwarded message, if it holds one.
/// Returns what the writer wrote and the forwarded block as it stands.
fn split_forwarded_html(html: &str) -> (String, Option<String>) {
    match forward_starts_at(html) {
        Some(at) => (html[..at].to_string(), Some(html[at..].to_string())),
        None => (html.to_string(), None),
    }
}

/// Where the tag that opens the forwarded block starts: the first start
/// tag whose class names [`FORWARD_MARK`].
///
/// The forwarded block has to come back byte for byte, so this needs the
/// offset in `html`, which the tokenizer does not give. It reads the tags
/// itself, stepping over quoted attribute values, comments and the text
/// of scripts and styles, so a `<` or `>` inside any of them cannot move
/// the cut, and the marker's name in the text is not taken for the tag.
fn forward_starts_at(html: &str) -> Option<usize> {
    let mut at = 0;
    while let Some(offset) = html[at..].find('<') {
        let start = at + offset;
        let rest = &html[start..];
        if let Some(comment) = rest.strip_prefix("<!--") {
            at = start + 4 + comment.find("-->").map_or(comment.len(), |end| end + 3);
            continue;
        }
        if !rest[1..].starts_with(|c: char| c.is_ascii_alphabetic()) {
            at = start + 1;
            continue;
        }
        let end = start + tag_length(rest);
        let tag = &html[start..end];
        let name: String = tag[1..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase();
        if tag.contains(FORWARD_MARK) && has_class(tag, FORWARD_MARK) {
            return Some(start);
        }
        at = end;
        // A script's or a style's text is not markup.
        if matches!(name.as_str(), "script" | "style") {
            let close = format!("</{name}");
            at = html[at..]
                .to_ascii_lowercase()
                .find(&close)
                .map_or(html.len(), |found| at + found);
        }
    }
    None
}

/// How long the tag at the start of `rest` is, up to and including the
/// `>` that closes it outside any quotes.
fn tag_length(rest: &str) -> usize {
    let mut quote: Option<char> = None;
    for (index, character) in rest.char_indices().skip(1) {
        match (quote, character) {
            (Some(open), c) if c == open => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(character),
            (None, '>') => return index + 1,
            (None, _) => {}
        }
    }
    rest.len()
}

/// Whether the one tag in `tag` has `class` among its classes.
fn has_class(tag: &str, class: &str) -> bool {
    let mut found = false;
    mailrs_gmail::html::walk(tag, |piece| {
        if let mailrs_gmail::html::Piece::Tag(tag) = piece
            && let Some(classes) = tag.attribute("class")
        {
            found |= classes.split_whitespace().any(|c| c == class);
        }
    });
    found
}

/// The same for the text part, which marks the forward with the line
/// every mail client writes there.
fn split_forwarded_text(text: &str) -> (String, Option<String>) {
    const MARK: &str = "---------- Forwarded message ----------";
    match text.find(MARK) {
        Some(at) => (
            text[..at].trim_end().to_string(),
            Some(text[at..].to_string()),
        ),
        None => (text.to_string(), None),
    }
}

/// One field out of a forwarded message's header block: the lines between
/// the marker and the first blank line.
fn header_of(text: &str, name: &str) -> String {
    let prefix = format!("{name}: ");
    text.lines()
        .skip(1)
        .take_while(|line| !line.trim().is_empty())
        .find_map(|line| line.strip_prefix(prefix.as_str()))
        .unwrap_or("")
        .trim()
        .to_string()
}

/// Whether `html` points at the inline image `cid` from one of its tags,
/// such as an image's `src` or a cell's `background`. The whole id has to
/// match, because `cid:logo` and `cid:logo2` name two different images.
/// Text that mentions the id, and a comment, point at nothing.
pub fn refers_to_cid(html: &str, cid: &str) -> bool {
    let needle = format!("cid:{cid}");
    let names = |value: &str| {
        value.match_indices(&needle).any(|(at, _)| {
            value[at + needle.len()..].chars().next().is_none_or(|c| {
                !c.is_ascii_alphanumeric() && !matches!(c, '-' | '_' | '.' | '@' | '+')
            })
        })
    };
    let mut found = false;
    mailrs_gmail::html::walk(html, |piece| {
        if let mailrs_gmail::html::Piece::Tag(tag) = piece
            && !found
        {
            found = tag.values().any(names);
        }
    });
    found
}

/// A file of the message a forward carries, with the bytes fetched for it.
/// An image the forwarded HTML shows keeps its id, so the `cid:` in that
/// HTML still finds it. One the HTML never names travels as a file, which
/// is how it arrived.
pub fn forwarded_file(
    found: mailrs_domain::Attachment,
    data: Vec<u8>,
    forwarded_html: Option<&str>,
) -> OutgoingAttachment {
    OutgoingAttachment {
        content_id: found
            .content_id
            .filter(|cid| forwarded_html.is_some_and(|html| refers_to_cid(html, cid))),
        filename: found.filename,
        mime_type: found.mime_type,
        data,
    }
}

/// Plain text ready to render as Markdown: every character that Markdown
/// reads as syntax is escaped, so a line of dashes stays a line of dashes
/// and `*` keeps its asterisks.
fn plain_as_markdown(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + text.len() / 8);
    for line in text.replace("\r\n", "\n").lines() {
        for ch in line.chars() {
            if matches!(
                ch,
                '\\' | '`'
                    | '*'
                    | '_'
                    | '{'
                    | '}'
                    | '['
                    | ']'
                    | '('
                    | ')'
                    | '#'
                    | '+'
                    | '-'
                    | '.'
                    | '!'
                    | '>'
                    | '|'
                    | '~'
                    | '='
            ) {
                out.push('\\');
            }
            out.push(ch);
        }
        out.push('\n');
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Draft {
    pub account_id: AccountId,
    pub from: Address,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    /// Recipients the others cannot see.
    pub bcc: Vec<Address>,
    pub subject: String,
    pub markdown: String,
    /// The body as the writer styled it, when the composer is in rich text.
    /// It decides what goes out; `markdown` is then the same body written
    /// as Markdown.
    pub rich: Option<RichBody>,
    /// `Message-ID` of the message this replies to, with angle brackets.
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    pub thread_id: Option<String>,
    pub attachments: Vec<OutgoingAttachment>,
    /// The message this draft forwards, when it forwards one.
    pub forwarded: Option<Box<Forwarded>>,
    /// The Gmail draft this composer saves into.
    pub draft_id: Option<String>,
    /// When a scheduled draft is due to go out.
    pub send_at: Option<EpochMillis>,
    /// Sign the message with the sender's key on the way out.
    pub sign: bool,
    /// Encrypt it to every recipient. With `sign`, the signature goes
    /// inside the encryption, which is the only place it means anything.
    pub encrypt: bool,
    /// Which standard signs or encrypts it. The composer chooses, since it
    /// is the one that asked both engines what they hold; a draft that is
    /// neither signed nor encrypted never uses this.
    pub standard: Standard,
}

/// When the composer hands a message over for sending.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendWhen {
    /// After the undo delay.
    Now,
    /// At this time, from a Gmail draft.
    At(EpochMillis),
}

impl Draft {
    pub fn new(account_id: AccountId, from: Address) -> Self {
        Draft {
            account_id,
            from,
            to: vec![],
            cc: vec![],
            bcc: vec![],
            subject: String::new(),
            markdown: String::new(),
            rich: None,
            in_reply_to: None,
            references: vec![],
            thread_id: None,
            attachments: vec![],
            forwarded: None,
            draft_id: None,
            send_at: None,
            sign: false,
            encrypt: false,
            standard: Standard::default(),
        }
    }

    /// Why this draft cannot be sent yet, if anything stops it.
    pub fn problem(&self) -> Option<String> {
        if self.to.is_empty() && self.cc.is_empty() && self.bcc.is_empty() {
            return Some(gettext("Add at least one recipient."));
        }
        self.to
            .iter()
            .chain(&self.cc)
            .chain(&self.bcc)
            .find(|a| !is_address(&a.email))
            .map(|a| {
                fill(
                    &gettext("“{address}” is not an email address."),
                    &[("address", &a.email)],
                )
            })
    }

    /// Takes the body of a message this composer reopens: the Markdown
    /// source from the text part, and the styled blocks from the HTML
    /// part, so a draft written in rich text comes back as it was
    /// written, wherever it was written.
    ///
    /// A forwarded message comes back whole. Reading it as prose and
    /// writing it out again is what breaks a forwarded newsletter, and a
    /// saved draft is a round trip like any other.
    pub fn take_body(&mut self, body: &MessageBody) {
        let (text, forwarded_text) = split_forwarded_text(&body_text(body));
        let (html, forwarded_html) = match body.html.as_deref() {
            Some(html) => {
                let (mine, theirs) = split_forwarded_html(html);
                (Some(mine), theirs)
            }
            None => (None, None),
        };
        self.markdown = text;
        self.rich = html
            .filter(|html| !html.trim().is_empty())
            .map(|html| RichBody::from_html(html.as_str()))
            .filter(|rich| !rich.is_empty());
        self.forwarded = match (forwarded_html, forwarded_text) {
            (None, None) => None,
            (html, text) => {
                let text = text.unwrap_or_default();
                Some(Box::new(Forwarded {
                    from: header_of(&text, "From"),
                    date: header_of(&text, "Date"),
                    subject: header_of(&text, "Subject"),
                    to: header_of(&text, "To"),
                    cc: header_of(&text, "Cc"),
                    html,
                    text,
                    whole: true,
                }))
            }
        };
    }

    /// Whether the message still shows the inline image `cid`, in what the
    /// writer wrote or in the message being forwarded.
    fn shows_image(&self, cid: &str) -> bool {
        let needle = format!("cid:{cid}");
        let written = match &self.rich {
            Some(rich) => rich
                .blocks
                .iter()
                .flat_map(|b| &b.spans)
                .any(|s| s.image.as_deref() == Some(needle.as_str())),
            None => self.markdown.contains(&needle),
        };
        written
            || self
                .forwarded
                .as_ref()
                .and_then(|f| f.html.as_deref())
                .is_some_and(|html| refers_to_cid(html, cid))
    }
}

/// What the composer does next with a message the writer asked to send.
/// [`gate`] decides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gate {
    /// It cannot go, and this says why.
    Refuse(String),
    /// The writer meant it to go out encrypted and it cannot be: ask
    /// before it goes out readable.
    ConfirmReadable,
    /// It promises a file it does not carry: ask before it goes without.
    ConfirmNoFile(Promise),
    /// Nothing stands in the way.
    Send,
}

/// What the gate needs to know beyond the draft.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Asking {
    /// The writer means the message to go out encrypted, whether or not
    /// Encrypt is still on. Send Readable clears it.
    pub secret: bool,
    /// A promised file is worth asking about: the preference is on, and
    /// Send Anyway has not already answered for this message.
    pub attachments: bool,
}

/// The next thing between `draft` and sending it.
///
/// The order is the one a person wants to hear it in. What stops the
/// message outright comes first, since no answer to a question fixes a
/// missing recipient. Encryption comes before the attachment, because a
/// message about to go out readable matters more than a forgotten file,
/// and the file question only makes sense about a message that will go.
/// The composer asks, records the answer in `asking`, and calls this
/// again until it says [`Gate::Send`] or the writer stops.
pub fn gate(draft: &Draft, when: SendWhen, now: EpochMillis, asking: Asking) -> Gate {
    if let Some(problem) = draft.problem() {
        return Gate::Refuse(problem);
    }
    if let SendWhen::At(at) = when
        && at <= now
    {
        return Gate::Refuse(gettext("Choose a time in the future"));
    }
    if asking.secret && !draft.encrypt {
        return Gate::ConfirmReadable;
    }
    if asking.attachments
        && let Some(promise) = unkept_promise(draft)
    {
        return Gate::ConfirmNoFile(promise);
    }
    Gate::Send
}

/// The file `draft` promises and does not carry. An image pasted into the
/// text keeps a promise of something to look at, since it arrives with
/// the message either way, but not a promise of a file: only an
/// attachment comes out of the reader's mail as one.
fn unkept_promise(draft: &Draft) -> Option<Promise> {
    let promise = attachcheck::promised(&draft.subject, &draft.markdown)?;
    let files = draft.attachments.iter().any(|a| a.content_id.is_none());
    let images = draft.attachments.iter().any(|a| a.content_id.is_some());
    let kept = files || (images && !promise.names_a_file);
    (!kept).then_some(promise)
}

/// Whether this reads as an address the message can go to.
pub fn is_address(email: &str) -> bool {
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

/// A draft that answers or forwards `original`. `mine` lists every address
/// the account sends as, preferred first: the reply comes from whichever one
/// the original was written to, and none of them lands in To or Cc.
/// `thread` is the whole conversation, oldest first. `original_text` and
/// `original_html` are the original's body; a forward carries the HTML
/// through unchanged, so what goes out is the message that arrived.
pub fn respond(
    kind: ReplyKind,
    account_id: AccountId,
    mine: &[Address],
    original: &MessageMeta,
    original_text: &str,
    original_html: Option<&str>,
    thread: &[MessageMeta],
) -> Draft {
    let blank = Address {
        name: None,
        email: String::new(),
    };
    let me = reply_from(mine, original)
        .or_else(|| mine.first())
        .unwrap_or(&blank);
    let mut draft = Draft::new(account_id, me.clone());
    let sender = original.from.clone().unwrap_or_else(|| Address {
        name: None,
        email: String::new(),
    });
    match kind {
        ReplyKind::Reply | ReplyKind::ReplyAll => {
            let from_me = mine.iter().any(|a| same_address(&sender, a));
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
            let ours: Vec<&Address> = mine.iter().collect();
            draft.to = dedupe(to, &ours);
            let taken: Vec<&Address> = draft.to.iter().chain(ours.iter().copied()).collect();
            draft.cc = dedupe(cc, &taken);
            draft.subject = prefixed("Re: ", &original.subject, &["re:"]);
            draft.markdown = format!(
                "\n\n{}\n{}",
                fill(
                    &attribution(),
                    &[
                        ("date", &full_date(original.date)),
                        ("sender", sender.display()),
                    ]
                ),
                quote(original_text)
            );
            draft.in_reply_to = original.rfc822_msgid.clone();
            draft.references = references(original, thread);
            draft.thread_id = Some(original.thread_id.clone());
        }
        ReplyKind::Forward => {
            draft.subject = prefixed("Fwd: ", &original.subject, &["fwd:", "fw:"]);
            draft.forwarded = Some(Box::new(Forwarded {
                from: format_recipients(std::slice::from_ref(&sender)),
                date: full_date(original.date),
                subject: original.subject.clone(),
                to: format_recipients(&original.to),
                cc: format_recipients(&original.cc),
                html: original_html
                    .filter(|h| !h.trim().is_empty())
                    .map(str::to_string),
                text: original_text.trim_end().to_string(),
                whole: false,
            }));
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

/// Puts `signature` below what is written and above the quote: at the top
/// of an empty new message, under the words in a message that already has
/// them, and before the quoted original in a reply.
pub fn with_signature(markdown: &str, signature: &str) -> String {
    let signature = signature.trim_end();
    if signature.trim().is_empty() {
        return markdown.to_string();
    }
    let at = quote_starts_at(markdown).unwrap_or(markdown.len());
    let (written, quoted) = markdown.split_at(at);
    let written = written.trim_end();
    let block = signature_block(signature);
    match quoted.is_empty() {
        true => format!("{written}{block}"),
        false => format!("{written}{block}\n\n{quoted}"),
    }
}

/// The block [`with_signature`] adds, so it can be found again.
fn signature_block(signature: &str) -> String {
    format!("\n\n-- \n{}", signature.trim_end())
}

/// The line that introduces a quoted original, in the writer's language.
/// Both values are named, so a translator may put the date and the sender
/// wherever the sentence wants them.
fn attribution() -> String {
    gettext("On {date}, {sender} wrote:")
}

/// The words the attribution ends with, which is how one is recognized
/// again: "wrote:" in English, "escreveu:" in Portuguese. A language that
/// ends the line on a value leaves nothing to match, and the colon every
/// such line carries stands in.
fn attribution_tail() -> String {
    let pattern = attribution();
    let tail = match pattern.rfind('}') {
        Some(at) => pattern[at + 1..].trim().to_string(),
        None => pattern,
    };
    match tail.is_empty() {
        true => ":".to_string(),
        false => tail,
    }
}

/// Where the quoted original starts, counting the "On Monday, Ann wrote:"
/// line that introduces it. `None` when nothing is quoted.
fn quote_starts_at(markdown: &str) -> Option<usize> {
    let tail = attribution_tail();
    let mut at = 0;
    let mut attribution = None;
    for line in markdown.split_inclusive('\n') {
        if line.trim_start().starts_with('>') {
            return Some(attribution.unwrap_or(at));
        }
        // A blank line between the attribution and the quote belongs to the
        // quote, so it does not clear what was found.
        if !line.trim().is_empty() {
            attribution = line.trim_end().ends_with(&tail).then_some(at);
        }
        at += line.len();
    }
    None
}

/// Where the signature block starts in `head`, when the text below the
/// separator is still `signature`.
///
/// The separator goes out as `-- `, the way every mail client writes it,
/// but a trip through the rich body and back trims the trailing space, so
/// both spellings count, and the lines below are compared without their
/// trailing whitespace.
fn signature_starts_at(head: &str, signature: &str) -> Option<usize> {
    let bare = |text: &str| {
        text.lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string()
    };
    let mut at = 0;
    let mut separator = None;
    for line in head.split_inclusive('\n') {
        if line.trim_end() == "--" {
            separator = Some(at);
        }
        at += line.len();
    }
    let start = separator?;
    let below = start + head[start..].find('\n')? + 1;
    (bare(&head[below..]) == bare(signature)).then_some(start)
}

/// Swaps the signature when the writer picks another send-as address.
/// Gmail keeps one signature per address, so the message has to follow.
///
/// Only a block [`with_signature`] left untouched is swapped. Once someone
/// has edited their sign-off, the text stays as they typed it, because
/// losing a rewritten one to a dropdown would be worse than showing the
/// wrong one.
pub fn restyle_signature(markdown: &str, old: &str, new: &str) -> String {
    let quote = quote_starts_at(markdown).unwrap_or(markdown.len());
    // Where the writer's own words end: above the old signature while it is
    // still where it was left, else right above the quote.
    let ends = match old.trim().is_empty() {
        true => Some(quote),
        false => signature_starts_at(&markdown[..quote], old),
    };
    let Some(ends) = ends else {
        return markdown.to_string();
    };
    let typed = markdown[..ends].trim_end_matches('\n');
    let tail = &markdown[quote..];
    let gap = if tail.is_empty() { "" } else { "\n\n" };
    let block = match new.trim().is_empty() {
        true => String::new(),
        false => signature_block(new),
    };
    format!("{typed}{block}{gap}{tail}")
}

/// Lines to put in place of others: `removed` lines from `first` on go,
/// and `lines` take their place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineChange {
    pub first: usize,
    pub removed: usize,
    pub lines: Vec<String>,
}

/// The lines [`restyle_signature`] would change in a body read as
/// `lines`, when it would change any.
///
/// The composer's buffer holds the body line by line, and writing the
/// whole of it again to swap a signature would move the cursor and cost
/// the writer their Undo. `lines` only needs to reach the first quoted
/// line, since the signature sits above the quote.
pub fn signature_change(lines: &[String], old: &str, new: &str) -> Option<LineChange> {
    let text = lines.join("\n");
    let swapped = restyle_signature(&text, old, new);
    if swapped == text {
        return None;
    }
    let after: Vec<&str> = swapped.split('\n').collect();
    let first = lines
        .iter()
        .zip(&after)
        .take_while(|(a, b)| a == *b)
        .count();
    // The lines kept at the end, counted without reaching back into the
    // ones kept at the start.
    let room = lines.len().min(after.len()) - first;
    let kept = lines
        .iter()
        .rev()
        .zip(after.iter().rev())
        .take(room)
        .take_while(|(a, b)| a == *b)
        .count();
    Some(LineChange {
        first,
        removed: lines.len() - first - kept,
        lines: after[first..after.len() - kept]
            .iter()
            .map(|line| line.to_string())
            .collect(),
    })
}

/// A message body as plain text, for quoting and for reopening drafts.
pub fn body_text(body: &MessageBody) -> String {
    match (&body.text, &body.html) {
        (Some(text), _) if !text.trim().is_empty() => text.replace("\r\n", "\n"),
        (_, Some(html)) => html_to_text(html),
        _ => String::new(),
    }
}

/// HTML as the plain text a reader takes in. The gmail crate holds the
/// one implementation, which the signatures and automatic replies use too.
pub use mailrs_gmail::html_to_text;

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
        .replace(
            "<blockquote>",
            &format!("<blockquote style=\"{}\">", richtext::QUOTE),
        )
        .replace("<pre>", &format!("<pre style=\"{}\">", richtext::PRE))
        .replace("<p>", &format!("<p style=\"{}\">", richtext::PARAGRAPH))
        .replace("<img ", &format!("<img style=\"{}\" ", richtext::IMAGE));
    richtext::document(&body)
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
        .map_or("penguin-mail.local", |(_, d)| d);
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

/// A message the composer built on its way out. The app sends these
/// bytes rather than building the message a second time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Built {
    /// The whole message, for one that is neither signed nor encrypted.
    Message(Vec<u8>),
    /// The body an engine signs or encrypts, for one that is. The engine
    /// may ask for a passphrase, so its work waits for the app.
    Body(Vec<u8>),
}

/// Builds as much of `draft` as can be built before it leaves the
/// composer: all of it, or the body an engine will sign or encrypt.
pub fn build(draft: &Draft, date_secs: i64, message_id: &str) -> Result<Built, String> {
    match draft.sign || draft.encrypt {
        true => build_body_part(draft).map(Built::Body),
        false => build_mime(draft, date_secs, message_id).map(Built::Message),
    }
}

/// The RFC 822 bytes for `draft`.
pub fn build_mime(draft: &Draft, date_secs: i64, message_id: &str) -> Result<Vec<u8>, String> {
    let (text, html) = written(draft);
    envelope(draft, date_secs, message_id)
        .body(body_tree(draft, text, html))
        .write_to_vec()
        .map_err(|e| e.to_string())
}

/// The body of `draft` as one MIME entity: the headers that describe the
/// body, a blank line, and the body. This is what the engine signs or
/// encrypts, which is why it carries no `From`, `To` or `Subject`.
pub fn build_body_part(draft: &Draft) -> Result<Vec<u8>, String> {
    let (text, html) = written(draft);
    let mut out = Vec::new();
    MessageBuilder::new()
        .body(body_tree(draft, text, html))
        .write_body(&mut out)
        .map_err(|e| e.to_string())?;
    Ok(out)
}

/// The message `draft` describes, with `entity` as its body.
///
/// `entity` is what either engine's `sign` or `encrypt` handed back: the
/// headers that describe it, a blank line, and the body under them. Those
/// headers go on the message and the rest goes in as its body, byte for
/// byte.
/// Rewrapping a line or re-encoding a part here would leave the signature
/// covering bytes that no longer exist, and the reader seeing a warning
/// instead of a message.
pub fn build_protected(
    draft: &Draft,
    date_secs: i64,
    message_id: &str,
    entity: Vec<u8>,
) -> Result<Vec<u8>, String> {
    envelope(draft, date_secs, message_id)
        .body(MimePart::raw(entity))
        .write_to_vec()
        .map_err(|e| e.to_string())
}

/// A draft for Gmail to keep while its message is meant to go out
/// encrypted: what [`build_protected`] writes, plus the header `mark`,
/// which tells the composer that reopens it what to switch back on.
pub fn build_protected_draft(
    draft: &Draft,
    date_secs: i64,
    message_id: &str,
    entity: Vec<u8>,
    mark: (&'static str, &'static str),
) -> Result<Vec<u8>, String> {
    envelope(draft, date_secs, message_id)
        .header(mark.0, Raw::new(mark.1))
        .body(MimePart::raw(entity))
        .write_to_vec()
        .map_err(|e| e.to_string())
}

/// The two ways of reading what the writer wrote, with whatever the draft
/// forwards after them.
fn written(draft: &Draft) -> (String, String) {
    let (mut text, mut html) = match &draft.rich {
        Some(rich) => (rich.to_plain(), rich.to_html()),
        None => (draft.markdown.clone(), markdown_to_html(&draft.markdown)),
    };
    if let Some(forwarded) = &draft.forwarded {
        text.push_str(&forwarded.to_plain());
        html.push_str(&forwarded.to_html());
    }
    (text, html)
}

/// Everything about the message but its body: who it is from and to, what
/// it answers, and when it was written.
fn envelope<'a>(draft: &'a Draft, date_secs: i64, message_id: &str) -> MessageBuilder<'a> {
    let mut builder = MessageBuilder::new()
        .from(mime_address(&draft.from))
        .subject(draft.subject.trim().to_string())
        .date(date_secs)
        .message_id(bare_id(message_id));
    if !draft.to.is_empty() {
        builder = builder.to(draft.to.iter().map(mime_address).collect::<Vec<_>>());
    }
    if !draft.cc.is_empty() {
        builder = builder.cc(draft.cc.iter().map(mime_address).collect::<Vec<_>>());
    }
    if !draft.bcc.is_empty() {
        builder = builder.bcc(draft.bcc.iter().map(mime_address).collect::<Vec<_>>());
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
    builder
}

/// The body of the message, nested the way a reader expects to find it.
///
/// `multipart/alternative` holds the two ways of reading what the writer
/// wrote. A `multipart/related` around it holds the images the HTML shows
/// as `cid:`, which is where RFC 2387 says to look for them: an image left
/// beside the text in `multipart/mixed` resolves in Gmail's web client and
/// nowhere near everywhere else. A `multipart/mixed` around that holds the
/// files the writer attached. Each wrapper appears only when it has
/// something to hold.
fn body_tree<'a>(draft: &'a Draft, text: String, html: String) -> MimePart<'a> {
    let mut body = MimePart::new(
        "multipart/alternative",
        vec![
            MimePart::new("text/plain", text),
            MimePart::new("text/html", html),
        ],
    );
    let shown: Vec<&OutgoingAttachment> = draft
        .attachments
        .iter()
        .filter(|a| {
            a.content_id
                .as_deref()
                .is_some_and(|cid| draft.shows_image(cid))
        })
        .collect();
    if !shown.is_empty() {
        let mut parts = Vec::with_capacity(shown.len() + 1);
        parts.push(body);
        parts.extend(shown.into_iter().map(inline_part));
        body = MimePart::new(
            ContentType::new("multipart/related").attribute("type", "text/html"),
            parts,
        );
    }
    let files: Vec<&OutgoingAttachment> = draft
        .attachments
        .iter()
        .filter(|a| a.content_id.is_none())
        .collect();
    if !files.is_empty() {
        let mut parts = Vec::with_capacity(files.len() + 1);
        parts.push(body);
        parts.extend(files.into_iter().map(file_part));
        body = MimePart::new("multipart/mixed", parts);
    }
    body
}

/// An image the HTML shows. It keeps its filename beside its `Content-ID`,
/// because a reader that lists parts by filename, Gmail's API among them,
/// passes over a part that has none, and then has no handle to fetch the
/// image the `cid:` asks for.
fn inline_part(attachment: &OutgoingAttachment) -> MimePart<'_> {
    let name = attachment.filename.clone();
    MimePart::new(
        ContentType::new(attachment.mime_type.clone()).attribute("name", name.clone()),
        attachment.data.clone(),
    )
    .header(
        "Content-Disposition",
        ContentType::new("inline").attribute("filename", name),
    )
    .cid(attachment.content_id.clone().unwrap_or_default())
}

/// A file the writer attached.
fn file_part(attachment: &OutgoingAttachment) -> MimePart<'_> {
    MimePart::new(
        ContentType::new(attachment.mime_type.clone())
            .attribute("name", attachment.filename.clone()),
        attachment.data.clone(),
    )
    .attachment(attachment.filename.clone())
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
            list_unsubscribe: None,
            one_click: false,
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
            &[me()],
            &second,
            "Noon works.\n\nSee you",
            None,
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
            &[me()],
            &sent,
            "hi",
            None,
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
            &[me()],
            &original,
            "x",
            None,
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

    fn sales() -> Address {
        addr(Some("Sales"), "sales@example.com")
    }

    fn identity(account: AccountId, email: &str, signature: &str, default: bool) -> Identity {
        Identity {
            account_id: account,
            account_email: format!("own{account}@example.com"),
            address: addr(None, email),
            signature: signature.to_string(),
            default,
        }
    }

    #[test]
    fn a_reply_comes_from_the_address_it_was_written_to() {
        let original = message(
            "m1",
            addr(Some("Ann"), "ann@example.com"),
            vec![sales()],
            vec![],
        );
        let draft = respond(
            ReplyKind::Reply,
            1,
            &[me(), sales()],
            &original,
            "x",
            None,
            std::slice::from_ref(&original),
        );
        assert_eq!(draft.from, sales());
        // Neither of the account's own addresses belongs in the answer.
        assert_eq!(draft.to, vec![addr(Some("Ann"), "ann@example.com")]);
    }

    #[test]
    fn a_reply_to_a_list_falls_back_to_the_first_address() {
        let original = message(
            "m1",
            addr(Some("Ann"), "ann@example.com"),
            vec![addr(None, "list@example.com")],
            vec![],
        );
        let draft = respond(
            ReplyKind::Reply,
            1,
            &[me(), sales()],
            &original,
            "x",
            None,
            std::slice::from_ref(&original),
        );
        assert_eq!(draft.from, me());
    }

    #[test]
    fn an_alias_in_cc_still_picks_the_sender() {
        let original = message(
            "m1",
            addr(Some("Ann"), "ann@example.com"),
            vec![addr(None, "team@example.com")],
            vec![addr(None, "SALES@example.com")],
        );
        assert_eq!(
            reply_from(&[me(), sales()], &original),
            Some(&sales()),
            "an alias copied in is still the address to answer from"
        );
    }

    fn lines(text: &str) -> Vec<String> {
        text.split('\n').map(String::from).collect()
    }

    /// `lines` with `change` made to them.
    fn changed(mut lines: Vec<String>, change: LineChange) -> String {
        lines.splice(change.first..change.first + change.removed, change.lines);
        lines.join("\n")
    }

    #[test]
    fn a_signature_swap_touches_only_the_signature_lines() {
        let body = "Hi Ann,\n\nMonday works.\n\n-- \nDana\n\nOn Monday, Ann wrote:\n> hi";
        let change = signature_change(&lines(body), "Dana", "Dana Reyes\nSales").unwrap();
        assert_eq!(
            change,
            LineChange {
                first: 5,
                removed: 1,
                lines: vec!["Dana Reyes".into(), "Sales".into()],
            }
        );
        assert_eq!(
            changed(lines(body), change),
            restyle_signature(body, "Dana", "Dana Reyes\nSales")
        );
    }

    #[test]
    fn a_signature_comes_and_goes_as_whole_lines() {
        for (body, old, new) in [
            ("Hi\n\nOn Monday, Ann wrote:\n> hi", "", "Dana"),
            ("Hi\n\n-- \nDana\n\nOn Monday, Ann wrote:\n> hi", "Dana", ""),
            ("Hi", "", "Dana"),
            ("Hi\n\n-- \nDana", "Dana", ""),
            // The rich buffer loses the space after the dashes.
            ("Hi\n\n--\nDana", "Dana", "Sales"),
        ] {
            let change = signature_change(&lines(body), old, new).unwrap();
            assert_eq!(
                changed(lines(body), change),
                restyle_signature(body, old, new),
                "{body:?}"
            );
        }
    }

    #[test]
    fn an_edited_signature_is_left_alone() {
        let body = "Hi\n\n-- \nDana, who rewrote this";
        assert_eq!(signature_change(&lines(body), "Dana", "Sales"), None);
    }

    #[test]
    fn the_composer_opens_on_the_address_the_draft_names() {
        let identities = [
            identity(1, "dana@example.com", "", true),
            identity(1, "sales@example.com", "", false),
        ];
        let from = addr(None, "SALES@example.com");
        assert_eq!(opening_identity(&identities, 1, &from, None), Some(1));
    }

    #[test]
    fn a_new_message_opens_on_the_address_the_account_last_used() {
        let identities = [
            identity(1, "dana@example.com", "", true),
            identity(1, "sales@example.com", "", false),
        ];
        let blank = addr(None, "");
        assert_eq!(
            opening_identity(&identities, 1, &blank, Some("sales@example.com")),
            Some(1)
        );
        // With nothing remembered, Gmail's own default wins.
        assert_eq!(opening_identity(&identities, 1, &blank, None), Some(0));
    }

    #[test]
    fn the_from_row_only_offers_the_drafts_own_account() {
        let identities = [
            identity(1, "dana@example.com", "", false),
            identity(2, "other@example.com", "", true),
            identity(2, "alias@example.com", "", false),
        ];
        let blank = addr(None, "");
        assert_eq!(opening_identity(&identities, 2, &blank, None), Some(1));
        assert_eq!(opening_identity(&identities, 9, &blank, None), None);
    }

    #[test]
    fn the_signature_follows_the_address() {
        // What the writer typed sits above the block, as it does once they
        // start answering.
        let typed = "Thanks Ann.\n\n-- \nDana\n\nOn Monday, Ann wrote:\n> hi";
        assert_eq!(
            restyle_signature(typed, "Dana", "Dana, Sales"),
            "Thanks Ann.\n\n-- \nDana, Sales\n\nOn Monday, Ann wrote:\n> hi"
        );
        let signed = with_signature("\n\nOn Monday, Ann wrote:\n> hi", "Dana");
        let swapped = restyle_signature(&signed, "Dana", "Dana, Sales");
        assert_eq!(
            swapped,
            "\n\n-- \nDana, Sales\n\nOn Monday, Ann wrote:\n> hi"
        );
        // And back again, so switching twice leaves no trail.
        assert_eq!(restyle_signature(&swapped, "Dana, Sales", "Dana"), signed);
    }

    #[test]
    fn an_address_with_no_signature_gains_and_loses_one() {
        // The signature goes below what was typed and above any quote.
        assert_eq!(restyle_signature("", "", "Dana"), "\n\n-- \nDana");
        assert_eq!(restyle_signature("\n\n-- \nDana", "Dana", ""), "");
        assert_eq!(
            restyle_signature("Hi Ann\n\nOn Monday, Ann wrote:\n> hi", "", "Dana"),
            "Hi Ann\n\n-- \nDana\n\nOn Monday, Ann wrote:\n> hi"
        );
    }

    #[test]
    fn the_signature_survives_the_trip_through_the_rich_body() {
        // In rich text the composer reads the body back as Markdown before
        // swapping, so the block has to come out of that round trip intact,
        // trailing space and all.
        let signed = with_signature("\n\nOn Monday, Ann wrote:\n> hi", "Dana");
        let round_tripped = crate::richtext::RichBody::from_markdown(&signed).to_markdown();
        let swapped = restyle_signature(&round_tripped, "Dana", "Dana, Sales");
        assert!(
            swapped.contains("\n\n-- \nDana, Sales"),
            "{swapped:?} came from {round_tripped:?}"
        );
        assert!(swapped.contains("> hi"));
    }

    #[test]
    fn a_signature_someone_edited_is_left_alone() {
        // Losing a rewritten sign-off to a dropdown would be worse than
        // showing the wrong one, so an edited block stays put.
        let edited = "\n\n-- \nDana (on leave)\n\nOn Monday, Ann wrote:\n> hi";
        assert_eq!(restyle_signature(edited, "Dana", "Sales"), edited);
    }

    #[test]
    fn prefixes_are_not_doubled() {
        let mut original = message("m1", addr(None, "a@example.com"), vec![], vec![]);
        original.subject = "RE: Budget".into();
        assert_eq!(
            respond(ReplyKind::Reply, 1, &[me()], &original, "", None, &[]).subject,
            "RE: Budget"
        );
        original.subject = "Fw: Budget".into();
        assert_eq!(
            respond(ReplyKind::Forward, 1, &[me()], &original, "", None, &[]).subject,
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
            &[me()],
            &original,
            "Menu attached.",
            None,
            &[],
        );
        assert!(draft.to.is_empty());
        assert_eq!(draft.subject, "Fwd: Lunch plans");
        // The writer starts on an empty page; the original travels beside it.
        assert!(draft.markdown.trim().is_empty());
        let forwarded = draft.forwarded.as_ref().unwrap();
        assert_eq!(forwarded.from, "Ann <ann@example.com>");
        assert_eq!(forwarded.text, "Menu attached.");
        assert!(
            forwarded
                .to_plain()
                .contains("From: Ann <ann@example.com>\n")
        );
        assert!(forwarded.to_plain().ends_with("Menu attached."));
        assert!(draft.in_reply_to.is_none() && draft.thread_id.is_none());
    }

    #[test]
    fn a_forwarded_html_message_goes_out_as_it_arrived() {
        let original = message(
            "m1",
            addr(Some("Ann"), "ann@example.com"),
            vec![me()],
            vec![],
        );
        // A newsletter: a table, a heading, an asterisk, a line of dashes.
        // Read as Markdown and written back out, none of it survives.
        let html = "<table><tr><td><h1>Sale *now* on</h1></td></tr></table>\
                    <p>Terms apply</p>";
        let text = "Sale *now* on\n-------------\n# Terms apply";
        let mut draft = respond(
            ReplyKind::Forward,
            1,
            &[me()],
            &original,
            text,
            Some(html),
            &[],
        );
        draft.to = vec![addr(None, "bob@example.com")];
        let raw = build_mime(&draft, 0, "id@example.com").unwrap();
        let parsed = MessageParser::default().parse(&raw).unwrap();
        let sent = parsed.body_html(0).unwrap();
        assert!(sent.contains("<table>"), "the table is still a table");
        assert!(sent.contains("<h1>Sale *now* on</h1>"), "and the heading");
        assert!(sent.contains("---------- Forwarded message ----------"));
        assert!(sent.contains("<b>From:</b> Ann &lt;ann@example.com&gt;"));
        let plain = parsed.body_text(0).unwrap();
        assert!(plain.contains("Sale *now* on"));
    }

    #[test]
    fn a_forwarded_text_message_keeps_its_line_breaks_and_punctuation() {
        let original = message(
            "m1",
            addr(Some("Ann"), "ann@example.com"),
            vec![me()],
            vec![],
        );
        let text = "Line one\nLine two\n-------------\n# Not a heading\n* Not a bullet";
        let mut draft = respond(ReplyKind::Forward, 1, &[me()], &original, text, None, &[]);
        draft.to = vec![addr(None, "bob@example.com")];
        let raw = build_mime(&draft, 0, "id@example.com").unwrap();
        let sent = MessageParser::default()
            .parse(&raw)
            .unwrap()
            .body_html(0)
            .unwrap()
            .to_string();
        assert!(sent.contains("# Not a heading"), "{sent}");
        assert!(sent.contains("* Not a bullet"), "{sent}");
        assert!(!sent.contains("<h1>"), "a hash in mail is a hash");
        assert!(!sent.contains("<li>"), "an asterisk in mail is an asterisk");
    }

    #[test]
    fn a_saved_forward_reopens_with_the_original_still_whole() {
        let original = message(
            "m1",
            addr(Some("Ann"), "ann@example.com"),
            vec![me()],
            vec![],
        );
        let html = "<table><tr><td>Sale</td></tr></table>";
        let mut draft = respond(
            ReplyKind::Forward,
            1,
            &[me()],
            &original,
            "Sale",
            Some(html),
            &[],
        );
        draft.to = vec![addr(None, "bob@example.com")];
        draft.markdown = "Thought you would want this.".into();
        let raw = build_mime(&draft, 0, "id@example.com").unwrap();
        let parsed = MessageParser::default().parse(&raw).unwrap();
        let saved = MessageBody {
            html: Some(parsed.body_html(0).unwrap().to_string()),
            text: Some(parsed.body_text(0).unwrap().to_string()),
            ..Default::default()
        };
        let mut reopened = Draft::new(1, me());
        reopened.take_body(&saved);
        assert!(reopened.markdown.contains("Thought you would want this."));
        assert!(!reopened.markdown.contains("Forwarded message"));
        let forwarded = reopened.forwarded.as_ref().expect("the forward came back");
        assert_eq!(forwarded.from, "Ann <ann@example.com>");
        assert_eq!(forwarded.subject, "Lunch plans");
        assert!(forwarded.to_html().contains("<table>"));

        // And sending the reopened draft writes the same message again.
        reopened.to = vec![addr(None, "bob@example.com")];
        let again = build_mime(&reopened, 0, "id2@example.com").unwrap();
        let sent = MessageParser::default()
            .parse(&again)
            .unwrap()
            .body_html(0)
            .unwrap()
            .to_string();
        assert!(sent.contains("<table>"));
        assert_eq!(sent.matches("Forwarded message").count(), 1);
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
            content_id: None,
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
    fn a_rich_body_decides_both_parts_of_the_message() {
        use crate::richtext::{Block, BlockKind, Span, Style};

        let bold = |text: &str| Span {
            style: Style {
                bold: true,
                ..Style::default()
            },
            ..Span::plain(text)
        };
        let link = |text: &str, url: &str| Span {
            link: Some(url.into()),
            ..Span::plain(text)
        };
        let mut draft = Draft::new(1, me());
        draft.to = vec![addr(None, "ann@example.com")];
        draft.bcc = vec![addr(None, "cy@example.com")];
        draft.rich = Some(RichBody {
            blocks: vec![
                Block::new(
                    BlockKind::Paragraph,
                    vec![
                        Span::plain("Hi "),
                        bold("Ann"),
                        Span::plain(", see "),
                        link("the menu", "https://example.com/menu"),
                    ],
                ),
                Block::new(BlockKind::Bullet, vec![Span::plain("soup")]),
            ],
        });
        draft.markdown = draft.rich.as_ref().unwrap().to_markdown();
        let raw = build_mime(&draft, 0, "id@example.com").unwrap();
        let parsed = MessageParser::default().parse(&raw).unwrap();
        assert_eq!(
            parsed.bcc().unwrap().first().unwrap().address(),
            Some("cy@example.com")
        );
        let text = parsed.body_text(0).unwrap().replace("\r\n", "\n");
        assert_eq!(
            text.trim_end(),
            "Hi Ann, see the menu <https://example.com/menu>\n- soup"
        );
        let html = parsed.body_html(0).unwrap();
        assert!(html.contains("<strong>Ann</strong>"), "{html}");
        assert!(html.contains("<li>soup</li>"), "{html}");
        assert!(!html.contains("**"), "{html}");
    }

    #[test]
    fn a_rich_body_keeps_the_images_it_still_shows() {
        use crate::richtext::{Block, BlockKind, Span};

        let mut draft = Draft::new(1, me());
        draft.to = vec![addr(None, "ann@example.com")];
        draft.rich = Some(RichBody {
            blocks: vec![Block::new(
                BlockKind::Paragraph,
                vec![Span::image("map", "cid:map1@mailrs")],
            )],
        });
        let image = |cid: &str| OutgoingAttachment {
            filename: format!("{cid}.png"),
            mime_type: "image/png".into(),
            data: vec![137, 80, 78, 71],
            content_id: Some(cid.into()),
        };
        draft.attachments = vec![image("map1@mailrs"), image("gone@mailrs")];
        let raw = build_mime(&draft, 0, "id@example.com").unwrap();
        let parsed = MessageParser::default().parse(&raw).unwrap();
        let ids: Vec<&str> = parsed
            .attachments()
            .filter_map(|a| a.content_id())
            .collect();
        assert_eq!(ids, ["map1@mailrs"]);
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
    fn a_greater_than_sign_in_an_attribute_stays_out_of_the_quote() {
        let text = html_to_text(r#"<p title="a > b">Hello</p><p>there</p>"#);
        assert_eq!(text, "Hello\n\nthere");
    }

    #[test]
    fn a_forward_is_found_by_its_tag_not_by_a_stray_angle_bracket() {
        let mine = r#"<p title="x<y">Mine</p>"#;
        let theirs = r#"<div title="a > b" class="mailrs-forwarded"><p>Theirs</p></div>"#;
        let (written, forwarded) = split_forwarded_html(&format!("{mine}{theirs}"));
        assert_eq!(written, mine);
        assert_eq!(forwarded.as_deref(), Some(theirs));
        // The marker's name said in the text, or in a comment, is no forward.
        for html in [
            r#"<p>class="mailrs-forwarded"</p>"#,
            r#"<!-- <div class="mailrs-forwarded"> --><p>hi</p>"#,
        ] {
            assert_eq!(
                split_forwarded_html(html),
                (html.to_string(), None),
                "{html}"
            );
        }
    }

    #[test]
    fn only_a_tag_refers_to_an_inline_image() {
        assert!(refers_to_cid(r#"<img alt="a > b" src="cid:logo">"#, "logo"));
        assert!(!refers_to_cid(r#"<img src="cid:logo2">"#, "logo"));
        // Words about the image, and markup nobody will render, are not it.
        assert!(!refers_to_cid("<p>see cid:logo above</p>", "logo"));
        assert!(!refers_to_cid(r#"<!-- <img src="cid:logo"> -->"#, "logo"));
    }

    #[test]
    fn html_bodies_become_readable_text() {
        let text = html_to_text(
            "<html><head><style>p{color:red}</style></head><body><p>Hello&nbsp;there</p><div>Line <b>two</b><br>three</div>\
             <script>x()</script><p></p><p>&amp; four</p></body></html>",
        );
        assert_eq!(text, "Hello there\n\nLine two\nthree\n\n& four");
    }

    #[test]
    fn list_and_quote_prefixes_toggle_on_whole_lines() {
        assert_eq!(
            toggle_prefix("milk\neggs", LinePrefix::Bullet),
            "- milk\n- eggs"
        );
        assert_eq!(
            toggle_prefix("- milk\n- eggs", LinePrefix::Bullet),
            "milk\neggs"
        );
        assert_eq!(
            toggle_prefix("- milk\n\n- eggs", LinePrefix::Numbered),
            "1. milk\n\n2. eggs"
        );
        assert_eq!(toggle_prefix("1. a\n2. b", LinePrefix::Numbered), "a\nb");
        assert_eq!(toggle_prefix("said", LinePrefix::Quote), "> said");
        assert_eq!(toggle_prefix("> said", LinePrefix::Quote), "said");
    }

    #[test]
    fn inline_images_go_in_as_related_parts() {
        let mut draft = Draft::new(1, me());
        draft.to = vec![addr(None, "ann@example.com")];
        draft.markdown = "Look: ![map](cid:map1@mailrs)".into();
        let image = |cid: &str| OutgoingAttachment {
            filename: format!("{cid}.png"),
            mime_type: "image/png".into(),
            data: vec![137, 80, 78, 71],
            content_id: Some(cid.into()),
        };
        draft.attachments = vec![image("map1@mailrs"), image("gone@mailrs")];
        let raw = build_mime(&draft, 0, "id@example.com").unwrap();
        let parsed = MessageParser::default().parse(&raw).unwrap();
        let ids: Vec<&str> = parsed
            .attachments()
            .filter_map(|a| a.content_id())
            .collect();
        assert_eq!(ids, ["map1@mailrs"]);
        assert!(
            parsed
                .body_html(0)
                .unwrap()
                .contains("src=\"cid:map1@mailrs\"")
        );
        let mime = String::from_utf8(raw).unwrap();
        assert!(
            mime.contains("multipart/related"),
            "a cid: image resolves inside multipart/related, not beside the text"
        );
        assert!(
            !mime.contains("multipart/mixed"),
            "nothing was attached, so nothing needs the mixed wrapper"
        );
    }

    #[test]
    fn an_inline_image_carries_a_filename_as_well_as_its_id() {
        let mut draft = Draft::new(1, me());
        draft.to = vec![addr(None, "ann@example.com")];
        draft.markdown = "Look: ![map](cid:map1@mailrs)".into();
        draft.attachments = vec![OutgoingAttachment {
            filename: "map.png".into(),
            mime_type: "image/png".into(),
            data: vec![137, 80, 78, 71],
            content_id: Some("map1@mailrs".into()),
        }];
        let raw = build_mime(&draft, 0, "id@example.com").unwrap();
        let mime = String::from_utf8(raw.clone()).unwrap();
        assert!(mime.contains("Content-Disposition: inline; filename=\"map.png\""));
        assert!(mime.contains("Content-Type: image/png; name=\"map.png\""));
        // Gmail's API lists a part by its filename, so a nameless part
        // never reaches the reader that has to fetch it.
        let parsed = MessageParser::default().parse(&raw).unwrap();
        assert_eq!(
            parsed
                .attachments()
                .next()
                .unwrap()
                .attachment_name()
                .unwrap(),
            "map.png"
        );
    }

    #[test]
    fn an_attached_file_and_a_shown_image_nest_one_inside_the_other() {
        let mut draft = Draft::new(1, me());
        draft.to = vec![addr(None, "ann@example.com")];
        draft.markdown = "Look: ![map](cid:map1@mailrs)".into();
        draft.attachments = vec![
            OutgoingAttachment {
                filename: "map.png".into(),
                mime_type: "image/png".into(),
                data: vec![137, 80, 78, 71],
                content_id: Some("map1@mailrs".into()),
            },
            OutgoingAttachment {
                filename: "notes.pdf".into(),
                mime_type: "application/pdf".into(),
                data: b"%PDF-1.4".to_vec(),
                content_id: None,
            },
        ];
        let raw = build_mime(&draft, 0, "id@example.com").unwrap();
        let mime = String::from_utf8(raw.clone()).unwrap();
        let mixed = mime.find("multipart/mixed").expect("a file was attached");
        let related = mime.find("multipart/related").expect("an image is shown");
        let alternative = mime
            .find("multipart/alternative")
            .expect("the body reads two ways");
        assert!(
            mixed < related && related < alternative,
            "mixed holds related holds alternative"
        );
        let parsed = MessageParser::default().parse(&raw).unwrap();
        assert_eq!(parsed.attachment_count(), 2);
    }

    #[test]
    fn signatures_sit_under_the_words_and_above_the_quote() {
        assert_eq!(with_signature("", "Dana\n"), "\n\n-- \nDana");
        assert_eq!(
            with_signature("\n\nOn Monday, Ann wrote:\n> hi", "Dana"),
            "\n\n-- \nDana\n\nOn Monday, Ann wrote:\n> hi"
        );
        // A message someone or something already wrote keeps its words
        // first, which is where the assistant's drafts arrive.
        assert_eq!(
            with_signature("Hello,\n\nMonday works.", "Dana"),
            "Hello,\n\nMonday works.\n\n-- \nDana"
        );
        assert_eq!(
            with_signature("Monday works.\n\nOn Monday, Ann wrote:\n> hi", "Dana"),
            "Monday works.\n\n-- \nDana\n\nOn Monday, Ann wrote:\n> hi"
        );
        assert_eq!(with_signature("body", "  "), "body");
    }

    #[test]
    fn a_rich_draft_comes_back_from_the_message_it_was_saved_as() {
        use crate::richtext::{Block, BlockKind, Span, Style};

        let bold = |text: &str| Span {
            style: Style {
                bold: true,
                ..Style::default()
            },
            ..Span::plain(text)
        };
        let mut draft = Draft::new(1, me());
        draft.to = vec![addr(None, "ann@example.com")];
        draft.subject = "Lunch".into();
        draft.rich = Some(RichBody {
            blocks: vec![
                Block::new(
                    BlockKind::Paragraph,
                    vec![
                        Span::plain("Hi "),
                        bold("Ann"),
                        Span::plain(", the menu is "),
                        Span {
                            link: Some("https://e.com/menu".into()),
                            ..Span::plain("here")
                        },
                    ],
                ),
                Block::default(),
                Block::new(BlockKind::Bullet, vec![Span::plain("soup")]),
                Block::new(BlockKind::Bullet, vec![Span::plain("salad")]),
            ],
        });
        draft.markdown = draft.rich.as_ref().unwrap().to_markdown();

        // Save it the way the composer does, then reopen it from Gmail.
        let raw = build_mime(&draft, 0, "id@example.com").unwrap();
        let parsed = MessageParser::default().parse(&raw).unwrap();
        let body = MessageBody {
            text: parsed.body_text(0).map(|t| t.to_string()),
            html: parsed.body_html(0).map(|h| h.to_string()),
            ..Default::default()
        };
        let mut reopened = Draft::new(1, me());
        reopened.take_body(&body);
        assert_eq!(reopened.rich, draft.rich, "{:#?}", reopened.rich);
        // What goes out the second time is what went out the first.
        let again = build_mime(&reopened, 0, "id@example.com").unwrap();
        let parsed_again = MessageParser::default().parse(&again).unwrap();
        assert_eq!(parsed_again.body_html(0), parsed.body_html(0));
    }

    #[test]
    fn a_draft_written_in_another_client_reopens_with_its_words() {
        let body = MessageBody {
            text: Some("Hi Ann,\r\nBringing soup.".into()),
            html: Some(
                "<div dir=\"ltr\"><div>Hi Ann,</div><div>Bringing <b>soup</b>.</div></div>".into(),
            ),
            ..Default::default()
        };
        let mut draft = Draft::new(1, me());
        draft.take_body(&body);
        assert_eq!(draft.markdown, "Hi Ann,\nBringing soup.");
        let rich = draft.rich.expect("the HTML part opens as rich text");
        assert_eq!(rich.to_plain(), "Hi Ann,\nBringing soup.");
        assert!(rich.blocks[1].spans[1].style.bold);
    }

    #[test]
    fn a_draft_with_no_html_reopens_as_markdown() {
        let body = MessageBody {
            text: Some("# Title\r\nBody".into()),
            html: None,
            ..Default::default()
        };
        let mut draft = Draft::new(1, me());
        draft.take_body(&body);
        assert_eq!(draft.markdown, "# Title\nBody");
        assert!(draft.rich.is_none());
    }

    #[test]
    fn drafts_reopen_from_their_text_part() {
        let body = MessageBody {
            text: Some("# Title\r\nBody".into()),
            html: Some("<h1>Title</h1>".into()),
            ..Default::default()
        };
        assert_eq!(body_text(&body), "# Title\nBody");
    }

    #[test]
    fn the_part_handed_to_gpg_is_the_body_and_nothing_about_the_sender() {
        let mut draft = Draft::new(1, me());
        draft.to = vec![addr(None, "ann@example.com")];
        draft.subject = "Lunch".into();
        draft.markdown = "Meet at six.".into();

        let part = String::from_utf8(build_body_part(&draft).unwrap()).unwrap();

        assert!(
            part.starts_with("Content-Type: multipart/alternative"),
            "{part}"
        );
        assert!(part.contains("Meet at six."), "{part}");
        for header in ["From:", "To:", "Subject:", "Date:"] {
            assert!(!part.contains(header), "{header} is in {part}");
        }
    }

    #[test]
    fn a_protected_message_carries_the_entity_byte_for_byte() {
        let mut draft = Draft::new(1, me());
        draft.to = vec![addr(None, "ann@example.com")];
        draft.subject = "Lunch".into();
        // What the engine hands back: one header, a blank line, the parts.
        let entity = "Content-Type: multipart/signed; micalg=pgp-sha256; \
                      protocol=\"application/pgp-signature\";\r\n \
                      boundary=\"=-=-pgp01\"\r\n\r\n\
                      --=-=-pgp01\r\nContent-Type: text/plain\r\n\r\nMeet at six.\r\n\
                      --=-=-pgp01\r\nContent-Type: application/pgp-signature\r\n\r\n\
                      -----BEGIN PGP SIGNATURE-----\r\n-----END PGP SIGNATURE-----\r\n\
                      --=-=-pgp01--\r\n";

        let raw =
            build_protected(&draft, 1_757_000_000, "<id@example.com>", entity.into()).unwrap();
        let raw = String::from_utf8(raw).unwrap();

        assert!(raw.contains("Subject: Lunch\r\n"), "{raw}");
        assert!(raw.contains("To: <ann@example.com>\r\n"), "{raw}");
        assert!(raw.ends_with(entity), "{raw}");
        // The one Content-Type on the message is the engine's own.
        assert_eq!(raw.matches("Content-Type: multipart/signed").count(), 1);
    }

    /// A draft that could go: one recipient, a body promising a file.
    fn ready() -> Draft {
        let mut draft = Draft::new(1, me());
        draft.to = vec![addr(None, "ann@example.com")];
        draft.subject = "Menu".into();
        draft.markdown = "Please find the attached file.".into();
        draft
    }

    const NOW: EpochMillis = 1_757_000_000_000;

    fn asking(secret: bool, attachments: bool) -> Asking {
        Asking {
            secret,
            attachments,
        }
    }

    #[test]
    fn a_message_that_cannot_go_is_refused_before_anything_is_asked() {
        let mut draft = ready();
        draft.to.clear();
        assert_eq!(
            gate(&draft, SendWhen::Now, NOW, asking(true, true)),
            Gate::Refuse(gettext("Add at least one recipient."))
        );
        // A time already gone counts the same, and only for Send Later.
        let past = SendWhen::At(NOW - 1);
        assert_eq!(
            gate(&ready(), past, NOW, asking(true, true)),
            Gate::Refuse(gettext("Choose a time in the future"))
        );
        assert_ne!(
            gate(
                &ready(),
                SendWhen::At(NOW + 60_000),
                NOW,
                asking(false, false)
            ),
            gate(&ready(), past, NOW, asking(false, false))
        );
    }

    #[test]
    fn going_out_readable_is_asked_about_before_the_missing_file() {
        let draft = ready();
        assert_eq!(
            gate(&draft, SendWhen::Now, NOW, asking(true, true)),
            Gate::ConfirmReadable
        );
        // Send Readable clears the wish, and the file is next.
        let Gate::ConfirmNoFile(promise) = gate(&draft, SendWhen::Now, NOW, asking(false, true))
        else {
            panic!("the promise should be asked about");
        };
        assert_eq!(promise.sentence, "Please find the attached file.");
        // Send Anyway answers that, and nothing is left to ask.
        assert_eq!(
            gate(&draft, SendWhen::Now, NOW, asking(false, false)),
            Gate::Send
        );
    }

    #[test]
    fn a_message_going_out_encrypted_is_not_asked_about() {
        let mut draft = ready();
        draft.encrypt = true;
        draft.attachments.push(OutgoingAttachment {
            filename: "menu.pdf".into(),
            mime_type: "application/pdf".into(),
            data: b"%PDF".to_vec(),
            content_id: None,
        });
        assert_eq!(
            gate(&draft, SendWhen::Now, NOW, asking(true, true)),
            Gate::Send
        );
    }

    #[test]
    fn a_pasted_picture_keeps_a_promise_of_something_to_look_at() {
        let mut draft = ready();
        draft.markdown = "See attached, the sea was warm.".into();
        draft.attachments.push(OutgoingAttachment {
            filename: "photo.png".into(),
            mime_type: "image/png".into(),
            data: vec![0x89],
            content_id: Some("photo@mailrs".into()),
        });
        assert_eq!(
            gate(&draft, SendWhen::Now, NOW, asking(false, true)),
            Gate::Send
        );
        // A picture is not the file a message says it attaches.
        draft.markdown = "Please find the attached file.".into();
        assert!(matches!(
            gate(&draft, SendWhen::Now, NOW, asking(false, true)),
            Gate::ConfirmNoFile(_)
        ));
    }

    #[test]
    fn a_plain_message_is_built_whole_and_a_protected_one_up_to_its_body() {
        let mut draft = Draft::new(1, me());
        draft.to = vec![addr(None, "ann@example.com")];
        draft.subject = "Lunch".into();
        draft.markdown = "Meet at six.".into();
        let Ok(Built::Message(raw)) = build(&draft, 1_757_000_000, "id@example.com") else {
            panic!("a plain message goes out whole");
        };
        let raw = String::from_utf8(raw).unwrap();
        assert!(raw.contains("Subject: Lunch\r\n"), "{raw}");
        assert!(raw.contains("Meet at six."), "{raw}");

        draft.sign = true;
        let Ok(Built::Body(part)) = build(&draft, 1_757_000_000, "id@example.com") else {
            panic!("a signed message leaves its body for the engine");
        };
        let part = String::from_utf8(part).unwrap();
        assert!(!part.contains("Subject:"), "{part}");
        assert!(part.contains("Meet at six."), "{part}");
    }
}
