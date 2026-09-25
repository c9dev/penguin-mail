//! The IMAP adapter's sync state: for each synced mailbox, the
//! UIDVALIDITY its UIDs belong to, the UIDNEXT seen last, and the
//! HIGHESTMODSEQ where the server keeps one. Only this adapter reads it.

use std::collections::BTreeMap;

use mailrs_imap::Selected;
use serde::{Deserialize, Serialize};

use crate::BackendError;
use crate::services::SyncState;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ImapState {
    pub mailboxes: BTreeMap<String, Kept>,
}

/// Where one mailbox stood at the last look.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Kept {
    pub uidvalidity: u32,
    pub uidnext: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modseq: Option<u64>,
}

impl ImapState {
    /// A state this adapter cannot read, such as Gmail's history id, is one
    /// it did not write, so the caller lists the mail again rather than
    /// guess.
    pub fn read(state: &SyncState) -> Result<ImapState, BackendError> {
        serde_json::from_str(state.as_str()).map_err(|_| BackendError::StateLost)
    }

    pub fn written(&self) -> SyncState {
        // Serializing a map of numbers cannot fail; an empty state would
        // read back as lost and list the mail again.
        SyncState::new(serde_json::to_string(self).unwrap_or_default())
    }
}

impl Kept {
    /// Where `selected` leaves the mailbox, whose highest UID is `top`.
    /// RFC 3501 lets a server leave UIDNEXT out; the next look then asks
    /// for new mail from above `top`.
    pub fn of(selected: &Selected, top: u32) -> Kept {
        Kept {
            uidvalidity: selected.uidvalidity,
            uidnext: selected.uidnext.unwrap_or(top.saturating_add(1)),
            modseq: selected.highestmodseq,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{ImapState, Kept};

    #[test]
    fn the_state_is_a_small_json_object_per_mailbox() {
        let state = ImapState {
            mailboxes: BTreeMap::from([(
                "INBOX".to_string(),
                Kept {
                    uidvalidity: 7,
                    uidnext: 43,
                    modseq: Some(1200),
                },
            )]),
        };
        assert_eq!(
            state.written().as_str(),
            r#"{"mailboxes":{"INBOX":{"uidvalidity":7,"uidnext":43,"modseq":1200}}}"#
        );
    }

    #[test]
    fn a_state_reads_back_what_it_wrote() {
        let state = ImapState {
            mailboxes: BTreeMap::from([(
                "Sent".to_string(),
                Kept {
                    uidvalidity: 3,
                    uidnext: 9,
                    modseq: None,
                },
            )]),
        };
        assert_eq!(ImapState::read(&state.written()).unwrap(), state);
    }

    #[test]
    fn a_state_another_adapter_wrote_is_a_lost_place() {
        let gmail = crate::services::SyncState::new("{\"history_id\":100}");
        assert!(matches!(
            ImapState::read(&gmail),
            Err(crate::BackendError::StateLost)
        ));
    }
}
