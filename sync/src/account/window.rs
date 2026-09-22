//! Bootstrap, backfill, pruning, and the inbox check: the processes that
//! load the window, trim it, and keep it in step with Gmail.

use std::collections::{BTreeSet, HashSet};

use mailrs_domain::{
    AccountId, AccountState, ChangeEvent, EpochMillis, Label, LabelKind, system_label,
};
use mailrs_gmail::{GmailError, RemoteLabel};
use mailrs_store::{accounts, labels, messages, window};

use super::AccountSync;
use crate::{GmailApi, SyncError};

const DAY_MILLIS: i64 = 24 * 60 * 60 * 1000;

/// Gmail search for every message in the inbox, whatever its age.
const INBOX_QUERY: &str = "in:inbox";

impl<G: GmailApi> AccountSync<G> {
    /// Gmail search for the window: recent mail plus everything in INBOX.
    pub fn window_query(&self) -> String {
        format!("{{newer_than:{}d in:inbox}}", self.window_days)
    }

    /// Starts a new sync generation. Records the history cursor before
    /// listing anything, replaces the labels, and loads the first window
    /// page. `backfill_step` loads the rest.
    pub async fn bootstrap(&self) -> Result<(), SyncError> {
        self.set_state(AccountState::Bootstrapping).await?;
        let profile = self.api.profile().await?;
        let remote_labels = self.api.labels().await?;
        let account_id = self.account_id;
        let history_id = profile.history_id;
        let labels = domain_labels(account_id, &remote_labels);
        let generation = self
            .db
            .write(move |c| {
                labels::replace_labels(c, account_id, &labels)?;
                accounts::start_generation(c, account_id, history_id)
            })
            .await?;
        self.emit(ChangeEvent::LabelsChanged { account_id });
        self.load_window_page(None, generation).await?;
        self.set_state(AccountState::Ok).await
    }

    /// Loads the next window page. Returns true while pages remain.
    pub async fn backfill_step(&self) -> Result<bool, SyncError> {
        let account_id = self.account_id;
        let cursor = self
            .db
            .read(move |c| accounts::sync_cursor(c, account_id))
            .await?;
        if cursor.backfill_done || cursor.history_id.is_none() {
            return Ok(false);
        }
        match self
            .load_window_page(cursor.backfill_cursor.clone(), cursor.sync_gen)
            .await
        {
            Ok(next) => Ok(next.is_some()),
            Err(SyncError::Gmail(GmailError::Http { status: 400, .. }))
                if cursor.backfill_cursor.is_some() =>
            {
                tracing::warn!(
                    account = account_id,
                    "Gmail rejected the saved page token; listing the window again"
                );
                self.db
                    .write(move |c| accounts::set_backfill(c, account_id, None, false))
                    .await?;
                Ok(true)
            }
            Err(err) => Err(err),
        }
    }

    /// Deletes threads that have left the window.
    pub async fn prune(&self, now: EpochMillis) -> Result<(), SyncError> {
        let account_id = self.account_id;
        let cutoff = now - self.window_days * DAY_MILLIS;
        let pruned = self
            .db
            .write(move |c| window::prune_window(c, account_id, cutoff))
            .await?;
        self.emit_threads(pruned.into_iter().collect());
        Ok(())
    }

