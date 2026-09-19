//! What the UI needs when a thread opens: all of its messages and their bodies.

use std::collections::BTreeSet;
use std::sync::Arc;

use mailrs_domain::MessageBody;
use mailrs_gmail::GmailError;
use mailrs_store::{accounts, bodies, messages};

use super::AccountSync;
use crate::{GmailApi, SyncError, now_millis};

impl<G: GmailApi> AccountSync<G> {
    /// Fetches every message of a thread, including ones older than the
    /// window. Deletes the thread locally when Gmail no longer has it.
    /// Announces the thread only when its stored messages or labels changed,
    /// since each announcement makes the UI reload its lists and counts.
    pub async fn ensure_thread(&self, thread_id: &str) -> Result<(), SyncError> {
        let account_id = self.account_id;
        let fetched = match self.api.thread_metadata(thread_id).await {
            Ok(metas) => Some(metas),
            Err(GmailError::NotFound) => None,
            Err(err) => return Err(err.into()),
        };
        let thread = thread_id.to_string();
        let changed = self
            .db
            .write(move |c| {
                let before = messages::thread_messages(c, account_id, &thread)?;
                match fetched {
                    Some(metas) => {
                        let generation = accounts::sync_cursor(c, account_id)?.sync_gen;
                        for meta in &metas {
                            messages::upsert_message(c, meta, generation)?;
                        }
                        messages::refresh_thread(c, account_id, &thread)?;
                        Ok(messages::thread_messages(c, account_id, &thread)? != before)
                    }
                    None => {
                        messages::delete_thread(c, account_id, &thread)?;
                        Ok(!before.is_empty())
                    }
                }
            })
            .await?;
        if changed {
            self.emit_threads(BTreeSet::from([thread_id.to_string()]));
        }
        Ok(())
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
        let (key, stored, cap) = (message_id.to_string(), body.clone(), self.body_cache_bytes);
        self.db
            .write(move |c| {
                if messages::thread_id_of(c, account_id, &key)?.is_some() {
                    bodies::put_body(c, account_id, &key, &stored, now)?;
                    bodies::evict_bodies(c, cap)?;
                }
                Ok(())
            })
            .await?;
        Ok(body)
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
