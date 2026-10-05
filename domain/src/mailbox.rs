//! Where a message sits on its server and what it carries, in words every
//! provider shares. A Gmail label and an IMAP folder are both server
//! mailboxes; read and starred are keywords; Gmail's inbox categories are
//! categories. The Gmail crate maps Gmail's labels onto these.

use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::UnknownVariant;

/// The company or protocol that serves an account's mail.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Provider {
    #[default]
    Gmail,
    /// Any server that speaks IMAP and SMTP. Who runs it, such as
    /// Fastmail, is the account's provider name.
    Imap,
    /// Outlook.com and Microsoft 365, through Microsoft Graph. The
    /// account's provider name says which.
    Microsoft,
    /// A server that speaks POP3 for incoming mail and SMTP for sending.
    /// The store holds the account's only copy of its mail; who runs the
    /// server is the account's provider name.
    Pop3,
}

impl Provider {
    pub const ALL: [Provider; 4] =
        [Provider::Gmail, Provider::Imap, Provider::Microsoft, Provider::Pop3];

    /// The stored form, in `accounts.provider`.
    pub fn as_str(self) -> &'static str {
        match self {
            Provider::Gmail => "gmail",
            Provider::Imap => "imap",
            Provider::Microsoft => "microsoft",
            Provider::Pop3 => "pop3",
        }
    }

    /// The provider's name as people know it. A brand or a protocol, so
    /// it is not translated. An IMAP or Microsoft account names its own
    /// provider through `Account::provider_name`.
    pub fn name(self) -> &'static str {
        match self {
            Provider::Gmail => "Gmail",
            Provider::Imap => "IMAP",
            Provider::Microsoft => "Microsoft",
            Provider::Pop3 => "POP3",
        }
    }
}

impl FromStr for Provider {
    type Err = UnknownVariant;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Provider::ALL
            .into_iter()
            .find(|p| p.as_str() == s)
            .ok_or_else(|| UnknownVariant(s.to_string()))
    }
}

/// What a POP3 account does with mail on the server once it is here.
/// Leaving it is the default; the store keeps every message either way.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RemoveSetting {
    #[default]
    Never,
    /// `DELE` each message the check after it arrived here.
    Downloaded,
    /// `DELE` a message once it has been here this many days.
    Days(u32),
}

impl RemoveSetting {
    /// The stored form: `accounts.pop3_remove` and `pop3_remove_days`.
    pub fn columns(self) -> (&'static str, Option<u32>) {
        match self {
            RemoveSetting::Never => ("never", None),
            RemoveSetting::Downloaded => ("downloaded", None),
            RemoveSetting::Days(days) => ("days", Some(days)),
        }
    }

    /// The setting the two columns hold, or `None` for a pair no write
    /// makes, such as `days` with no count.
    pub fn from_columns(mode: &str, days: Option<u32>) -> Option<RemoveSetting> {
        match (mode, days) {
            ("never", _) => Some(RemoveSetting::Never),
            ("downloaded", _) => Some(RemoveSetting::Downloaded),
            ("days", Some(days)) if days > 0 => Some(RemoveSetting::Days(days)),
            _ => None,
        }
    }
}

/// What a server mailbox is for, whatever the server calls it. Starred
/// mail is the `$flagged` keyword rather than a role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Role {
    Inbox,
    Sent,
    Drafts,
    Trash,
    Junk,
    Archive,
    All,
    Important,
}

impl Role {
    pub const ALL: [Role; 8] = [
        Role::Inbox,
        Role::Sent,
        Role::Drafts,
        Role::Trash,
        Role::Junk,
        Role::Archive,
        Role::All,
        Role::Important,
    ];

    /// The stored form, in `mailboxes.role`.
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Inbox => "inbox",
            Role::Sent => "sent",
            Role::Drafts => "drafts",
            Role::Trash => "trash",
            Role::Junk => "junk",
            Role::Archive => "archive",
            Role::All => "all",
            Role::Important => "important",
        }
    }
}

impl FromStr for Role {
    type Err = UnknownVariant;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Role::ALL
            .into_iter()
            .find(|r| r.as_str() == s)
            .ok_or_else(|| UnknownVariant(s.to_string()))
    }
}

