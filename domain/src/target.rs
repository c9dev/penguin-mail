//! What a mail action applies to.

use crate::{AccountId, ThreadSummary};

/// A thread, or one message of it when the list shows messages rather than
/// conversations.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Target {
    pub account_id: AccountId,
    pub thread_id: String,
    /// Set to act on this message alone.
    pub message_id: Option<String>,
}

impl Target {
    /// A whole thread.
    pub fn thread(account_id: AccountId, thread_id: impl Into<String>) -> Target {
        Target {
            account_id,
            thread_id: thread_id.into(),
            message_id: None,
        }
    }

    /// What a list row stands for: its thread, or its one message.
    pub fn from_row(row: &ThreadSummary) -> Target {
        Target {
            account_id: row.account_id,
            thread_id: row.id.clone(),
            message_id: row.message_id.clone(),
        }
    }
}
