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
    /// Gmail over IMAP: the first look has told the engine where the
    /// window's messages sit.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub located: bool,
}

/// The most UIDs a mailbox's [`Kept::deleted`] remembers, the highest
/// kept. A client that marks more and never expunges loses only the
/// return of the oldest if it undoes them.
pub(super) const MOST_REMEMBERED_DELETED: usize = 1000;

/// Where one mailbox stood at the last look.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Kept {
    pub uidvalidity: u32,
    pub uidnext: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modseq: Option<u64>,
    /// The UIDs, ascending, that another client had marked `\Deleted`
    /// at the last look, which the store therefore let go. One that
    /// comes back undeleted is fetched again.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deleted: Vec<u32>,
}

impl Kept {
    /// Remembers `uids` as the mailbox's deleted UIDs, the highest
    /// [`MOST_REMEMBERED_DELETED`] of them.
    pub fn remember_deleted(&mut self, mut uids: Vec<u32>) {
        uids.sort_unstable();
        uids.dedup();
        let excess = uids.len().saturating_sub(MOST_REMEMBERED_DELETED);
        uids.drain(..excess);
        self.deleted = uids;
    }
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
            deleted: Vec::new(),
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
            located: false,
            mailboxes: BTreeMap::from([(
                "INBOX".to_string(),
                Kept {
                    uidvalidity: 7,
                    uidnext: 43,
                    modseq: Some(1200),
                    deleted: Vec::new(),
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
            located: false,
            mailboxes: BTreeMap::from([(
                "Sent".to_string(),
                Kept {
                    uidvalidity: 3,
                    uidnext: 9,
                    modseq: None,
                    deleted: Vec::new(),
                },
            )]),
        };
        assert_eq!(ImapState::read(&state.written()).unwrap(), state);
    }

    #[test]
    fn a_mailbox_remembers_its_highest_deleted_uids_up_to_the_cap() {
        let mut kept = Kept {
            uidvalidity: 1,
            uidnext: 5000,
            modseq: None,
            deleted: Vec::new(),
        };
        kept.remember_deleted((1..=1500).rev().chain([1500]).collect());
        assert_eq!(kept.deleted.len(), super::MOST_REMEMBERED_DELETED);
        assert_eq!((kept.deleted[0], kept.deleted[999]), (501, 1500));
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
