//! Types shared by every Penguin Mail crate. Conversions only, no I/O.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::translate::gettext;

pub mod category;
mod folder;
pub mod gmail;
pub mod mailbox;
pub mod invitation;
pub mod smart;
pub mod subject;
pub mod system_label;
mod target;
pub mod translate;

pub use category::Category;
pub use folder::Folder;
pub use mailbox::{
    Applied, MailSet, MailboxKind, Membership, Memberships, Provider, RemoteMailbox, Role,
};
pub use invitation::Invitation;
pub use smart::SmartMailbox;
pub use target::Target;

/// Local database id of an account. Gmail has no account id of its own.
pub type AccountId = i64;

/// Milliseconds since the Unix epoch, the unit of Gmail's `internalDate`.
pub type EpochMillis = i64;

/// A stored or typed string that names no known variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownVariant(pub String);

impl fmt::Display for UnknownVariant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown variant: {}", self.0)
    }
}

impl std::error::Error for UnknownVariant {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AccountState {
    Ok,
    Bootstrapping,
    NeedsReauth,
    BackingOff,
    Offline,
    /// The account's sync loop crashed twice in a row, and the engine gave
    /// up on it until the app starts again.
    Stopped,
}

impl AccountState {
    pub const ALL: [AccountState; 6] = [
        AccountState::Ok,
        AccountState::Bootstrapping,
        AccountState::NeedsReauth,
        AccountState::BackingOff,
        AccountState::Offline,
        AccountState::Stopped,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            AccountState::Ok => "ok",
            AccountState::Bootstrapping => "bootstrapping",
            AccountState::NeedsReauth => "needs_reauth",
            AccountState::BackingOff => "backing_off",
            AccountState::Offline => "offline",
            AccountState::Stopped => "stopped",
        }
    }
}

impl FromStr for AccountState {
    type Err = UnknownVariant;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        AccountState::ALL
            .into_iter()
            .find(|state| state.as_str() == s)
            .ok_or_else(|| UnknownVariant(s.to_string()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    pub id: AccountId,
    pub email: String,
    pub state: AccountState,
    /// Who serves the account's mail. An account saved before providers
    /// existed was a Gmail account.
    #[serde(default)]
    pub provider: Provider,
}

/// Which Google client an account signed in with. A refresh token only
/// works with the client that issued it, so an account keeps the one it
/// signed in through until it signs in again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SignInClient {
    /// A client the person made in their own Google Cloud project and
    /// pasted into the old setup page, kept in `config.toml`.
    Own,
    /// The project's client, compiled into the build.
    BuiltIn,
}

impl SignInClient {
    pub fn as_str(self) -> &'static str {
        match self {
            SignInClient::Own => "own",
            SignInClient::BuiltIn => "built_in",
        }
    }
}

impl FromStr for SignInClient {
    type Err = UnknownVariant;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "own" => Ok(SignInClient::Own),
            "built_in" => Ok(SignInClient::BuiltIn),
            other => Err(UnknownVariant(other.to_string())),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LabelKind {
    System,
    User,
}

impl LabelKind {
    pub fn as_str(self) -> &'static str {
        match self {
            LabelKind::System => "system",
            LabelKind::User => "user",
        }
    }
}

impl FromStr for LabelKind {
    type Err = UnknownVariant;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "system" => Ok(LabelKind::System),
            "user" => Ok(LabelKind::User),
            other => Err(UnknownVariant(other.to_string())),
        }
    }
}

/// Apple Mail's flag colours. Gmail keeps only whether mail is starred, so
/// the colour lives on this computer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FlagColor {
    Red,
    Orange,
    Yellow,
    Green,
    Blue,
    Purple,
    Gray,
}

