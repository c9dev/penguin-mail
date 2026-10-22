//! The inbox's unread mail, listed whole into a disk-backed snapshot
//! before Mark All as Read changes any of it.

use mailrs_domain::Role;
use mailrs_store::threads;

use super::{AccountSync, Snapshot};
use crate::{BackendError, RemoteRef, SyncError};

const PAGE: usize = 500;

impl AccountSync {
    /// Every unread message in the inbox, older mail than the store keeps
    /// among it. The server lists it, following its cursor to the end,
    /// unless the account's mailboxes live on this computer. Listing all of
    /// it first means a message the run fails to change cannot hide the
    /// mail behind it.
    pub(crate) async fn unread_inbox(&self) -> Result<Snapshot, SyncError> {
        let snapshot = Snapshot::new()?;
        if self.services.capabilities().local_mailboxes {
            let mut after = String::new();
            loop {
                let (account_id, from) = (self.account_id, after.clone());
                let page = self
                    .db
                    .read(move |c| threads::unread_inbox_ids(c, account_id, &from, PAGE as i64))
                    .await?;
                let Some((_, last)) = page.last() else { break };
                after = last.clone();
                let refs: Vec<RemoteRef> = page
                    .into_iter()
                    .map(|(thread_id, id)| RemoteRef { id, thread_id })
                    .collect();
                snapshot.append(&refs)?;
            }
            return Ok(snapshot);
        }
        let Some(inbox) = self.mail.mailbox_for(Role::Inbox) else {
            return Ok(snapshot);
        };
        let mut cursor = None;
        loop {
            let page = self
                .mail
                .unread_page(&inbox, cursor.as_deref(), PAGE)
                .await?;
            snapshot.append(&page.refs)?;
            let Some(next) = page.next else { break };
            if !snapshot.cursor(&next)? {
                return Err(BackendError::StateLost.into());
            }
            cursor = Some(next);
        }
        Ok(snapshot)
    }
}
