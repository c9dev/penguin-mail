//! Types shared by every mailrs crate. Conversions only, no I/O.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

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
}

impl AccountState {
    pub const ALL: [AccountState; 5] = [
        AccountState::Ok,
        AccountState::Bootstrapping,
        AccountState::NeedsReauth,
        AccountState::BackingOff,
        AccountState::Offline,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            AccountState::Ok => "ok",
            AccountState::Bootstrapping => "bootstrapping",
            AccountState::NeedsReauth => "needs_reauth",
            AccountState::BackingOff => "backing_off",
            AccountState::Offline => "offline",
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Label {
    pub account_id: AccountId,
    pub id: String,
    pub name: String,
    pub kind: LabelKind,
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
}

impl MessageMeta {
    pub fn is_unread(&self) -> bool {
        self.has_label("UNREAD")
    }

    pub fn has_label(&self, label_id: &str) -> bool {
        self.label_ids.iter().any(|l| l == label_id)
    }
}

/// One row of a thread list, aggregated from the thread's local messages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageBody {
    pub html: Option<String>,
    pub text: Option<String>,
    pub attachments: Vec<Attachment>,
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
}
