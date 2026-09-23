//! Metadata from Gmail, and the store written from it. Every path that
//! needs a message's metadata comes through here: window pages, the inbox
//! check, history replay, re-bootstrap, a search's rows, opening a thread,
//! and newsletter headers.
//!
//! [`AccountSync::fetch`] takes the messages wanted and picks the calls.
//! Gmail has no batch get: a `messages.get` costs 5 units and a
//! `threads.get` 10, and the thread call answers for every message of the
//! conversation, so two or more wanted messages of one thread come in one
//! thread call for the same units or fewer. Calls run
//! [`FETCH_CONCURRENCY`] at a time, and a message Gmail no longer has
//! comes back among the gone rather than as an error. [`store_fetched`]
//! writes the answer into the store.

use std::collections::{BTreeSet, HashMap, HashSet};
use futures::StreamExt;
use mailrs_domain::{AccountId, MessageMeta};
use mailrs_gmail::{MessageRef, cost};
use mailrs_store::messages::Change;
use mailrs_store::{accounts, messages};
use rusqlite::Connection;

use super::{AccountSync, FETCH_CONCURRENCY};
use crate::{BackendError, MailBackend, SyncError};

/// One message to fetch, with its thread when the caller knows it. Only a
/// known thread lets the fetch share a `threads.get` between messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Want {
    pub id: String,
    pub thread_id: Option<String>,
}

impl Want {
    /// A message whose thread the caller does not know.
    pub fn message(id: impl Into<String>) -> Want {
        Want {
            id: id.into(),
            thread_id: None,
        }
    }
}

impl From<MessageRef> for Want {
    fn from(listed: MessageRef) -> Want {
        Want {
            id: listed.id,
            thread_id: Some(listed.thread_id),
        }
    }
}

/// What a fetch brought back.
#[derive(Debug, Default)]
pub(super) struct Fetched {
    /// The wanted messages Gmail still has, in no particular order.
    pub metas: Vec<MessageMeta>,
    /// The wanted messages Gmail no longer has. A message missing from its
    /// own thread counts: Gmail never moves a message to another thread,
    /// so it was deleted after the caller learned of it.
    pub gone: Vec<String>,
    /// Every message of each thread fetched along the way, including ones
    /// nobody wanted, for a caller that keeps whole threads.
    pub whole: Vec<Vec<MessageMeta>>,
    /// Threads asked for whole that Gmail no longer has.
    pub gone_threads: Vec<String>,
    /// The history cursor before Gmail was asked. A replay that has moved
    /// it since may have stored changes newer than this answer; see
    /// [`overtaken`].
    pub asked_at: Option<u64>,
}

/// One call to Gmail.
enum Call {
    Message(String),
    /// A thread, and the ids of it wanted; `None` wants every message.
    Thread {
        thread_id: String,
        ids: Option<HashSet<String>>,
    },
}

impl AccountSync {
    /// Metadata for `wants` at the least cost. See the module docs.
    pub(super) async fn fetch(&self, wants: Vec<Want>) -> Result<Fetched, SyncError> {
        self.run_calls(plan(wants)).await
    }

    /// Every message of each thread, one `threads.get` each.
    pub(super) async fn fetch_whole(&self, threads: Vec<String>) -> Result<Fetched, SyncError> {
        let calls = threads
            .into_iter()
            .map(|thread_id| Call::Thread {
                thread_id,
                ids: None,
            })
            .collect();
        self.run_calls(calls).await
    }

