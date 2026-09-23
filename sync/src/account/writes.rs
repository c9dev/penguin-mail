//! Triage: label changes applied to the store at once and to Gmail after.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use futures::StreamExt;
use mailrs_domain::translate::{fill, fill_plural, gettext};
use mailrs_domain::{ChangeEvent, Target};
use mailrs_gmail::{BATCH_LIMIT, GmailError};
use mailrs_store::messages::Change;
use mailrs_store::{messages, reminders, threads};

use super::{AccountSync, FETCH_CONCURRENCY};
use crate::{BackendError, MailBackend, SyncError, TriageAction, backoff_delay, with_jitter};

/// Attempts per write before a triage gives up.
const WRITE_ATTEMPTS: u32 = 3;

/// Messages from which one `batchModify` beats a call each. Gmail charges
/// 50 units for the batch and 5 for every single `modify`, so ten is where
/// the batch starts paying.
const BATCH_FROM: usize = 10;

impl AccountSync {
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
            .map(drop)
    }

    /// Erases the targets from Gmail and then from the store, and answers
    /// for each target in the order given. Gmail goes first because nothing
    /// can undo this: when it refuses, such as when the account has not
    /// granted the delete permission, the store keeps every row it had.
    ///
    /// The messages come from the store, or for a thread it lacks, from the
    /// Trash listing that showed the row; only a thread neither knows costs
    /// a `threads.get`. They then go out in one `batchDelete` per thousand,
    /// so erasing 200 listed conversations is one call of 50 units rather
    /// than 400 calls and 12,000 units. A target whose messages sat in a
    /// batch Gmail refused fails with that batch's error. An error in
    /// working out what to erase fails the whole call before Gmail is
    /// asked anything.
    pub async fn erase_all(
        &self,
        targets: &[Target],
    ) -> Result<Vec<Result<(), SyncError>>, SyncError> {
        let ids = self.erasable(targets).await?;
        let mut all: Vec<String> = ids.iter().flatten().cloned().collect();
        all.sort();
        all.dedup();
        let mut refused: BTreeMap<String, BackendError> = BTreeMap::new();
        for chunk in all.chunks(BATCH_LIMIT) {
            if let Err(err) = self.services.mail.delete_messages(chunk).await {
                // Gmail refuses a missing permission before it erases
                // anything, so no later batch would fare better.
                let stop = matches!(err, BackendError::NeedsPermission);
                refused.extend(chunk.iter().map(|id| (id.clone(), err.clone())));
                if stop {
                    for id in &all {
                        refused
                            .entry(id.clone())
                            .or_insert(BackendError::NeedsPermission);
                    }
                    break;
                }
            }
        }
        let erased: Vec<String> = all
            .iter()
            .filter(|id| !refused.contains_key(*id))
            .cloned()
            .collect();
        if !erased.is_empty() {
            let touched: BTreeSet<String> = targets
                .iter()
                .zip(&ids)
                .filter(|(_, ids)| ids.iter().any(|id| !refused.contains_key(id)))
                .map(|(t, _)| t.thread_id.clone())
                .collect();
            let (account_id, threads) = (self.account_id, touched.clone());
            let stored = self
                .db
                .write(move |c| {
                    let deletions: Vec<Change> = erased
                        .iter()
                        .map(|id| Change::Delete {
                            message_id: id.clone(),
                        })
                        .collect();
                    messages::apply(c, account_id, &deletions)?;
                    for thread in &threads {
                        // Nothing is left to remind anybody about.
                        if threads::get_thread(c, account_id, thread)?.is_none() {
                            reminders::remove(c, account_id, thread)?;
                        }
                    }
                    Ok(())
                })
                .await;
            self.forget_listed(&touched);
            if let Err(err) = stored {
                // Gmail has erased them, and the next history replay
                // takes them out of the store.
                tracing::warn!(error = %err, "erased at Gmail, but the store kept its copy");
            }
            self.emit_threads(touched);
        }
        Ok(ids
            .iter()
            .map(|ids| match ids.iter().find_map(|id| refused.get(id)) {
                _ if ids.is_empty() => Err(BackendError::NotFound.into()),
                Some(err) => Err(err.clone().into()),
                None => Ok(()),
            })
            .collect())
    }

    /// The ids each target names: from the store, else from what a search
    /// listed, else from Gmail, one `threads.get` per thread nothing here
    /// knows. An empty list means Gmail no longer has the thread.
    async fn erasable(&self, targets: &[Target]) -> Result<Vec<Vec<String>>, SyncError> {
        let wanted: Vec<(String, Option<String>)> = targets
            .iter()
            .map(|t| (t.thread_id.clone(), t.message_id.clone()))
            .collect();
        let mut ids = self.stored_ids(wanted.clone()).await?;
        let mut unknown: BTreeSet<String> = BTreeSet::new();
        for ((thread, only), found) in wanted.iter().zip(ids.iter_mut()) {
            if !found.is_empty() {
                continue;
            }
            match self.listed_ids(thread) {
                Some(listed) => found.extend(
                    listed
                        .into_iter()
                        .filter(|id| only.as_ref().is_none_or(|o| o == id)),
                ),
                None => {
                    unknown.insert(thread.clone());
                }
            }
        }
        if unknown.is_empty() {
            return Ok(ids);
        }
        // Neither the store nor a listing knows these threads, so fetch
        // them whole into the store and read them from there.
        self.ensure_threads(&unknown).await?;
        let again = self.stored_ids(wanted.clone()).await?;
        for (((thread, _), found), fetched) in wanted.iter().zip(ids.iter_mut()).zip(again) {
            if unknown.contains(thread) {
                *found = fetched;
            }
        }
        Ok(ids)
    }

    /// The stored ids of each thread, or of the one message named with it.
    async fn stored_ids(
        &self,
        wanted: Vec<(String, Option<String>)>,
    ) -> Result<Vec<Vec<String>>, SyncError> {
        let account_id = self.account_id;
        Ok(self
            .db
            .read(move |c| {
                let mut ids = Vec::with_capacity(wanted.len());
                for (thread, only) in &wanted {
                    ids.push(
                        messages::thread_messages(c, account_id, thread)?
                            .into_iter()
                            .filter(|m| only.as_ref().is_none_or(|id| &m.id == id))
                            .map(|m| m.id)
                            .collect::<Vec<String>>(),
                    );
                }
                Ok(ids)
            })
            .await?)
    }

    /// Applies `action` to every target at once: one store transaction and,
    /// from [`BATCH_FROM`] messages up, one `batchModify` per thousand
    /// instead of a call each. Trashing 200 conversations costs 50 quota
    /// units this way against 2000 one at a time.
    ///
    /// On a refusal the messages Gmail did not take lose this action's
    /// change again, a `WriteFailed` event says so, and the caller reports
    /// the failure against each target it handed in. Messages Gmail took
    /// keep it, since Gmail has them that way. The rollback reverses only
    /// this action's labels rather than restoring a copy taken before it: a
    /// replay during the retries may have stored newer changes, such as an
    /// archive made in the browser, and history will not send them again.
    ///
    /// On success it returns what the action changed on each message, which
    /// is what Undo reverses: a message that already had a label the action
    /// adds, or lacked one it removes, changed less than the action names.
    pub async fn triage_all(
        &self,
        targets: &[Target],
        action: &TriageAction,
    ) -> Result<Vec<Relabelled>, SyncError> {
        if targets.is_empty() {
            return Ok(Vec::new());
        }
        let account_id = self.account_id;
        let wanted = messages_wanted(targets);
        let threads: BTreeSet<String> = wanted.keys().cloned().collect();

        // Search results and the Trash or Spam lists show threads the store
        // may not hold yet. Fetch those first so there is something to change.
        self.ensure_threads(&threads).await?;

        let (add, remove) = action.label_delta();
        let snapshot: Vec<(String, String, Vec<String>)> = {
            let (wanted, add, remove) = (wanted.clone(), add.clone(), remove.clone());
            self.db
                .write(move |c| {
                    let mut before: Vec<(String, String, Vec<String>)> = Vec::new();
                    for (thread, only) in &wanted {
                        for message in messages::thread_messages(c, account_id, thread)? {
                            if only.as_ref().is_none_or(|ids| ids.contains(&message.id)) {
                                before.push((thread.clone(), message.id, message.label_ids));
                            }
                        }
                    }
                    let changes: Vec<Change> = before
                        .iter()
                        .flat_map(|(_, id, _)| relabel(id, &add, &remove))
                        .collect();
                    messages::apply(c, account_id, &changes)?;
                    Ok(before)
                })
                .await?
        };
        self.emit_threads(threads.clone());

        let ids: Vec<String> = snapshot.iter().map(|(_, id, _)| id.clone()).collect();
        let writing = Writing {
            action,
            conversations: threads.len(),
        };
        let mut budget = Budget::new(self.retry_max, self.wait_ceiling);
        let mut taken = BTreeSet::new();
        if let Err(err) = self
            .write_labels(&mut budget, &ids, &writing, &add, &remove, &mut taken)
            .await
        {
            self.db
                .write(move |c| {
                    let mut back = Vec::new();
                    for (thread, id, before) in &snapshot {
                        if taken.contains(id) {
                            continue;
                        }
                        let change = Relabelled::from_labels(thread, id, before, &add, &remove);
                        back.extend(relabel(id, &change.removed, &change.added));
                    }
                    messages::apply(c, account_id, &back)?;
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
        Ok(snapshot
            .iter()
            .map(|(thread, id, before)| Relabelled::from_labels(thread, id, before, &add, &remove))
            .collect())
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
    /// waits its minute over rather than twenty minutes. `taken` collects
    /// the messages Gmail accepted, so a failure part way can tell them
    /// from the rest.
    async fn write_labels(
        &self,
        budget: &mut Budget,
        ids: &[String],
        writing: &Writing<'_>,
        add: &[String],
        remove: &[String],
        taken: &mut BTreeSet<String>,
    ) -> Result<(), BackendError> {
        if ids.len() < BATCH_FROM {
            for id in ids {
                self.write_one(budget, id, writing, add, remove).await?;
                taken.insert(id.clone());
            }
            return Ok(());
        }
        for chunk in ids.chunks(BATCH_LIMIT) {
            match self.write_batch(budget, chunk, writing, add, remove).await {
                Ok(()) => taken.extend(chunk.iter().cloned()),
                Err(err) if refuses_batch(&err) => {
                    tracing::warn!(
                        account = self.account_id,
                        error = %err,
                        "Gmail refused the batch; changing each message on its own"
                    );
                    for id in chunk {
                        self.write_one(budget, id, writing, add, remove).await?;
                        taken.insert(id.clone());
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
    ) -> Result<(), BackendError> {
        loop {
            match self.services.mail.batch_modify(ids, add, remove).await {
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
    ) -> Result<(), BackendError> {
        loop {
            // The same labels a batch sends, rather than `messages.trash`
            // and `untrash`: untrash takes the trash label off and puts
            // nothing back, which would leave a few messages out of the
            // inbox that a batch of many would have returned to it.
            let done = self.services.mail.modify_labels(message_id, add, remove).await;
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
        self.services.mail.stand_by(delay).await;
    }
}

/// What a label change did to one message: the labels it put on that the
/// message lacked, and the ones it took off that the message had. Undo
/// reverses exactly this, so a message that was already read stays read
/// when marking its thread read is undone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relabelled {
    pub thread_id: String,
    pub message_id: String,
    pub added: Vec<String>,
    pub removed: Vec<String>,
}

impl Relabelled {
    /// The change `add` and `remove` make to a message that carried
    /// `before`.
    fn from_labels(
        thread_id: &str,
        message_id: &str,
        before: &[String],
        add: &[String],
        remove: &[String],
    ) -> Relabelled {
        Relabelled {
            thread_id: thread_id.to_string(),
            message_id: message_id.to_string(),
            added: add
                .iter()
                .filter(|l| !before.contains(l))
                .cloned()
                .collect(),
            removed: remove
                .iter()
                .filter(|l| before.contains(l))
                .cloned()
                .collect(),
        }
    }

    /// Whether the message came out as it went in.
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty()
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
    fn wait(&mut self, err: &BackendError) -> Option<Duration> {
        if !err.is_transient() {
            return None;
        }
        let limited = matches!(err, BackendError::RateLimited(_));
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
fn retry_delay(err: &BackendError, attempt: u32, max: Duration) -> Duration {
    match err {
        BackendError::RateLimited(Some(after)) => {
            with_jitter(*after, rand::random_range(-1.0..=1.0))
        }
        _ => backoff_delay(attempt, max, rand::random_range(-1.0..=1.0)),
    }
}

/// Whether Gmail turned the batch down for the batch's own sake, which a
/// call per message may still get through.
fn refuses_batch(err: &BackendError) -> bool {
    matches!(
        err,
        BackendError::NotFound | BackendError::Gmail(GmailError::Http { status: 400, .. })
    )
}

/// What the window says while an action sits out a rate limit.
fn still_waiting(writing: &Writing<'_>) -> String {
    fill(
        &gettext("Gmail is busy. Still working on {conversations}."),
        &[("conversations", &conversations(writing.conversations))],
    )
}

/// What the toast says when a write did not land. A rate limit that
/// outlasted the waiting names how long the action held on and how much
/// of it did not go through, so nobody has to guess what to redo.
fn write_failure(writing: &Writing<'_>, err: &BackendError, waited: Duration) -> String {
    let what = writing.action.describe().to_lowercase();
    let many = conversations(writing.conversations);
    let values = [("action", what.as_str()), ("conversations", many.as_str())];
    match err {
        BackendError::RateLimited(_) if waited.is_zero() => fill(
            &gettext(
                "Gmail is busy, so {action} did not go through for {conversations}. \
                 Try again in a moment.",
            ),
            &values,
        ),
        BackendError::RateLimited(_) => {
            let waited = roughly(waited);
            fill(
                &gettext(
                    "Gmail stayed busy for {waited}, so {action} did not go through \
                     for {conversations}.",
                ),
                &[("waited", waited.as_str()), values[0], values[1]],
            )
        }
        _ => fill(
            &gettext("{action} failed: {reason}"),
            &[
                ("action", &writing.action.describe()),
                ("reason", &err.to_string()),
            ],
        ),
    }
}

fn conversations(count: usize) -> String {
    fill_plural(
        "{count} conversation",
        "{count} conversations",
        count,
        &[("count", &count.to_string())],
    )
}

/// A wait in words, for a message a person reads.
fn roughly(waited: Duration) -> String {
    match waited.as_secs() {
        0..2 => gettext("a moment"),
        seconds @ 2..45 => fill_plural(
            "{count} second",
            "{count} seconds",
            seconds as usize,
            &[("count", &seconds.to_string())],
        ),
        _ => gettext("a minute"),
    }
}

/// The store changes that put Gmail's `add` labels on message `id` and
/// take its `remove` labels off.
fn relabel(id: &str, add: &[String], remove: &[String]) -> Vec<Change> {
    add.iter()
        .map(|l| Change::label(id, l, true))
        .chain(remove.iter().map(|l| Change::label(id, l, false)))
        .collect()
}
