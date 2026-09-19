//! The GTK interface. Widgets are built in code; only the thread row is a
//! GObject subclass, because list rows need a widget type to recycle.

pub mod autocomplete;
pub mod composer;
pub mod conversation;
pub mod moving;
pub mod preferences;
pub mod rules;
pub mod search_suggest;
pub mod sidebar;
pub mod smart_editor;
pub mod thread_list;
pub mod thread_row;
pub mod vacation;
pub mod welcome;
pub mod when;
pub mod window;

use mailrs_domain::{AccountId, FlagColor, MessageMeta, ThreadSummary};
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
    /// Messages waiting to go out at a set time, across all accounts.
    Scheduled,
    /// Conversations set aside with Remind Me, across all accounts.
    Reminders,
    /// Sent mail nobody has answered for a few days, across all accounts.
    FollowUp,
    /// Flagged mail of one colour, across all accounts.
    Flag(FlagColor),
    /// Mail from VIPs: all of them under "VIPs", or one person.
    Vips { emails: Vec<String>, name: String },
    /// A saved search from Preferences' smart mailboxes, by id.
    Smart { id: String, name: String },
    /// Mail Gmail keeps out of the regular listing, fetched on demand.
    Folder {
        account_id: Option<AccountId>,
        folder: Folder,
    },
}

/// Gmail's Spam, Trash, and All Mail, which the local window does not hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Folder {
    Junk,
    Trash,
    AllMail,
}

impl Folder {
    pub const ALL: [Folder; 3] = [Folder::Junk, Folder::Trash, Folder::AllMail];

    /// The Gmail search that lists this folder.
    pub fn query(self) -> &'static str {
        match self {
            Folder::Junk => "in:spam",
            Folder::Trash => "in:trash",
            Folder::AllMail => "-in:spam -in:trash",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Folder::Junk => "Junk",
            Folder::Trash => "Trash",
            Folder::AllMail => "All Mail",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Folder::Junk => "mail-mark-junk-symbolic",
            Folder::Trash => "user-trash-symbolic",
            Folder::AllMail => "mailrs-archive-symbolic",
        }
    }

    /// Whether a message with `labels` still belongs in this folder.
    pub fn holds(self, labels: &[String]) -> bool {
        let has = |l: &str| labels.iter().any(|x| x == l);
        match self {
            Folder::Junk => has("SPAM"),
            Folder::Trash => has("TRASH"),
            Folder::AllMail => !has("SPAM") && !has("TRASH"),
        }
    }
}

impl Mailbox {
    pub fn title(&self) -> String {
        match self {
            Mailbox::Unified(label) => unified_name(label).into(),
            Mailbox::Label { name, .. } => name.clone(),
            Mailbox::Search { .. } => "Search".into(),
            Mailbox::Folder { folder, .. } => folder.name().into(),
            Mailbox::Scheduled => "Send Later".into(),
            Mailbox::Reminders => "Remind Me".into(),
            Mailbox::FollowUp => "Follow Up".into(),
            Mailbox::Flag(color) => format!("{} Flag", color.name()),
            Mailbox::Vips { name, .. } | Mailbox::Smart { name, .. } => name.clone(),
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
            Mailbox::Flag(color) => Some(ThreadFilter::unified("").with_flag(*color)),
            Mailbox::Vips { emails, .. } => {
                Some(ThreadFilter::unified("").from_senders(emails.clone()))
            }
            Mailbox::Search { .. }
            | Mailbox::Folder { .. }
            | Mailbox::Scheduled
            | Mailbox::Reminders
            | Mailbox::FollowUp
            | Mailbox::Smart { .. } => None,
        }
    }

    pub fn account(&self) -> Option<AccountId> {
        match self {
            Mailbox::Unified(_)
            | Mailbox::Scheduled
            | Mailbox::Reminders
            | Mailbox::FollowUp
            | Mailbox::Flag(_)
            | Mailbox::Vips { .. }
            | Mailbox::Smart { .. } => None,
            Mailbox::Label { account_id, .. } => Some(*account_id),
            Mailbox::Search { account_id, .. } | Mailbox::Folder { account_id, .. } => *account_id,
        }
    }

