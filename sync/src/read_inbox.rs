//! Marks each account's inbox read: every unread message in it, on the
//! server as well as on this computer. The tray's Mark All as Read runs
//! this across the accounts.

use mailrs_domain::mailbox::keyword::SEEN;
use mailrs_domain::{AccountId, Target};

use crate::account::Snapshot;
use crate::actions::Staged;
use crate::{
    Accounts, History, MailAction, MailActions, MailOp, Recorded, SyncError, TriageAction,
};

/// How many messages one page marks read.
pub const READ_PAGE: usize = 100;

/// What marking the inboxes read came to. It counts rather than lists the
/// mail, and both the run and its Undo keep the listing on disk, so memory
/// stays one page whatever the size of the inboxes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InboxesRead {
    /// Messages that went read.
    pub read: usize,
    /// Messages that stayed unread because the change failed.
    pub refused: usize,
    /// Per account, why its unread mail could not be listed, or the first
    /// reason a message of it stayed unread.
    pub failed: Vec<(AccountId, String)>,
    /// The one undo entry the whole run went into.
    pub recorded: Option<Recorded>,
}

impl<A: Accounts> MailActions<A> {
    /// Marks every unread message in each account's inbox read, including
    /// mail older than this computer keeps, in pages of [`READ_PAGE`].
    /// With `History::Record` the whole run is one undo entry, so one Undo
    /// takes all of it back.
    pub async fn read_inboxes(&self, accounts: &[AccountId], history: History) -> InboxesRead {
        self.read_inboxes_in_pages(accounts, history, READ_PAGE)
            .await
    }

    pub(crate) async fn read_inboxes_in_pages(
        &self,
        accounts: &[AccountId],
        history: History,
        page: usize,
    ) -> InboxesRead {
        let mut read = InboxesRead::default();
        let mut staged = Vec::new();
        for &account_id in accounts {
            let before = read.read;
            match self.read_inbox(account_id, page, &mut read).await {
                // An account with nothing marked read has nothing to undo.
                Ok(_) if read.read == before => {}
                Ok(snapshot) => staged.push(Staged {
                    account_id,
                    snapshot,
                    undo: vec![MailOp::SetKeyword {
                        keyword: SEEN.into(),
                        on: false,
                    }],
                }),
                Err(err) => read.failed.push((account_id, err.to_string())),
            }
        }
        if history == History::Record && read.read > 0 {
            read.recorded =
                Some(self.record_staged(MailAction::Triage(TriageAction::MarkRead), staged));
        }
        read
    }

    /// Lists the inbox's unread mail whole before changing any of it, then
    /// marks it read a page at a time, checking off in the snapshot what
    /// went read for Undo. A refused page cannot hide the mail after it.
    async fn read_inbox(
        &self,
        account_id: AccountId,
        page: usize,
        read: &mut InboxesRead,
    ) -> Result<Snapshot, SyncError> {
        let sync = self
            .accounts
            .account(account_id)
            .ok_or(SyncError::UnknownAccount(account_id))?;
        let snapshot = sync.unread_inbox().await?;
        let mut refusal = None;
        let mut after = 0;
        loop {
            let (last, refs) = snapshot.batch(after, page, false)?;
            if refs.is_empty() {
                break;
            }
            let targets: Vec<Target> = refs
                .into_iter()
                .map(|r| Target {
                    account_id,
                    thread_id: r.thread_id,
                    message_id: Some(r.id),
                })
                .collect();
            let ran = self
                .run(
                    &targets,
                    MailAction::Triage(TriageAction::MarkRead),
                    History::Skip,
                )
                .await;
            snapshot.checked(&ran.done)?;
            read.read += ran.done.len();
            read.refused += ran.failed.len();
            if refusal.is_none() {
                refusal = ran.failed.into_iter().next().map(|f| f.error);
            }
            after = last;
        }
        if let Some(error) = refusal {
            read.failed.push((account_id, error));
        }
        Ok(snapshot)
    }
}
