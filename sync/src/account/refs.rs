//! The server's names for messages and the store's ids for them. On a
//! folder server a message that moves takes a new name, so the engine
//! hands the server each message's name from its remote ref and reads the
//! server's answers back into store ids. A label server's names are the
//! store's ids, and nothing here reads the store for one.

use std::collections::HashMap;

use mailrs_domain::MessageMeta;
use mailrs_store::remote_refs::{self, Resolved};

use super::AccountSync;
use crate::{Found, MailBackend, SyncError, Want};

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

    use mailrs_domain::Location;
    use mailrs_store::remote_refs::Resolved;

    use super::renamed;
    use crate::Found;
    use crate::fake::meta;

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
}
