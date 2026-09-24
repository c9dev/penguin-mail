//! The IMAP change feed.

use super::state::{ImapState, Kept};
use super::{Imap, ImapApi, Submit};
use crate::BackendError;
use crate::services::Changes;

impl<I: ImapApi, S: Submit> Imap<I, S> {
    /// Where every synced mailbox stands now, with no changes: the start
    /// of the feed.
    pub(super) async fn feed_start(&self) -> Result<Changes, BackendError> {
        let mut state = ImapState::default();
        for mailbox in self.synced().await? {
            let selected = self.select(&mailbox, None).await?;
            state.mailboxes.insert(mailbox, Kept::of(&selected));
        }
        Ok(Changes {
            changes: Vec::new(),
            state: state.written(),
        })
    }
}
