//! Triage: label changes applied to the store at once and to Gmail after.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use futures::StreamExt;
use mailrs_domain::{ChangeEvent, Target};
use mailrs_gmail::{BATCH_LIMIT, GmailError};
use mailrs_store::{messages, reminders, threads};

use super::{AccountSync, FETCH_CONCURRENCY};
use crate::{GmailApi, SyncError, TriageAction, backoff_delay, with_jitter};

/// Attempts per write before a triage gives up.
const WRITE_ATTEMPTS: u32 = 3;

/// Messages from which one `batchModify` beats a call each. Gmail charges
/// 50 units for the batch and 5 for every single `modify`, so ten is where
/// the batch starts paying.
const BATCH_FROM: usize = 10;

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
        self.triage_all(&[Target::thread(self.account_id, thread_id)], action)
            .await
    }

    /// Applies `action` to one message of a thread, as the list does when
    /// conversation grouping is off.
    pub async fn triage_message(
        &self,
        thread_id: &str,
        message_id: &str,
        action: &TriageAction,
    ) -> Result<(), SyncError> {
        let target = Target {
            message_id: Some(message_id.to_string()),
            ..Target::thread(self.account_id, thread_id)
        };
        self.triage_all(&[target], action).await
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

    /// Applies `action` to every target at once: one store transaction and,
    /// from [`BATCH_FROM`] messages up, one `batchModify` per thousand
    /// instead of a call each. Trashing 200 conversations costs 50 quota
    /// units this way against 2000 one at a time.
    ///
    /// The whole set succeeds or fails together, since Gmail answers the
    /// batch once. On a refusal the store goes back to its earlier labels,
    /// a `WriteFailed` event says so, and the caller reports the failure
    /// against each target it handed in.
    pub async fn triage_all(
        &self,
        targets: &[Target],
        action: &TriageAction,
    ) -> Result<(), SyncError> {
        if targets.is_empty() {
            return Ok(());
        }
        let account_id = self.account_id;
        let wanted = messages_wanted(targets);
        let threads: BTreeSet<String> = wanted.keys().cloned().collect();

        // Search results and the Trash or Spam lists show threads the store
        // may not hold yet. Fetch those first so there is something to change.
        self.ensure_threads(&threads).await?;

        let (add, remove) = action.label_delta();
        let snapshot: Vec<(String, Vec<String>)> = {
            let (wanted, add, remove) = (wanted.clone(), add.clone(), remove.clone());
            self.db
                .write(move |c| {
                    let mut before: Vec<(String, Vec<String>)> = Vec::new();
                    for (thread, only) in &wanted {
                        for message in messages::thread_messages(c, account_id, thread)? {
                            if only.as_ref().is_none_or(|ids| ids.contains(&message.id)) {
                                before.push((message.id, message.label_ids));
                            }
                        }
                    }
                    for (id, _) in &before {
                        messages::add_labels(c, account_id, id, &add)?;
                        messages::remove_labels(c, account_id, id, &remove)?;
                    }
                    for thread in wanted.keys() {
                        messages::refresh_thread(c, account_id, thread)?;
                    }
                    Ok(before)
                })
                .await?
        };
        self.emit_threads(threads.clone());

        let ids: Vec<String> = snapshot.iter().map(|(id, _)| id.clone()).collect();
        let writing = Writing {
            action,
            conversations: threads.len(),
        };
        let mut budget = Budget::new(self.retry_max, self.wait_ceiling);
        if let Err(err) = self
            .write_labels(&mut budget, &ids, &writing, &add, &remove)
            .await
        {
            let rolled_back = threads.clone();
            self.db
                .write(move |c| {
                    for (id, labels) in &snapshot {
                        if messages::thread_id_of(c, account_id, id)?.is_some() {
                            messages::set_labels(c, account_id, id, labels)?;
                        }
                    }
                    for thread in &rolled_back {
                        messages::refresh_thread(c, account_id, thread)?;
                    }
                    Ok(())
                })
                .await?;
            self.emit_threads(threads);
            self.emit(ChangeEvent::WriteFailed {
                account_id,
                message: write_failure(&writing, &err, budget.waited),
            });
            return Err(err.into());
        }
        Ok(())
    }

    /// Fetches the threads the store does not hold yet, several at a time.
    async fn ensure_threads(&self, threads: &BTreeSet<String>) -> Result<(), SyncError> {
        let account_id = self.account_id;
        let candidates: Vec<String> = threads.iter().cloned().collect();
        let missing: Vec<String> = self
            .db
            .read(move |c| {
                let mut missing = Vec::new();
                for thread in candidates {
                    if messages::thread_messages(c, account_id, &thread)?.is_empty() {
                        missing.push(thread);
                    }
                }
                Ok(missing)
            })
            .await?;
        let fetched: Vec<Result<(), SyncError>> = futures::stream::iter(missing)
            .map(|thread| async move { self.ensure_thread(&thread).await })
            .buffer_unordered(FETCH_CONCURRENCY)
            .collect()
            .await;
        fetched.into_iter().collect()
    }

    /// Sends the label change to Gmail: one batch per thousand messages, or
    /// a call each when there are too few for a batch to pay. A batch Gmail
    /// rejects outright falls back to single calls, so one id it dislikes
    /// does not sink the whole selection. The waiting belongs to the action,
    /// not to each message, so a rate-limited archive of twenty threads
    /// waits its minute over rather than twenty minutes.
    async fn write_labels(
        &self,
        budget: &mut Budget,
        ids: &[String],
        writing: &Writing<'_>,
        add: &[String],
        remove: &[String],
    ) -> Result<(), GmailError> {
        if ids.len() < BATCH_FROM {
            for id in ids {
                self.write_one(budget, id, writing, add, remove).await?;
            }
            return Ok(());
        }
        for chunk in ids.chunks(BATCH_LIMIT) {
            match self.write_batch(budget, chunk, writing, add, remove).await {
                Ok(()) => {}
                Err(err) if refuses_batch(&err) => {
                    tracing::warn!(
                        account = self.account_id,
                        error = %err,
                        "Gmail refused the batch; changing each message on its own"
                    );
                    for id in chunk {
                        self.write_one(budget, id, writing, add, remove).await?;
                    }
                }
                Err(err) => return Err(err),
            }
        }
        Ok(())
    }

    async fn write_batch(
        &self,
        budget: &mut Budget,
        ids: &[String],
        writing: &Writing<'_>,
        add: &[String],
        remove: &[String],
    ) -> Result<(), GmailError> {
        loop {
            match self.api.batch_modify(ids, add, remove).await {
                Err(err) => match budget.wait(&err) {
                    Some(delay) => self.hold_on(budget, delay, writing).await,
                    None => return Err(err),
                },
                done => return done,
            }
        }
    }

    async fn write_one(
        &self,
        budget: &mut Budget,
        message_id: &str,
        writing: &Writing<'_>,
        add: &[String],
        remove: &[String],
    ) -> Result<(), GmailError> {
        loop {
            let done = match writing.action {
                TriageAction::Trash => self.api.trash(message_id).await,
                TriageAction::Untrash => self.api.untrash(message_id).await,
                _ => self.api.modify_labels(message_id, add, remove).await,
            };
            match done {
                Err(err) => match budget.wait(&err) {
                    Some(delay) => self.hold_on(budget, delay, writing).await,
                    None => return Err(err),
                },
                done => return done,
            }
        }
    }

    /// Sits out `delay` before trying Gmail again. The first wait of an
    /// action says so, so the window shows the work is still going rather
    /// than nothing at all, and the whole wait counts as the user waiting
    /// on Gmail, so backfill stands aside for it.
    async fn hold_on(&self, budget: &Budget, delay: Duration, writing: &Writing<'_>) {
        if budget.waits == 1 {
            self.emit(ChangeEvent::WaitingOnGmail {
                account_id: self.account_id,
                message: still_waiting(writing),
            });
        }
        let _waiting = self.waiting();
        tokio::time::sleep(delay).await;
    }
}

