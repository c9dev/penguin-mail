//! Gmail's history as the neutral feed of changes, and the sync state that
//! says where it stands: `{"history_id":N}`, the same text migration 26
//! wrote for every account.

use mailrs_gmail::labels as gmail;
use mailrs_gmail::{GmailError, HistoryChange};
use serde::{Deserialize, Serialize};

use super::{Google, paced};
use crate::BackendError;
use crate::api::GmailApi;
use crate::services::{Changes, RemoteChange, SyncState};

#[derive(Serialize, Deserialize)]
struct GmailState {
    history_id: u64,
}

impl GmailState {
    /// A state this adapter cannot read is one it did not write, so the
    /// caller lists the mail again rather than guess.
    fn read(state: &SyncState) -> Result<GmailState, BackendError> {
        serde_json::from_str(state.as_str()).map_err(|_| BackendError::StateLost)
    }

    fn written(&self) -> SyncState {
        SyncState::new(format!("{{\"history_id\":{}}}", self.history_id))
    }
}

impl<G: GmailApi> Google<G> {
    pub(super) async fn history_changes(
        &self,
        since: Option<&SyncState>,
    ) -> Result<Changes, BackendError> {
        let Some(since) = since else {
            let profile = paced(self.gmail.profile()).await?;
            return Ok(Changes {
                changes: Vec::new(),
                state: GmailState {
                    history_id: profile.history_id,
                }
                .written(),
            });
        };
        let start = GmailState::read(since)?.history_id;
        let mut changes = Vec::new();
        let mut latest;
        let mut page_token: Option<String> = None;
        loop {
            let page = match paced(self.gmail.history(start, page_token.as_deref())).await {
                Ok(page) => page,
                // `history.list` answers 404 for a start older than the
                // history Gmail keeps; it has nothing else to miss.
                // Anywhere else a 404 is a message or a label that is gone.
                Err(GmailError::NotFound) => return Err(BackendError::StateLost),
                Err(err) => return Err(err.into()),
            };
            changes.extend(page.changes.into_iter().flat_map(neutral));
            latest = page.history_id;
            match page.next_page_token {
                Some(token) => page_token = Some(token),
                None => break,
            }
        }
        Ok(Changes {
            changes,
            state: GmailState { history_id: latest }.written(),
        })
    }
}

/// One history change as the changes it stands for. Gmail says a message
/// gained `UNREAD` where the feed says it lost `$seen`, so one label change
/// can be a gain and a loss at once.
fn neutral(change: HistoryChange) -> Vec<RemoteChange> {
    match change {
        HistoryChange::MessageAdded { id, thread_id } => {
            vec![RemoteChange::Added { id, thread_id }]
        }
        HistoryChange::MessageDeleted { id, .. } => vec![RemoteChange::Deleted { id }],
        HistoryChange::LabelsAdded {
            id,
            thread_id,
            label_ids,
        } => split(id, thread_id, &label_ids, true),
        HistoryChange::LabelsRemoved {
            id,
            thread_id,
            label_ids,
        } => split(id, thread_id, &label_ids, false),
    }
}

fn split(id: String, thread_id: String, labels: &[String], added: bool) -> Vec<RemoteChange> {
    let (mut gained, mut lost) = (Vec::new(), Vec::new());
    for label in labels {
        let (membership, carried) = gmail::membership_of(label);
        match carried == added {
            true => gained.push(membership),
            false => lost.push(membership),
        }
    }
    let mut out = Vec::new();
    if !gained.is_empty() {
        out.push(RemoteChange::Gained {
            id: id.clone(),
            thread_id,
            memberships: gained,
        });
    }
    if !lost.is_empty() {
        out.push(RemoteChange::Lost {
            id,
            memberships: lost,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use mailrs_domain::Membership;
    use mailrs_domain::mailbox::keyword::SEEN;

    use crate::fake::{FakeGmail, meta};
    use crate::{BackendError, Google, MailBackend, RemoteChange, SyncState};

    fn google() -> (Arc<FakeGmail>, Google<FakeGmail>) {
        let gmail = Arc::new(FakeGmail::new());
        (Arc::clone(&gmail), Google::new(gmail))
    }

    #[tokio::test]
    async fn with_no_state_the_feed_starts_where_gmail_stands() {
        let (_, google) = google();
        let start = google.changes(None).await.unwrap();
        assert!(start.changes.is_empty());
        assert_eq!(start.state, SyncState::new("{\"history_id\":100}"));
    }

    #[tokio::test]
    async fn gmail_marking_mail_unread_is_a_lost_seen() {
        let (gmail, google) = google();
        gmail.seed(meta("a", "t1", 0, &[]));
        let start = google.changes(None).await.unwrap().state;
        gmail.remote_relabel("a", &["INBOX", "UNREAD"], &[]);

        let found = google.changes(Some(&start)).await.unwrap();

        assert_eq!(
            found.changes,
            [
                RemoteChange::Gained {
                    id: "a".into(),
                    thread_id: "t1".into(),
                    memberships: vec![Membership::Mailbox("INBOX".into())],
                },
                RemoteChange::Lost {
                    id: "a".into(),
                    memberships: vec![Membership::Keyword(SEEN.into())],
                },
            ]
        );
        assert_ne!(found.state, start);
    }

    #[tokio::test]
    async fn a_history_cursor_gmail_no_longer_keeps_is_a_lost_place() {
        let (gmail, google) = google();
        let start = google.changes(None).await.unwrap().state;
        gmail.expire_history();
        assert!(matches!(
            google.changes(Some(&start)).await,
            Err(BackendError::StateLost)
        ));
    }

    #[tokio::test]
    async fn a_state_this_adapter_did_not_write_is_a_lost_place() {
        let (_, google) = google();
        assert!(matches!(
            google
                .changes(Some(&SyncState::new("uid 4 of INBOX")))
                .await,
            Err(BackendError::StateLost)
        ));
    }
}
