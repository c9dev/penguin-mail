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

    /// The message whose preview a row shows, independently of the scope
    /// of actions on that row. Older rows without a source keep their scope.
    pub fn for_preview(row: &ThreadSummary) -> Target {
        Target {
            account_id: row.account_id,
            thread_id: row.id.clone(),
            message_id: row
                .preview_message_id
                .clone()
                .or_else(|| row.message_id.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_grouped_preview_does_not_narrow_its_action_target() {
        let row = ThreadSummary {
            account_id: 1,
            id: "thread".into(),
            preview_message_id: Some("matching-message".into()),
            ..ThreadSummary::default()
        };
        assert_eq!(Target::from_row(&row), Target::thread(1, "thread"));
        assert_eq!(
            Target::for_preview(&row).message_id.as_deref(),
            Some("matching-message")
        );
        let row = ThreadSummary {
            message_id: Some("message".into()),
            preview_message_id: None,
            ..row
        };
        assert_eq!(Target::for_preview(&row), Target::from_row(&row));
    }
}
