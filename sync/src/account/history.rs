//! Incremental sync: applies what the server changed since the stored sync
//! state.

use std::collections::{BTreeSet, HashMap};

use mailrs_domain::{ChangeEvent, Membership, MessageMeta, Role};
use mailrs_store::messages::Change;
use mailrs_store::{accounts, labels, messages};

use super::AccountSync;
use crate::{BackendError, MailBackend, RemoteChange, SyncError, SyncState, Want};

impl AccountSync {
    /// Applies every change since the stored sync state in one transaction,
    /// then stores the new state. Bootstraps instead when there is no state
    /// yet, and lists the mail again when the server has lost its place.
    pub async fn incremental(&self) -> Result<(), SyncError> {
        let account_id = self.account_id;
        let cursor = self
            .db
            .read(move |c| accounts::sync_cursor(c, account_id))
            .await?;
        let Some(since) = cursor.state.map(SyncState::new) else {
            return self.bootstrap().await;
        };
        let found = match self.services.mail.changes(Some(&since)).await {
            Ok(found) => found,
            Err(BackendError::StateLost) => {
                tracing::info!(
                    account = account_id,
                    "the server lost its place; listing the mail again"
                );
                return self.rebootstrap().await;
            }
            Err(err) => return Err(err.into()),
        };
        if found.changes.is_empty() && found.state == since {
            self.mark_caught_up();
            return Ok(());
        }

        let fetched = self.fetch_for_history(&found.changes).await?;
        let mail = &self.services.mail;
        let named_mailboxes: BTreeSet<String> = found
            .changes
            .iter()
            .filter_map(|change| match change {
                RemoteChange::Gained { memberships, .. } => Some(memberships),
                _ => None,
            })
            .flatten()
            .filter_map(|m| match m {
                Membership::Mailbox(id) => Some(id.clone()),
                _ => None,
            })
            .chain(fetched.values().flat_map(|m| m.label_ids.clone()))
            .filter(|id| mail.made_by_person(id))
            .collect();
        let generation = cursor.sync_gen;
        let (changes, state) = (found.changes, found.state);
        let (touched, new_mail, unknown_mailbox) = self
            .db
            .write(move |c| {
                // Whether each message the changes name was stored before
                // this replay, read once, since the change set applies the
                // whole batch in one call.
                let named: Vec<String> = changes
                    .iter()
                    .filter_map(|change| match change {
                        RemoteChange::Added { id, .. } | RemoteChange::Gained { id, .. } => {
                            Some(id.clone())
                        }
                        _ => None,
                    })
                    .collect();
                let stored = messages::existing_ids(c, account_id, &named)?;
                let mut batch = Vec::new();
                let mut new_mail = Vec::new();
                for change in &changes {
                    match change {
                        RemoteChange::Added { id, .. } => {
                            if let Some(meta) = fetched.get(id) {
                                if !stored.contains(id)
                                    && is_new_inbox_mail(meta)
                                    && !new_mail.contains(id)
                                {
                                    new_mail.push(id.clone());
                                }
                                batch.push(Change::Upsert {
                                    meta: Box::new(meta.clone()),
                                    generation,
                                });
                            }
                        }
                        RemoteChange::Deleted { id } => {
                            batch.push(Change::Delete {
                                message_id: id.clone(),
                            });
                        }
                        RemoteChange::Gained {
                            id, memberships, ..
                        } if stored.contains(id) => {
                            batch.extend(
                                memberships.iter().map(|m| Change::of(id, m.clone(), true)),
                            );
                        }
                        RemoteChange::Gained { id, .. } => {
                            if let Some(meta) = fetched.get(id) {
                                batch.push(Change::Upsert {
                                    meta: Box::new(meta.clone()),
                                    generation,
                                });
                            }
                        }
                        RemoteChange::Lost { id, memberships } => {
                            batch.extend(
                                memberships.iter().map(|m| Change::of(id, m.clone(), false)),
                            );
                        }
                    }
                }
                let touched = messages::apply(c, account_id, &batch)?.threads;
                accounts::set_sync_state(c, account_id, state.as_str())?;
                let known: BTreeSet<String> = labels::list_labels(c, account_id)?
                    .into_iter()
                    .map(|l| l.id)
                    .collect();
                let unknown = named_mailboxes.iter().any(|id| !known.contains(id));
                Ok((touched, new_mail, unknown))
            })
            .await?;
        // A mailbox a person made that the store has never listed was made
        // elsewhere since the mailboxes were last listed, so the sidebar
        // lacks it.
        if unknown_mailbox {
            self.refresh_labels().await?;
        }
        self.mark_caught_up();
        self.emit_threads(touched);
        if !new_mail.is_empty() {
            self.emit(ChangeEvent::NewMail {
                account_id,
                message_ids: new_mail,
            });
        }
        Ok(())
    }

    /// Metadata for the messages the changes add, plus messages that moved
    /// into the inbox from outside the window.
    async fn fetch_for_history(
        &self,
        changes: &[RemoteChange],
    ) -> Result<HashMap<String, MessageMeta>, SyncError> {
        let account_id = self.account_id;
        let inbox = self
            .services
            .mail
            .mailbox_for(Role::Inbox)
            .map(Membership::Mailbox);
        // Each change names the message's thread, so several new messages
        // of one conversation share a thread fetch.
        let want = |id: &String, thread_id: &String| Want {
            id: id.clone(),
            thread_id: Some(thread_id.clone()),
        };
        let mut wanted: Vec<Want> = changes
            .iter()
            .filter_map(|change| match change {
                RemoteChange::Added { id, thread_id } => Some(want(id, thread_id)),
                _ => None,
            })
            .collect();
        let into_inbox: Vec<Want> = changes
            .iter()
            .filter_map(|change| match change {
                RemoteChange::Gained {
                    id,
                    thread_id,
                    memberships,
                } if inbox
                    .as_ref()
                    .is_some_and(|inbox| memberships.contains(inbox)) =>
                {
                    Some(want(id, thread_id))
                }
                _ => None,
            })
            .collect();
        if !into_inbox.is_empty() {
            let candidates: Vec<String> = into_inbox.iter().map(|w| w.id.clone()).collect();
            let known = self
                .db
                .read(move |c| messages::existing_ids(c, account_id, &candidates))
                .await?;
            wanted.extend(into_inbox.into_iter().filter(|w| !known.contains(&w.id)));
        }
        Ok(self
            .fetch(wanted)
            .await?
            .metas
            .into_iter()
            .map(|m| (m.id.clone(), m))
            .collect())
    }
}

/// Unread mail that someone else sent to INBOX.
fn is_new_inbox_mail(meta: &MessageMeta) -> bool {
    meta.in_role(Role::Inbox)
        && meta.is_unread()
        && !meta.in_role(Role::Sent)
        && !meta.in_role(Role::Drafts)
}
