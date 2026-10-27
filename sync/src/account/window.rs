//! Bootstrap, backfill, pruning, and the inbox check: the processes that
//! load the window, trim it, and keep it in step with Gmail.

use std::collections::{BTreeSet, HashMap, HashSet};

use mailrs_domain::{AccountState, ChangeEvent, EpochMillis, Location, MailSet, MessageMeta, Role};
use mailrs_store::messages::Change;
use mailrs_store::{accounts, mailboxes, messages, window};

use super::fetch::store_fetched;
use super::{AccountSync, window_longer, window_reach};
use crate::{BackendError, MailBackend, RemoteRef, SyncError, Want};

const DAY_MILLIS: i64 = 24 * 60 * 60 * 1000;

/// The mailboxes the messages in `listed` sit in, each with the names of
/// its messages there. A folder server's name for a message says which
/// mailbox holds it, so a relisting compares only these: every other
/// mailbox holds none of the listed messages, and a parent that holds no
/// mail, which refuses SELECT, is never asked.
fn mailboxes_holding(listed: &[RemoteRef]) -> HashMap<String, HashSet<String>> {
    let mut members: HashMap<String, HashSet<String>> = HashMap::new();
    for message in listed {
        if let Some(at) = Location::parse(&message.id) {
            members
                .entry(at.mailbox)
                .or_default()
                .insert(message.id.clone());
        }
    }
    members
}

impl AccountSync {
    /// Starts a new sync generation. Records the history cursor before
    /// listing anything, replaces the labels, and loads the first window
    /// page. `backfill_step` loads the rest.
    pub async fn bootstrap(&self) -> Result<(), SyncError> {
        self.set_state(AccountState::Bootstrapping).await?;
        let start = self.mail.state_now().await?;
        let listed = self.mail.mailboxes().await?;
        let account_id = self.account_id;
        let window_days = self.window_days;
        let generation = self
            .db
            .write(move |c| {
                mailboxes::replace_listed(c, account_id, &listed)?;
                accounts::set_window_days(c, account_id, window_days)?;
                accounts::start_generation(c, account_id, start.as_str())
            })
            .await?;
        self.emit(ChangeEvent::LabelsChanged { account_id });
        self.load_window_page(None, generation).await?;
        self.set_state(AccountState::Ok).await
    }

    /// Compares the window this run keeps with the one the stored mail
    /// was loaded for, once a run, and acts on a change. A longer window
    /// pages through the listing again from the top, in the background
    /// like any backfill, fetching only the mail the store lacks; a shorter
    /// one deletes the mail outside it at once, so the disk holds what the
    /// setting says. An account from before the window was recorded, and a
    /// POP3 account, whose store is the only copy of its mail, only note
    /// the window.
    pub async fn follow_window(&self) -> Result<(), SyncError> {
        use std::sync::atomic::Ordering;
        if self.window_followed.load(Ordering::SeqCst) {
            return Ok(());
        }
        let (account_id, now) = (self.account_id, self.window_days);
        let local = self.mail.capabilities().local_mailboxes;
        let (stored, cursor) = self
            .db
            .read(move |c| {
                Ok((
                    accounts::window_days(c, account_id)?,
                    accounts::sync_cursor(c, account_id)?,
                ))
            })
            .await?;
        if let Some(before) = stored
            && window_reach(before) != window_reach(now)
            && !local
            && cursor.state.is_some()
        {
            if window_longer(before, now) {
                self.prune(crate::now_millis()).await?;
            }
            // A backfill still on its way pages again too: a page token
            // belongs to the listing of the old window.
            if window_longer(now, before) || !cursor.backfill_done {
                tracing::info!(
                    account = account_id,
                    "the window changed; listing it again for the mail it now reaches"
                );
                self.db
                    .write(move |c| accounts::extend_backfill(c, account_id))
                    .await?;
            }
        }
        if stored != Some(now) {
            self.db
                .write(move |c| accounts::set_window_days(c, account_id, now))
                .await?;
        }
        self.window_followed.store(true, Ordering::SeqCst);
        Ok(())
    }

    /// Lists the window again once the server has lost its place, and fetches only what changed in the gap. A full
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
    /// since there is too little in it to compare against, and so does a
    /// window too large for the server to list whole.
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
        let start = self.mail.state_now().await?;
        let listed = self.mail.mailboxes().await?;
        let listed_ids: Vec<String> = listed.iter().map(|m| m.id.clone()).collect();
        let generation = self
            .db
            .write(move |c| {
                mailboxes::replace_listed(c, account_id, &listed)?;
                accounts::start_generation(c, account_id, start.as_str())
            })
            .await?;
        self.emit(ChangeEvent::LabelsChanged { account_id });