/// Who made a server mailbox and how mail sits in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MailboxKind {
    /// The server made it and names it the same in every account, such as
    /// Gmail's `INBOX` or `CATEGORY_SOCIAL`.
    System,
    /// A person's label: one message can carry several.
    Label,
    /// A person's folder: a message sits in one.
    Folder,
    /// A folder that holds other folders and no mail, as an IMAP server
    /// lists a `\Noselect` or `\NonExistent` parent. Nothing opens it or
    /// moves mail into it.
    Group,
    /// A mark a folder server keeps beside the one folder a message sits
    /// in: an Outlook category. A message carries any number of them, and
    /// moving it to another folder keeps them.
    Tag,
}

impl MailboxKind {
    pub const ALL: [MailboxKind; 5] = [
        MailboxKind::System,
        MailboxKind::Label,
        MailboxKind::Folder,
        MailboxKind::Group,
        MailboxKind::Tag,
    ];

    /// The stored form, in `mailboxes.kind`.
    pub fn as_str(self) -> &'static str {
        match self {
            MailboxKind::System => "system",
            MailboxKind::Label => "label",
            MailboxKind::Folder => "folder",
            MailboxKind::Group => "group",
            MailboxKind::Tag => "tag",
        }
    }
}

impl FromStr for MailboxKind {
    type Err = UnknownVariant;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        MailboxKind::ALL
            .into_iter()
            .find(|k| k.as_str() == s)
            .ok_or_else(|| UnknownVariant(s.to_string()))
    }
}

/// One server mailbox as the server lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteMailbox {
    /// The server's id for it: a Gmail label id, later an IMAP path.
    pub id: String,
    pub name: String,
    pub kind: MailboxKind,
    pub role: Option<Role>,
    /// The server's background colour for it, as `#rrggbb`.
    pub color: Option<String>,
    /// The server keeps it out of its own mailbox list.
    pub hidden: bool,
}

/// Keywords a message can carry, spelled as IMAP spells them, which JMAP
/// kept. A server may store others; these are the ones the app reads.
pub mod keyword {
    pub const SEEN: &str = "$seen";
    pub const FLAGGED: &str = "$flagged";
    pub const ANSWERED: &str = "$answered";
    pub const DRAFT: &str = "$draft";
    pub const MUTED: &str = "$muted";
}

/// One thing a message can be in or carry.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Membership {
    /// A server mailbox, by the server's id for it.
    Mailbox(String),
    Keyword(String),
    /// An inbox category, such as Gmail's `CATEGORY_SOCIAL`.
    Category(String),
}

/// Everything one message is in or carries.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Memberships {
    pub mailboxes: Vec<String>,
    pub keywords: Vec<String>,
    pub categories: Vec<String>,
}

impl Memberships {
    /// A read message in no mailbox: `$seen` and nothing else.
    pub fn read() -> Memberships {
        Memberships {
            keywords: vec![keyword::SEEN.into()],
            ..Memberships::default()
        }
    }

    pub fn has(&self, membership: &Membership) -> bool {
        match membership {
            Membership::Mailbox(id) => self.mailboxes.contains(id),
            Membership::Keyword(k) => self.keywords.contains(k),
            Membership::Category(c) => self.categories.contains(c),
        }
    }

    /// Sorts each list and drops repeats, so two lists of the same
    /// memberships compare equal whatever order they arrived in.
    pub fn sort(&mut self) {
        for list in [&mut self.mailboxes, &mut self.keywords, &mut self.categories] {
            list.sort();
            list.dedup();
        }
    }
}

/// Which stored mail a list draws from, in words every provider shares:
/// the mailbox with a role in each account in question, one server
/// mailbox, the mail carrying a keyword, unread mail, or an inbox
/// category. Thread lists, the sidebar's counts, triage and the rules all
/// name mail this way.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum MailSet {
    Role(Role),
    /// One server mailbox, by the server's id for it. A person's labels
    /// and folders have ids only their own account knows.
    Mailbox(String),
    Keyword(String),
    /// Mail without `$seen`. Adding it to a message marks the message
    /// unread; removing it marks the message read.
    Unseen,
    Category(String),
}

impl MailSet {
    pub fn flagged() -> MailSet {
        MailSet::Keyword(keyword::FLAGGED.into())
    }

    pub fn muted() -> MailSet {
        MailSet::Keyword(keyword::MUTED.into())
    }
}

/// What one change did to one message: the memberships it gained and the
/// ones it lost, leaving out any the message already had or already
/// lacked. Undo reverses exactly this, so a message that was read before
/// a thread was marked read stays read when that is undone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    pub thread_id: String,
    pub message_id: String,
    pub gained: Vec<Membership>,
    pub lost: Vec<Membership>,
}

