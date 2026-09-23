//! Metadata for the rows of a Gmail search, and the whole threads a search
//! fetched, kept so opening one of them asks Gmail nothing.
//!
//! A search returns messages, and a thread can hold more messages than the
//! search matched, so a search hit alone never tells what the whole thread
//! holds. When two or more hits share a thread, one `threads.get` costs no
//! more than their `messages.get` calls, and it answers for every message
//! of the thread. Those threads are kept here until the reader opens one.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::time::{Duration, Instant};

use mailrs_domain::MessageMeta;
use mailrs_gmail::MessageRef;
use mailrs_store::messages::Change;
use mailrs_store::{accounts, messages};

use super::AccountSync;
use super::fetch::Want;
use crate::{MailBackend, SyncError};

/// How long a fetched thread is kept for opening. Only memory depends on
/// it: whether a kept thread may still be stored is the history cursor's
/// call.
const KEPT_FOR: Duration = Duration::from_secs(10 * 60);

/// A whole thread a search fetched.
pub(super) struct Listed {
    at: Instant,
    /// The history cursor before Gmail was asked. While the cursor stays
    /// there, no replay has passed over a change the thread missed.
    history_id: Option<u64>,
    metas: Vec<MessageMeta>,
}

/// The messages one thread had among a search's hits.
pub(super) struct Hits {
    at: Instant,
    ids: BTreeSet<String>,
}

impl AccountSync {
    /// The messages a Gmail search returns, by id and thread, newest
    /// first, at most `limit` of them. One call of 5 quota units, whatever
    /// the count, so a caller takes the ids first and pays for metadata
    /// only as it shows rows. The hits are kept per thread for
    /// [`KEPT_FOR`], so Delete Forever on a listed row knows its messages
    /// without asking Gmail.
    pub async fn search_ids(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<MessageRef>, SyncError> {
        let size = u32::try_from(limit)
            .unwrap_or(u32::MAX)
            .min(crate::ID_PAGE_SIZE);
        let page = self.services.mail.list_messages(query, None, size).await?;
        let found: Vec<MessageRef> = page.messages.into_iter().take(limit).collect();
        let mut by_thread: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
        for hit in &found {
            by_thread
                .entry(hit.thread_id.as_str())
                .or_default()
                .insert(hit.id.clone());
        }
        let mut kept = self.hits.lock().expect("search hits poisoned");
        kept.retain(|_, hits| hits.at.elapsed() < KEPT_FOR);
        for (thread, ids) in by_thread {
            kept.insert(
                thread.to_string(),
                Hits {
                    at: Instant::now(),
                    ids,
                },
            );
        }
        Ok(found)
    }

    /// The messages of `thread_id` a recent search saw, without asking
    /// Gmail: every message of it when the search fetched it whole,
    /// otherwise the hits it listed. `None` when no search named it.
    pub(super) fn listed_ids(&self, thread_id: &str) -> Option<BTreeSet<String>> {
        let whole = self
            .listed
            .lock()
            .expect("listed threads poisoned")
            .get(thread_id)
            .filter(|listed| listed.at.elapsed() < KEPT_FOR)
            .map(|listed| listed.metas.iter().map(|m| m.id.clone()).collect());
        whole.or_else(|| {
            self.hits
                .lock()
                .expect("search hits poisoned")
                .get(thread_id)
                .filter(|hits| hits.at.elapsed() < KEPT_FOR)
                .map(|hits| hits.ids.clone())
        })
    }

    /// Lets go of what searches kept about threads that are gone.
    pub(super) fn forget_listed(&self, threads: &BTreeSet<String>) {
        let mut listed = self.listed.lock().expect("listed threads poisoned");
        let mut hits = self.hits.lock().expect("search hits poisoned");
        for thread in threads {
            listed.remove(thread);
            hits.remove(thread);
        }
    }

    /// Metadata for the messages a search listed, newest first. The store
    /// answers for the messages it already holds, which costs nothing.
    /// Two or more hits in one thread the store lacks come from one
    /// `threads.get`, which is kept for opening; each other hit costs a
    /// `messages.get`. Messages Gmail no longer has are left out.
    pub async fn metadata_of(&self, refs: &[MessageRef]) -> Result<Vec<MessageMeta>, SyncError> {
        let account_id = self.account_id;
        let wanted: Vec<String> = refs.iter().map(|r| r.id.clone()).collect();
        let mut metas = self
            .db
            .read(move |c| messages::by_ids(c, account_id, &wanted))
            .await?;
        let held: HashSet<String> = metas.iter().map(|m| m.id.clone()).collect();
        let wants: Vec<Want> = refs
            .iter()
            .filter(|r| !held.contains(&r.id))
            .cloned()
            .map(Want::from)
            .collect();
        let fetched = self.fetch(wants).await?;
        let mut kept = self.listed.lock().expect("listed threads poisoned");
        kept.retain(|_, listed| listed.at.elapsed() < KEPT_FOR);
        for whole in fetched.whole {
            if let Some(first) = whole.first() {
                kept.insert(
                    first.thread_id.clone(),
                    Listed {
                        at: Instant::now(),
                        history_id: fetched.asked_at,
                        metas: whole,
                    },
                );
            }
        }
        drop(kept);
        metas.extend(fetched.metas);
        metas.sort_by(|a, b| b.date.cmp(&a.date).then_with(|| a.id.cmp(&b.id)));
        Ok(metas)
    }

    /// Opens a thread: stores the copy a search fetched whole, when it is
    /// still current, then does what [`AccountSync::ensure_thread`] does.
    /// With the copy stored, a recent history replay lets `ensure_thread`
    /// trust it and ask Gmail nothing.
    ///
    /// The copy is current while the history cursor has not moved since
    /// before Gmail was asked: a replay that moved it could have passed
    /// over a change to this thread, which the store did not hold then.
    pub async fn open_thread(&self, thread_id: &str) -> Result<(), SyncError> {
        let kept = self
            .listed
            .lock()
            .expect("listed threads poisoned")
            .remove(thread_id);
        if let Some(listed) = kept {
            let (account_id, thread) = (self.account_id, thread_id.to_string());
            let stored = self
                .db
                .write(move |c| {
                    let cursor = accounts::sync_cursor(c, account_id)?;
                    if cursor.history_id.is_none()
                        || cursor.history_id != listed.history_id
                        || !messages::thread_messages(c, account_id, &thread)?.is_empty()
                    {
                        return Ok(false);
                    }
                    let mut changes: Vec<Change> = listed
                        .metas
                        .iter()
                        .map(|meta| Change::Upsert {
                            meta: Box::new(meta.clone()),
                            generation: cursor.sync_gen,
                        })
                        .collect();
                    changes.push(Change::MarkWhole {
                        thread_id: thread.clone(),
                    });
                    messages::apply(c, account_id, &changes)?;
                    Ok(true)
                })
                .await?;
            if stored {
                self.emit_threads(BTreeSet::from([thread_id.to_string()]));
            }
        }
        self.ensure_thread(thread_id).await
    }
}
