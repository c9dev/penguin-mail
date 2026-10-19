//! Gmail over IMAP (X-GM-EXT-1). Gmail keeps every message once and shows
//! it in a folder for each label, so this dialect syncs only All Mail,
//! Trash and Spam. A message is named by its X-GM-MSGID in lower-case hex,
//! which is the Gmail API's message id and survives a move; the mailbox and
//! UID it sits at now live in the store's remote ref. Its labels come from
//! X-GM-LABELS and are written as the Gmail API labels are.

mod feed;
mod ids;
pub(super) mod labels;
mod listing;

pub(super) use ids::Places;

use mailrs_domain::{MailSet, MailboxKind, RemoteMailbox, Role};

use super::{Imap, ImapApi, Submit};
use crate::BackendError;
use labels::Where;

/// Which dialect of IMAP an account speaks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Dialect {
    /// Any IMAP server: a message is named by where it sits.
    #[default]
    Plain,
    /// Gmail's IMAP.
    Gmail,
}

/// The three mailboxes a Gmail account syncs, by the server's names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Trio {
    pub all: String,
    pub trash: String,
    pub spam: String,
}

impl Trio {
    /// The order a listing reads them in.
    pub fn in_order(&self) -> Vec<String> {
        vec![self.all.clone(), self.trash.clone(), self.spam.clone()]
    }

    /// The order a look reads them in: a message that left All Mail for
    /// Trash is met in Trash first.
    pub fn look_order(&self) -> Vec<String> {
        vec![self.trash.clone(), self.spam.clone(), self.all.clone()]
    }

    /// Which of the three `mailbox` is. Anything else counts as All Mail.
    pub fn place_of(&self, mailbox: &str) -> Where {
        match mailbox {
            m if m == self.trash => Where::Trash,
            m if m == self.spam => Where::Spam,
            _ => Where::AllMail,
        }
    }
}

/// The system labels the store lists besides a person's own.
const SYSTEM_LISTED: [&str; 6] = ["INBOX", "SENT", "DRAFT", "TRASH", "SPAM", "IMPORTANT"];

/// The inbox tabs the store lists.
const TABS_LISTED: [&str; 5] = [
    "CATEGORY_PERSONAL",
    "CATEGORY_SOCIAL",
    "CATEGORY_PROMOTIONS",
    "CATEGORY_UPDATES",
    "CATEGORY_FORUMS",
];

impl<I: ImapApi, S: Submit> Imap<I, S> {
    /// Whether this account speaks Gmail's IMAP.
    pub(super) fn gmail(&self) -> bool {
        self.settings.dialect == Dialect::Gmail
    }

    /// All Mail, Trash and Spam as the server names them, found by their
    /// special use since the names follow the account's language. All Mail
    /// is missing when the person left it out of IMAP in Gmail's settings.
    pub(super) async fn gm_trio(&self) -> Result<Trio, BackendError> {
        self.ensure_listed().await?;
        let known = self.known();
        let named = |role: Role| {
            known
                .folders
                .iter()
                .find(|f| f.role == Some(role) && !f.parent_only)
                .map(|f| f.id.clone())
        };
        match (named(Role::All), named(Role::Trash), named(Role::Junk)) {
            (Some(all), Some(trash), Some(spam)) => Ok(Trio { all, trash, spam }),
            (None, _, _) => Err(BackendError::Refused(
                "Gmail leaves All Mail out of IMAP".into(),
            )),
            _ => Err(BackendError::Refused(
                "Gmail lists no Trash or Spam folder".into(),
            )),
        }
    }

