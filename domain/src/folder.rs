//! Gmail's archived mail, Spam, Trash, and All Mail, which the local window
//! does not hold.
//! The app lists them with a query tree instead, which the Gmail adapter
//! prints as a Gmail search.

use crate::query::Query;
use crate::{MailSet, MessageMeta, Role};

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

    /// The query that lists this folder. Archive and All Mail leave out
    /// the same mail [`Folder::holds`] does.
    pub fn query(self) -> Query {
        let outside = |roles: &[Role]| {
            Query::And(
                roles
                    .iter()
                    .map(|role| Query::not_in(MailSet::Role(*role)))
                    .collect(),
            )
        };
        match self {
            Folder::Archive => outside(&[
                Role::Inbox,
                Role::Sent,
                Role::Drafts,
                Role::Junk,
                Role::Trash,
            ]),
            Folder::Junk => Query::is_in(MailSet::Role(Role::Junk)),
            Folder::Trash => Query::is_in(MailSet::Role(Role::Trash)),
            Folder::AllMail => outside(&[Role::Junk, Role::Trash]),
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
    use crate::mailbox::keyword::SEEN;
    use crate::query::Query;
    use crate::{MailSet, MessageMeta, Role};

    /// A read message in `mailboxes`, which have `roles`.
    fn message(mailboxes: &[&str], roles: &[Role]) -> MessageMeta {
        crate::tests::message("m1", mailboxes, roles, &[SEEN])
    }

    #[test]
    fn junk_and_trash_hold_their_own_mailbox() {
        assert!(Folder::Junk.holds(&message(&["SPAM"], &[Role::Junk])));
        assert!(!Folder::Junk.holds(&message(&["INBOX"], &[Role::Inbox])));
        assert!(Folder::Trash.holds(&message(&["TRASH"], &[Role::Trash])));
        assert!(!Folder::Trash.holds(&message(&["SPAM"], &[Role::Junk])));
    }

    #[test]
    fn all_mail_holds_everything_outside_junk_and_trash() {
        assert!(Folder::AllMail.holds(&message(&[], &[])));
        assert!(Folder::AllMail.holds(&message(&["INBOX", "Label_1"], &[Role::Inbox])));
        assert!(!Folder::AllMail.holds(&message(&["SPAM"], &[Role::Junk])));
        assert!(!Folder::AllMail.holds(&message(&["TRASH"], &[Role::Trash])));
    }

    #[test]
    fn archive_holds_received_mail_outside_the_inbox_sent_and_drafts() {
        assert!(Folder::Archive.holds(&message(&["Label_1"], &[])));
        assert!(Folder::Archive.holds(&message(&[], &[])));
        let placed = [
            ("INBOX", Role::Inbox),
            ("SENT", Role::Sent),
            ("DRAFT", Role::Drafts),
            ("SPAM", Role::Junk),
            ("TRASH", Role::Trash),
        ];
        for (place, role) in placed {
            assert!(!Folder::Archive.holds(&message(&[place], &[role])), "{place}");
        }
    }

    #[test]
    fn each_folder_names_the_roles_it_holds_or_leaves_out() {
        let not = |role| Query::not_in(MailSet::Role(role));
        assert_eq!(Folder::Junk.query(), Query::is_in(MailSet::Role(Role::Junk)));
        assert_eq!(Folder::Trash.query(), Query::is_in(MailSet::Role(Role::Trash)));
        assert_eq!(
            Folder::AllMail.query(),
            Query::And(vec![not(Role::Junk), not(Role::Trash)])
        );
        assert_eq!(
            Folder::Archive.query(),
            Query::And(vec![
                not(Role::Inbox),
                not(Role::Sent),
                not(Role::Drafts),
                not(Role::Junk),
                not(Role::Trash),
            ])
        );
    }
}
