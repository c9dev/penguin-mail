//! The server's names for messages and the store's ids for them. On a
//! folder server a message that moves takes a new name, so the engine
//! hands the server each message's name from its remote ref and reads the
//! server's answers back into store ids. A label server's names are the
//! store's ids, and nothing here reads the store for one.

use std::collections::HashMap;

use mailrs_domain::{Membership, Memberships, MessageMeta};
use mailrs_store::messages;
use mailrs_store::remote_refs::{self, Resolved};

use super::AccountSync;
use crate::api::DraftRef;
use crate::{Found, MailBackend, RemoteChange, SyncError, Want};

impl AccountSync {
    /// Whether a message on this account's server takes a new name when
    /// it moves, as on a folder server, whose names are mailbox and UID.
    pub(super) fn renames(&self) -> bool {
        !self.services.mail.capabilities().labels
    }

    /// The server's name for each of `ids`, in the same order. An id
    /// without a ref goes as it is.
    pub(super) async fn remotes(&self, ids: &[String]) -> Result<Vec<String>, SyncError> {
        if !self.renames() || ids.is_empty() {
            return Ok(ids.to_vec());
        }
        let (account_id, wanted) = (self.account_id, ids.to_vec());
        let known = self
            .db
            .read(move |c| remote_refs::remotes_of(c, account_id, &wanted))
            .await?;
        Ok(ids
            .iter()
            .map(|id| known.get(id).cloned().unwrap_or_else(|| id.clone()))
            .collect())
    }

    /// The server's name for `id` now.
    pub(super) async fn remote(&self, id: &str) -> Result<String, SyncError> {
        Ok(self
            .remotes(&[id.to_string()])
            .await?
            .pop()
            .unwrap_or_else(|| id.to_string()))
    }

    /// `wants` by the names the server knows the messages by now.
    pub(super) async fn wants_by_remote(&self, wants: Vec<Want>) -> Result<Vec<Want>, SyncError> {
        if !self.renames() {
            return Ok(wants);
        }
        let ids: Vec<String> = wants.iter().map(|w| w.id.clone()).collect();
        let names = self.remotes(&ids).await?;
        Ok(wants
            .into_iter()
            .zip(names)
            .map(|(want, id)| Want {
                id,
                thread_id: want.thread_id,
            })
            .collect())
    }

    /// What each of `names` from the server stands for in the store.
    pub(super) async fn resolve(
        &self,
        names: Vec<String>,
    ) -> Result<HashMap<String, Resolved>, SyncError> {
        if !self.renames() || names.is_empty() {
            return Ok(HashMap::new());
        }
        let account_id = self.account_id;
        Ok(self
            .db
            .read(move |c| remote_refs::resolve(c, account_id, &names))
            .await?)
    }

    /// `found` with the server's names read back as store ids.
    pub(super) async fn found_as_stored(&self, found: Found) -> Result<Found, SyncError> {
        if !self.renames() {
            return Ok(found);
        }
        let resolved = self.resolve(names_in(&found)).await?;
        Ok(renamed(found, &resolved))
    }

    /// `changes` under store ids. A change that names a stale name drops
    /// out: it tells of the place a message left, and the feed reports its
    /// new place on its own.
    pub(super) async fn changes_as_stored(
        &self,
        changes: Vec<RemoteChange>,
    ) -> Result<Vec<RemoteChange>, SyncError> {
        if !self.renames() {
            return Ok(changes);
        }
        let names = changes
            .iter()
            .filter_map(named_in)
            .map(str::to_string)
            .collect();
        let resolved = self.resolve(names).await?;
        let changes: Vec<RemoteChange> = changes
            .into_iter()
            .filter_map(|change| change_as_stored(change, &resolved))
            .collect();
        self.drop_changes_already_held(changes).await
    }

    /// `drafts` under store ids. A draft whose name is stale drops out.
    pub(super) async fn drafts_as_stored(
        &self,
        drafts: Vec<DraftRef>,
    ) -> Result<Vec<DraftRef>, SyncError> {
        if !self.renames() {
            return Ok(drafts);
        }
        let names = drafts
            .iter()
            .flat_map(|d| [d.draft_id.clone(), d.message_id.clone()])
            .collect();
        let resolved = self.resolve(names).await?;
        Ok(drafts
            .into_iter()
            .filter_map(|d| {
                Some(DraftRef {
                    draft_id: stored_id(&d.draft_id, &resolved)?,
                    message_id: stored_id(&d.message_id, &resolved)?,
                })
            })
            .collect())
    }

