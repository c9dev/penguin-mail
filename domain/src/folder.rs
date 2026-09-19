//! Gmail's Spam, Trash, and All Mail, which the local window does not hold.
//! The app lists them with a Gmail search instead.

use crate::system_label::{SPAM, TRASH};

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

    /// Whether a message with `labels` still belongs in this folder.
    pub fn holds(self, labels: &[String]) -> bool {
        let has = |l: &str| labels.iter().any(|x| x == l);
        match self {
            Folder::Junk => has(SPAM),
            Folder::Trash => has(TRASH),
            Folder::AllMail => !has(SPAM) && !has(TRASH),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Folder;
    use crate::system_label::{INBOX, SPAM, TRASH};

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
}
