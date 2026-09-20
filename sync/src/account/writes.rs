//! Triage: label changes applied to the store at once and to Gmail after.

use std::collections::BTreeSet;

use mailrs_domain::ChangeEvent;
use mailrs_gmail::GmailError;
use mailrs_store::{messages, reminders, threads};

use super::AccountSync;
use crate::{GmailApi, SyncError, TriageAction, backoff_delay};

/// Attempts per message before a triage write gives up.
const WRITE_ATTEMPTS: u32 = 3;

impl<G: GmailApi> AccountSync<G> {
    /// Applies `action` to every message of a thread. The store changes first
    /// so the UI updates at once; Gmail follows. If Gmail refuses, the store
    /// goes back to its earlier labels and a `WriteFailed` event says so.
    /// The next history replay reconciles any messages Gmail did change.
    pub async fn triage_thread(
        &self,
        thread_id: &str,
        action: &TriageAction,
    ) -> Result<(), SyncError> {
        self.triage(thread_id, None, action).await
    }

    /// Applies `action` to one message of a thread, as the list does when
    /// conversation grouping is off.
    pub async fn triage_message(
        &self,
        thread_id: &str,
        message_id: &str,
        action: &TriageAction,
    ) -> Result<(), SyncError> {
        self.triage(thread_id, Some(message_id), action).await
    }

    /// Erases a thread, or one message of it, from Gmail and then from the
    /// store. Gmail goes first because nothing can undo this: when it
    /// refuses, such as when the account has not granted the delete
    /// permission, the store keeps every row it had.
    pub async fn erase(&self, thread_id: &str, only: Option<&str>) -> Result<(), SyncError> {
        let account_id = self.account_id;
        let ids = self.message_ids(thread_id, only).await?;
        if ids.is_empty() {
            // The Trash list comes from a Gmail search, so the store may
            // not hold the thread yet.
            self.ensure_thread(thread_id).await?;
        }
        let ids = match ids.is_empty() {
            true => self.message_ids(thread_id, only).await?,
            false => ids,
        };
        if ids.is_empty() {
            return Err(SyncError::Gmail(GmailError::NotFound));
        }
        self.api.delete_messages(&ids).await?;
        let thread = thread_id.to_string();
        self.db
            .write(move |c| {
                for id in &ids {
                    messages::delete_message(c, account_id, id)?;
                }
                messages::refresh_thread(c, account_id, &thread)?;
                // Nothing is left to remind anybody about.
                if threads::get_thread(c, account_id, &thread)?.is_none() {
                    reminders::remove(c, account_id, &thread)?;
                }
                Ok(())
            })
            .await?;
        self.emit_threads(BTreeSet::from([thread_id.to_string()]));
        Ok(())
    }

    /// The ids of the messages a target names, in the store.
    async fn message_ids(
        &self,
        thread_id: &str,
        only: Option<&str>,
    ) -> Result<Vec<String>, SyncError> {
        let (account_id, thread) = (self.account_id, thread_id.to_string());
        let only = only.map(str::to_string);
        Ok(self
            .db
            .read(move |c| {
                Ok(messages::thread_messages(c, account_id, &thread)?
                    .into_iter()
                    .filter(|m| only.as_ref().is_none_or(|id| &m.id == id))
                    .map(|m| m.id)
                    .collect())
            })
            .await?)
    }

    async fn triage(
        &self,
        thread_id: &str,
        only: Option<&str>,
        action: &TriageAction,
    ) -> Result<(), SyncError> {
        let account_id = self.account_id;
        // Search results and the Trash or Spam lists show threads the store
        // may not hold yet. Fetch those first so there is something to change.
        let thread = thread_id.to_string();
        let stored = self
            .db
            .read(move |c| messages::thread_messages(c, account_id, &thread))
            .await?;
        if stored.is_empty() {
            self.ensure_thread(thread_id).await?;
        }
        let (add, remove) = action.label_delta();
        let snapshot: Vec<(String, Vec<String>)> = {
            let (thread, add, remove) = (thread_id.to_string(), add.clone(), remove.clone());
            let only = only.map(str::to_string);
            self.db
                .write(move |c| {
                    let before: Vec<(String, Vec<String>)> =
                        messages::thread_messages(c, account_id, &thread)?
                            .into_iter()
                            .filter(|m| only.as_ref().is_none_or(|id| &m.id == id))
                            .map(|m| (m.id, m.label_ids))
                            .collect();
                    for (id, _) in &before {
                        messages::add_labels(c, account_id, id, &add)?;
                        messages::remove_labels(c, account_id, id, &remove)?;
                    }
                    messages::refresh_thread(c, account_id, &thread)?;
                    Ok(before)
                })
                .await?
        };
        self.emit_threads(BTreeSet::from([thread_id.to_string()]));

        let ids: Vec<String> = snapshot.iter().map(|(id, _)| id.clone()).collect();
        for id in &ids {
            if let Err(err) = self.remote_write(id, action, &add, &remove).await {
                let thread = thread_id.to_string();
                self.db
                    .write(move |c| {
                        for (id, labels) in &snapshot {
                            if messages::thread_id_of(c, account_id, id)?.is_some() {
                                messages::set_labels(c, account_id, id, labels)?;
                            }
                        }
                        messages::refresh_thread(c, account_id, &thread)
                    })
                    .await?;
                self.emit_threads(BTreeSet::from([thread_id.to_string()]));
                self.emit(ChangeEvent::WriteFailed {
                    account_id,
                    message: format!("{} failed: {err}", action.describe()),
                });
                return Err(err.into());
            }
        }
        Ok(())
    }

    async fn remote_write(
        &self,
        message_id: &str,
        action: &TriageAction,
        add: &[String],
        remove: &[String],
    ) -> Result<(), GmailError> {
        let mut attempt = 0;
        loop {
            let result = match action {
                TriageAction::Trash => self.api.trash(message_id).await,
                TriageAction::Untrash => self.api.untrash(message_id).await,
                _ => self.api.modify_labels(message_id, add, remove).await,
            };
            match result {
                Err(err) if err.is_transient() && attempt + 1 < WRITE_ATTEMPTS => {
                    let delay = match &err {
                        GmailError::RateLimited {
                            retry_after: Some(after),
                        } => *after,
                        _ => backoff_delay(attempt, self.retry_max, rand::random_range(-1.0..=1.0)),
                    };
                    attempt += 1;
                    tokio::time::sleep(delay).await;
                }
                other => return other,
            }
        }
    }
}
