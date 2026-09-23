//! Mail operations as Gmail label changes, sent the way that costs least.
//! Gmail files everything as a label, so each operation adds or removes
//! one, and `batchModify` takes one change over many messages.

use mailrs_domain::{Membership, gmail};
use mailrs_gmail::{BATCH_LIMIT, GmailError};

use super::{Google, paced};
use crate::api::GmailApi;
use crate::services::Unapplied;
use crate::{BackendError, MailOp};

/// Messages from which one `batchModify` beats a call each. Gmail charges
/// 50 units for the batch and 5 for every single `modify`, so ten is where
/// the batch starts paying.
const BATCH_FROM: usize = 10;

/// The labels `ops` add and remove, each list in the order the operations
/// name them. A move to a role, or a keyword Gmail has no label for, is
/// not something Gmail can do.
pub(super) fn labels_for(ops: &[MailOp]) -> Result<(Vec<String>, Vec<String>), BackendError> {
    let (mut add, mut remove) = (Vec::new(), Vec::new());
    for op in ops {
        let (membership, on) = match op {
            MailOp::AddToMailbox(id) => (Membership::Mailbox(id.clone()), true),
            MailOp::RemoveFromMailbox(id) => (Membership::Mailbox(id.clone()), false),
            MailOp::SetKeyword { keyword, on } => (Membership::Keyword(keyword.clone()), *on),
            MailOp::SetCategory { category, on } => (Membership::Category(category.clone()), *on),
            MailOp::MoveToRole(_) | MailOp::Destroy => return Err(BackendError::Unsupported),
        };
        match gmail::label_of(&membership, on) {
            Some((label, true)) => add.push(label),
            Some((label, false)) => remove.push(label),
            None => return Err(BackendError::Unsupported),
        }
    }
    Ok((add, remove))
}

impl<G: GmailApi> Google<G> {
    /// Sends `ops` over `messages`: one `batchDelete` for Destroy, else one
    /// `batchModify` per thousand messages, or a `modify` each when there
    /// are too few for a batch to pay. A batch Gmail rejects outright falls
    /// back to single calls, so one id it dislikes does not sink the whole
    /// selection.
    pub(super) async fn write(&self, messages: &[String], ops: &[MailOp]) -> Result<(), Unapplied> {
        if ops == [MailOp::Destroy] {
            return paced(self.gmail.delete_messages(messages))
                .await
                .map_err(|err| Unapplied {
                    taken: 0,
                    error: err.into(),
                });
        }
        let (add, remove) = labels_for(ops).map_err(|error| Unapplied { taken: 0, error })?;
        let batch = messages.len() >= BATCH_FROM;
        let mut taken = 0;
        for chunk in messages.chunks(BATCH_LIMIT) {
            let written = match batch {
                true => self.batch(chunk, &add, &remove).await,
                false => self.one_by_one(chunk, &add, &remove).await,
            };
            written.map_err(|refused| Unapplied {
                taken: taken + refused.taken,
                error: refused.error,
            })?;
            taken += chunk.len();
        }
        Ok(())
    }

    async fn batch(
        &self,
        ids: &[String],
        add: &[String],
        remove: &[String],
    ) -> Result<(), Unapplied> {
        match paced(self.gmail.batch_modify(ids, add, remove)).await {
            Ok(()) => Ok(()),
            // Gmail turned the batch down for the batch's own sake, which
            // a call per message may still get through.
            Err(GmailError::NotFound | GmailError::Http { status: 400, .. }) => {
                tracing::warn!("Gmail refused the batch; changing each message on its own");
                self.one_by_one(ids, add, remove).await
            }
            Err(err) => Err(Unapplied {
                taken: 0,
                error: err.into(),
            }),
        }
    }