    /// Drops the memberships a `Gained` or `Lost` change names that the
    /// store already reflects. A flag report carries a message's whole
    /// flag set, and a change this app made itself comes back in the next
    /// one, so without this a look would write what the store holds and
    /// redraw threads that did not change. Reads each named message's
    /// keywords once from the store.
    async fn drop_changes_already_held(
        &self,
        changes: Vec<RemoteChange>,
    ) -> Result<Vec<RemoteChange>, SyncError> {
        let ids: Vec<String> = changes
            .iter()
            .filter_map(|change| match change {
                RemoteChange::Gained { id, .. } | RemoteChange::Lost { id, .. } => {
                    Some(id.clone())
                }
                _ => None,
            })
            .collect();
        if ids.is_empty() {
            return Ok(changes);
        }
        let account_id = self.account_id;
        let held = self
            .db
            .read(move |c| messages::memberships_of(c, account_id, &ids))
            .await?;
        Ok(changes
            .into_iter()
            .filter_map(|change| still_to_apply(change, &held))
            .collect())
    }
}

/// The store id the server's `name` stands for: the stored message a ref
/// gives that name, the name itself for a message the store has not met,
/// or `None` for a stale name, which the message it named has left.
pub(super) fn stored_id(name: &str, resolved: &HashMap<String, Resolved>) -> Option<String> {
    match resolved.get(name) {
        Some(Resolved::Stored(id)) => Some(id.clone()),
        Some(Resolved::Stale) => None,
        None => Some(name.to_string()),
    }
}

/// The message `change` names, when it names one.
fn named_in(change: &RemoteChange) -> Option<&str> {
    match change {
        RemoteChange::Added { id, .. }
        | RemoteChange::Deleted { id }
        | RemoteChange::Gained { id, .. }
        | RemoteChange::Lost { id, .. } => Some(id),
        RemoteChange::Vanished { .. }
        | RemoteChange::Holds { .. }
        | RemoteChange::StateLost { .. }
        | RemoteChange::CompareKeywords { .. } => None,
    }
}

/// `change` under the store's id for the message it names, or `None` when
/// that name is stale.
fn change_as_stored(
    change: RemoteChange,
    resolved: &HashMap<String, Resolved>,
) -> Option<RemoteChange> {
    Some(match change {
        RemoteChange::Added { id, thread_id } => RemoteChange::Added {
            id: stored_id(&id, resolved)?,
            thread_id,
        },
        RemoteChange::Deleted { id } => RemoteChange::Deleted {
            id: stored_id(&id, resolved)?,
        },
        RemoteChange::Gained {
            id,
            thread_id,
            memberships,
        } => RemoteChange::Gained {
            id: stored_id(&id, resolved)?,
            thread_id,
            memberships,
        },
        RemoteChange::Lost { id, memberships } => RemoteChange::Lost {
            id: stored_id(&id, resolved)?,
            memberships,
        },
        // These name UIDs or a whole mailbox, which the engine reads
        // against the refs of the mailbox as they stand, so a message that
        // moved away is not there.
        whole @ (RemoteChange::Vanished { .. }
        | RemoteChange::Holds { .. }
        | RemoteChange::StateLost { .. }
        | RemoteChange::CompareKeywords { .. }) => whole,
    })
}

/// `change` with whatever `held` already gives each message dropped from
/// its memberships: a `Gained` keeps only what the message does not carry
/// yet, a `Lost` only what it still carries. `None` once nothing is left
/// to write. A message `held` does not name is not yet stored, so a
/// `Gained` for it passes through unfiltered and a `Lost` for it drops,
/// since neither writes anything a missing message can hold.
fn still_to_apply(
    change: RemoteChange,
    held: &HashMap<String, Memberships>,
) -> Option<RemoteChange> {
    match change {
        RemoteChange::Gained {
            id,
            thread_id,
            memberships,
        } => {
            let current = held.get(&id);
            let memberships: Vec<Membership> = memberships
                .into_iter()
                .filter(|m| !current.is_some_and(|h| h.has(m)))
                .collect();
            (!memberships.is_empty()).then_some(RemoteChange::Gained {
                id,
                thread_id,
                memberships,
            })
        }
        RemoteChange::Lost { id, memberships } => {
            let current = held.get(&id);
            let memberships: Vec<Membership> = memberships
                .into_iter()
                .filter(|m| current.is_some_and(|h| h.has(m)))
                .collect();
            (!memberships.is_empty()).then_some(RemoteChange::Lost { id, memberships })
        }
        other => Some(other),
    }
}

/// Every message name `found` uses, once each.
fn names_in(found: &Found) -> Vec<String> {
    let mut names: Vec<String> = found
        .metas
        .iter()
        .map(|m| m.id.clone())
        .chain(found.gone.iter().cloned())
        .chain(found.whole.iter().flatten().map(|m| m.id.clone()))
        .chain(found.links.keys().cloned())
        .chain(found.located.keys().cloned())
        .collect();
    names.sort();
    names.dedup();
    names
}

