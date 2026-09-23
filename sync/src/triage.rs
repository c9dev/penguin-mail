use std::str::FromStr;

use mailrs_domain::translate::{fill, gettext};

/// A label change the user asked for on a whole thread. Ordered, so a bulk
/// action can group the targets that want the same change.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum TriageAction {
    Archive,
    MarkRead,
    MarkUnread,
    Star,
    Unstar,
    AddLabel(String),
    RemoveLabel(String),
    Trash,
    /// Takes a message back out of the trash.
    Untrash,
    /// Moves out of the inbox and into Spam.
    Junk,
    /// Moves out of Spam and back to the inbox.
    NotJunk,
    /// Mutes the thread: Gmail's mute label goes on and the inbox label
    /// comes off, so this reply and the ones after it stay archived.
    Mute,
    /// Takes the mute label off and puts the thread back in the inbox.
    Unmute,
    /// Any label change, such as putting a conversation back in the inbox
    /// when its reminder comes due.
    Relabel {
        add: Vec<String>,
        remove: Vec<String>,
    },
}

impl TriageAction {
    /// The action in words, for a toast the person reads.
    pub fn describe(&self) -> String {
        match self {
            TriageAction::Archive => gettext("Archive"),
            TriageAction::MarkRead => gettext("Mark read"),
            TriageAction::MarkUnread => gettext("Mark unread"),
            TriageAction::Star => gettext("Star"),
            TriageAction::Unstar => gettext("Unstar"),
            TriageAction::AddLabel(label) => {
                fill(&gettext("Add label {label}"), &[("label", label)])
            }
            TriageAction::RemoveLabel(label) => {
                fill(&gettext("Remove label {label}"), &[("label", label)])
            }
            TriageAction::Trash => gettext("Move to trash"),
            TriageAction::Untrash => gettext("Move out of trash"),
            TriageAction::Junk => gettext("Mark as junk"),
            TriageAction::NotJunk => gettext("Mark as not junk"),
            TriageAction::Mute => gettext("Mute"),
            TriageAction::Unmute => gettext("Unmute"),
            TriageAction::Relabel { .. } => gettext("Change labels"),
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
            "untrash" => Ok(TriageAction::Untrash),
            "junk" => Ok(TriageAction::Junk),
            "notjunk" => Ok(TriageAction::NotJunk),
            "mute" => Ok(TriageAction::Mute),
            "unmute" => Ok(TriageAction::Unmute),
            _ => {
                if let Some(rest) = s.strip_prefix("label:") {
                    label(rest).map(TriageAction::AddLabel)
                } else if let Some(rest) = s.strip_prefix("unlabel:") {
                    label(rest).map(TriageAction::RemoveLabel)
                } else {
                    Err(format!(
                        "unknown action `{s}`; use archive, read, unread, star, unstar, trash, mute, unmute, label:ID, or unlabel:ID"
                    ))
                }
            }
        }
    }
}
