//! Replies to a muted thread. Gmail's own filter keeps them out of the
//! Inbox; on any other server the engine files each one as the feed
//! brings it in, before the window or a notification hears of it.

use mailrs_domain::Target;
use mailrs_store::threads;

use super::AccountSync;
use crate::ops::ops_for;
use crate::{MailBackend, SyncError, TriageAction};

impl AccountSync {
    /// Takes the replies to muted threads out of `new_mail` and answers
    /// the rest, the mail worth announcing. On a server that does not
    /// file them itself, each reply gets what Mute gave its thread: it is
    /// marked muted and moved to the Archive, and read as well, so the
    /// Archive shows nothing new. A muted thread's reply goes unannounced
    /// even when filing it fails.
    pub(super) async fn file_muted_replies(&self, new_mail: Vec<String>) -> Vec<String> {
        if new_mail.is_empty() {
            return new_mail;
        }
        let (account_id, ids) = (self.account_id, new_mail.clone());
        let muted = match self
            .db
            .read(move |c| threads::in_muted_threads(c, account_id, &ids))
            .await
        {
            Ok(muted) => muted,
            Err(err) => {
                tracing::warn!(account = account_id, error = %err, "could not tell which new mail answers a muted thread");
                return new_mail;
            }
        };
        if muted.is_empty() {
            return new_mail;
        }
        if !self.services.mail.capabilities().files_muted_replies {
            let targets: Vec<Target> = muted
                .iter()
                .map(|(id, thread)| Target {
                    account_id,
                    thread_id: thread.clone(),
                    message_id: Some(id.clone()),
                })
                .collect();
            if let Err(err) = self.file_as_muted(&targets).await {
                tracing::warn!(account = account_id, error = %err, "could not file a muted thread's reply");
            }
        }
        new_mail
            .into_iter()
            .filter(|id| !muted.iter().any(|(m, _)| m == id))
            .collect()
    }

    /// Marks `targets` read, then mutes them as the Mute button does. The
    /// marks go first: they travel with an IMAP move.
    async fn file_as_muted(&self, targets: &[Target]) -> Result<(), SyncError> {
        let mail = &self.services.mail;
        let (caps, roles, tags) = (mail.capabilities(), self.roles(), self.tags().await?);
        let set_of = |id: &str| mail.set_of(id);
        let mut ops = ops_for(&TriageAction::MarkRead, &caps, &roles, &tags, set_of)?;
        ops.extend(ops_for(&TriageAction::Mute, &caps, &roles, &tags, set_of)?);
        self.change_all(targets, &ops, &TriageAction::Mute.describe(), None)
            .await
            .map(drop)
    }
}
