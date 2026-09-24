//! Conversions from Gmail wire types to the types the rest of Penguin Mail uses.

use mailrs_domain::{AccountId, MessageMeta};
use mailrs_mime::address::parse_address_list;
use mailrs_mime::snippet::unescape_snippet;

use crate::labels::set_label_ids;
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
    let mut meta = MessageMeta {
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
        held: Default::default(),
        roles: vec![],
        list_unsubscribe: header(msg, "List-Unsubscribe").map(|v| v.trim().to_string()),
        one_click: header(msg, "List-Unsubscribe-Post").is_some_and(|v| v.contains("One-Click")),
    };
    set_label_ids(&mut meta, &msg.label_ids);
    meta
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
    fn text_becomes_escaped_html() {
        assert_eq!(text_to_html("a < b\nc"), "a &lt; b<br>c");
    }
}
