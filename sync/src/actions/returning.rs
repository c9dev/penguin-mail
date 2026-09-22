//! Remind Me coming due. `MailAction::Remind` archives a conversation and
//! records the hour; this part puts it back in the inbox when that hour
//! comes, so the Gmail change and the store change happen in one place.

use mailrs_domain::{EpochMillis, MessageMeta, Target, system_label};
use mailrs_store::{messages, reminders};

use super::MailActions;
use crate::{Accounts, SyncError, TriageAction};

/// A conversation its reminder brought back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Returned {
    pub target: Target,
    /// Its newest message, for the notification that announces it. `None`
    /// when the store holds none or could not be read.
    pub newest: Option<MessageMeta>,
}

impl<A: Accounts> MailActions<A> {
    /// Puts each conversation whose reminder is due by `now` back in the
    /// inbox, unread, and drops the reminder. A conversation whose account
    /// is not syncing, or that Gmail would not take back, keeps its
    /// reminder and comes back on a later pass.
    pub async fn return_due(&self, now: EpochMillis) -> Result<Vec<Returned>, SyncError> {
        let due = self.db.read(move |c| reminders::due(c, now)).await?;
        let back = TriageAction::Relabel {
            add: vec![system_label::INBOX.into(), system_label::UNREAD.into()],
            remove: vec![],
        };
        let mut returned = Vec::new();
        for item in due {
            let Some(sync) = self.accounts.account(item.account_id) else {
                continue;
            };
            if let Err(err) = sync.triage_thread(&item.thread_id, &back).await {
                tracing::warn!(error = %err, "a reminder could not return its conversation; will retry");
                continue;
            }
            let target = Target::thread(item.account_id, &item.thread_id);
            let (account_id, thread) = (item.account_id, item.thread_id);
            let newest = self
                .db
                .write(move |c| {
                    reminders::remove(c, account_id, &thread)?;
                    Ok(messages::thread_messages(c, account_id, &thread)?
                        .into_iter()
                        .last())
                })
                .await
                .unwrap_or_else(|err| {
                    tracing::warn!(error = %err, "could not drop a reminder that came due");
                    None
                });
            returned.push(Returned { target, newest });
        }
        Ok(returned)
    }
}
