//! Bootstrap, backfill, pruning, and the inbox check: the processes that
//! load the window, trim it, and keep it in step with Gmail.

use std::collections::{BTreeSet, HashMap, HashSet};

use mailrs_domain::{AccountState, ChangeEvent, EpochMillis, MessageMeta, system_label};
use mailrs_gmail::{GmailError, MessageRef};
use mailrs_store::messages::Change;
use mailrs_store::{accounts, labels, messages, window};

use super::AccountSync;
use super::fetch::{Want, store_fetched};
use super::labels::domain_labels;
use crate::{BackendError, ID_PAGE_SIZE, LIST_PAGE_SIZE, MailBackend, SyncError};

const DAY_MILLIS: i64 = 24 * 60 * 60 * 1000;

/// Gmail search for every message in the inbox, whatever its age.
const INBOX_QUERY: &str = "in:inbox";

impl AccountSync {
    /// Gmail search for the window: recent mail plus everything in INBOX.
    pub fn window_query(&self) -> String {
        format!("{{newer_than:{}d in:inbox}}", self.window_days)
    }

    /// Starts a new sync generation. Records the history cursor before
    /// listing anything, replaces the labels, and loads the first window
    /// page. `backfill_step` loads the rest.
    pub async fn bootstrap(&self) -> Result<(), SyncError> {
        self.set_state(AccountState::Bootstrapping).await?;
        let profile = self.services.mail.profile().await?;
        let remote_labels = self.services.mail.labels().await?;
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

    /// Lists the window again once Gmail no longer keeps history as old as
    /// the cursor, and fetches only what changed in the gap. A full
    /// bootstrap would fetch every message again: 300 messages cost over
    /// 900 units that way, even with nothing changed.
    ///
    /// It lists the window's ids at 500 a page, then the ids carrying each
    /// label, and compares that with the labels the store holds. A message
    /// the store lacks, or whose labels differ, is fetched; the rest are
    /// written again under the new generation from the store's own copy,
    /// so the sweep that follows keeps them and drops only what left the
    /// window. The history cursor is taken first, as in a bootstrap, so a
    /// change made while this runs arrives through the next replay.
    ///
    /// A store that never finished loading the window bootstraps instead,
    /// since there is too little in it to compare against.
    pub async fn rebootstrap(&self) -> Result<(), SyncError> {
        let account_id = self.account_id;
        let cursor = self
            .db
            .read(move |c| accounts::sync_cursor(c, account_id))
            .await?;
        if !cursor.backfill_done {
            return self.bootstrap().await;
        }
        self.set_state(AccountState::Bootstrapping).await?;
        let profile = self.services.mail.profile().await?;
        let remote_labels = self.services.mail.labels().await?;
        let labels = domain_labels(account_id, &remote_labels);
        let label_ids: Vec<String> = labels.iter().map(|l| l.id.clone()).collect();
        let history_id = profile.history_id;
        let generation = self
            .db
            .write(move |c| {
                labels::replace_labels(c, account_id, &labels)?;
                accounts::start_generation(c, account_id, history_id)
            })
            .await?;
        self.emit(ChangeEvent::LabelsChanged { account_id });

        let query = self.window_query();
        let listed = self.list_ids(None, &query).await?;
        let mut members: HashMap<String, HashSet<String>> = HashMap::new();
        for label in &label_ids {
            let carrying = self.list_ids(Some(label), &query).await?;
            members.insert(label.clone(), carrying.into_iter().map(|m| m.id).collect());
        }
        let ids: Vec<String> = listed.iter().map(|m| m.id.clone()).collect();
        let stored: HashMap<String, MessageMeta> = self
            .db
            .read(move |c| messages::by_ids(c, account_id, &ids))
            .await?
            .into_iter()
            .map(|m| (m.id.clone(), m))
            .collect();
        let mut wants = Vec::new();
        let mut unchanged = Vec::new();
        for message in listed {
            let Some(held) = stored.get(&message.id) else {
                wants.push(Want::from(message));
                continue;
            };
            // Only the labels just listed can be compared; a label Gmail
            // does not list stays as the store has it.
            let before: BTreeSet<&str> = held
                .label_ids
                .iter()
                .map(String::as_str)
                .filter(|l| members.contains_key(*l))
                .collect();
            let now: BTreeSet<&str> = members
                .iter()
                .filter(|(_, ids)| ids.contains(&message.id))
                .map(|(label, _)| label.as_str())
                .collect();
            if before == now {
                unchanged.push(held.clone());
            } else {
                wants.push(Want::from(message));
            }
        }
        tracing::info!(
            account = account_id,
            fetched = wants.len(),
            kept = unchanged.len(),
            "listed the mail again after history expired"
        );
        let metas = self.fetch(wants).await?.metas;
        let touched = self
            .db
            .write(move |c| {
                let mut touched = store_fetched(c, account_id, generation, &metas, &[])?;
                let kept: Vec<Change> = unchanged
                    .iter()
                    .map(|held| Change::Keep {
                        message_id: held.id.clone(),
                        generation,
                    })
                    .collect();
                messages::apply(c, account_id, &kept)?;
                accounts::set_backfill(c, account_id, None, true)?;
                touched.extend(window::sweep_stale(c, account_id, generation)?);
                Ok(touched)
            })
            .await?;
        self.emit_threads(touched);
        self.set_state(AccountState::Ok).await
    }

    /// Every id `query` lists, narrowed to one label when `label` names
    /// one, at 500 a page.
    async fn list_ids(
        &self,
        label: Option<&str>,
        query: &str,
    ) -> Result<Vec<MessageRef>, SyncError> {
        let mut found = Vec::new();
        let mut page_token: Option<String> = None;
        loop {
            let token = page_token.as_deref();
            let page = match label {
                Some(label) => {
                    self.services.mail
                        .list_labelled(label, query, token, ID_PAGE_SIZE)
                        .await?
                }
                None => self.services.mail.list_messages(query, token, ID_PAGE_SIZE).await?,
            };
            found.extend(page.messages);
            match page.next_page_token {
                Some(token) => page_token = Some(token),
                None => return Ok(found),
            }
        }
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
            Err(SyncError::Backend(BackendError::Gmail(GmailError::Http { status: 400, .. })))
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
        let remote: HashSet<String> = self
            .list_ids(None, INBOX_QUERY)
            .await?
            .into_iter()
            .map(|m| m.id)
            .collect();
        // Every message that differs is one the store holds, so the store
        // names its thread and the fetch can share a call per thread.
        let mut differ: Vec<Want> = self
            .db
            .read(move |c| {
                let local = messages::labelled(c, account_id, system_label::INBOX)?;
                let only_remote: Vec<String> = remote.difference(&local).cloned().collect();
                let stored = messages::existing_ids(c, account_id, &only_remote)?;
                let mut differ = Vec::new();
                for id in local.difference(&remote).cloned().chain(stored) {
                    let thread_id = messages::thread_id_of(c, account_id, &id)?;
                    differ.push(Want { id, thread_id });
                }
                Ok(differ)
            })
            .await?;
        if differ.is_empty() {
            return Ok(());
        }
        differ.sort_by(|a, b| a.id.cmp(&b.id));
        tracing::info!(
            account = account_id,
            messages = ?differ.iter().map(|w| &w.id).collect::<Vec<_>>(),
            "the stored inbox differs from Gmail's; fetching those messages again"
        );
        let fetched = self.fetch(differ).await?;
        let generation = cursor.sync_gen;
        let touched = self
            .db
            .write(move |c| store_fetched(c, account_id, generation, &fetched.metas, &fetched.gone))
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
            .services
            .mail
            .list_messages(&self.window_query(), page_token.as_deref(), LIST_PAGE_SIZE)
            .await?;
        // A listed message deleted since is left out; the sweep and
        // history take care of what the store had of it.
        let wants = page.messages.into_iter().map(Want::from).collect();
        let metas = self.fetch(wants).await?.metas;
        let next = page.next_page_token;
        let account_id = self.account_id;
        let stored_next = next.clone();
        let touched = self
            .db
            .write(move |c| {
                let mut touched = store_fetched(c, account_id, generation, &metas, &[])?;
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