/// One triage, as the messages about it need to name it.
struct Writing<'a> {
    action: &'a TriageAction,
    conversations: usize,
}

/// The waiting one mail action may do, however many messages it touches
/// and however many calls it takes. A rate limit is worth sitting out, so
/// it spends the clock rather than a count of tries; anything else
/// transient still gets [`WRITE_ATTEMPTS`] goes and no more.
struct Budget {
    /// Retries left for a transient failure that is not a rate limit.
    retries: u32,
    /// Waits taken so far, which sets the backoff curve.
    waits: u32,
    /// Time spent waiting on Gmail.
    waited: Duration,
    max: Duration,
    ceiling: Duration,
}

impl Budget {
    fn new(max: Duration, ceiling: Duration) -> Self {
        Budget {
            retries: WRITE_ATTEMPTS - 1,
            waits: 0,
            waited: Duration::ZERO,
            max,
            ceiling,
        }
    }

    /// How long to wait before trying again, or `None` when `err` is not
    /// worth retrying, the action has used up its attempts, or waiting
    /// again would take it past the ceiling.
    fn wait(&mut self, err: &GmailError) -> Option<Duration> {
        if !err.is_transient() {
            return None;
        }
        let limited = matches!(err, GmailError::RateLimited { .. });
        if !limited && self.retries == 0 {
            return None;
        }
        let delay = retry_delay(err, self.waits, self.max);
        if self.waited + delay > self.ceiling {
            return None;
        }
        self.waited += delay;
        self.waits += 1;
        if !limited {
            self.retries -= 1;
        }
        Some(delay)
    }
}