impl FlagColor {
    pub const ALL: [FlagColor; 7] = [
        FlagColor::Red,
        FlagColor::Orange,
        FlagColor::Yellow,
        FlagColor::Green,
        FlagColor::Blue,
        FlagColor::Purple,
        FlagColor::Gray,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            FlagColor::Red => "red",
            FlagColor::Orange => "orange",
            FlagColor::Yellow => "yellow",
            FlagColor::Green => "green",
            FlagColor::Blue => "blue",
            FlagColor::Purple => "purple",
            FlagColor::Gray => "gray",
        }
    }

    /// The colour's name, as a menu or a sidebar row shows it.
    pub fn name(self) -> String {
        match self {
            FlagColor::Red => gettext("Red"),
            FlagColor::Orange => gettext("Orange"),
            FlagColor::Yellow => gettext("Yellow"),
            FlagColor::Green => gettext("Green"),
            FlagColor::Blue => gettext("Blue"),
            FlagColor::Purple => gettext("Purple"),
            FlagColor::Gray => gettext("Gray"),
        }
    }
}

impl FromStr for FlagColor {
    type Err = UnknownVariant;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        FlagColor::ALL
            .into_iter()
            .find(|c| c.as_str() == s)
            .ok_or_else(|| UnknownVariant(s.to_string()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Label {
    pub account_id: AccountId,
    pub id: String,
    pub name: String,
    pub kind: LabelKind,
    /// Gmail's background colour for the label, as `#rrggbb`.
    #[serde(default)]
    pub color: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Address {
    pub name: Option<String>,
    pub email: String,
}

impl Address {
    /// The display name when there is one, otherwise the address.
    pub fn display(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.email)
    }
}

/// What the list views need about one message. Bodies live in [`MessageBody`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageMeta {
    pub account_id: AccountId,
    pub id: String,
    pub thread_id: String,
    pub rfc822_msgid: Option<String>,
    pub from: Option<Address>,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub subject: String,
    pub date: EpochMillis,
    pub snippet: String,
    pub size: i64,
    pub has_attachments: bool,
    pub label_ids: Vec<String>,
    /// The `List-Unsubscribe` header as it arrived, angle brackets and
    /// all. Every metadata fetch asks for it, so the newsletters list
    /// answers without fetching a single body.
    #[serde(default)]
    pub list_unsubscribe: Option<String>,
    /// The sender promised RFC 8058 one-click through
    /// `List-Unsubscribe-Post`.
    #[serde(default)]
    pub one_click: bool,
}

impl MessageMeta {
    /// The message lacks `$seen`.
    pub fn is_unread(&self) -> bool {
        self.has(&MailSet::Unseen)
    }

    pub fn is_flagged(&self) -> bool {
        self.has(&MailSet::flagged())
    }

    pub fn is_muted(&self) -> bool {
        self.has(&MailSet::muted())
    }

    /// The message sits in its account's mailbox with `role`.
    pub fn in_role(&self, role: Role) -> bool {
        self.has(&MailSet::Role(role))
    }

    /// The message sits in the server mailbox `id`.
    pub fn in_mailbox(&self, id: &str) -> bool {
        self.label_ids.iter().any(|l| l == id)
    }

    // The store still hands out Gmail's labels; this reads them until the
    // message carries its memberships and roles itself.
    fn has(&self, set: &MailSet) -> bool {
        gmail::label_of_set(set).is_some_and(|label| self.label_ids.contains(&label))
    }
}

/// One row of a thread list, aggregated from the thread's local messages.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadSummary {
    pub account_id: AccountId,
    /// The thread id. A single-message row still names its thread.
    pub id: String,
    /// Set when the row stands for one message rather than a whole thread,
    /// as it does with conversation grouping turned off.
    pub message_id: Option<String>,
    pub last_message_at: EpochMillis,
    /// Subject of the earliest local message, so replies don't show "Re:".
    pub subject: String,
    /// Snippet of the newest message.
    pub snippet: String,
    /// Display name of the newest message's sender.
    pub from: String,
    pub message_count: i64,
    pub unread: bool,
    pub starred: bool,
    pub has_attachments: bool,
    /// The thread carries Gmail's mute label, so replies skip the inbox.
    #[serde(default)]
    pub muted: bool,
    /// The flag colour chosen here for a starred row, if one was.
    #[serde(default)]
    pub flag_color: Option<FlagColor>,
    /// Address of the newest message's sender, for marking VIPs.
    #[serde(default)]
    pub from_email: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    pub part_id: String,
    pub filename: String,
    pub mime_type: String,
    pub size: i64,
    /// Gmail's handle for downloading the content with `messages.attachments.get`.
    pub attachment_id: Option<String>,
    /// `Content-ID` without angle brackets, for resolving `cid:` URLs.
    pub content_id: Option<String>,
}

/// What a message's headers say about where it came from, beyond the From
/// line the sender wrote. `mailrs_mime::provenance` reads it; the details
/// panel under a message shows it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    /// The domain that handed the message over, from the envelope sender
    /// or from SPF.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mailed_by: Option<String>,
    /// The domain whose DKIM key signed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signed_by: Option<String>,
    /// Whether the last hop to the mail server used TLS. None when the
    /// message says nothing either way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encrypted: Option<bool>,
}

