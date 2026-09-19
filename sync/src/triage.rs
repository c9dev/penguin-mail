use std::str::FromStr;

/// A label change the user asked for on a whole thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TriageAction {
    Archive,
    MarkRead,
    MarkUnread,
    Star,
    Unstar,
    AddLabel(String),
    RemoveLabel(String),
    Trash,
}

impl TriageAction {
    /// Labels to add and to remove on each message.
    pub fn label_delta(&self) -> (Vec<String>, Vec<String>) {
        let one = |label: &str| vec![label.to_string()];
        match self {
            TriageAction::Archive => (vec![], one("INBOX")),
            TriageAction::MarkRead => (vec![], one("UNREAD")),
            TriageAction::MarkUnread => (one("UNREAD"), vec![]),
            TriageAction::Star => (one("STARRED"), vec![]),
            TriageAction::Unstar => (vec![], one("STARRED")),
            TriageAction::AddLabel(label) => (vec![label.clone()], vec![]),
            TriageAction::RemoveLabel(label) => (vec![], vec![label.clone()]),
            TriageAction::Trash => (one("TRASH"), one("INBOX")),
        }
    }

    pub fn describe(&self) -> String {
        match self {
            TriageAction::Archive => "Archive".into(),
            TriageAction::MarkRead => "Mark read".into(),
            TriageAction::MarkUnread => "Mark unread".into(),
            TriageAction::Star => "Star".into(),
            TriageAction::Unstar => "Unstar".into(),
            TriageAction::AddLabel(label) => format!("Add label {label}"),
            TriageAction::RemoveLabel(label) => format!("Remove label {label}"),
            TriageAction::Trash => "Move to trash".into(),
        }
    }
}

impl FromStr for TriageAction {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let label = |rest: &str| {
            if rest.is_empty() {
                Err(format!("`{s}` needs a label id after the colon"))
            } else {
                Ok(rest.to_string())
            }
        };
        match s {
            "archive" => Ok(TriageAction::Archive),
            "read" => Ok(TriageAction::MarkRead),
            "unread" => Ok(TriageAction::MarkUnread),
            "star" => Ok(TriageAction::Star),
            "unstar" => Ok(TriageAction::Unstar),
            "trash" => Ok(TriageAction::Trash),
            _ => {
                if let Some(rest) = s.strip_prefix("label:") {
                    label(rest).map(TriageAction::AddLabel)
                } else if let Some(rest) = s.strip_prefix("unlabel:") {
                    label(rest).map(TriageAction::RemoveLabel)
                } else {
                    Err(format!(
                        "unknown action `{s}`; use archive, read, unread, star, unstar, trash, label:ID, or unlabel:ID"
                    ))
                }
            }
        }
    }
}