/// `found` under store ids. Whatever a stale name stood for drops out: a
/// message that moved is stored under its first id and reported under
/// its new name.
fn renamed(found: Found, resolved: &HashMap<String, Resolved>) -> Found {
    let stored_meta = |mut meta: MessageMeta| {
        meta.id = stored_id(&meta.id, resolved)?;
        Some(meta)
    };
    Found {
        metas: found.metas.into_iter().filter_map(&stored_meta).collect(),
        gone: found
            .gone
            .iter()
            .filter_map(|name| stored_id(name, resolved))
            .collect(),
        whole: found
            .whole
            .into_iter()
            .map(|thread| thread.into_iter().filter_map(&stored_meta).collect())
            .collect(),
        gone_threads: found.gone_threads,
        links: found
            .links
            .into_iter()
            .filter_map(|(name, links)| Some((stored_id(&name, resolved)?, links)))
            .collect(),
        located: found
            .located
            .into_iter()
            .filter_map(|(name, at)| Some((stored_id(&name, resolved)?, at)))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use mailrs_domain::{Location, Membership, Memberships};
    use mailrs_imap::UidSet;
    use mailrs_store::remote_refs::Resolved;

    use super::{change_as_stored, renamed, still_to_apply};
    use crate::fake::meta;
    use crate::{Found, RemoteChange};

    fn at(mailbox: &str, uidvalidity: u32, uid: u32) -> Location {
        Location {
            mailbox: mailbox.into(),
            uidvalidity,
            uid,
        }
    }

    #[test]
    fn a_moved_message_comes_back_under_its_first_id_and_its_old_name_says_nothing() {
        let found = Found {
            metas: vec![
                meta("Archive/3/10", "Archive/3/10", 0, &[]),
                meta("INBOX/7/43", "INBOX/7/43", 0, &[]),
            ],
            gone: vec!["INBOX/7/42".into(), "INBOX/7/44".into()],
            located: HashMap::from([
                ("Archive/3/10".to_string(), at("Archive", 3, 10)),
                ("INBOX/7/43".to_string(), at("INBOX", 7, 43)),
            ]),
            ..Found::default()
        };
        let resolved = HashMap::from([
            (
                "Archive/3/10".to_string(),
                Resolved::Stored("INBOX/7/42".into()),
            ),
            ("INBOX/7/42".to_string(), Resolved::Stale),
        ]);

        let found = renamed(found, &resolved);

        let ids: Vec<&str> = found.metas.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["INBOX/7/42", "INBOX/7/43"]);
        assert_eq!(found.gone, ["INBOX/7/44"]);
        assert_eq!(found.located.get("INBOX/7/42"), Some(&at("Archive", 3, 10)));
        assert!(!found.located.contains_key("Archive/3/10"));
    }

    #[test]
    fn a_change_about_a_place_a_message_left_says_nothing() {
        let resolved = HashMap::from([
            (
                "Archive/3/10".to_string(),
                Resolved::Stored("INBOX/7/42".into()),
            ),
            ("INBOX/7/42".to_string(), Resolved::Stale),
        ]);
        let vanished = RemoteChange::Deleted {
            id: "INBOX/7/42".into(),
        };
        assert_eq!(change_as_stored(vanished, &resolved), None);
        let arrived = RemoteChange::Added {
            id: "Archive/3/10".into(),
            thread_id: "Archive/3/10".into(),
        };
        assert_eq!(
            change_as_stored(arrived, &resolved),
            Some(RemoteChange::Added {
                id: "INBOX/7/42".into(),
                thread_id: "Archive/3/10".into(),
            })
        );
        let holds = RemoteChange::Holds {
            mailbox: "Archive".into(),
            uidvalidity: 3,
            uids: UidSet::from_uids([10]),
        };
        assert_eq!(change_as_stored(holds.clone(), &resolved), Some(holds));
    }

    #[test]
    fn a_gained_or_lost_change_drops_what_the_store_already_reflects() {
        let held = HashMap::from([(
            "INBOX/1/4".to_string(),
            Memberships {
                keywords: vec!["$seen".into()],
                ..Memberships::default()
            },
        )]);
        let gained = RemoteChange::Gained {
            id: "INBOX/1/4".into(),
            thread_id: "INBOX/1/4".into(),
            memberships: vec![
                Membership::Keyword("$seen".into()),
                Membership::Keyword("$flagged".into()),
            ],
        };
        assert_eq!(
            still_to_apply(gained, &held),
            Some(RemoteChange::Gained {
                id: "INBOX/1/4".into(),
                thread_id: "INBOX/1/4".into(),
                memberships: vec![Membership::Keyword("$flagged".into())],
            })
        );
        let lost_of_what_it_lacks = RemoteChange::Lost {
            id: "INBOX/1/4".into(),
            memberships: vec![Membership::Keyword("$answered".into())],
        };
        assert_eq!(still_to_apply(lost_of_what_it_lacks, &held), None);
        let lost_of_a_message_not_yet_stored = RemoteChange::Lost {
            id: "INBOX/1/9".into(),
            memberships: vec![Membership::Keyword("$seen".into())],
        };
        assert_eq!(still_to_apply(lost_of_a_message_not_yet_stored, &held), None);
    }
}
