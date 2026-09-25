//! Incremental sync: applies what the server changed since the stored sync
//! state.

use std::collections::{BTreeSet, HashMap};
use std::ops::RangeInclusive;

use mailrs_domain::{ChangeEvent, EpochMillis, Membership, MessageMeta, Role};
use mailrs_store::messages::Change;
use mailrs_store::{accounts, labels, messages, remote_refs};

use super::AccountSync;
use super::fetch::{Placing, store_fetched};
use crate::{BackendError, MailBackend, RemoteChange, SyncError, SyncState, Want};

impl AccountSync {
    /// Applies every change since the stored sync state in one transaction,
    /// then stores the new state. Bootstraps instead when there is no state
    /// yet, and lists the mail again when the server has lost its place. A
    /// mailbox the server renumbered is listed again first, alone.
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

        let mut changes = self.changes_as_stored(found.changes).await?;
        // The state is written only after the batch below, so a relisting
        // that fails here runs again at the next look.
        for change in &changes {
            if let RemoteChange::StateLost {
                mailbox,
                uidvalidity,
            } = change
            {
                self.relist_mailbox(mailbox, *uidvalidity).await?;
            }
        }
        let mut compared = Vec::new();
        for change in &changes {
            if let RemoteChange::CompareKeywords {
                mailbox,
                uidvalidity,
                uids,
            } = change
            {
                compared.extend(
                    self.keywords_that_differ(mailbox, *uidvalidity, uids.clone())
                        .await?,
                );
            }
        }
        changes.retain(|change| {
            !matches!(
                change,
                RemoteChange::StateLost { .. } | RemoteChange::CompareKeywords { .. }
            )
        });
        changes.extend(compared);
        let (fetched, placing) = self.fetch_for_history(&changes).await?;
        let mail = &self.services.mail;
        let named_mailboxes: BTreeSet<String> = changes
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
            .chain(fetched.values().flat_map(|m| m.held.mailboxes.clone()))
            .filter(|id| mail.made_by_person(id))
            .collect();
        let generation = cursor.sync_gen;
        let state = found.state;
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
                                batch.push(placing.upsert(account_id, meta, generation));
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
                                batch.push(placing.upsert(account_id, meta, generation));
                            }
                        }
                        RemoteChange::Lost { id, memberships } => {
                            batch.extend(
                                memberships.iter().map(|m| Change::of(id, m.clone(), false)),
                            );
                        }
                        // The server's set is tested one stored UID at a
                        // time, never walked: one range can name four
                        // billion UIDs.
                        RemoteChange::Vanished {
                            mailbox,
                            uidvalidity,
                            uids,
                        } => {
                            let gone =
                                remote_refs::in_mailbox_where(c, account_id, mailbox, |v, uid| {
                                    v == *uidvalidity && uids.contains(uid)
                                })?;
                            batch.extend(
                                gone.into_iter()
                                    .map(|message_id| Change::Delete { message_id }),
                            );
                        }
                        RemoteChange::Holds {
                            mailbox,
                            uidvalidity,
                            uids,
                        } => {
                            let gone =
                                remote_refs::in_mailbox_where(c, account_id, mailbox, |v, uid| {
                                    v != *uidvalidity || !uids.contains(uid)
                                })?;
                            batch.extend(
                                gone.into_iter()
                                    .map(|message_id| Change::Delete { message_id }),
                            );
                        }
                        // Relisted and compared above.
                        RemoteChange::StateLost { .. } | RemoteChange::CompareKeywords { .. } => {}
                    }
                }
                let touched = messages::apply(c, account_id, &batch)?.threads;
                placing.write_refs(c, account_id)?;
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

    /// The keyword changes that bring the stored messages located in
    /// `mailbox` under `uidvalidity`, with a UID in `uids`, to what the
    /// server says they carry. The server's answer comes a window at a
    /// time and each window is compared with the store's rows as they are
    /// read, so this holds one window's keywords and the changes found.
    /// Only a keyword the server stores can be lost; one kept on this
    /// computer alone stays.
    async fn keywords_that_differ(
        &self,
        mailbox: &str,
        uidvalidity: u32,
        uids: RangeInclusive<u32>,
    ) -> Result<Vec<RemoteChange>, SyncError> {
        let account_id = self.account_id;
        let (mut from, to) = (*uids.start(), *uids.end());
        let mut changes = Vec::new();
        while from <= to {
            let page = self
                .services
                .mail
                .keywords_in(mailbox, uidvalidity, from..=to)
                .await?;
            let Some(covered) = page.covered else {
                break;
            };
            let next = covered.end().checked_add(1);
            // This mailbox's own PERMANENTFLAGS, not the Inbox's: Archive
            // or another mailbox can store a different set of keywords.
            let storable = page.storable;
            let (name, found) = (mailbox.to_string(), page.found);
            changes.extend(
                self.db
                    .read(move |c| {
                        let mut changes = Vec::new();
                        remote_refs::keywords_in(
                            c,
                            account_id,
                            &name,
                            uidvalidity,
                            covered,
                            |uid, id, held| {
                                if let Ok(at) = found.binary_search_by_key(&uid, |k| k.uid) {
                                    changes.extend(keyword_changes(
                                        id,
                                        &found[at].keywords,
                                        held,
                                        storable,
                                    ));
                                }
                            },
                        )?;
                        Ok(changes)
                    })
                    .await?,
            );
            match next {
                Some(next) => from = next,
                None => break,
            }
        }
        Ok(changes)
    }

    /// Lists `mailbox` again after the server renumbered it, its UIDs now
    /// under `uidvalidity`. A listed message the store already held,
    /// matched by its `Message-ID` and its date, keeps its id, its thread
    /// and its cached body and takes its new location. A stored message
    /// the listing lacks is deleted, and the rest arrive as new mail does,
    /// without a new-mail notice. The listing goes [`RELIST_BATCH`]
    /// messages at a time, so what it holds stays one batch's metadata
    /// whatever the mailbox's size; a batch matched and stored is not
    /// matched again if a later one fails and the look runs again.
    pub(super) async fn relist_mailbox(&self, mailbox: &str, uidvalidity: u32) -> Result<(), SyncError> {
        let account_id = self.account_id;
        let listed = self
            .services
            .mail
            .window_ids(self.window_days, Some(mailbox))
            .await?;
        let generation = self
            .db
            .read(move |c| Ok(accounts::sync_cursor(c, account_id)?.sync_gen))
            .await?;
        let (mut kept, mut touched) = (0, BTreeSet::new());
        for batch in listed.chunks(RELIST_BATCH) {
            let mut fetched = self
                .fetch(batch.iter().cloned().map(Want::from).collect())
                .await?;
            let msgids: Vec<String> = fetched
                .metas
                .iter()
                .filter_map(|m| m.rfc822_msgid.clone())
                .collect();
            let name = mailbox.to_string();
            let held = self
                .db
                .read(move |c| {
                    remote_refs::renumbered_by_message_id(c, account_id, &name, uidvalidity, &msgids)
                })
                .await?;
            // Two stored messages can share a Message-ID and a date; each
            // listed one takes one of them.
            let mut by_key: HashMap<(String, EpochMillis), Vec<String>> = HashMap::new();
            for (msgid, date, id) in held {
                by_key.entry((msgid, date)).or_default().push(id);
            }
            for meta in &mut fetched.metas {
                let Some(msgid) = meta.rfc822_msgid.clone() else {
                    continue;
                };
                if let Some(old) = by_key.get_mut(&(msgid, meta.date)).and_then(Vec::pop) {
                    fetched.placing.rename(&meta.id, &old);
                    meta.id = old;
                    kept += 1;
                }
            }
            touched.extend(
                self.db
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
                    .await?,
            );
        }
        // What the listing matched now sits under `uidvalidity`; whatever
        // still sits under another is gone.
        let name = mailbox.to_string();
        let gone = self
            .db
            .write(move |c| {
                let gone = remote_refs::in_mailbox_where(c, account_id, &name, |v, _| {
                    v != uidvalidity
                })?;
                let deletes: Vec<Change> = gone
                    .iter()
                    .map(|id| Change::Delete {
                        message_id: id.clone(),
                    })
                    .collect();
                let touched = messages::apply(c, account_id, &deletes)?.threads;
                Ok((gone.len(), touched))
            })
            .await?;
        touched.extend(gone.1);
        tracing::info!(
            account = account_id,
            mailbox,
            listed = listed.len(),
            kept,
            gone = gone.0,
            "the server renumbered a mailbox; listed it again"
        );
        self.emit_threads(touched);
        Ok(())
    }

    /// Metadata for the messages the changes add, plus messages that moved
    /// into the inbox from outside the window, and how to store them.
    async fn fetch_for_history(
        &self,
        changes: &[RemoteChange],
    ) -> Result<(HashMap<String, MessageMeta>, Placing), SyncError> {
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
        let fetched = self.fetch(wanted).await?;
        let metas = fetched
            .metas
            .into_iter()
            .map(|m| (m.id.clone(), m))
            .collect();
        Ok((metas, fetched.placing))
    }
}