impl Applied {
    /// Whether the message came out as it went in.
    pub fn is_empty(&self) -> bool {
        self.gained.is_empty() && self.lost.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pop3_provider_is_stored_and_read_back() {
        assert_eq!(Provider::Pop3.as_str(), "pop3");
        assert_eq!("pop3".parse::<Provider>().unwrap(), Provider::Pop3);
        assert_eq!(Provider::Pop3.name(), "POP3");
    }

    #[test]
    fn the_removal_setting_round_trips_through_its_two_columns() {
        for setting in [RemoveSetting::Never, RemoveSetting::Downloaded, RemoveSetting::Days(14)] {
            let (mode, days) = setting.columns();
            assert_eq!(RemoveSetting::from_columns(mode, days), Some(setting));
        }
        assert_eq!(RemoveSetting::Days(14).columns(), ("days", Some(14)));
        assert_eq!(RemoveSetting::from_columns("days", None), None, "days with no count");
        assert_eq!(RemoveSetting::from_columns("days", Some(0)), None);
        assert_eq!(RemoveSetting::from_columns("sometimes", None), None);
        assert_eq!(RemoveSetting::default(), RemoveSetting::Never);
    }

    #[test]
    fn sorted_memberships_compare_equal_whatever_their_order() {
        let mut first = Memberships {
            mailboxes: vec!["Label_2".into(), "INBOX".into(), "INBOX".into()],
            keywords: vec![keyword::SEEN.into(), keyword::FLAGGED.into()],
            categories: vec![],
        };
        let mut second = Memberships {
            mailboxes: vec!["INBOX".into(), "Label_2".into()],
            keywords: vec![keyword::FLAGGED.into(), keyword::SEEN.into()],
            categories: vec![],
        };
        first.sort();
        second.sort();
        assert_eq!(first, second);
    }

    #[test]
    fn a_read_message_carries_seen_alone() {
        let read = Memberships::read();
        assert_eq!(read.keywords, [keyword::SEEN]);
        assert!(read.mailboxes.is_empty() && read.categories.is_empty());
    }

    #[test]
    fn stored_names_round_trip() {
        for role in Role::ALL {
            assert_eq!(role.as_str().parse::<Role>(), Ok(role));
        }
        for kind in MailboxKind::ALL {
            assert_eq!(kind.as_str().parse::<MailboxKind>(), Ok(kind));
        }
        for provider in Provider::ALL {
            assert_eq!(provider.as_str().parse::<Provider>(), Ok(provider));
        }
        assert!("outbox".parse::<Role>().is_err());
    }

    #[test]
    fn a_tag_is_stored_as_tag() {
        assert_eq!(MailboxKind::Tag.as_str(), "tag");
        assert_eq!("tag".parse::<MailboxKind>(), Ok(MailboxKind::Tag));
        assert_eq!("tag".parse::<crate::LabelKind>(), Ok(crate::LabelKind::Tag));
    }

    #[test]
    fn a_group_is_stored_as_group() {
        assert_eq!(MailboxKind::Group.as_str(), "group");
        assert_eq!("group".parse::<MailboxKind>(), Ok(MailboxKind::Group));
    }

    #[test]
    fn a_change_that_moved_nothing_is_empty() {
        let applied = Applied {
            thread_id: "t1".into(),
            message_id: "m1".into(),
            gained: vec![],
            lost: vec![],
        };
        assert!(applied.is_empty());
        let read = Applied {
            gained: vec![Membership::Keyword(keyword::SEEN.into())],
            ..applied
        };
        assert!(!read.is_empty());
    }

    #[test]
    fn memberships_answer_for_each_kind() {
        let held = Memberships {
            mailboxes: vec!["INBOX".into()],
            keywords: vec![keyword::FLAGGED.into()],
            categories: vec!["CATEGORY_SOCIAL".into()],
        };
        assert!(held.has(&Membership::Mailbox("INBOX".into())));
        assert!(held.has(&Membership::Keyword(keyword::FLAGGED.into())));
        assert!(held.has(&Membership::Category("CATEGORY_SOCIAL".into())));
        assert!(!held.has(&Membership::Mailbox("TRASH".into())));
    }

    #[test]
    fn a_mail_set_names_flagged_and_muted_by_keyword() {
        assert_eq!(MailSet::flagged(), MailSet::Keyword(keyword::FLAGGED.into()));
        assert_eq!(MailSet::muted(), MailSet::Keyword(keyword::MUTED.into()));
    }
}
