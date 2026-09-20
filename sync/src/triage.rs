use std::str::FromStr;

use mailrs_domain::system_label;
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
    /// Any label change; the undo of most other actions.
    Relabel {
        add: Vec<String>,
        remove: Vec<String>,
    },
}

impl TriageAction {
    /// Labels to add and to remove on each message.
    pub fn label_delta(&self) -> (Vec<String>, Vec<String>) {
        let one = |label: &str| vec![label.to_string()];
        match self {
            TriageAction::Archive => (vec![], one(system_label::INBOX)),
            TriageAction::MarkRead => (vec![], one(system_label::UNREAD)),
            TriageAction::MarkUnread => (one(system_label::UNREAD), vec![]),
            TriageAction::Star => (one(system_label::STARRED), vec![]),
            TriageAction::Unstar => (vec![], one(system_label::STARRED)),
            TriageAction::AddLabel(label) => (vec![label.clone()], vec![]),
            TriageAction::RemoveLabel(label) => (vec![], vec![label.clone()]),
            TriageAction::Trash => (one(system_label::TRASH), one(system_label::INBOX)),
            TriageAction::Untrash => (one(system_label::INBOX), one(system_label::TRASH)),
            TriageAction::Junk => (one(system_label::SPAM), one(system_label::INBOX)),
            TriageAction::NotJunk => (one(system_label::INBOX), one(system_label::SPAM)),
            TriageAction::Mute => (one(system_label::MUTE), one(system_label::INBOX)),
            TriageAction::Unmute => (one(system_label::INBOX), one(system_label::MUTE)),
            TriageAction::Relabel { add, remove } => (add.clone(), remove.clone()),
        }
    }

    /// The action that undoes this one.
    pub fn inverse(&self) -> TriageAction {
        let relabel = |add: &[&str], remove: &[&str]| TriageAction::Relabel {
            add: add.iter().map(|l| l.to_string()).collect(),
            remove: remove.iter().map(|l| l.to_string()).collect(),
        };
        match self {
            TriageAction::Archive => relabel(&[system_label::INBOX], &[]),
            TriageAction::MarkRead => TriageAction::MarkUnread,
            TriageAction::MarkUnread => TriageAction::MarkRead,
            TriageAction::Star => TriageAction::Unstar,
            TriageAction::Unstar => TriageAction::Star,
            TriageAction::AddLabel(label) => TriageAction::RemoveLabel(label.clone()),
            TriageAction::RemoveLabel(label) => TriageAction::AddLabel(label.clone()),
            TriageAction::Trash => TriageAction::Untrash,
            TriageAction::Untrash => TriageAction::Trash,
            TriageAction::Junk => TriageAction::NotJunk,
            TriageAction::NotJunk => TriageAction::Junk,
            TriageAction::Mute => TriageAction::Unmute,
            TriageAction::Unmute => TriageAction::Mute,
            TriageAction::Relabel { add, remove } => TriageAction::Relabel {
                add: remove.clone(),
                remove: add.clone(),
            },
        }
    }

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
