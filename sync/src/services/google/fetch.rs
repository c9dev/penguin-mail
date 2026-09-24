//! Metadata from Gmail at the least cost. Gmail has no batch get: a
//! `messages.get` costs 5 units and a `threads.get` 10, and the thread call
//! answers for every message of the conversation, so two or more wanted
//! messages of one thread come in one thread call for the same units or
//! fewer. Calls run `FETCH_CONCURRENCY` at a time, and a message Gmail no
//! longer has comes back among the gone rather than as an error.

use std::collections::{HashMap, HashSet};

use futures::StreamExt;
use mailrs_domain::MessageMeta;
use mailrs_gmail::{GmailError, MessageRef, cost};

use super::{Google, ID_PAGE_SIZE, paced};
use crate::api::GmailApi;
use crate::services::{Found, RemoteRef, Want};
use crate::{BackendError, FETCH_CONCURRENCY};

impl From<MessageRef> for RemoteRef {
    fn from(listed: MessageRef) -> RemoteRef {
        RemoteRef {
            id: listed.id,
            thread_id: listed.thread_id,
        }
    }
}

/// Gmail's search for the sync window: recent mail plus everything in the
/// inbox.
pub(super) fn window_query(days: i64) -> String {
    format!("{{newer_than:{days}d in:inbox}}")
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

impl<G: GmailApi> Google<G> {
    /// Every id `query` lists, narrowed to one label when `label` names
    /// one, at 500 a page for the same 5 units a page of 100 costs.
    pub(super) async fn every_id(
        &self,
        label: Option<&str>,
        query: &str,
    ) -> Result<Vec<RemoteRef>, BackendError> {
        let mut found = Vec::new();
        let mut page_token: Option<String> = None;
        loop {
            let token = page_token.as_deref();
            let page = match label {
                Some(label) => {
                    paced(self.gmail.list_labelled(label, query, token, ID_PAGE_SIZE)).await?
                }
                None => paced(self.gmail.list_messages(query, token, ID_PAGE_SIZE)).await?,
            };
            found.extend(page.messages.into_iter().map(RemoteRef::from));
            match page.next_page_token {
                Some(token) => page_token = Some(token),
                None => return Ok(found),
            }
        }
    }

    pub(super) async fn fetch_planned(&self, wants: Vec<Want>) -> Result<Found, BackendError> {
        self.run_calls(plan(wants)).await
    }

    pub(super) async fn fetch_threads(&self, threads: Vec<String>) -> Result<Found, BackendError> {
        let calls = threads
            .into_iter()
            .map(|thread_id| Call::Thread {
                thread_id,
                ids: None,
            })
            .collect();
        self.run_calls(calls).await
    }

    async fn run_calls(&self, calls: Vec<Call>) -> Result<Found, BackendError> {
        let answers: Vec<(Call, Result<Vec<MessageMeta>, GmailError>)> =
            futures::stream::iter(calls)
                .map(|call| async move {
                    let answer = match &call {
                        Call::Message(id) => paced(self.gmail.message_metadata(id))
                            .await
                            .map(|m| vec![m]),
                        Call::Thread { thread_id, .. } => {
                            paced(self.gmail.thread_metadata(thread_id)).await
                        }
                    };
                    (call, answer)
                })
                .buffer_unordered(FETCH_CONCURRENCY)
                .collect()
                .await;
        let mut found = Found::default();
        for (call, answer) in answers {
            match (call, answer) {
                (Call::Message(_), Ok(metas)) => found.metas.extend(metas),
                (Call::Message(id), Err(GmailError::NotFound)) => found.gone.push(id),
                (Call::Thread { ids: None, .. }, Ok(all)) => {
                    found.metas.extend(all.iter().cloned());
                    found.whole.push(all);
                }
                (Call::Thread { ids: Some(ids), .. }, Ok(all)) => {
                    // Gmail never moves a message to another thread, so one
                    // missing from its own was deleted after it was listed.
                    let present: HashSet<&str> = all.iter().map(|m| m.id.as_str()).collect();
                    found.gone.extend(
                        ids.iter()
                            .filter(|id| !present.contains(id.as_str()))
                            .cloned(),
                    );
                    found
                        .metas
                        .extend(all.iter().filter(|m| ids.contains(&m.id)).cloned());
                    found.whole.push(all);
                }
                (
                    Call::Thread {
                        thread_id,
                        ids: None,
                    },
                    Err(GmailError::NotFound),
                ) => found.gone_threads.push(thread_id),
                (Call::Thread { ids: Some(ids), .. }, Err(GmailError::NotFound)) => {
                    found.gone.extend(ids)
                }
                (_, Err(err)) => return Err(err.into()),
            }
        }
        Ok(found)
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use mailrs_domain::Address;

    use super::*;
    use crate::fake::{FakeGmail, meta};
    use crate::{BackendError, Google, MailBackend, SearchQuery};

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

    #[tokio::test]
    async fn a_native_search_reaches_gmail_as_typed() {
        let gmail = Arc::new(FakeGmail::new());
        let now = crate::now_millis();
        gmail.seed(meta(
            "old",
            "t1",
            now - 400 * 24 * 60 * 60 * 1000,
            &["Label_1"],
        ));
        gmail.seed(meta("new", "t2", now, &["INBOX"]));
        gmail.with(|s| {
            let old = s.messages.get_mut("old").expect("seeded");
            old.has_attachments = true;
            old.from = Some(Address {
                name: None,
                email: "bo@example.org".into(),
            });
        });
        let google = Google::new(gmail);
        let typed = SearchQuery::Native("from:bo@example.org has:attachment older_than:90d".into());
        let found = google.search(&typed, 10).await.unwrap();
        let ids: Vec<&str> = found.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["old"]);
    }

    #[tokio::test]
    async fn a_tree_search_reaches_gmail_printed_in_its_syntax() {
        let gmail = Arc::new(FakeGmail::new());
        let google = Google::new(Arc::clone(&gmail));
        let junk = SearchQuery::Tree(mailrs_domain::Folder::Junk.query());
        google.search(&junk, 10).await.unwrap();
        assert_eq!(gmail.with(|s| s.searched.clone()), ["in:spam"]);
    }

    #[tokio::test]
    async fn gmail_files_its_own_sent_mail_and_appends_nothing() {
        let google = Google::new(Arc::new(FakeGmail::new()));
        assert!(matches!(
            google.append(b"raw", "SENT").await,
            Err(BackendError::Unsupported)
        ));
    }
}