/// Which messages of each thread the targets name. `None` means the whole
/// thread, which wins over any single message named alongside it.
fn messages_wanted(targets: &[Target]) -> BTreeMap<String, Option<BTreeSet<String>>> {
    let mut wanted: BTreeMap<String, Option<BTreeSet<String>>> = BTreeMap::new();
    for target in targets {
        let named = wanted
            .entry(target.thread_id.clone())
            .or_insert_with(|| Some(BTreeSet::new()));
        match (&target.message_id, named) {
            (None, whole) => *whole = None,
            (Some(id), Some(ids)) => {
                ids.insert(id.clone());
            }
            (Some(_), None) => {}
        }
    }
    wanted
}

/// How long to wait before retry number `attempt`. Gmail's own `Retry-After`
/// wins where it sent one. Either way the wait moves by up to a fifth, so
/// six accounts told to come back in two seconds do not all come back on
/// the same tick and get limited again.
fn retry_delay(err: &GmailError, attempt: u32, max: Duration) -> Duration {
    match err {
        GmailError::RateLimited {
            retry_after: Some(after),
        } => with_jitter(*after, rand::random_range(-1.0..=1.0)),
        _ => backoff_delay(attempt, max, rand::random_range(-1.0..=1.0)),
    }
}

/// Whether Gmail turned the batch down for the batch's own sake, which a
/// call per message may still get through.
fn refuses_batch(err: &GmailError) -> bool {
    matches!(
        err,
        GmailError::NotFound | GmailError::Http { status: 400, .. }
    )
}

/// What the window says while an action sits out a rate limit.
fn still_waiting(writing: &Writing<'_>) -> String {
    format!(
        "Gmail is busy. Still working on {}.",
        conversations(writing.conversations)
    )
}

/// What the toast says when a write did not land. A rate limit that
/// outlasted the waiting names how long the action held on and how much
/// of it did not go through, so nobody has to guess what to redo.
fn write_failure(writing: &Writing<'_>, err: &GmailError, waited: Duration) -> String {
    let what = writing.action.describe().to_lowercase();
    let many = conversations(writing.conversations);
    match err {
        GmailError::RateLimited { .. } if waited.is_zero() => {
            format!(
                "Gmail is busy, so {what} did not go through for {many}. Try again in a moment."
            )
        }
        GmailError::RateLimited { .. } => format!(
            "Gmail stayed busy for {}, so {what} did not go through for {many}.",
            roughly(waited)
        ),
        _ => format!("{} failed: {err}", writing.action.describe()),
    }
}

fn conversations(count: usize) -> String {
    match count {
        1 => "1 conversation".to_string(),
        many => format!("{many} conversations"),
    }
}

/// A wait in words, for a message a person reads.
fn roughly(waited: Duration) -> String {
    match waited.as_secs() {
        0..2 => "a moment".to_string(),
        seconds @ 2..45 => format!("{seconds} seconds"),
        _ => "a minute".to_string(),
    }
}