impl Provenance {
    /// Whether it says anything at all. A message that answers none of
    /// the three gets no details panel.
    pub fn is_empty(&self) -> bool {
        self.mailed_by.is_none() && self.signed_by.is_none() && self.encrypted.is_none()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageBody {
    pub html: Option<String>,
    pub text: Option<String>,
    pub attachments: Vec<Attachment>,
    /// The `List-Unsubscribe` header: `<mailto:…>` and `<https:…>` links.
    #[serde(default)]
    pub list_unsubscribe: Option<String>,
    /// `List-Unsubscribe-Post: List-Unsubscribe=One-Click` was present, so
    /// a single POST to the https link unsubscribes (RFC 8058).
    #[serde(default)]
    pub one_click_unsubscribe: bool,
    /// The `text/calendar` part as it arrived, when the message carries
    /// one. `mailrs_domain::invitation::read` turns it into an event.
    #[serde(default)]
    pub calendar: Option<String>,
    /// Who really sent it, read off the headers.
    #[serde(default)]
    pub provenance: Provenance,
    /// The wrapper the message arrived in, when it arrived in one, and
    /// which standard wrote it. The parts themselves are not here: a
    /// signature covers the bytes as they were sent, and these ones have
    /// been through Gmail's decoding, so whoever checks a signature fetches
    /// the raw message instead.
    #[serde(default)]
    pub protection: Option<Protection>,
}

/// What a message arrived wrapped in: a signature beside the message, or
/// the message inside ciphertext, under OpenPGP or under S/MIME. The two
/// standards are read by different engines, so the wrapper names which.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Protection {
    /// `multipart/signed` with `protocol="application/pgp-signature"`.
    Signed,
    /// `multipart/encrypted` with `protocol="application/pgp-encrypted"`.
    Encrypted,
    /// `multipart/signed` with `protocol="application/pkcs7-signature"`.
    SmimeSigned,
    /// `application/pkcs7-mime` with `smime-type=signed-data`: the message
    /// inside the signature rather than beside it, which is what Outlook
    /// sends unless somebody told it not to.
    SmimeOpaque,
    /// `application/pkcs7-mime` with `smime-type=enveloped-data`.
    SmimeEnveloped,
}

impl Protection {
    pub const ALL: [Protection; 5] = [
        Protection::Signed,
        Protection::Encrypted,
        Protection::SmimeSigned,
        Protection::SmimeOpaque,
        Protection::SmimeEnveloped,
    ];

    /// The stored form.
    pub fn as_str(self) -> &'static str {
        match self {
            Protection::Signed => "signed",
            Protection::Encrypted => "encrypted",
            Protection::SmimeSigned => "smime-signed",
            Protection::SmimeOpaque => "smime-opaque",
            Protection::SmimeEnveloped => "smime-enveloped",
        }
    }

    /// Whether S/MIME wrote this wrapper, which says which engine opens it.
    pub fn is_smime(self) -> bool {
        matches!(
            self,
            Protection::SmimeSigned | Protection::SmimeOpaque | Protection::SmimeEnveloped
        )
    }
}

impl FromStr for Protection {
    type Err = UnknownVariant;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Protection::ALL
            .into_iter()
            .find(|protection| protection.as_str() == s)
            .ok_or_else(|| UnknownVariant(s.to_string()))
    }
}