    async fn run_calls(&self, calls: Vec<Call>) -> Result<Fetched, SyncError> {
        let account_id = self.account_id;
        let asked_at = self
            .db
            .read(move |c| Ok(accounts::sync_cursor(c, account_id)?.history_id))
            .await?;
        let answers: Vec<(Call, Result<Vec<MessageMeta>, BackendError>)> =
            futures::stream::iter(calls)
                .map(|call| {
                    let mail = self.services.mail.clone();
                    async move {
                        let answer = match &call {
                            Call::Message(id) => mail.message_metadata(id).await.map(|m| vec![m]),
                            Call::Thread { thread_id, .. } => mail.thread_metadata(thread_id).await,
                        };
                        (call, answer)
                    }
                })
                .buffer_unordered(FETCH_CONCURRENCY)
                .collect()
                .await;
        let mut fetched = Fetched {
            asked_at,
            ..Fetched::default()
        };
        for (call, answer) in answers {
            match (call, answer) {
                (Call::Message(_), Ok(metas)) => fetched.metas.extend(metas),
                (Call::Message(id), Err(BackendError::NotFound)) => fetched.gone.push(id),
                (Call::Thread { ids: None, .. }, Ok(all)) => {
                    fetched.metas.extend(all.iter().cloned());
                    fetched.whole.push(all);
                }
                (Call::Thread { ids: Some(ids), .. }, Ok(all)) => {
                    let found: HashSet<&str> = all.iter().map(|m| m.id.as_str()).collect();
                    fetched.gone.extend(
                        ids.iter()
                            .filter(|id| !found.contains(id.as_str()))
                            .cloned(),
                    );
                    fetched
                        .metas
                        .extend(all.iter().filter(|m| ids.contains(&m.id)).cloned());
                    fetched.whole.push(all);
                }
                (
                    Call::Thread {
                        thread_id,
                        ids: None,
                    },
                    Err(BackendError::NotFound),
                ) => fetched.gone_threads.push(thread_id),
                (Call::Thread { ids: Some(ids), .. }, Err(BackendError::NotFound)) => {
                    fetched.gone.extend(ids)
                }
                (_, Err(err)) => return Err(err.into()),
            }
        }
        Ok(fetched)
    }
}

/// The cheapest calls that cover `wants`: a thread wherever fetching its
/// wanted messages one by one would cost at least as many units, and a
/// message call for the rest, including every message whose thread is not
/// known. Calls come in the order the wants first name each thread.
fn plan(wants: Vec<Want>) -> Vec<Call> {
    let mut order: Vec<Option<String>> = Vec::new();
    let mut by_thread: HashMap<Option<String>, Vec<String>> = HashMap::new();
    let mut seen: HashSet<String> = HashSet::new();
    for want in wants {
        if !seen.insert(want.id.clone()) {
            continue;
        }
        let ids = by_thread.entry(want.thread_id.clone()).or_default();
        if ids.is_empty() {
            order.push(want.thread_id);
        }
        ids.push(want.id);
    }
    let mut calls = Vec::new();
    for thread in order {
        let ids = by_thread.remove(&thread).unwrap_or_default();
        match thread {
            Some(thread_id) if ids.len() as u32 * cost::GET >= cost::THREAD => {
                calls.push(Call::Thread {
                    thread_id,
                    ids: Some(ids.into_iter().collect()),
                });
            }
            _ => calls.extend(ids.into_iter().map(Call::Message)),
        }
    }
    calls
}

/// Whether a history replay moved the cursor since `fetched` asked Gmail.
/// The answer may then be older than what the replay stored, and the
/// replay has moved past the change, so no later history would put it
/// right: the caller fetches again, or keeps only what the store lacks.
pub(super) fn overtaken(
    c: &Connection,
    account_id: AccountId,
    fetched: &Fetched,
) -> mailrs_store::Result<bool> {
    Ok(accounts::sync_cursor(c, account_id)?.history_id != fetched.asked_at)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn want(id: &str, thread: Option<&str>) -> Want {
        Want {
            id: id.into(),
            thread_id: thread.map(str::to_string),
        }
    }

    fn described(calls: &[Call]) -> Vec<String> {
        calls
            .iter()
            .map(|call| match call {
                Call::Message(id) => format!("message {id}"),
                Call::Thread { thread_id, ids } => {
                    let mut ids: Vec<&String> = ids.iter().flatten().collect();
                    ids.sort();
                    format!("thread {thread_id} {ids:?}")
                }
            })
            .collect()
    }

    #[test]
    fn two_messages_of_a_known_thread_share_one_call() {
        let calls = plan(vec![
            want("a", Some("t1")),
            want("b", Some("t2")),
            want("c", Some("t1")),
            want("d", None),
            want("e", None),
            want("a", Some("t1")),
        ]);
        assert_eq!(
            described(&calls),
            [
                r#"thread t1 ["a", "c"]"#,
                "message b",
                "message d",
                "message e"
            ]
        );
    }
}