    pub fn folder(&self) -> Option<Folder> {
        match self {
            Mailbox::Folder { folder, .. } => Some(*folder),
            _ => None,
        }
    }

    /// Unread counts matter for inboxes; drafts show how many there are.
    pub fn counts_unread(&self) -> bool {
        matches!(self, Mailbox::Unified("INBOX") | Mailbox::Vips { .. })
            || matches!(self, Mailbox::Label { label_id, .. } if label_id == "INBOX")
    }
}

/// Label colours from Gmail's palette: name, background, and text.
pub const LABEL_COLORS: [(&str, &str, &str); 9] = [
    ("Red", "#fb4c2f", "#ffffff"),
    ("Orange", "#ffad47", "#ffffff"),
    ("Yellow", "#fad165", "#000000"),
    ("Green", "#16a766", "#ffffff"),
    ("Teal", "#2da2bb", "#ffffff"),
    ("Blue", "#4a86e8", "#ffffff"),
    ("Purple", "#a479e2", "#ffffff"),
    ("Pink", "#f691b3", "#ffffff"),
    ("Gray", "#999999", "#ffffff"),
];

pub const UNIFIED: [&str; 4] = ["INBOX", "STARRED", "SENT", "DRAFT"];

pub fn unified_name(label: &str) -> &'static str {
    match label {
        "INBOX" => "All Inboxes",
        "STARRED" => "Flagged",
        "SENT" => "Sent",
        "DRAFT" => "Drafts",
        _ => "Mail",
    }
}

pub fn account_label_name(label: &str) -> &'static str {
    match label {
        "INBOX" => "Inbox",
        "STARRED" => "Flagged",
        "SENT" => "Sent",
        "DRAFT" => "Drafts",
        _ => "Mail",
    }
}

pub fn mailbox_icon(label: &str) -> &'static str {
    match label {
        "INBOX" => "mailrs-inbox-symbolic",
        "STARRED" => "mailrs-flag-symbolic",
        "SENT" => "mail-send-symbolic",
        "DRAFT" => "document-edit-symbolic",
        _ => "mailrs-tag-symbolic",
    }
}

/// Turns search hits into list rows, newest first: one per thread when
/// `grouped`, one per message otherwise.
pub fn summarize_search(mut hits: Vec<MessageMeta>, grouped: bool) -> Vec<ThreadSummary> {
    hits.sort_by_key(|m| std::cmp::Reverse(m.date));
    if !grouped {
        return hits
            .into_iter()
            .map(|hit| ThreadSummary {
                account_id: hit.account_id,
                id: hit.thread_id.clone(),
                message_id: Some(hit.id.clone()),
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
                flag_color: None,
                from_email: hit
                    .from
                    .as_ref()
                    .map(|a| a.email.clone())
                    .unwrap_or_default(),
            })
            .collect();
    }
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
            message_id: None,
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
            flag_color: None,
            from_email: hit
                .from
                .as_ref()
                .map(|a| a.email.clone())
                .unwrap_or_default(),
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
        let rows = summarize_search(
            vec![
                hit("a1", "ta", 100, "Plans", &[]),
                hit("b1", "tb", 300, "Other", &["STARRED"]),
                hit("a2", "ta", 200, "Re: Plans", &["UNREAD"]),
            ],
            true,
        );
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

    #[test]
    fn ungrouped_hits_stay_separate() {
        let rows = summarize_search(
            vec![
                hit("a1", "ta", 100, "Plans", &[]),
                hit("a2", "ta", 200, "Re: Plans", &[]),
            ],
            false,
        );
        let ids: Vec<Option<&str>> = rows.iter().map(|r| r.message_id.as_deref()).collect();
        assert_eq!(ids, [Some("a2"), Some("a1")]);
        assert_eq!(rows[0].subject, "Re: Plans");
    }
}