/// A server rule: mail matching `criteria` gets `action`. `mailrs_gmail`
/// carries this to and from Gmail's own shape.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Filter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default)]
    pub criteria: FilterCriteria,
    #[serde(default)]
    pub action: FilterAction,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilterCriteria {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// Words the mail must contain, in Gmail search syntax.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    /// Words the mail must not contain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub negated_query: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub has_attachment: bool,
}

/// What a rule does to the mail it matches, in mail sets: the sets it
/// adds the mail to and the ones it takes the mail out of. Taking mail
/// out of the inbox role skips the inbox; out of `MailSet::Unseen` marks
/// it read; out of the junk role keeps it out of Spam.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilterAction {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub add: Vec<MailSet>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remove: Vec<MailSet>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forward: Option<String>,
}

impl Filter {
    /// A filter that sends mail from `email` straight to the Trash.
    pub fn block(email: &str) -> Filter {
        Filter {
            id: None,
            criteria: FilterCriteria {
                from: Some(email.to_string()),
                ..FilterCriteria::default()
            },
            action: FilterAction {
                add: vec![MailSet::Role(Role::Trash)],
                remove: vec![MailSet::Role(Role::Inbox)],
                forward: None,
            },
        }
    }
}

/// Gmail's automatic reply ("vacation responder") for one account.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Vacation {
    pub enabled: bool,
    pub subject: String,
    /// Plain text. Gmail gets an HTML copy with the same lines.
    pub body: String,
    /// Reply only to people in the account's contacts.
    pub contacts_only: bool,
    /// Reply only to people in the account's Workspace domain.
    pub domain_only: bool,
    pub start: Option<EpochMillis>,
    pub end: Option<EpochMillis>,
}

#[cfg(test)]
pub(crate) mod tests {
    use crate::{MessageMeta, Role};

    /// A message in account 1 carrying Gmail's `labels`.
    pub(crate) fn message(id: &str, labels: &[&str]) -> MessageMeta {
        MessageMeta {
            account_id: 1,
            id: id.into(),
            thread_id: "t1".into(),
            rfc822_msgid: None,
            from: None,
            to: vec![],
            cc: vec![],
            subject: String::new(),
            date: 0,
            snippet: String::new(),
            size: 0,
            has_attachments: false,
            label_ids: labels.iter().map(|l| l.to_string()).collect(),
            list_unsubscribe: None,
            one_click: false,
        }
    }

    #[test]
    fn a_message_answers_for_its_roles_and_keywords() {
        let m = message("m1", &["INBOX", "UNREAD", "STARRED", "MUTE", "Label_2"]);
        assert!(m.is_unread() && m.is_flagged() && m.is_muted());
        assert!(m.in_role(Role::Inbox));
        assert!(!m.in_role(Role::Sent));
        assert!(m.in_mailbox("Label_2"));
        assert!(!m.in_mailbox("Label_3"));
        let read = message("m2", &["SENT"]);
        assert!(!read.is_unread() && !read.is_flagged() && !read.is_muted());
        assert!(read.in_role(Role::Sent));
    }
}

/// What the sync engine reports to the UI. Views re-query the store in response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChangeEvent {
    AccountStateChanged {
        account_id: AccountId,
        state: AccountState,
    },
    LabelsChanged {
        account_id: AccountId,
    },
    /// Threads whose summary changed or that disappeared. Re-query to find out which.
    ThreadsChanged {
        account_id: AccountId,
        thread_ids: Vec<String>,
    },
    /// Unread mail that arrived in INBOX since the last poll.
    NewMail {
        account_id: AccountId,
        message_ids: Vec<String>,
    },
    WriteFailed {
        account_id: AccountId,
        message: String,
    },
    /// Gmail asked a mail action to slow down and the action is waiting it
    /// out. Says so once per action, so the window can show that the work
    /// is still going instead of looking stuck.
    WaitingOnGmail {
        account_id: AccountId,
        message: String,
    },
}
