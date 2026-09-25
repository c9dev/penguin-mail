use std::str::FromStr;

use mailrs_domain::MailSet;
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
    /// Moves the messages into the server mailbox with this id, as Move to
    /// Folder does on a folder account.
    MoveTo(String),
    /// Any change of mail sets, such as putting a conversation back in the
    /// inbox, unread, when its reminder comes due.
    Relabel {
        add: Vec<MailSet>,
        remove: Vec<MailSet>,
    },
}

impl TriageAction {
    /// The action in words, for a toast the person reads. A mailbox shows
    /// by its server id; [`TriageAction::describe_named`] shows its name.
    pub fn describe(&self) -> String {
        self.describe_named(|id| id.to_string())
    }

    /// The action in words, each server mailbox it names shown as `name`
    /// gives it: `name` turns an id such as `Label_5` into the name the
    /// person gave the label or folder.
    pub fn describe_named(&self, name: impl Fn(&str) -> String) -> String {
        match self {
            TriageAction::Archive => gettext("Archive"),
            TriageAction::MarkRead => gettext("Mark read"),
            TriageAction::MarkUnread => gettext("Mark unread"),
            TriageAction::Star => gettext("Star"),
            TriageAction::Unstar => gettext("Unstar"),
            TriageAction::AddLabel(label) => {
                fill(&gettext("Add label {label}"), &[("label", &name(label))])
            }
            TriageAction::RemoveLabel(label) => {
                fill(&gettext("Remove label {label}"), &[("label", &name(label))])
            }
            TriageAction::Trash => gettext("Move to trash"),
            TriageAction::Untrash => gettext("Move out of trash"),
            TriageAction::Junk => gettext("Mark as junk"),
            TriageAction::NotJunk => gettext("Mark as not junk"),
            TriageAction::Mute => gettext("Mute"),
            TriageAction::Unmute => gettext("Unmute"),
            TriageAction::MoveTo(folder) => {
                fill(&gettext("Move to {folder}"), &[("folder", &name(folder))])
            }
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
