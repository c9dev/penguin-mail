//! Gmail's archived mail, Spam, Trash, and All Mail, which the local window
//! does not hold.
//! The app lists them with a Gmail search instead.

use crate::system_label::{DRAFT, INBOX, SENT, SPAM, TRASH};

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

    /// Whether a message with `labels` still belongs in this folder.
    pub fn holds(self, labels: &[String]) -> bool {
        let has = |l: &str| labels.iter().any(|x| x == l);
        match self {
            Folder::Archive => ![INBOX, SENT, DRAFT, SPAM, TRASH].iter().any(|l| has(l)),
            Folder::Junk => has(SPAM),
            Folder::Trash => has(TRASH),
            Folder::AllMail => !has(SPAM) && !has(TRASH),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Folder;
    use crate::system_label::{INBOX, SENT, SPAM, TRASH};

    fn labels(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|l| l.to_string()).collect()
    }

    #[test]
    fn junk_and_trash_hold_their_own_label() {
        assert!(Folder::Junk.holds(&labels(&[SPAM])));
        assert!(!Folder::Junk.holds(&labels(&[INBOX])));
        assert!(Folder::Trash.holds(&labels(&[TRASH])));
        assert!(!Folder::Trash.holds(&labels(&[SPAM])));
    }

    #[test]
    fn all_mail_holds_everything_outside_spam_and_trash() {
        assert!(Folder::AllMail.holds(&labels(&[])));
        assert!(Folder::AllMail.holds(&labels(&[INBOX, "Label_1"])));
        assert!(!Folder::AllMail.holds(&labels(&[SPAM])));
        assert!(!Folder::AllMail.holds(&labels(&[TRASH])));
    }

    #[test]
    fn archive_holds_received_mail_outside_the_inbox() {
        assert!(Folder::Archive.holds(&labels(&["Label_1"])));
        assert!(Folder::Archive.holds(&labels(&[])));
        assert!(!Folder::Archive.holds(&labels(&[INBOX])));
        assert!(!Folder::Archive.holds(&labels(&[SENT])));
        assert!(!Folder::Archive.holds(&labels(&[TRASH])));
    }
}
