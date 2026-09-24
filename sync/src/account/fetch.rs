//! Metadata from the account's mail backend, and the store written from
//! it. Every path that needs a message's metadata comes through here:
//! window pages, the inbox check, history replay, re-bootstrap, a
//! search's rows, opening a thread, and newsletter headers.
//!
//! The backend picks the calls and pays for them at the least cost it
//! can; this side notes the sync state before asking, so an answer a
//! replay overtook can be told apart, and [`store_fetched`] writes the
//! answer into the store. On a folder server the wanted ids go out under
//! the server's current names and come back as store ids, and mail from a
//! server without threads is threaded here.

use std::collections::{BTreeSet, HashMap};

use mailrs_domain::{AccountId, Location, MessageMeta};
use mailrs_store::messages::Change;
use mailrs_store::threading::Links;
use mailrs_store::{accounts, messages, remote_refs};
use rusqlite::Connection;

use super::AccountSync;
use crate::{Found, MailBackend, SyncError, Want};

/// What a fetch brought back.
#[derive(Debug, Default)]
pub(super) struct Fetched {
    /// The wanted messages the server still has, in no particular order.
    pub metas: Vec<MessageMeta>,
    /// The wanted messages the server no longer has.
    pub gone: Vec<String>,
    /// Every message of each thread fetched along the way, including ones
    /// nobody wanted, for a caller that keeps whole threads.
    pub whole: Vec<Vec<MessageMeta>>,
    /// The sync state before the server was asked. A replay that has
    /// moved it since may have stored changes newer than this answer; see
    /// [`overtaken`].
    pub asked_at: Option<String>,
    /// How the answer goes into the store.
    pub placing: Placing,
}

impl Fetched {
    fn new(found: Found, asked_at: Option<String>, local: bool) -> Fetched {
        Fetched {
            metas: found.metas,
            gone: found.gone,
            whole: found.whole,
            asked_at,
            placing: Placing::new(local, found.links, found.located),
        }
    }
}

/// How fetched messages go into the store: under the thread the server
/// named, or, for a server that keeps no threads, in the thread local
/// threading finds from the links each message's headers name. Where the
/// server said where a message sits, the message's remote ref records it.
#[derive(Debug, Clone, Default)]
pub(super) struct Placing {
    local: bool,
    links: HashMap<String, Links>,
    located: HashMap<String, Location>,
}

impl Placing {
    pub(super) fn new(
        local: bool,
        links: HashMap<String, Links>,
        located: HashMap<String, Location>,
    ) -> Placing {
        Placing {
            local,
            links,
            located,
        }
    }

    /// The change that stores `meta` under `generation`. A server without
    /// threads does not know the store's account id, so its messages take
    /// this account's.
    pub(super) fn upsert(
        &self,
        account_id: AccountId,
        meta: &MessageMeta,
        generation: i64,
    ) -> Change {
        if !self.local {
            return Change::Upsert {
                meta: Box::new(meta.clone()),
                generation,
            };
        }
        let mut meta = meta.clone();
        meta.account_id = account_id;
        Change::UpsertLocal {
            links: self.links.get(&meta.id).cloned().unwrap_or_default(),
            meta: Box::new(meta),
            generation,
        }
    }

    /// Records where the server said each message sits. A message the
    /// store does not hold gets no ref.
    pub(super) fn write_refs(
        &self,
        c: &Connection,
        account_id: AccountId,
    ) -> mailrs_store::Result<()> {
        for (id, at) in &self.located {
            remote_refs::locate(c, account_id, id, at)?;
        }
        Ok(())
    }
}

impl AccountSync {
    /// Metadata for `wants`, from the account's backend at its least cost.
    pub(super) async fn fetch(&self, wants: Vec<Want>) -> Result<Fetched, SyncError> {
        let asked_at = self.sync_state().await?;
        let wants = self.wants_by_remote(wants).await?;
        let found = self.services.mail.fetch(wants).await?;
        let found = self.found_as_stored(found).await?;
        Ok(Fetched::new(found, asked_at, self.local_threads()))
    }

    /// Every message of each thread.
    pub(super) async fn fetch_whole(&self, threads: Vec<String>) -> Result<Fetched, SyncError> {
        let asked_at = self.sync_state().await?;
        let found = self.services.mail.fetch_whole(threads).await?;
        let found = self.found_as_stored(found).await?;
        Ok(Fetched::new(found, asked_at, self.local_threads()))
    }

    /// Whether the account's server keeps no threads, so local threading
    /// places its mail.
    pub(super) fn local_threads(&self) -> bool {
        !self.services.mail.capabilities().server_threads
    }

    /// The stored sync state, read before the server is asked so an answer
    /// a replay overtook can be told apart.
    async fn sync_state(&self) -> Result<Option<String>, SyncError> {
        let account_id = self.account_id;
        Ok(self
            .db
            .read(move |c| Ok(accounts::sync_cursor(c, account_id)?.state))
            .await?)
    }
}

/// Whether a replay moved the sync state since `fetched` asked the server.
/// The answer may then be older than what the replay stored, and the
/// replay has moved past the change, so no later history would put it
/// right: the caller fetches again, or keeps only what the store lacks.
pub(super) fn overtaken(
    c: &Connection,
    account_id: AccountId,
    fetched: &Fetched,
) -> mailrs_store::Result<bool> {
    Ok(accounts::sync_cursor(c, account_id)?.state != fetched.asked_at)
}

/// Writes fetched metadata inside the caller's transaction: each meta
/// stored under `generation` as `placing` says, each id in `gone`
/// deleted, each located message's remote ref, and each thread touched
/// refreshed. Returns the threads it touched, which the caller announces
/// once the transaction has committed.
pub(super) fn store_fetched(
    c: &Connection,
    account_id: AccountId,
    generation: i64,
    metas: &[MessageMeta],
    gone: &[String],
    placing: &Placing,
) -> mailrs_store::Result<BTreeSet<String>> {
    let changes: Vec<Change> = metas
        .iter()
        .map(|meta| placing.upsert(account_id, meta, generation))
        .chain(gone.iter().map(|id| Change::Delete {
            message_id: id.clone(),
        }))
        .collect();
    let touched = messages::apply(c, account_id, &changes)?.threads;
    placing.write_refs(c, account_id)?;
    Ok(touched)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use mailrs_store::messages::Change;
    use mailrs_store::threading::Links;

    use super::Placing;
    use crate::fake::meta;

    fn links() -> HashMap<String, Links> {
        HashMap::from([(
            "b".to_string(),
            Links {
                in_reply_to: Some("<a@example.com>".into()),
                references: vec!["<a@example.com>".into()],
            },
        )])
    }

    #[test]
    fn a_server_with_threads_stores_each_message_under_its_own_thread() {
        let placing = Placing::new(false, links(), HashMap::new());
        assert_eq!(
            placing.upsert(1, &meta("b", "t1", 0, &[]), 4),
            Change::Upsert {
                meta: Box::new(meta("b", "t1", 0, &[])),
                generation: 4
            }
        );
    }

    #[test]
    fn a_server_without_threads_hands_local_threading_the_links_under_this_account() {
        let placing = Placing::new(true, links(), HashMap::new());
        let mut from_server = meta("b", "b", 0, &[]);
        from_server.account_id = 0;
        let Change::UpsertLocal {
            meta,
            links,
            generation,
        } = placing.upsert(7, &from_server, 4)
        else {
            panic!("a server without threads stores through local threading");
        };
        assert_eq!((meta.account_id, meta.id.as_str(), generation), (7, "b", 4));
        assert_eq!(links.in_reply_to.as_deref(), Some("<a@example.com>"));
    }
}
