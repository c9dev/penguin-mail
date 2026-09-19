//! The GTK interface. Widgets are built in code; only the thread row is a
//! GObject subclass, because list rows need a widget type to recycle.

pub mod composer;
pub mod conversation;
pub mod sidebar;
pub mod thread_list;
pub mod thread_row;
pub mod welcome;
pub mod window;

use mailrs_domain::{AccountId, MessageMeta, ThreadSummary};
use mailrs_store::threads::ThreadFilter;

/// What the thread list shows.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Mailbox {
    /// One system label across every account.
    Unified(&'static str),
    Label {
        account_id: AccountId,
        label_id: String,
        name: String,
    },
    Search {
        query: String,
        account_id: Option<AccountId>,
    },
}

impl Mailbox {
    pub fn title(&self) -> String {
        match self {
            Mailbox::Unified(label) => unified_name(label).into(),
            Mailbox::Label { name, .. } => name.clone(),
            Mailbox::Search { .. } => "Search".into(),
        }
    }

    pub fn filter(&self) -> Option<ThreadFilter> {
        match self {
            Mailbox::Unified(label) => Some(ThreadFilter::unified(*label)),
            Mailbox::Label {
                account_id,
                label_id,
                ..
            } => Some(ThreadFilter::account(*account_id, label_id.clone())),
            Mailbox::Search { .. } => None,
        }
    }

    pub fn account(&self) -> Option<AccountId> {
        match self {
            Mailbox::Unified(_) => None,
            Mailbox::Label { account_id, .. } => Some(*account_id),
            Mailbox::Search { account_id, .. } => *account_id,
        }
    }

    /// Unread counts matter for inboxes; drafts show how many there are.
    pub fn counts_unread(&self) -> bool {
        matches!(self, Mailbox::Unified("INBOX"))
            || matches!(self, Mailbox::Label { label_id, .. } if label_id == "INBOX")
    }
}

pub const UNIFIED: [&str; 4] = ["INBOX", "STARRED", "SENT", "DRAFT"];

pub fn unified_name(label: &str) -> &'static str {
    match label {
        "INBOX" => "All Inboxes",
        "STARRED" => "Starred",
        "SENT" => "Sent",
        "DRAFT" => "Drafts",
        _ => "Mail",
    }
}

pub fn account_label_name(label: &str) -> &'static str {
    match label {
        "INBOX" => "Inbox",
        "STARRED" => "Starred",
        "SENT" => "Sent",
        "DRAFT" => "Drafts",
        _ => "Mail",
    }
}

pub fn mailbox_icon(label: &str) -> &'static str {
    match label {
        "INBOX" => "mailrs-inbox-symbolic",
        "STARRED" => "starred-symbolic",
        "SENT" => "mail-send-symbolic",
        "DRAFT" => "document-edit-symbolic",
        _ => "mailrs-tag-symbolic",
    }
}

/// Groups search hits, newest first, into one row per thread.
pub fn summarize_search(mut hits: Vec<MessageMeta>) -> Vec<ThreadSummary> {
    hits.sort_by(|a, b| b.date.cmp(&a.date));
    let mut rows: Vec<(ThreadSummary, i64)> = Vec::new();
    for hit in hits {
        if let Some((row, oldest)) = rows
            .iter_mut()
            .find(|(r, _)| r.account_id == hit.account_id && r.id == hit.thread_id)
        {
            row.message_count += 1;
            row.unread |= hit.is_unread();
            row.starred |= hit.has_label("STARRED");
            row.has_attachments |= hit.has_attachments;
            if hit.date <= *oldest {
                *oldest = hit.date;
                row.subject = hit.subject.clone();
            }
            continue;
        }
        let summary = ThreadSummary {
            account_id: hit.account_id,
            id: hit.thread_id.clone(),
            last_message_at: hit.date,
            subject: hit.subject.clone(),
            snippet: hit.snippet.clone(),
            from: hit
                .from
                .as_ref()
                .map(|a| a.display().to_string())
                .unwrap_or_default(),
            message_count: 1,
            unread: hit.is_unread(),
            starred: hit.has_label("STARRED"),
            has_attachments: hit.has_attachments,
        };
        rows.push((summary, hit.date));
    }
    rows.into_iter().map(|(row, _)| row).collect()
}

#[cfg(test)]
mod tests {
    use mailrs_domain::{Address, MessageMeta};

    use super::summarize_search;

    fn hit(id: &str, thread: &str, date: i64, subject: &str, labels: &[&str]) -> MessageMeta {
        MessageMeta {
            account_id: 1,
            id: id.into(),
            thread_id: thread.into(),
            rfc822_msgid: None,
            from: Some(Address {
                name: Some(format!("Sender {id}")),
                email: format!("{id}@example.com"),
            }),
            to: vec![],
            cc: vec![],
            subject: subject.into(),
            date,
            snippet: format!("snippet {id}"),
            size: 0,
            has_attachments: false,
            label_ids: labels.iter().map(|l| l.to_string()).collect(),
        }
    }

    #[test]
    fn hits_group_by_thread_newest_first() {
        let rows = summarize_search(vec![
            hit("a1", "ta", 100, "Plans", &[]),
            hit("b1", "tb", 300, "Other", &["STARRED"]),
            hit("a2", "ta", 200, "Re: Plans", &["UNREAD"]),
        ]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, "tb");
        assert!(rows[0].starred);
        let plans = &rows[1];
        assert_eq!(
            (
                plans.message_count,
                plans.subject.as_str(),
                plans.from.as_str()
            ),
            (2, "Plans", "Sender a2")
        );
        assert!(plans.unread);
        assert_eq!(plans.last_message_at, 200);
    }
}
