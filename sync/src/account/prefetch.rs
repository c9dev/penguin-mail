//! Message text fetched in the background for quick Inbox opening.
//! Adapted from zwnk's GPL-3.0-or-later patch for issue #50:
//! https://gist.github.com/zwnk/b05a6e885ae9ed11f6523d349c2a319e

use std::collections::HashSet;

use mailrs_store::bodies;

use super::AccountSync;
use crate::SyncError;

impl AccountSync {
    /// Fetches at most one missing body among the newest `count` Inbox
    /// messages. One at a time gives foreground work a chance to take the
    /// server before every fetch. Returns whether another candidate waits.
    pub(crate) async fn prefetch_bodies_step(&self, count: usize) -> Result<bool, SyncError> {
        if count == 0 || self.mail.capabilities().local_mailboxes {
            return Ok(false);
        }
        let account_id = self.account_id;
        let candidates = self
            .db
            .read(move |c| bodies::recent_inbox(c, account_id, count))
            .await?;
        let candidates: Vec<String> = {
            let mut tried = self.prefetch_tried.lock().expect("prefetch tried");
            let recent: HashSet<&str> = candidates.iter().map(|(id, _)| id.as_str()).collect();
            tried.retain(|id| recent.contains(id.as_str()));
            candidates
                .into_iter()
                .filter(|(id, cached)| !cached && !tried.contains(id))
                .map(|(id, _)| id)
                .collect()
        };
        let Some(message_id) = candidates.first() else {
            return Ok(false);
        };
        let more = candidates.len() > 1;
        match self.prefetch_body(message_id).await {
            Err(SyncError::Backend(err))
                if err.is_transient()
                    || matches!(
                        err,
                        crate::BackendError::NeedsReauth | crate::BackendError::NeedsPermission
                    ) =>
            {
                // Leave this candidate untried so the engine's backoff can retry it.
                return Err(err.into());
            }
            Err(SyncError::Store(err)) => return Err(err.into()),
            Err(err) => {
                tracing::warn!(account = self.account_id, %err, "could not prefetch a body")
            }
            Ok(_) => {}
        }
        self.prefetch_tried
            .lock()
            .expect("prefetch tried")
            .insert(message_id.clone());
        Ok(more)
    }
}

#[cfg(test)]
mod tests {
    use crate::fake::meta;
    use crate::now_millis;
    use crate::tests::harness;
    use mailrs_domain::MessageBody;

    #[tokio::test]
    async fn prefetch_tracking_stays_within_the_recent_inbox() {
        let h = harness().await;
        h.bootstrap_all().await;
        let now = now_millis();
        for i in 0..8 {
            let id = format!("m{i}");
            h.fake.seed(meta(&id, &id, now + i, &["INBOX"]));
            h.fake.with(|s| {
                s.bodies.insert(
                    id,
                    MessageBody {
                        text: Some("ready".into()),
                        ..Default::default()
                    },
                );
            });
            h.bootstrap_all().await;
            h.sync.prefetch_bodies_step(2).await.unwrap();
            assert_eq!(
                h.sync.prefetch_tried.lock().unwrap().len(),
                ((i + 1) as usize).min(2)
            );
        }
    }
}
