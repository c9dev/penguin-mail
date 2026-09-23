//! Metadata from the account's mail backend, and the store written from
//! it. Every path that needs a message's metadata comes through here:
//! window pages, the inbox check, history replay, re-bootstrap, a
//! search's rows, opening a thread, and newsletter headers.
//!
//! The backend picks the calls and pays for them at the least cost it
//! can; this side notes the sync state before asking, so an answer a
//! replay overtook can be told apart, and [`store_fetched`] writes the
//! answer into the store.

use std::collections::BTreeSet;

use mailrs_domain::{AccountId, MessageMeta};
use mailrs_store::messages::Change;
use mailrs_store::{accounts, messages};
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
}

impl Fetched {
    fn new(found: Found, asked_at: Option<String>) -> Fetched {
        Fetched {
            metas: found.metas,
            gone: found.gone,
            whole: found.whole,
            asked_at,
        }
    }
}

impl AccountSync {
    /// Metadata for `wants`, from the account's backend at its least cost.
    pub(super) async fn fetch(&self, wants: Vec<Want>) -> Result<Fetched, SyncError> {
        let asked_at = self.sync_state().await?;
        Ok(Fetched::new(
            self.services.mail.fetch(wants).await?,
            asked_at,
        ))
    }

    /// Every message of each thread.
    pub(super) async fn fetch_whole(&self, threads: Vec<String>) -> Result<Fetched, SyncError> {
        let asked_at = self.sync_state().await?;
        Ok(Fetched::new(
            self.services.mail.fetch_whole(threads).await?,
            asked_at,
        ))
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
/// stored under `generation`, each id in `gone` deleted, and each thread
/// either touched refreshed. Returns the threads it touched, which the
/// caller announces once the transaction has committed.
pub(super) fn store_fetched(
    c: &Connection,
    account_id: AccountId,
    generation: i64,
    metas: &[MessageMeta],
    gone: &[String],
) -> mailrs_store::Result<BTreeSet<String>> {
    let changes: Vec<Change> = metas
        .iter()
        .map(|meta| Change::Upsert {
            meta: Box::new(meta.clone()),
            generation,
        })
        .chain(gone.iter().map(|id| Change::Delete {
            message_id: id.clone(),
        }))
        .collect();
    Ok(messages::apply(c, account_id, &changes)?.threads)
}
