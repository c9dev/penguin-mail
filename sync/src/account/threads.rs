//! What the UI needs when a thread opens: all of its messages and their bodies.

use std::collections::BTreeSet;

use mailrs_domain::MessageBody;
use mailrs_gmail::GmailError;
use mailrs_store::{accounts, bodies, messages};

use super::AccountSync;
use crate::{GmailApi, SyncError, now_millis};

impl<G: GmailApi> AccountSync<G> {
    /// Fetches every message of a thread, including ones older than the
    /// window. Deletes the thread locally when Gmail no longer has it.
    pub async fn ensure_thread(&self, thread_id: &str) -> Result<(), SyncError> {
        let account_id = self.account_id;
        let fetched = match self.api.thread_metadata(thread_id).await {
            Ok(metas) => Some(metas),
            Err(GmailError::NotFound) => None,
            Err(err) => return Err(err.into()),
        };
        let thread = thread_id.to_string();
        self.db
            .write(move |c| match fetched {
                Some(metas) => {
                    let generation = accounts::sync_cursor(c, account_id)?.sync_gen;
                    for meta in &metas {
                        messages::upsert_message(c, meta, generation)?;
                    }
                    messages::refresh_thread(c, account_id, &thread)
                }
                None => messages::delete_thread(c, account_id, &thread),
            })
            .await?;
        self.emit_threads(BTreeSet::from([thread_id.to_string()]));
        Ok(())
    }

    /// A message body from the cache, or from Gmail on a miss. Bodies of
    /// messages that are not stored come back uncached.
    pub async fn body(&self, message_id: &str) -> Result<MessageBody, SyncError> {
        let account_id = self.account_id;
        let now = now_millis();
        let key = message_id.to_string();
        let cached = self.db.write(move |c| bodies::get_body(c, account_id, &key, now)).await?;
        if let Some(body) = cached {
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
}
