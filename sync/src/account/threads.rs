//! What the UI needs when a thread opens: all of its messages and their bodies.

use std::collections::BTreeSet;
use std::sync::Arc;

use mailrs_domain::MessageBody;
use mailrs_store::{accounts, bodies, messages};

use super::AccountSync;
use super::fetch::overtaken;
use crate::{GmailApi, SyncError, now_millis};

/// Fetches of one thread before an answer history keeps overtaking is
/// written anyway, adding only what the store lacks.
const FETCH_TRIES: u32 = 3;

/// What one fetch of a thread did.
enum Written {
    Stored {
        changed: bool,
    },
    /// A history replay moved the cursor while Gmail answered.
    Overtaken,
}

impl<G: GmailApi> AccountSync<G> {
    /// Whether the store already holds all of this thread and history has
    /// spoken for the mailbox since. Gmail sends every change through
    /// history, so a recent replay means the stored copy matches, and
    /// opening the thread needs no round trip. A thread the window holds
    /// only in part does not count: history speaks for the messages the
    /// store has, not for older ones it never fetched.
    async fn stored_and_current(&self, thread_id: &str) -> Result<bool, SyncError> {
        let fresh = self
            .caught_up
            .lock()
            .expect("caught up")
            .is_some_and(|at| at.elapsed() < super::FRESH_FOR);
        if !fresh {
            return Ok(false);
        }
        let (account_id, thread) = (self.account_id, thread_id.to_string());
        Ok(self
            .db
            .read(move |c| messages::is_whole(c, account_id, &thread))
            .await?)
    }

    /// Fetches every message of a thread, including ones older than the
    /// window. Deletes the thread locally when Gmail no longer has it.
    /// Announces the thread only when its stored messages or labels changed,
    /// since each announcement makes the UI reload its lists and counts.
    ///
    /// A history replay can finish while Gmail's answer is on its way. The
    /// answer may then be older than what the replay stored, and the replay
    /// has moved the cursor past the change, so no later history would put
    /// it right. When the cursor moved, the thread is fetched again; after
    /// [`FETCH_TRIES`] the answer only adds messages the store lacks.
    pub async fn ensure_thread(&self, thread_id: &str) -> Result<(), SyncError> {
        if self.stored_and_current(thread_id).await? {
            return Ok(());
        }
        for attempt in 1..=FETCH_TRIES {
            let last = attempt == FETCH_TRIES;
            match self.fetch_thread(thread_id, last).await? {
                Written::Stored { changed } => {
                    if changed {
                        self.emit_threads(BTreeSet::from([thread_id.to_string()]));
                    }
                    return Ok(());
                }
                Written::Overtaken => {
                    tracing::debug!(
                        account = self.account_id,
                        attempt,
                        "history moved while a thread was fetched; fetching it again"
                    );
                }
            }
        }
        Ok(())
    }

    /// One fetch of a thread and its write to the store. With `last`, an
    /// answer overtaken by history still adds the messages the store does
    /// not hold, and leaves the stored ones as history left them.
    async fn fetch_thread(&self, thread_id: &str, last: bool) -> Result<Written, SyncError> {
        let account_id = self.account_id;
        let mut answer = self.fetch_whole(vec![thread_id.to_string()]).await?;
        let found = answer.whole.pop();
        let thread = thread_id.to_string();
        self.db
            .write(move |c| {
                let cursor = accounts::sync_cursor(c, account_id)?;
                let overtaken = overtaken(c, account_id, &answer)?;
                if overtaken && !last {
                    return Ok(Written::Overtaken);
                }
                let before = messages::thread_messages(c, account_id, &thread)?;
                match found {
                    Some(metas) => {
                        for meta in &metas {
                            if overtaken && before.iter().any(|m| m.id == meta.id) {
                                continue;
                            }
                            messages::upsert_message(c, meta, cursor.sync_gen)?;
                        }
                        messages::refresh_thread(c, account_id, &thread)?;
                        messages::mark_whole(c, account_id, &thread)?;
                        let changed = messages::thread_messages(c, account_id, &thread)? != before;
                        Ok(Written::Stored { changed })
                    }
                    // History deletes what Gmail deleted before the cursor,
                    // so an overtaken "not found" leaves the store alone.
                    None if overtaken => Ok(Written::Stored { changed: false }),
                    None => {
                        messages::delete_thread(c, account_id, &thread)?;
                        Ok(Written::Stored {
                            changed: !before.is_empty(),
                        })
                    }
                }
            })
            .await
            .map_err(Into::into)
    }

    /// A message body from the cache, or from Gmail on a miss. Bodies of
    /// messages that are not stored come back uncached.
    ///
    /// A cache hit reads on the reader pool, so it does not wait behind
    /// backfill writes. Its access time goes to the writer afterwards.
    pub async fn body(&self, message_id: &str) -> Result<MessageBody, SyncError> {
        let account_id = self.account_id;
        let now = now_millis();
        let key = message_id.to_string();
        let cached = self
            .db
            .read(move |c| bodies::peek_body(c, account_id, &key))
            .await?;
        if let Some(body) = cached {
            self.touch_body(message_id, now);
            return Ok(body);
        }
        let body = self.api.message_body(message_id).await?;
        let size =
            body.html.as_ref().map_or(0, String::len) + body.text.as_ref().map_or(0, String::len);
        let sweep = self.due_for_eviction(size as i64);
        let (key, stored, cap) = (message_id.to_string(), body.clone(), self.body_cache_bytes);
        self.db
            .write(move |c| {
                if messages::thread_id_of(c, account_id, &key)?.is_some() {
                    bodies::put_body(c, account_id, &key, &stored, now)?;
                    if sweep {
                        bodies::evict_bodies(c, cap)?;
                    }
                }
                Ok(())
            })
            .await?;
        Ok(body)
    }

    /// Whether storing `size` more bytes of body calls for an eviction
    /// pass. Each pass sums every stored body, so it runs on the first
    /// body stored and then once every sixty-fourth of the cache written
    /// since, which lets the cache run over its cap by that much per
    /// account at most.
    fn due_for_eviction(&self, size: i64) -> bool {
        let mut unswept = self.unswept.lock().expect("unswept bytes poisoned");
        let written = unswept.map_or(i64::MAX, |bytes| bytes.saturating_add(size));
        if written >= (self.body_cache_bytes / 64).max(1) {
            *unswept = Some(0);
            true
        } else {
            *unswept = Some(written);
            false
        }
    }

    /// Records a cache hit. Hits that arrive before the writer gets to the
    /// first one share its transaction. A lost access time only makes that
    /// body look older to eviction, so failures are logged and dropped.
    fn touch_body(&self, message_id: &str, now: i64) {
        let mut touched = self.touched.lock().expect("touched bodies poisoned");
        touched.push((message_id.to_string(), now));
        if touched.len() > 1 {
            return;
        }
        let (db, account_id) = (self.db.clone(), self.account_id);
        let (pending, unsent) = (Arc::clone(&self.touched), Arc::clone(&self.touched));
        tokio::spawn(async move {
            let written = db
                .write(move |c| {
                    let reads =
                        std::mem::take(&mut *pending.lock().expect("touched bodies poisoned"));
                    bodies::touch_bodies(c, account_id, &reads)
                })
                .await;
            if let Err(err) = written {
                // Empty the list so the next hit schedules a new write.
                unsent.lock().expect("touched bodies poisoned").clear();
                tracing::debug!(error = %err, "could not record body reads");
            }
        });
    }
}