        // The server's own names, since on a folder server each one says
        // which mailbox holds the message; `listing_as_stored` reads them
        // back as store ids below.
        let (listed, members) = match self.list_window(&listed_ids).await {
            Err(SyncError::Backend(BackendError::Unsupported)) => {
                // Too much to list in one go: a cut listing would sweep the
                // rest away, so the window loads again a page at a time and
                // sweeps only after its last page.
                tracing::info!(
                    account = account_id,
                    "the window is too large to compare; loading it again"
                );
                return self.bootstrap().await;
            }
            listed => listed?,
        };
        // What each compared mailbox stands for, so the stored copy of a
        // message can be asked whether it is in it.
        let sets: Vec<(String, MailSet)> = members
            .keys()
            .map(|id| (id.clone(), self.mail.set_of(id)))
            .collect();
        let (listed, members) = self.listing_as_stored(listed, members).await?;
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
            // Only the mailboxes just listed can be compared; one the server
            // does not list stays as the store has it.
            let before: BTreeSet<&str> = sets
                .iter()
                .filter(|(_, set)| held.in_set(set))
                .map(|(id, _)| id.as_str())
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
        let fetched = self.fetch(wants).await?;
        let touched = self
            .db
            .write(move |c| {
                let mut touched = store_fetched(
                    c,
                    account_id,
                    generation,
                    &fetched.metas,
                    &[],
                    &fetched.placing,
                )?;
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

    /// The window's ids as the server names them, and the ids each of
    /// `labels` holds. `Unsupported` when a listing is too large to take
    /// whole.
    async fn list_window(
        &self,
        labels: &[String],
    ) -> Result<(Vec<RemoteRef>, HashMap<String, HashSet<String>>), SyncError> {
        let server = self.mail.untranslated();
        let listed = server.window_ids(self.window_days, None).await?;
        // A label server lists per label even when it keeps refs: its
        // ids do not say which mailbox holds a message.
        if self.renames() && !self.mail.capabilities().labels {
            let members = mailboxes_holding(&listed);
            return Ok((listed, members));
        }
        let mut members: HashMap<String, HashSet<String>> = HashMap::new();
        for label in labels {
            let carrying = server.window_ids(self.window_days, Some(label)).await?;
            members.insert(label.clone(), carrying.into_iter().map(|m| m.id).collect());
        }
        Ok((listed, members))
    }

    /// Loads the next window page. Returns true while pages remain.
    pub async fn backfill_step(&self) -> Result<bool, SyncError> {
        let account_id = self.account_id;
        let cursor = self
            .db
            .read(move |c| accounts::sync_cursor(c, account_id))
            .await?;
        if cursor.backfill_done || cursor.state.is_none() {
            return Ok(false);
        }
        match self
            .load_window_page(cursor.backfill_cursor.clone(), cursor.sync_gen)
            .await
        {
            Ok(next) => Ok(next.is_some()),
            Err(SyncError::Backend(BackendError::StateLost))
                if cursor.backfill_cursor.is_some() =>
            {
                tracing::warn!(
                    account = account_id,
                    "the server no longer takes the saved page token; listing the window again"
                );
                self.db
                    .write(move |c| accounts::set_backfill(c, account_id, None, false))
                    .await?;
                Ok(true)
            }
            Err(err) => Err(err),
        }
    }

    /// Deletes threads that have left the window. All mail leaves none,
    /// and neither does a POP3 account, whose store is the only copy.
    pub async fn prune(&self, now: EpochMillis) -> Result<(), SyncError> {
        let account_id = self.account_id;
        let Some(days) = window_reach(self.window_days) else {
            return Ok(());
        };
        if self.mail.capabilities().local_mailboxes {
            return Ok(());
        }
        let cutoff = now - days * DAY_MILLIS;
        let pruned = self
            .db
            .write(move |c| window::prune_window(c, account_id, cutoff))
            .await?;
        self.emit_threads(pruned.into_iter().collect());
        Ok(())
    }

    /// Starts keeping `mailbox` in step, as a person opening it asks: its
    /// window is listed and stored now, and the slow poll looks at it from
    /// then on. A label server keeps every label in step already and is
    /// asked nothing; a mailbox already followed is asked nothing either,
    /// since the slow poll covers it after its first successful load.
    pub async fn follow_mailbox(&self, mailbox: &str) -> Result<(), SyncError> {
        if !self.renames() || self.mail.capabilities().labels {
            return Ok(());
        }
        if !self.network_available() {
            return Err(BackendError::Offline(String::new()).into());
        }
        let Some(following) = self.mail.follow(mailbox).await else {
            return Ok(());
        };
        let listed = self
            .mail
            .window_ids(self.window_days, Some(mailbox))
            .await?;
        let mut fetched = self
            .fetch(listed.into_iter().map(Want::from).collect())
            .await?;
        let account_id = self.account_id;
        let mailbox = mailbox.to_string();
        let touched = following
            .store(&self.db, move |c| {
                fetched.remap_renumbered(c, account_id, &mailbox)?;
                let generation = accounts::sync_cursor(c, account_id)?.sync_gen;
                store_fetched(
                    c,
                    account_id,
                    generation,
                    &fetched.metas,
                    &[],
                    &fetched.placing,
                )
            })
            .await?;
        self.emit_threads(touched);
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
        let _moves = self.mail.hold_moves().await;
        let account_id = self.account_id;
        let cursor = self
            .db
            .read(move |c| accounts::sync_cursor(c, account_id))
            .await?;
        if !cursor.backfill_done || cursor.state.is_none() {
            return Ok(());
        }
        let inbox = match self.mail.inbox_ids().await {
            Err(SyncError::Backend(BackendError::Unsupported)) => return Ok(()),
            listed => listed?,
        };
        let remote: HashSet<String> = inbox.into_iter().map(|m| m.id).collect();
        // Every message that differs is one the store holds, so the store
        // names its thread and the fetch can share a call per thread.
        let mut differ: Vec<Want> = self
            .db
            .read(move |c| {
                let local = messages::held_by(c, account_id, &MailSet::Role(Role::Inbox))?;
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
            .write(move |c| {
                store_fetched(
                    c,
                    account_id,
                    generation,
                    &fetched.metas,
                    &fetched.gone,
                    &fetched.placing,
                )
            })
            .await?;
        self.emit_threads(touched);
        Ok(())
    }

    /// `listed` and `members`, which name messages as the server does now,
    /// under the store's ids. On a folder server a message the app moved,
    /// or a relisting carried over, keeps its first id while the server
    /// names it by where it sits; a name that is some stored message's
    /// stale id drops out.
    async fn listing_as_stored(
        &self,
        listed: Vec<RemoteRef>,
        members: HashMap<String, HashSet<String>>,
    ) -> Result<(Vec<RemoteRef>, HashMap<String, HashSet<String>>), SyncError> {
        if !self.renames() {
            return Ok((listed, members));
        }
        let resolved = self
            .mail
            .resolve(listed.iter().map(|m| m.id.clone()).collect())
            .await?;
        let listed = listed
            .into_iter()
            .filter_map(|m| {
                Some(RemoteRef {
                    id: resolved.stored(&m.id)?,
                    thread_id: m.thread_id,
                })
            })
            .collect();
        let members = members
            .into_iter()
            .map(|(mailbox, names)| {
                let ids = names
                    .iter()
                    .filter_map(|name| resolved.stored(name))
                    .collect();
                (mailbox, ids)
            })
            .collect();
        Ok((listed, members))
    }

    /// Whether the backfill running now lengthens the window over mail the
    /// store holds.
    async fn extending(&self) -> Result<bool, SyncError> {
        let account_id = self.account_id;
        Ok(self
            .db
            .read(move |c| Ok(accounts::sync_cursor(c, account_id)?.backfill_extends))
            .await?)
    }

    /// `listed` split into what the store lacks and the ids of what it
    /// holds.
    async fn split_held(
        &self,
        listed: Vec<RemoteRef>,
    ) -> Result<(Vec<RemoteRef>, Vec<String>), SyncError> {
        let ids: Vec<String> = listed.iter().map(|m| m.id.clone()).collect();
        let account_id = self.account_id;
        let existing = self
            .db
            .read(move |c| messages::existing_ids(c, account_id, &ids))
            .await?;
        let (held, missing): (Vec<_>, Vec<_>) =
            listed.into_iter().partition(|m| existing.contains(&m.id));
        Ok((missing, held.into_iter().map(|m| m.id).collect()))
    }

    /// Stores one page of the window listing and saves the next page token.
    /// After the last page, deletes messages from earlier generations.
    async fn load_window_page(
        &self,
        page_token: Option<String>,
        generation: i64,
    ) -> Result<Option<String>, SyncError> {
        let page = self
            .mail
            .backfill(self.window_days, page_token.as_deref())
            .await?;
        let account_id = self.account_id;
        // A backfill that lengthens the window keeps what the store holds
        // and fetches the rest; any other fetches the whole page, so a
        // new generation reads every message as the server has it now.
        let (refs, held) = match self.extending().await? {
            true => self.split_held(page.refs).await?,
            false => (page.refs, Vec::new()),
        };
        // A listed message deleted since is left out; the sweep and
        // history take care of what the store had of it.
        let wants = refs.into_iter().map(Want::from).collect();
        let fetched = self.fetch(wants).await?;
        let next = page.next;
        let stored_next = next.clone();
        let touched = self
            .db
            .write(move |c| {
                let kept: Vec<Change> = held
                    .into_iter()
                    .map(|message_id| Change::Keep {
                        message_id,
                        generation,
                    })
                    .collect();
                messages::apply(c, account_id, &kept)?;
                let mut touched = store_fetched(
                    c,
                    account_id,
                    generation,
                    &fetched.metas,
                    &[],
                    &fetched.placing,
                )?;
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