    /// The label folders Gmail lists for the person's own labels, as store
    /// ids with the server's name for each: everything outside the
    /// `[Gmail]` parent that holds All Mail, and the Inbox.
    async fn gm_label_folders(&self) -> Result<Vec<(String, mailrs_imap::Listed)>, BackendError> {
        let trio = self.gm_trio().await?;
        let listed = self.api.list().await?;
        let delimiter = listed.iter().find_map(|l| l.delimiter).unwrap_or('/');
        let parent = trio
            .all
            .rsplit_once(delimiter)
            .map(|(parent, _)| parent.to_string());
        Ok(listed
            .into_iter()
            .filter(|l| !l.name.eq_ignore_ascii_case("INBOX"))
            .filter(|l| match &parent {
                Some(parent) => {
                    l.name != *parent && !l.name.starts_with(&format!("{parent}{delimiter}"))
                }
                None => true,
            })
            .map(|l| (l.display.replace(delimiter, "/"), l))
            .collect())
    }

    /// The store's mailboxes for a Gmail account: Gmail's own labels with
    /// fixed ids, the inbox tabs, and a label for each of the person's
    /// folders named by its full name.
    pub(super) async fn gm_mailboxes(&self) -> Result<Vec<RemoteMailbox>, BackendError> {
        // Remembers the roles, for the calls that answer at once.
        self.list_mailboxes().await?;
        let mut boxes: Vec<RemoteMailbox> = SYSTEM_LISTED
            .iter()
            .chain(TABS_LISTED.iter())
            .map(|id| RemoteMailbox {
                id: (*id).into(),
                name: (*id).into(),
                kind: MailboxKind::System,
                role: mailrs_gmail::labels::role_of(id),
                color: None,
                hidden: false,
            })
            .collect();
        for (name, listed) in self.gm_label_folders().await? {
            boxes.push(RemoteMailbox {
                id: name.clone(),
                name,
                kind: match listed.no_select {
                    true => MailboxKind::Group,
                    false => MailboxKind::Label,
                },
                role: None,
                color: None,
                hidden: false,
            });
        }
        Ok(boxes)
    }

    /// Unread counts from STATUS of the Inbox, Spam and each label's own
    /// folder, which Gmail counts across All Mail.
    pub(super) async fn gm_unread(
        &self,
    ) -> Result<std::collections::HashMap<MailSet, i64>, BackendError> {
        use futures::{StreamExt, stream};
        use std::time::Duration;
        use tokio::time::timeout;

        let limit = Duration::from_secs(10);
        let trio = self.gm_trio().await?;
        let mut folders: Vec<(String, String)> = vec![
            ("INBOX".into(), "INBOX".into()),
            (trio.spam.clone(), "SPAM".into()),
        ];
        for (name, listed) in self.gm_label_folders().await? {
            if !listed.no_select {
                folders.push((listed.name, name));
            }
        }
        let mut counts = std::collections::HashMap::new();
        let mut replies = stream::iter(folders)
            .map(|(folder, id)| async move {
                let count = timeout(limit, self.api.unread(&folder)).await;
                (id, count)
            })
            .buffer_unordered(2);
        while let Some((id, count)) = replies.next().await {
            match count {
                Ok(Ok(count)) => {
                    let set = mailrs_gmail::labels::set_of(&id);
                    if let MailSet::Role(_) = set {
                        counts.insert(MailSet::Mailbox(id), i64::from(count));
                    }
                    counts.insert(set, i64::from(count));
                }
                Ok(Err(
                    mailrs_imap::ImapError::NoMailbox(_) | mailrs_imap::ImapError::Refused(_),
                )) => {
                    continue;
                }
                Ok(Err(_)) | Err(_) => break,
            }
        }
        Ok(counts)
    }
}

#[cfg(test)]
mod tests {
    use super::Trio;
    use super::labels::Where;

    #[test]
    fn a_mailbox_that_is_neither_trash_nor_spam_counts_as_all_mail() {
        let trio = Trio {
            all: "[Gmail]/Todos".into(),
            trash: "[Gmail]/Lixo".into(),
            spam: "[Gmail]/Spam".into(),
        };
        assert_eq!(trio.place_of("[Gmail]/Lixo"), Where::Trash);
        assert_eq!(trio.place_of("[Gmail]/Spam"), Where::Spam);
        assert_eq!(trio.place_of("[Gmail]/Todos"), Where::AllMail);
        assert_eq!(trio.look_order()[2], "[Gmail]/Todos");
    }
}
