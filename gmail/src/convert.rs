//! Conversions from Gmail wire types to the types the rest of Penguin Mail uses.

use mailrs_domain::{AccountId, MessageMeta};

use crate::address::parse_address_list;
use crate::model::{HistoryList, Message, MessagePart};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryChange {
    MessageAdded {
        id: String,
        thread_id: String,
    },
    MessageDeleted {
        id: String,
        thread_id: String,
    },
    LabelsAdded {
        id: String,
        thread_id: String,
        label_ids: Vec<String>,
    },
    LabelsRemoved {
        id: String,
        thread_id: String,
        label_ids: Vec<String>,
    },
}

impl HistoryChange {
    pub fn message_id(&self) -> &str {
        match self {
            HistoryChange::MessageAdded { id, .. }
            | HistoryChange::MessageDeleted { id, .. }
            | HistoryChange::LabelsAdded { id, .. }
            | HistoryChange::LabelsRemoved { id, .. } => id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryPage {
    pub changes: Vec<HistoryChange>,
    pub next_page_token: Option<String>,
    /// The mailbox's current history id, which becomes the next cursor.
    pub history_id: u64,
}

/// Flattens history records into changes, keeping Gmail's order.
pub fn history_page(list: HistoryList) -> HistoryPage {
    let mut changes = Vec::new();
    for record in list.history {
        for m in record.messages_added {
            changes.push(HistoryChange::MessageAdded {
                id: m.message.id,
                thread_id: m.message.thread_id,
            });
        }
        for m in record.messages_deleted {
            changes.push(HistoryChange::MessageDeleted {
                id: m.message.id,
                thread_id: m.message.thread_id,
            });
        }
        for m in record.labels_added {
            changes.push(HistoryChange::LabelsAdded {
                id: m.message.id,
                thread_id: m.message.thread_id,
                label_ids: m.label_ids,
            });
        }
        for m in record.labels_removed {
            changes.push(HistoryChange::LabelsRemoved {
                id: m.message.id,
                thread_id: m.message.thread_id,
                label_ids: m.label_ids,
            });
        }
    }
    HistoryPage {
        changes,
        next_page_token: list.next_page_token,
        history_id: list.history_id,
    }
}

/// Converts a `format=metadata` or `format=full` message.
pub fn message_meta(msg: &Message, account_id: AccountId) -> MessageMeta {
    MessageMeta {
        account_id,
        id: msg.id.clone(),
        thread_id: msg.thread_id.clone(),
        rfc822_msgid: header(msg, "Message-ID").map(str::to_string),
        from: header(msg, "From").and_then(|v| parse_address_list(v).into_iter().next()),
        to: header(msg, "To")
            .map(parse_address_list)
            .unwrap_or_default(),
        cc: header(msg, "Cc")
            .map(parse_address_list)
            .unwrap_or_default(),
        subject: header(msg, "Subject").unwrap_or_default().to_string(),
        date: msg.internal_date.unwrap_or(0),
        snippet: unescape_snippet(&msg.snippet),
        size: msg.size_estimate,
        has_attachments: msg
            .payload
            .as_ref()
            .is_some_and(|p| p.mime_type.eq_ignore_ascii_case("multipart/mixed")),
        label_ids: msg.label_ids.clone(),
    }
}

fn header<'a>(msg: &'a Message, name: &str) -> Option<&'a str> {
    msg.payload.as_ref().and_then(|p| find_header(p, name))
}

pub(crate) fn find_header<'a>(part: &'a MessagePart, name: &str) -> Option<&'a str> {
    part.headers
        .iter()
        .find(|h| h.name.eq_ignore_ascii_case(name))
        .map(|h| h.value.as_str())
}

/// Gmail snippets arrive HTML-escaped. Decodes named and numeric entities and
/// leaves anything unrecognised as written.
pub fn unescape_snippet(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let tail = &rest[amp..];
        let decoded = tail
            .find(';')
            .filter(|&end| end <= 10)
            .and_then(|end| decode_entity(&tail[1..end]).map(|c| (c, end)));
        match decoded {
            Some((c, end)) => {
                out.push(c);
                rest = &tail[end + 1..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn decode_entity(entity: &str) -> Option<char> {
    match entity {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "nbsp" => Some('\u{a0}'),
        _ => {
            let number = entity.strip_prefix('#')?;
            let code = match number.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => number.parse().ok()?,
            };
            char::from_u32(code)
        }
    }
}

/// Plain text from the small HTML Gmail stores for signatures and automatic
/// replies: block tags and `<br>` become line breaks, other tags go.
pub fn html_to_text(html: &str) -> String {
    let mut out = String::new();
    let mut rest = html;
    while let Some(open) = rest.find('<') {
        out.push_str(&decode_entities(&rest[..open]));
        let Some(close) = rest[open..].find('>') else {
            rest = "";
            break;
        };
        let tag = rest[open + 1..open + close].trim().to_ascii_lowercase();
        rest = &rest[open + close + 1..];
        let closing = tag.starts_with('/');
        let name: String = tag
            .trim_start_matches('/')
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect();
        match name.as_str() {
            "br" => out.push('\n'),
            "p" if closing => end_line(&mut out, 2),
            "div" | "p" | "li" | "tr" | "ul" | "ol" | "table" | "blockquote" | "h1" | "h2"
            | "h3" | "h4" | "h5" | "h6" => end_line(&mut out, 1),
            _ => {}
        }
    }
    out.push_str(&decode_entities(rest));
    let lines: Vec<&str> = out.lines().map(str::trim_end).collect();
    let mut text = lines.join("\n");
    while text.contains("\n\n\n") {
        text = text.replace("\n\n\n", "\n\n");
    }
    text.trim().to_string()
}

/// Ends the current line so `out` finishes with `breaks` line breaks.
fn end_line(out: &mut String, breaks: usize) {
    if out.is_empty() {
        return;
    }
    let have = out.len() - out.trim_end_matches('\n').len();
    for _ in have..breaks {
        out.push('\n');
    }
}

fn decode_entities(text: &str) -> String {
    text.replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&amp;", "&")
}

/// Escaped HTML with a `<br>` for each line break.
pub fn text_to_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\n', "<br>")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gmail_html_turns_into_lines() {
        assert_eq!(
            html_to_text(
                r#"<div dir="ltr">Ann Lee<div>Maple &amp; Finch</div><div><br></div></div>"#
            ),
            "Ann Lee\nMaple & Finch"
        );
        assert_eq!(html_to_text("<p>One</p><p>Two</p>"), "One\n\nTwo");
        assert_eq!(html_to_text("plain"), "plain");
    }

    #[test]
    fn text_becomes_escaped_html() {
        assert_eq!(text_to_html("a < b\nc"), "a &lt; b<br>c");
    }
}