/// The changes that bring stored message `id`, which carries `held`, to
/// the `carried` keywords the server gives it: a gain of each it lacks,
/// and a loss of each it holds that the server stores (`storable`) and no
/// longer gives.
fn keyword_changes(
    id: &str,
    carried: &[String],
    held: &[String],
    storable: &[&str],
) -> Vec<RemoteChange> {
    let gained: Vec<Membership> = carried
        .iter()
        .filter(|k| !held.contains(k))
        .map(|k| Membership::Keyword(k.clone()))
        .collect();
    let lost: Vec<Membership> = held
        .iter()
        .filter(|k| storable.contains(&k.as_str()) && !carried.contains(k))
        .map(|k| Membership::Keyword(k.clone()))
        .collect();
    let mut changes = Vec::new();
    if !gained.is_empty() {
        changes.push(RemoteChange::Gained {
            id: id.to_string(),
            thread_id: id.to_string(),
            memberships: gained,
        });
    }
    if !lost.is_empty() {
        changes.push(RemoteChange::Lost {
            id: id.to_string(),
            memberships: lost,
        });
    }
    changes
}

/// The most messages one step of a relisting fetches and matches.
const RELIST_BATCH: usize = 500;

/// Unread mail that someone else sent to the inbox.
fn is_new_inbox_mail(meta: &MessageMeta) -> bool {
    meta.in_role(Role::Inbox)
        && meta.is_unread()
        && !meta.in_role(Role::Sent)
        && !meta.in_role(Role::Drafts)
}
