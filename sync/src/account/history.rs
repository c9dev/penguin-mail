//! Incremental sync: replays Gmail history since the stored cursor.

use std::collections::{BTreeSet, HashMap};

use mailrs_domain::{ChangeEvent, MessageMeta};
use mailrs_gmail::{GmailError, HistoryChange};
use mailrs_store::{accounts, messages};

use super::AccountSync;
use crate::{GmailApi, SyncError};

impl<G: GmailApi> AccountSync<G> {
    /// Replays history since the stored cursor in one transaction, then moves
    /// the cursor. Bootstraps instead when there is no cursor yet or when
    /// Gmail no longer keeps history that old.
    pub async fn incremental(&self) -> Result<(), SyncError> {
        let account_id = self.account_id;
        let cursor = self.db.read(move |c| accounts::sync_cursor(c, account_id)).await?;
        let Some(start) = cursor.history_id else {
            return self.bootstrap().await;
        };

        let mut changes = Vec::new();
        let mut latest = start;
        let mut page_token: Option<String> = None;
        loop {
            let page = match self.api.history(start, page_token.as_deref()).await {
                Ok(page) => page,
                Err(GmailError::NotFound) => {
                    tracing::info!(account = account_id, "history cursor expired; bootstrapping again");
                    return self.bootstrap().await;
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
            return Ok(());
        }

        let fetched = self.fetch_for_history(&changes).await?;
        let generation = cursor.sync_gen;
        let (touched, new_mail) = self
            .db
            .write(move |c| {
                let mut touched = BTreeSet::new();
                let mut new_mail = Vec::new();
                for change in &changes {
                    match change {
                        HistoryChange::MessageAdded { id, .. } => {
                            if let Some(meta) = fetched.get(id) {
                                let existed = messages::thread_id_of(c, account_id, id)?.is_some();
                                messages::upsert_message(c, meta, generation)?;
                                touched.insert(meta.thread_id.clone());
                                if !existed && is_new_inbox_mail(meta) {
                                    new_mail.push(id.clone());
                                }
                            }
                        }
                        HistoryChange::MessageDeleted { id, .. } => {
                            touched.extend(messages::delete_message(c, account_id, id)?);
                        }
                        HistoryChange::LabelsAdded { id, label_ids, .. } => {
                            match messages::add_labels(c, account_id, id, label_ids)? {
                                Some(thread_id) => {
                                    touched.insert(thread_id);
                                }
                                None => {
                                    if let Some(meta) = fetched.get(id) {
                                        messages::upsert_message(c, meta, generation)?;
                                        touched.insert(meta.thread_id.clone());
                                    }
                                }
                            }
                        }
                        HistoryChange::LabelsRemoved { id, label_ids, .. } => {
                            touched.extend(messages::remove_labels(c, account_id, id, label_ids)?);
                        }
                    }
                }
                for thread_id in &touched {
                    messages::refresh_thread(c, account_id, thread_id)?;
                }
                accounts::set_history_id(c, account_id, latest)?;
                Ok((touched, new_mail))
            })
            .await?;
        self.emit_threads(touched);
        if !new_mail.is_empty() {
            self.emit(ChangeEvent::NewMail { account_id, message_ids: new_mail });
        }
        Ok(())
    }

    /// Metadata for messages the history adds, plus messages that moved into
    /// INBOX from outside the window.
    async fn fetch_for_history(&self, changes: &[HistoryChange]) -> Result<HashMap<String, MessageMeta>, SyncError> {
        let account_id = self.account_id;
        let mut wanted: Vec<String> = changes
            .iter()
            .filter_map(|change| match change {
                HistoryChange::MessageAdded { id, .. } => Some(id.clone()),
                _ => None,
            })
            .collect();
        let into_inbox: Vec<String> = changes
            .iter()
            .filter_map(|change| match change {
                HistoryChange::LabelsAdded { id, label_ids, .. } if label_ids.iter().any(|l| l == "INBOX") => {
                    Some(id.clone())
                }
                _ => None,
            })
            .collect();
        if !into_inbox.is_empty() {
            let candidates = into_inbox.clone();
            let known = self.db.read(move |c| messages::existing_ids(c, account_id, &candidates)).await?;
            wanted.extend(into_inbox.into_iter().filter(|id| !known.contains(id)));
        }
        wanted.sort();
        wanted.dedup();
        Ok(self.fetch_metadata(&wanted).await?.into_iter().map(|m| (m.id.clone(), m)).collect())
    }
}

/// Unread mail that someone else sent to INBOX.
fn is_new_inbox_mail(meta: &MessageMeta) -> bool {
    meta.has_label("INBOX") && meta.is_unread() && !meta.has_label("SENT") && !meta.has_label("DRAFT")
}
