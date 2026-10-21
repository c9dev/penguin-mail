//! A disk-backed Trash snapshot, checked again one delete batch at a time.

use std::collections::HashSet;
use std::sync::Arc;

use mailrs_domain::{Role, Target};

use super::{AccountSync, Snapshot};
use crate::{BackendError, Failure, Outcome, Permitted, RemoteRef, SyncError};

const BATCH: usize = 100;

/// Counts and the first error, without retaining every deleted message.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Emptied {
    pub deleted: usize,
    pub failed: usize,
    pub error: Option<String>,
    pub needs_permission: bool,
}

impl Emptied {
    fn add(&mut self, outcome: &Outcome) {
        self.deleted += outcome.done.len();
        self.failed += outcome.failed.len();
        if self.error.is_none() {
            self.error = outcome.first_error().map(str::to_owned);
        }
    }
}

impl AccountSync {
    pub(crate) async fn empty_trash(
        &self,
        mut finished: impl FnMut(&Outcome) + Send,
    ) -> Result<Permitted<Emptied>, SyncError> {
        if !self.mail.capabilities().delete_forever {
            return Err(BackendError::Unsupported.into());
        }
        let moves = self.mail.hold_moves().await;
        let Some(trash) = self.mail.mailbox_for(Role::Trash) else {
            return Ok(Permitted::Done(Emptied::default()));
        };
        let snapshot = Arc::new(Snapshot::new()?);
        if matches!(self.services.mail, crate::AnyMail::Pop3(_)) {
            let (snapshot, trash, account) =
                (Arc::clone(&snapshot), trash.clone(), self.account_id);
            self.db
                .read(move |c| snapshot.local(c, account, &trash))
                .await?;
        } else {
            let mut cursor = None;
            loop {
                let page = self
                    .mail
                    .mailbox_page(&trash, cursor.as_deref(), 500)
                    .await?;
                snapshot.append(&page.refs)?;
                let Some(next) = page.next else { break };
                if !snapshot.cursor(&next)? {
                    return Err(BackendError::StateLost.into());
                }
                cursor = Some(next);
            }
        }
        // Listing and metadata errors must surface before the first irreversible write.
        let limit = BATCH.min(self.mail.capabilities().batch_limit.max(1));
        let mut after = 0;
        loop {
            let (last, refs) = snapshot.batch(after, limit, false)?;
            if refs.is_empty() {
                break;
            }
            let targets = self.trash_targets(&refs, &trash).await?;
            snapshot.checked(&targets)?;
            after = last;
        }
        let mut result = Emptied::default();
        after = 0;
        loop {
            let (last, refs) = snapshot.batch(after, limit, true)?;
            if refs.is_empty() {
                break;
            }
            // Another client can restore mail while preflight reads the other batches.
            let targets = match self.trash_targets(&refs, &trash).await {
                Ok(targets) => targets,
                Err(error) => {
                    result.error.get_or_insert_with(|| error.to_string());
                    break;
                }
            };
            let ids = targets
                .iter()
                .map(|t| t.message_id.iter().cloned().collect())
                .collect();
            let results = match self.erase_ids(&targets, ids, &moves, Some(&trash)).await {
                Ok(results) => results,
                Err(error) => {
                    result.error.get_or_insert_with(|| error.to_string());
                    break;
                }
            };
            let mut outcome = Outcome::default();
            for (target, answer) in targets.into_iter().zip(results) {
                match answer {
                    Ok(()) => outcome.done.push(target),
                    Err(error) => {
                        result.needs_permission |=
                            matches!(error, SyncError::Backend(BackendError::NeedsPermission));
                        outcome.failed.push(Failure {
                            target,
                            error: error.to_string(),
                        });
                    }
                }
            }
            finished(&outcome);
            result.add(&outcome);
            if result.needs_permission {
                break;
            }
            after = last;
        }
        if result.needs_permission && result.deleted == 0 {
            Ok(Permitted::NeedsPermission)
        } else {
            Ok(Permitted::Done(result))
        }
    }

    async fn trash_targets(
        &self,
        refs: &[RemoteRef],
        trash: &str,
    ) -> Result<Vec<Target>, SyncError> {
        let fetched = self
            .fetch(refs.iter().cloned().map(crate::Want::from).collect())
            .await?;
        let wanted: HashSet<_> = refs.iter().map(|hit| hit.id.as_str()).collect();
        let still_here: HashSet<_> = fetched
            .metas
            .iter()
            .filter(|m| {
                wanted.contains(m.id.as_str()) && m.held.mailboxes.iter().any(|id| id == trash)
            })
            .map(|m| m.id.clone())
            .collect();
        let account_id = self.account_id;
        let metas = self
            .db
            .write(move |c| fetched.store_missing(c, account_id).map(|(metas, _)| metas))
            .await?;
        Ok(metas
            .into_iter()
            .filter(|m| still_here.contains(&m.id))
            .map(|m| Target {
                account_id,
                thread_id: m.thread_id,
                message_id: Some(m.id),
            })
            .collect())
    }
}