    async fn one_by_one(
        &self,
        ids: &[String],
        add: &[String],
        remove: &[String],
    ) -> Result<(), Unapplied> {
        for (taken, id) in ids.iter().enumerate() {
            // The same labels a batch sends, rather than `messages.trash`
            // and `untrash`: untrash takes the trash label off and puts
            // nothing back, which would leave a few messages out of the
            // inbox that a batch of many would have returned to it.
            paced(self.gmail.modify_labels(id, add, remove))
                .await
                .map_err(|err| Unapplied {
                    taken,
                    error: err.into(),
                })?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use mailrs_domain::Role;
    use mailrs_domain::mailbox::keyword::{MUTED, SEEN};
    use mailrs_gmail::GmailError;

    use super::labels_for;
    use crate::fake::{FakeGmail, meta};
    use crate::{BackendError, Google, MailBackend, MailOp};

    fn owned(labels: &[&str]) -> Vec<String> {
        labels.iter().map(|l| l.to_string()).collect()
    }

    #[test]
    fn operations_become_the_labels_triage_always_sent() {
        let trash = [
            MailOp::AddToMailbox("TRASH".into()),
            MailOp::RemoveFromMailbox("INBOX".into()),
        ];
        assert_eq!(
            labels_for(&trash).unwrap(),
            (owned(&["TRASH"]), owned(&["INBOX"]))
        );
        let read = [MailOp::SetKeyword {
            keyword: SEEN.into(),
            on: true,
        }];
        assert_eq!(labels_for(&read).unwrap(), (owned(&[]), owned(&["UNREAD"])));
        let unread = [MailOp::SetKeyword {
            keyword: SEEN.into(),
            on: false,
        }];
        assert_eq!(
            labels_for(&unread).unwrap(),
            (owned(&["UNREAD"]), owned(&[]))
        );
        let mute = [
            MailOp::SetKeyword {
                keyword: MUTED.into(),
                on: true,
            },
            MailOp::RemoveFromMailbox("INBOX".into()),
        ];
        assert_eq!(
            labels_for(&mute).unwrap(),
            (owned(&["MUTE"]), owned(&["INBOX"]))
        );
        let sort = [MailOp::SetCategory {
            category: "CATEGORY_SOCIAL".into(),
            on: true,
        }];
        assert_eq!(
            labels_for(&sort).unwrap(),
            (owned(&["CATEGORY_SOCIAL"]), owned(&[]))
        );
    }

    #[test]
    fn gmail_cannot_move_to_a_role() {
        assert!(matches!(
            labels_for(&[MailOp::MoveToRole(Role::Archive)]),
            Err(BackendError::Unsupported)
        ));
    }

    #[tokio::test]
    async fn a_refused_batch_goes_one_message_at_a_time() {
        let gmail = Arc::new(FakeGmail::new());
        let ids: Vec<String> = (0..12).map(|n| format!("m{n}")).collect();
        for id in &ids {
            gmail.seed(meta(id, id, 0, &["INBOX"]));
        }
        gmail.fail_next(GmailError::Http {
            status: 400,
            body: String::new(),
        });
        let google = Google::new(Arc::clone(&gmail));

        google
            .apply(&ids, &[MailOp::RemoveFromMailbox("INBOX".into())])
            .await
            .unwrap();

        let writes = gmail.with(|s| s.remote_writes.clone());
        assert_eq!(writes.len(), 12);
        assert!(
            writes.iter().all(|w| w.starts_with("modify ")),
            "{writes:?}"
        );
    }

    #[tokio::test]
    async fn a_refusal_says_how_far_the_write_got_and_the_rest_can_follow() {
        let gmail = Arc::new(FakeGmail::new());
        for id in ["a", "b", "c"] {
            gmail.seed(meta(id, id, 0, &["INBOX"]));
        }
        gmail.fail_next(GmailError::RateLimited { retry_after: None });
        let google = Google::new(Arc::clone(&gmail));
        let archive = [MailOp::RemoveFromMailbox("INBOX".into())];

        let refused = google
            .apply(&owned(&["a", "b", "c"]), &archive)
            .await
            .unwrap_err();

        assert_eq!(refused.taken, 0);
        assert!(matches!(refused.error, BackendError::RateLimited(_)));
        google
            .apply(&owned(&["a", "b", "c"]), &archive)
            .await
            .unwrap();
        assert_eq!(gmail.with(|s| s.remote_writes.len()), 3);
    }
}
