//! Gmail's label words over IMAP and the store's: X-GM-LABELS on one
//! side, the Gmail API's label ids on the other.

use mailrs_imap::GmailMeta;

/// The mailbox Gmail's IMAP shows a message in. A message lives in one of
/// the three; its labels say the rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::services::imap) enum GmailPlace {
    AllMail,
    Trash,
    Spam,
}

/// The inbox tabs Gmail sorts mail into, in the words of its search box.
/// A message in the Inbox under none of them is in the personal tab.
pub(in crate::services::imap) const TABS: [(&str, &str); 4] = [
    ("social", "CATEGORY_SOCIAL"),
    ("promotions", "CATEGORY_PROMOTIONS"),
    ("updates", "CATEGORY_UPDATES"),
    ("forums", "CATEGORY_FORUMS"),
];

/// The labels Gmail's own sidebar and the store both give a fixed id.
const SYSTEM: [&str; 8] = [
    "INBOX",
    "SENT",
    "DRAFT",
    "TRASH",
    "SPAM",
    "IMPORTANT",
    "STARRED",
    "UNREAD",
];

/// Whether Gmail made the label `id` itself.
pub(in crate::services::imap) fn is_system(id: &str) -> bool {
    SYSTEM.contains(&id) || mailrs_gmail::labels::is_category(id)
}

/// The Gmail API label ids that describe a message: its X-GM-LABELS and
/// flags read as the API would list them, in `at`, with the inbox tab
/// `tab` when Gmail has sorted it. In Trash or Spam the message carries
/// that mailbox's label and keeps its other labels, as Gmail keeps them,
/// but not the Inbox's.
pub(in crate::services::imap) fn store_labels(
    meta: &GmailMeta,
    flags: &[String],
    at: GmailPlace,
    tab: Option<&str>,
) -> Vec<String> {
    let mut labels: Vec<String> = Vec::new();
    let mut add = |label: &str| {
        if !labels.iter().any(|l| l == label) {
            labels.push(label.to_string());
        }
    };
    match at {
        GmailPlace::Trash => add("TRASH"),
        GmailPlace::Spam => add("SPAM"),
        GmailPlace::AllMail => {}
    }
    for label in &meta.labels {
        match label.as_str() {
            "\\Inbox" if at == GmailPlace::AllMail => add("INBOX"),
            "\\Sent" => add("SENT"),
            "\\Draft" => add("DRAFT"),
            "\\Important" => add("IMPORTANT"),
            "\\Starred" => add("STARRED"),
            // Another of Gmail's own marks that the store keeps no label
            // for, or the Inbox of a message in Trash or Spam.
            other if other.starts_with('\\') => {}
            name => add(name),
        }
    }
    let flagged = flags.iter().any(|f| f.eq_ignore_ascii_case("\\Flagged"));
    if flagged {
        add("STARRED");
    }
    if !flags.iter().any(|f| f.eq_ignore_ascii_case("\\Seen")) {
        add("UNREAD");
    }
    if let Some(tab) = tab {
        add(tab);
    }
    labels
}

/// The X-GM-LABELS word for the store's label `id`, for a write: `None`
/// for a label that is a flag (Starred, Unread), a move (Trash, Spam) or a
/// tab, which IMAP has no label for.
pub(in crate::services::imap) fn imap_label(id: &str) -> Option<String> {
    match id {
        "INBOX" => Some("\\Inbox".into()),
        "IMPORTANT" => Some("\\Important".into()),
        "MUTE" => Some("\\Muted".into()),
        "SENT" | "DRAFT" | "TRASH" | "SPAM" | "STARRED" | "UNREAD" => None,
        tab if mailrs_gmail::labels::is_category(tab) => None,
        name => Some(name.to_string()),
    }
}

/// The SEARCH key that finds the mail carrying the store's label `id` in
/// All Mail, without the sync window. `None` for a label whose mail is not
/// in All Mail (Trash, Spam) or that stands for no set of messages.
pub(in crate::services::imap) fn search_key(id: &str) -> Option<String> {
    let label = |word: &str| mailrs_imap::gm_labels(word).ok();
    match id {
        "INBOX" => label("\\Inbox"),
        "SENT" => label("\\Sent"),
        "DRAFT" => label("\\Draft"),
        "IMPORTANT" => label("\\Important"),
        "TRASH" | "SPAM" | "UNREAD" | "STARRED" => None,
        "CATEGORY_PERSONAL" => {
            let others: Vec<String> = TABS
                .iter()
                .map(|(tab, _)| format!("-category:{tab}"))
                .collect();
            mailrs_imap::gm_raw(&format!("in:inbox {}", others.join(" "))).ok()
        }
        tab if mailrs_gmail::labels::is_category(tab) => TABS
            .iter()
            .find(|(_, id)| *id == tab)
            .and_then(|(name, _)| mailrs_imap::gm_raw(&format!("category:{name} in:inbox")).ok()),
        name => label(name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(labels: &[&str]) -> GmailMeta {
        GmailMeta {
            msgid: 1,
            thrid: 1,
            labels: labels.iter().map(|l| l.to_string()).collect(),
        }
    }

    fn flags(list: &[&str]) -> Vec<String> {
        list.iter().map(|f| f.to_string()).collect()
    }

    #[test]
    fn labels_become_the_api_ids_and_unseen_becomes_unread() {
        let got = store_labels(
            &meta(&["\\Inbox", "\\Important", "Work/Clients", "\\Muted"]),
            &flags(&["\\Flagged"]),
            GmailPlace::AllMail,
            Some("CATEGORY_UPDATES"),
        );
        assert_eq!(
            got,
            [
                "INBOX",
                "IMPORTANT",
                "Work/Clients",
                "STARRED",
                "UNREAD",
                "CATEGORY_UPDATES"
            ]
        );
    }

    #[test]
    fn a_message_in_trash_keeps_its_labels_but_not_the_inbox() {
        let got = store_labels(
            &meta(&["\\Inbox", "Work"]),
            &flags(&["\\Seen"]),
            GmailPlace::Trash,
            None,
        );
        assert_eq!(got, ["TRASH", "Work"]);
    }

    #[test]
    fn a_write_label_exists_for_inbox_and_user_labels_only() {
        assert_eq!(imap_label("INBOX").as_deref(), Some("\\Inbox"));
        assert_eq!(
            imap_label("Família/Férias 2026").as_deref(),
            Some("Família/Férias 2026")
        );
        assert_eq!(imap_label("STARRED"), None);
        assert_eq!(imap_label("CATEGORY_SOCIAL"), None);
    }

    #[test]
    fn the_personal_tab_is_the_inbox_outside_the_other_four() {
        let key = search_key("CATEGORY_PERSONAL").unwrap();
        assert!(
            key.starts_with("X-GM-RAW \"in:inbox -category:social"),
            "{key}"
        );
    }
}
