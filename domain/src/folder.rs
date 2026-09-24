//! Gmail's archived mail, Spam, Trash, and All Mail, which the local window
//! does not hold.
//! The app lists them with a Gmail search instead.

use crate::{MessageMeta, Role};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Folder {
    /// Mail someone received and took out of the inbox. Gmail has no
    /// label for it; the search leaves out the inbox, what the account
    /// sent, drafts, spam and trash.
    Archive,
    Junk,
    Trash,
    AllMail,
}

impl Folder {
    /// In the order the sidebar lists them.
    pub const ALL: [Folder; 4] = [
        Folder::Archive,
        Folder::Junk,
        Folder::Trash,
        Folder::AllMail,
    ];

    /// The Gmail search that lists this folder.
    pub fn query(self) -> &'static str {
        match self {
            Folder::Archive => "-in:inbox -in:sent -in:drafts -in:spam -in:trash",
            Folder::Junk => "in:spam",
            Folder::Trash => "in:trash",
            Folder::AllMail => "-in:spam -in:trash",
        }
    }

    /// Whether `message` still belongs in this folder. Archive leaves out
    /// what the account sent and its drafts, as the Gmail search does.
    pub fn holds(self, message: &MessageMeta) -> bool {
        let placed = [Role::Inbox, Role::Sent, Role::Drafts, Role::Junk, Role::Trash];
        match self {
            Folder::Archive => !placed.iter().any(|r| message.in_role(*r)),
            Folder::Junk => message.in_role(Role::Junk),
            Folder::Trash => message.in_role(Role::Trash),
            Folder::AllMail => !message.in_role(Role::Junk) && !message.in_role(Role::Trash),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Folder;
    use crate::MessageMeta;

    /// A message carrying Gmail's `labels`, which the store still keeps.
    fn message(labels: &[&str]) -> MessageMeta {
        crate::tests::message("m1", labels)
    }

    #[test]
    fn junk_and_trash_hold_their_own_mailbox() {
        assert!(Folder::Junk.holds(&message(&["SPAM"])));
        assert!(!Folder::Junk.holds(&message(&["INBOX"])));
        assert!(Folder::Trash.holds(&message(&["TRASH"])));
        assert!(!Folder::Trash.holds(&message(&["SPAM"])));
    }

    #[test]
    fn all_mail_holds_everything_outside_junk_and_trash() {
        assert!(Folder::AllMail.holds(&message(&[])));
        assert!(Folder::AllMail.holds(&message(&["INBOX", "Label_1"])));
        assert!(!Folder::AllMail.holds(&message(&["SPAM"])));
        assert!(!Folder::AllMail.holds(&message(&["TRASH"])));
    }

    #[test]
    fn archive_holds_received_mail_outside_the_inbox_sent_and_drafts() {
        assert!(Folder::Archive.holds(&message(&["Label_1"])));
        assert!(Folder::Archive.holds(&message(&[])));
        for place in ["INBOX", "SENT", "DRAFT", "SPAM", "TRASH"] {
            assert!(!Folder::Archive.holds(&message(&[place])), "{place}");
        }
    }
}
