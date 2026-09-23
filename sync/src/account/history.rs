//! Incremental sync: replays Gmail history since the stored cursor.

use std::collections::{BTreeSet, HashMap};

use mailrs_domain::{ChangeEvent, MessageMeta, system_label};
use mailrs_gmail::HistoryChange;
use mailrs_store::messages::Change;
use mailrs_store::{accounts, labels, messages};

use super::AccountSync;
use super::fetch::Want;
use super::labels::is_user_label;
use crate::{BackendError, MailBackend, SyncError};

impl AccountSync {
    /// Replays history since the stored cursor in one transaction, then moves
    /// the cursor. Bootstraps instead when there is no cursor yet or when
    /// Gmail no longer keeps history that old.
    pub async fn incremental(&self) -> Result<(), SyncError> {
        let account_id = self.account_id;
        let cursor = self
            .db
            .read(move |c| accounts::sync_cursor(c, account_id))
            .await?;
        let Some(start) = cursor.history_id else {
            return self.bootstrap().await;
        };

        let mut changes = Vec::new();
        let mut latest;
        let mut page_token: Option<String> = None;
        loop {
            let page = match self.services.mail.history(start, page_token.as_deref()).await {
                Ok(page) => page,
                Err(BackendError::StateLost) => {
                    tracing::info!(
                        account = account_id,
                        "history cursor expired; listing the mail again"
                    );
                    return self.rebootstrap().await;
                }
                Err(err) => return Err(err.into()),
            };
            changes.extend(page.changes);
            latest = page.history_id;
            match page.next_page_token {
                Some(token) => page_token = Some(token),
                None => break,
            }
        }
        if changes.is_empty() && latest == start {
            self.mark_caught_up();
            return Ok(());
        }

        let fetched = self.fetch_for_history(&changes).await?;
        let named_labels: BTreeSet<String> = changes
            .iter()
            .filter_map(|change| match change {
                HistoryChange::LabelsAdded { label_ids, .. } => Some(label_ids.clone()),
                _ => None,
            })
            .flatten()
            .chain(fetched.values().flat_map(|m| m.label_ids.clone()))
            .filter(|id| is_user_label(id))
            .collect();
        let generation = cursor.sync_gen;
        let (touched, new_mail, unknown_label) = self
            .db
            .write(move |c| {
                // Whether each message the history names was stored before
                // this replay, read once, since the change set applies the
                // whole batch in one call.
                let named: Vec<String> = changes
                    .iter()
                    .filter_map(|change| match change {
                        HistoryChange::MessageAdded { id, .. }
                        | HistoryChange::LabelsAdded { id, .. } => Some(id.clone()),
                        _ => None,
                    })
                    .collect();
                let stored = messages::existing_ids(c, account_id, &named)?;
                let mut batch = Vec::new();
                let mut new_mail = Vec::new();
                for change in &changes {
                    match change {
                        HistoryChange::MessageAdded { id, .. } => {
                            if let Some(meta) = fetched.get(id) {
                                if !stored.contains(id)
                                    && is_new_inbox_mail(meta)
                                    && !new_mail.contains(id)
                                {
                                    new_mail.push(id.clone());
                                }
                                batch.push(Change::Upsert {
                                    meta: Box::new(meta.clone()),
                                    generation,
                                });
                            }
                        }
                        HistoryChange::MessageDeleted { id, .. } => {
                            batch.push(Change::Delete {
                                message_id: id.clone(),
                            });
                        }
                        HistoryChange::LabelsAdded { id, label_ids, .. } if stored.contains(id) => {
                            batch.extend(label_ids.iter().map(|l| Change::label(id, l, true)));
                        }
                        HistoryChange::LabelsAdded { id, .. } => {
                            if let Some(meta) = fetched.get(id) {
                                batch.push(Change::Upsert {
                                    meta: Box::new(meta.clone()),
                                    generation,
                                });
                            }
                        }
                        HistoryChange::LabelsRemoved { id, label_ids, .. } => {
                            batch.extend(label_ids.iter().map(|l| Change::label(id, l, false)));
                        }
                    }
                }
                let touched = messages::apply(c, account_id, &batch)?.threads;
                accounts::set_history_id(c, account_id, latest)?;
                let known: BTreeSet<String> = labels::list_labels(c, account_id)?
                    .into_iter()
                    .map(|l| l.id)
                    .collect();
                let unknown = named_labels.iter().any(|id| !known.contains(id));
                Ok((touched, new_mail, unknown))
            })
            .await?;
        // A label the store has never seen was made elsewhere since the
        // labels were last listed, so the sidebar lacks it.
        if unknown_label {
            self.refresh_labels().await?;
        }
        self.mark_caught_up();
        self.emit_threads(touched);
        if !new_mail.is_empty() {
            self.emit(ChangeEvent::NewMail {
                account_id,
                message_ids: new_mail,
            });
        }
        Ok(())
    }

    /// Metadata for messages the history adds, plus messages that moved into
    /// INBOX from outside the window.
    async fn fetch_for_history(
        &self,
        changes: &[HistoryChange],
    ) -> Result<HashMap<String, MessageMeta>, SyncError> {
        let account_id = self.account_id;
        // History names each message's thread, so several new messages of
        // one conversation share a `threads.get`.
        let want = |id: &String, thread_id: &String| Want {
            id: id.clone(),
            thread_id: Some(thread_id.clone()),
        };
        let mut wanted: Vec<Want> = changes
            .iter()
            .filter_map(|change| match change {
                HistoryChange::MessageAdded { id, thread_id } => Some(want(id, thread_id)),
                _ => None,
            })
            .collect();
        let into_inbox: Vec<Want> = changes
            .iter()
            .filter_map(|change| match change {
                HistoryChange::LabelsAdded {
                    id,
                    thread_id,
                    label_ids,
                } if label_ids.iter().any(|l| l == system_label::INBOX) => {
                    Some(want(id, thread_id))
                }
                _ => None,
            })
            .collect();
        if !into_inbox.is_empty() {
            let candidates: Vec<String> = into_inbox.iter().map(|w| w.id.clone()).collect();
            let known = self
                .db
                .read(move |c| messages::existing_ids(c, account_id, &candidates))
                .await?;
            wanted.extend(into_inbox.into_iter().filter(|w| !known.contains(&w.id)));
        }
        Ok(self
            .fetch(wanted)
            .await?
            .metas
            .into_iter()
            .map(|m| (m.id.clone(), m))
            .collect())
    }
}

/// Unread mail that someone else sent to INBOX.
fn is_new_inbox_mail(meta: &MessageMeta) -> bool {
    meta.has_label(system_label::INBOX)
        && meta.is_unread()
        && !meta.has_label(system_label::SENT)
        && !meta.has_label(system_label::DRAFT)
}
