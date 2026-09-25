//! Mail actions: operations applied to the store at once and to the
//! server after.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::time::Duration;

use futures::StreamExt;
use mailrs_domain::translate::{fill, fill_plural, gettext};
use mailrs_domain::{Applied, ChangeEvent, Location, Memberships, Role, Target};
use mailrs_store::messages::Change;
use mailrs_store::{mailboxes, messages, reminders, remote_refs, threads};

use super::{AccountSync, FETCH_CONCURRENCY};
use crate::ops::{Roles, local_changes, ops_for, reverse_changes, split_keywords};
use crate::services::{Relocated, Unapplied};
use crate::{
    BackendError, MailBackend, MailOp, SyncError, TriageAction, backoff_delay, with_jitter,
};

/// Attempts per write before a triage gives up.
const WRITE_ATTEMPTS: u32 = 3;

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
        // A server that cannot erase mail answers before anything is sent.
        if !self.services.mail.capabilities().delete_forever {
            return Err(BackendError::Unsupported.into());
        }
        let ids = self.erasable(targets).await?;
        let mut all: Vec<String> = ids.iter().flatten().cloned().collect();
        all.sort();
        all.dedup();
        let mut refused: BTreeMap<String, BackendError> = BTreeMap::new();
        let limit = self.services.mail.capabilities().batch_limit;
        for chunk in all.chunks(limit) {
            if let Err(Unapplied { error: err, .. }) =
                self.services.mail.apply(chunk, &[MailOp::Destroy]).await
            {
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

    /// Applies `action` to every target at once, as the operations
    /// `ops_for` makes of it for this account. See [`Self::change_all`].
    pub async fn triage_all(
        &self,
        targets: &[Target],
        action: &TriageAction,
    ) -> Result<Vec<Applied>, SyncError> {
        let mail = &self.services.mail;
        let ops = ops_for(action, &mail.capabilities(), &self.roles(), |id| mail.set_of(id))?;
        self.change_all(targets, &ops, &action.describe()).await
    }

    /// The server's id for each role's mailbox.
    fn roles(&self) -> Roles {
        Role::ALL
            .into_iter()
            .filter_map(|role| self.services.mail.mailbox_for(role).map(|id| (role, id)))
            .collect()
    }

    /// Applies `ops` to every target at once: one store transaction, then
    /// the server. `what` names the change for the messages a person
    /// reads. Trashing 200 conversations is one `batchModify` of 50
    /// quota units this way, against 2000 one at a time.
    ///
    /// On a refusal the messages the server did not take lose this
    /// change again, a `WriteFailed` event says so, and the caller
    /// reports the failure against each target it handed in. Messages
    /// the server took keep it. The rollback reverses only what this
    /// change did rather than restoring a copy taken before it: a replay
    /// during the retries may have stored newer changes, such as an
    /// archive made in the browser, and history will not send them again.
    ///
    /// On success it returns what each message gained and lost, which is
    /// what Undo reverses: a message that already had what the change
    /// gives, or lacked what it takes, changed less than the action names.
    pub async fn change_all(
        &self,
        targets: &[Target],
        ops: &[MailOp],
        what: &str,
    ) -> Result<Vec<Applied>, SyncError> {
        if targets.is_empty() {
            return Ok(Vec::new());
        }
        let account_id = self.account_id;
        let wanted = messages_wanted(targets);
        let threads: BTreeSet<String> = wanted.keys().cloned().collect();

        // Search results and the Trash or Spam lists show threads the store
        // may not hold yet. Fetch those first so there is something to change.
        self.ensure_threads(&threads).await?;

        let (to_server, kept_here) =
            split_keywords(ops, self.services.mail.capabilities().keywords);
        let (ids, applied) = {
            let (wanted, ops, roles) = (wanted.clone(), ops.to_vec(), self.roles());
            self.db
                .write(move |c| {
                    let mut ids = Vec::new();
                    for (thread, only) in &wanted {
                        for message in messages::thread_messages(c, account_id, thread)? {
                            if only
                                .as_ref()
                                .is_none_or(|named| named.contains(&message.id))
                            {
                                ids.push(message.id);
                            }
                        }
                    }
                    let held = messages::memberships_of(c, account_id, &ids)?;
                    let none = Memberships::default();
                    let changes: Vec<Change> = ids
                        .iter()
                        .flat_map(|id| {
                            local_changes(id, held.get(id).unwrap_or(&none), &ops, &roles)
                        })
                        .collect();
                    let applied = messages::apply(c, account_id, &changes)?.applied;
                    for keyword in &kept_here {
                        messages::mark_local(c, account_id, &ids, keyword)?;
                    }
                    Ok((ids, applied))
                })
                .await?
        };
        self.emit_threads(threads.clone());
        if to_server.is_empty() {
            return Ok(applied);
        }

        let moves = self.hold_moves().await;
        let names = self.remotes(&ids).await?;
        let writing = Writing {
            what: what.to_string(),
            conversations: threads.len(),
        };
        let mut budget = Budget::new(self.retry_max, self.wait_ceiling);
        let mut progress = Progress::default();
        let written = self
            .write_ops(&mut budget, &ids, &names, &writing, &to_server, &mut progress)
            .await;
        // What the server moved before any refusal has moved, so its refs
        // follow whatever else happened. A failure here still lets the
        // rollback below run for the messages the server never took.
        let relocated = self.relocate(&ids, &names, progress.moved).await;
        drop(moves);
        if let Err(err) = written {
            let back: Vec<Change> = applied
                .iter()
                .filter(|a| !progress.taken.contains(&a.message_id))
                .flat_map(reverse_changes)
                .collect();
            self.db
                .write(move |c| {
                    messages::apply(c, account_id, &back)?;
                    Ok(())
                })
                .await?;
            self.emit_threads(threads);
            self.emit(ChangeEvent::WriteFailed {
                account_id,
                message: write_failure(&writing, &err, budget.waited),
            });
            if let Err(lost) = relocated {
                tracing::warn!(account = account_id, error = %lost, "could not record where the server moved mail");
            }
            return Err(err.into());
        }
        relocated?;
        Ok(applied)
    }

    /// Sends `ops` over the messages `ids` names, which the server knows
    /// as `names`, waiting out what is worth waiting out and sending the
    /// rest after each wait. The waiting belongs to the action, not to each
    /// message, so a rate-limited archive of twenty threads waits its
    /// minute once rather than twenty times. `progress` collects the
    /// messages the server accepted and where it moved any, so a failure
    /// part way can tell them from the rest.
    async fn write_ops(
        &self,
        budget: &mut Budget,
        ids: &[String],
        names: &[String],
        writing: &Writing,
        ops: &[MailOp],
        progress: &mut Progress,
    ) -> Result<(), BackendError> {
        let mut from = 0;
        loop {
            match self.services.mail.apply(&names[from..], ops).await {
                Ok(moved) => {
                    progress.moved.extend(moved);
                    progress.taken.extend(ids[from..].iter().cloned());
                    return Ok(());
                }
                Err(Unapplied {
                    taken: through,
                    error,
                    relocated,
                }) => {
                    progress.moved.extend(relocated);
                    let through = through.min(names.len() - from);
                    progress
                        .taken
                        .extend(ids[from..from + through].iter().cloned());
                    from += through;
                    match budget.wait(&error) {
                        Some(delay) => self.hold_on(budget, delay, writing).await,
                        None => return Err(error),
                    }
                }
            }
        }
    }

    /// Records where the server moved messages: each one's remote ref, and
    /// the mailbox it sits in now, which the store has not filed it under
    /// when the server made that mailbox for this move. A mailbox the store
    /// has not listed is listed.
    async fn relocate(
        &self,
        ids: &[String],
        names: &[String],
        moved: Vec<Relocated>,
    ) -> Result<(), SyncError> {
        if moved.is_empty() {
            return Ok(());
        }
        let by_name: HashMap<&str, &str> = names
            .iter()
            .map(String::as_str)
            .zip(ids.iter().map(String::as_str))
            .collect();
        let placed: Vec<(String, Location)> = moved
            .into_iter()
            .filter_map(|r| Some((by_name.get(r.from.as_str())?.to_string(), r.to)))
            .collect();
        let account_id = self.account_id;
        let (touched, unknown) = self
            .db
            .write(move |c| {
                let known: HashSet<String> = mailboxes::listed(c, account_id)?
                    .into_iter()
                    .map(|m| m.id)
                    .collect();
                let ids: Vec<String> = placed.iter().map(|(id, _)| id.clone()).collect();
                let held = messages::memberships_of(c, account_id, &ids)?;
                let none = Memberships::default();
                let mut changes = Vec::new();
                let mut unknown = BTreeSet::new();
                for (id, at) in &placed {
                    if !known.contains(&at.mailbox) {
                        unknown.insert(at.mailbox.clone());
                    }
                    changes.extend(local_changes(
                        id,
                        held.get(id).unwrap_or(&none),
                        &[MailOp::MoveToMailbox(at.mailbox.clone())],
                        &Roles::new(),
                    ));
                }
                let touched = messages::apply(c, account_id, &changes)?.threads;
                for (id, at) in &placed {
                    remote_refs::locate(c, account_id, id, at)?;
                }
                Ok((touched, unknown))
            })
            .await?;
        if !unknown.is_empty() {
            self.refresh_labels().await?;
            self.tell_archive_made(unknown).await?;
        }
        self.emit_threads(touched);
        Ok(())
    }

    /// Says so when a move landed in an Archive the server made for it.
    /// `new` holds the mailboxes the moves went into that the store had
    /// not listed; once listed again, one of them holding the Archive role
    /// is the one this archive made.
    async fn tell_archive_made(&self, new: BTreeSet<String>) -> Result<(), SyncError> {
        let account_id = self.account_id;
        let made = self
            .db
            .read(move |c| {
                Ok(mailboxes::listed(c, account_id)?
                    .into_iter()
                    .find(|m| m.role == Some(Role::Archive) && new.contains(&m.id)))
            })
            .await?;
        if let Some(archive) = made {
            self.emit(ChangeEvent::ArchiveMade {
                account_id,
                name: archive.name,
            });
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

    /// Sits out `delay` before trying Gmail again. The first wait of an
    /// action says so, so the window shows the work is still going rather
    /// than nothing at all, and the whole wait counts as the user waiting
    /// on Gmail, so backfill stands aside for it.
    async fn hold_on(&self, budget: &Budget, delay: Duration, writing: &Writing) {
        if budget.waits == 1 {
            self.emit(ChangeEvent::WaitingOnGmail {
                account_id: self.account_id,
                message: still_waiting(writing),
            });
        }
        self.services.mail.stand_by(delay).await;
    }
}

/// One mail action, as the messages about it need to name it.
struct Writing {
    what: String,
    conversations: usize,
}

/// What reached the server during one mail action.
#[derive(Default)]
struct Progress {
    /// The messages the server took.
    taken: BTreeSet<String>,
    /// Where the server moved messages, by the names they went under.
    moved: Vec<Relocated>,
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

/// What the window says while an action sits out a rate limit.
fn still_waiting(writing: &Writing) -> String {
    fill(
        &gettext("Gmail is busy. Still working on {conversations}."),
        &[("conversations", &conversations(writing.conversations))],
    )
}

/// What the toast says when a write did not land. A rate limit that
/// outlasted the waiting names how long the action held on and how much
/// of it did not go through, so nobody has to guess what to redo.
fn write_failure(writing: &Writing, err: &BackendError, waited: Duration) -> String {
    let what = writing.what.to_lowercase();
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
            &[("action", &writing.what), ("reason", &err.to_string())],
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