    /// Makes the stored inbox match Gmail's. History replay applies each
    /// change once, so a message whose labels were written from an older
    /// copy after the replay that carried a change stays wrong for good:
    /// no later history mentions it. This lists Gmail's inbox, compares it
    /// with the messages the store has in INBOX, and fetches the stored
    /// ones that differ. It waits until the window has loaded, when the two
    /// should agree.
    ///
    /// A message Gmail has in the inbox and the store lacks altogether is
    /// most likely one that arrived during the listing. It is left to the
    /// next replay, which stores it and announces it as new mail.
    ///
    /// The engine runs it between replays, so the cursor cannot move while
    /// it works: anything Gmail changes after the fetch has a history id
    /// past the cursor and reaches the store through the next replay.
    pub async fn reconcile_inbox(&self) -> Result<(), SyncError> {
        let account_id = self.account_id;
        let cursor = self
            .db
            .read(move |c| accounts::sync_cursor(c, account_id))
            .await?;
        if !cursor.backfill_done || cursor.history_id.is_none() {
            return Ok(());
        }
        let mut remote = HashSet::new();
        let mut page_token: Option<String> = None;
        loop {
            let page = self
                .api
                .list_messages(INBOX_QUERY, page_token.as_deref())
                .await?;
            remote.extend(page.messages.into_iter().map(|m| m.id));
            match page.next_page_token {
                Some(token) => page_token = Some(token),
                None => break,
            }
        }
        let mut differ: Vec<String> = self
            .db
            .read(move |c| {
                let local = messages::labelled(c, account_id, system_label::INBOX)?;
                let only_remote: Vec<String> = remote.difference(&local).cloned().collect();
                let stored = messages::existing_ids(c, account_id, &only_remote)?;
                Ok(local.difference(&remote).cloned().chain(stored).collect())
            })
            .await?;
        if differ.is_empty() {
            return Ok(());
        }
        differ.sort();
        tracing::info!(
            account = account_id,
            messages = ?differ,
            "the stored inbox differs from Gmail's; fetching those messages again"
        );
        let metas = self.fetch_metadata(&differ).await?;
        let generation = cursor.sync_gen;
        let touched = self
            .db
            .write(move |c| {
                let mut touched = BTreeSet::new();
                for meta in &metas {
                    messages::upsert_message(c, meta, generation)?;
                    touched.insert(meta.thread_id.clone());
                }
                // Gmail answered "not found" for the rest.
                let fetched: HashSet<&str> = metas.iter().map(|m| m.id.as_str()).collect();
                for id in differ.iter().filter(|id| !fetched.contains(id.as_str())) {
                    touched.extend(messages::delete_message(c, account_id, id)?);
                }
                for thread_id in &touched {
                    messages::refresh_thread(c, account_id, thread_id)?;
                }
                Ok(touched)
            })
            .await?;
        self.emit_threads(touched);
        Ok(())
    }

    /// Stores one page of the window listing and saves the next page token.
    /// After the last page, deletes messages from earlier generations.
    async fn load_window_page(
        &self,
        page_token: Option<String>,
        generation: i64,
    ) -> Result<Option<String>, SyncError> {
        let page = self
            .api
            .list_messages(&self.window_query(), page_token.as_deref())
            .await?;
        let ids: Vec<String> = page.messages.into_iter().map(|m| m.id).collect();
        let metas = self.fetch_metadata(&ids).await?;
        let next = page.next_page_token;
        let account_id = self.account_id;
        let stored_next = next.clone();
        let touched = self
            .db
            .write(move |c| {
                let mut touched = BTreeSet::new();
                for meta in &metas {
                    messages::upsert_message(c, meta, generation)?;
                    touched.insert(meta.thread_id.clone());
                }
                for thread_id in &touched {
                    messages::refresh_thread(c, account_id, thread_id)?;
                }
                let done = stored_next.is_none();
                accounts::set_backfill(c, account_id, stored_next.as_deref(), done)?;
                if done {
                    touched.extend(window::sweep_stale(c, account_id, generation)?);
                }
                Ok(touched)
            })
            .await?;
        self.emit_threads(touched);
        Ok(next)
    }
}

fn domain_labels(account_id: AccountId, remote: &[RemoteLabel]) -> Vec<Label> {
    remote
        .iter()
        .map(|l| Label {
            account_id,
            id: l.id.clone(),
            name: l.name.clone(),
            kind: if l.kind.as_deref() == Some("system") {
                LabelKind::System
            } else {
                LabelKind::User
            },
            color: l.color.as_ref().map(|c| c.background_color.clone()),
        })
        .collect()
}
