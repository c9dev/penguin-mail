//! Metadata for the rows of a Gmail search, and the whole threads a search
//! fetched, kept so opening one of them asks Gmail nothing.
//!
//! A search returns messages, and a thread can hold more messages than the
//! search matched, so a search hit alone never tells what the whole thread
//! holds. When two or more hits share a thread, one `threads.get` costs no
//! more than their `messages.get` calls, and it answers for every message
//! of the thread. Those threads are kept here until the reader opens one.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::time::{Duration, Instant};

use mailrs_domain::query::{Query, Term, parse, resolve_names};
use mailrs_domain::{MailSet, MessageMeta, Role};
use mailrs_store::messages::Change;
use mailrs_store::{accounts, labels, messages};

use super::AccountSync;
use super::fetch::Placing;
use crate::{AnyMail, BackendError, RemoteRef, SearchQuery, SyncError, Want};

/// How long a fetched thread is kept for opening. Only memory depends on
/// it: whether a kept thread may still be stored is the history cursor's
/// call.
const KEPT_FOR: Duration = Duration::from_secs(10 * 60);

/// A whole thread a search fetched.
pub(super) struct Listed {
    at: Instant,
    /// The sync state before the server was asked. While the state stays
    /// there, no replay has passed over a change the thread missed.
    state: Option<String>,
    metas: Vec<MessageMeta>,
    /// How the thread goes into the store when it opens.
    placing: Placing,
}

/// The messages one thread had among a search's hits.
pub(super) struct Hits {
    at: Instant,
    ids: BTreeSet<String>,
}

/// What a search past the window found on the server. `store_only` says
/// the server could not say the whole query, so only the store's copy
/// answers it, and a person should hear that the results may miss older
/// mail.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Searched {
    pub refs: Vec<RemoteRef>,
    pub store_only: bool,
}

impl AccountSync {
    /// The messages a Gmail search returns, by id and thread, newest
    /// first, at most `limit` of them. One call of 5 quota units, whatever
    /// the count, so a caller takes the ids first and pays for metadata
    /// only as it shows rows. The hits are kept per thread for
    /// [`KEPT_FOR`], so Delete Forever on a listed row knows its messages
    /// without asking Gmail.
    pub async fn search_ids(
        &self,
        query: &SearchQuery,
        limit: usize,
    ) -> Result<Vec<RemoteRef>, SyncError> {
        let found = match query {
            // IMAP does not sync Junk or Trash until they are followed,
            // so their listings must ask the server even for recent mail.
            SearchQuery::Tree(Query::Term(Term::In(MailSet::Role(Role::Junk | Role::Trash))))
                if matches!(&self.services.mail, AnyMail::Imap(_)) =>
            {
                self.mail.search(query, limit).await?
            }
            // A server that reads no Gmail syntax lists folders and smart
            // mailboxes from the store's copy: sync already holds that
            // mail, and a server search would cost a round trip per
            // mailbox for the same answer.
            SearchQuery::Tree(tree) if !self.services.capabilities().native_search => {
                self.stored_matches(tree, limit).await?
            }
            _ => self.mail.search(query, limit).await?,
        };
        self.keep_hits(&found);
        Ok(found)
    }

    /// The messages `query` matches on the server, past the window the
    /// store's own search covers, at most `limit`, under the store's ids
    /// and threads where the store holds them. A server that cannot say
    /// the query answers no hits and `store_only`, and the caller runs the
    /// query over the store alone. The hits are kept as a Gmail search's
    /// are.
    pub async fn search_tree(&self, query: &Query, limit: usize) -> Result<Searched, SyncError> {
        let found = match self
            .mail
            .search(&SearchQuery::Tree(query.clone()), limit)
            .await
        {
            Ok(found) => found,
            Err(SyncError::Backend(BackendError::Unsupported)) => {
                return Ok(Searched {
                    refs: Vec::new(),
                    store_only: true,
                });
            }
            Err(err) => return Err(err),
        };
        let account_id = self.account_id;
        let refs = self
            .db
            .read(move |c| {
                let mut refs = Vec::with_capacity(found.len());
                for hit in found {
                    let thread_id =
                        messages::thread_id_of(c, account_id, &hit.id)?.unwrap_or(hit.thread_id);
                    refs.push(RemoteRef {
                        id: hit.id,
                        thread_id,
                    });
                }
                Ok(refs)
            })
            .await?;
        self.keep_hits(&refs);
        Ok(Searched {
            refs,
            store_only: false,
        })
    }

    /// The messages a listing of `query` shows. Text typed in Gmail's
    /// operators, on an account whose server reads no Gmail syntax, becomes
    /// a query tree that the store answers for the mail it holds and the
    /// server past it; `store_only` says the server did not search, so
    /// older mail may be missing. Everything else goes to `search_ids`.
    pub async fn search_listing(
        &self,
        query: &SearchQuery,
        limit: usize,
    ) -> Result<Searched, SyncError> {
        match query {
            SearchQuery::Native(text) if !self.services.capabilities().native_search => {
                self.search_typed(text, limit).await
            }
            _ => Ok(Searched {
                refs: self.search_ids(query, limit).await?,
                store_only: false,
            }),
        }
    }

    /// Typed search text on a server that reads no Gmail syntax: the
    /// store's matches, newest first, then what the server finds past
    /// them, at most `limit` from each. A typed `label:` takes the name of the
    /// mailbox Gmail would spell that way. A server search that fails
    /// leaves the store's answer, marked `store_only` as one the server
    /// cannot run is, so the person still sees what this computer holds.
    async fn search_typed(&self, text: &str, limit: usize) -> Result<Searched, SyncError> {
        let account_id = self.account_id;
        let names: Vec<String> = self
            .db
            .read(move |c| labels::list_labels(c, account_id))
            .await?
            .into_iter()
            .map(|label| label.name)
            .collect();
        let tree = resolve_names(parse(text), &names);
        let mut refs = self.stored_matches(&tree, limit).await?;
        let past = match self.search_tree(&tree, limit).await {
            Ok(past) => past,
            Err(err) => {
                tracing::warn!(error = %err, "the server search failed; the store answers alone");
                Searched {
                    refs: Vec::new(),
                    store_only: true,
                }
            }
        };
        let held: HashSet<String> = refs.iter().map(|hit| hit.id.clone()).collect();
        refs.extend(past.refs.into_iter().filter(|hit| !held.contains(&hit.id)));
        // A server alias may resolve to a stored match when metadata is read.
        // Keep both bounded sets until the listing can count unique messages.
        self.keep_hits(&refs);
        Ok(Searched {
            refs,
            store_only: past.store_only,
        })
    }

    /// The stored messages `tree` matches, newest first, at most `limit`,
    /// by store id and thread. Junk and Trash stay out unless the tree
    /// names them, as in a Gmail search.
    async fn stored_matches(
        &self,
        tree: &Query,
        limit: usize,
    ) -> Result<Vec<RemoteRef>, SyncError> {
        let (account_id, tree) = (self.account_id, tree.clone());
        let now = chrono::Local::now();
        let matched = self
            .db
            .read(move |c| mailrs_store::query::matching(c, account_id, &tree, &now, limit))
            .await?;
        Ok(matched
            .into_iter()
            .map(|hit| RemoteRef {
                id: hit.message_id,
                thread_id: hit.thread_id,
            })
            .collect())
    }

    /// Keeps a search's hits per thread for [`KEPT_FOR`].
    fn keep_hits(&self, found: &[RemoteRef]) {
        let mut by_thread: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
        for hit in found {
            by_thread
                .entry(hit.thread_id.as_str())
                .or_default()
                .insert(hit.id.clone());
        }
        let mut kept = self.hits.lock().expect("search hits poisoned");
        kept.retain(|_, hits| hits.at.elapsed() < KEPT_FOR);
        for (thread, ids) in by_thread {
            kept.insert(
                thread.to_string(),
                Hits {
                    at: Instant::now(),
                    ids,
                },
            );
        }
    }

    /// The messages of `thread_id` a recent search saw, without asking
    /// Gmail: every message of it when the search fetched it whole,
    /// otherwise the hits it listed. `None` when no search named it.
    pub(super) fn listed_ids(&self, thread_id: &str) -> Option<BTreeSet<String>> {
        let whole = self
            .listed
            .lock()
            .expect("listed threads poisoned")
            .get(thread_id)
            .filter(|listed| listed.at.elapsed() < KEPT_FOR)
            .map(|listed| listed.metas.iter().map(|m| m.id.clone()).collect());
        whole.or_else(|| {
            self.hits
                .lock()
                .expect("search hits poisoned")
                .get(thread_id)
                .filter(|hits| hits.at.elapsed() < KEPT_FOR)
                .map(|hits| hits.ids.clone())
        })
    }

    /// Lets go of what searches kept about threads that are gone.
    pub(super) fn forget_listed(&self, threads: &BTreeSet<String>) {
        let mut listed = self.listed.lock().expect("listed threads poisoned");
        let mut hits = self.hits.lock().expect("search hits poisoned");
        for thread in threads {
            listed.remove(thread);
            hits.remove(thread);
        }
    }

    /// Metadata for the messages a search listed, newest first. The store
    /// answers for the messages it already holds, which costs nothing.
    /// Two or more hits in one thread the store lacks come from one
    /// `threads.get`, which is kept for opening; each other hit costs a
    /// `messages.get`. Messages Gmail no longer has are left out.
    /// Locally threaded hits are stored first so their rows name the
    /// thread they will still belong to when opened.
    pub async fn metadata_of(&self, refs: &[RemoteRef]) -> Result<Vec<MessageMeta>, SyncError> {
        let account_id = self.account_id;
        let wanted: Vec<String> = refs.iter().map(|r| r.id.clone()).collect();
        let mut metas = self
            .db
            .read(move |c| messages::by_ids(c, account_id, &wanted))
            .await?;
        let mut held: HashSet<String> = metas.iter().map(|m| m.id.clone()).collect();
        let wants: Vec<Want> = refs
            .iter()
            .filter(|r| !held.contains(&r.id))
            .cloned()
            .map(Want::from)
            .collect();
        let mut fetched = self.fetch(wants).await?;
        // Graph search can return REST ids even when sync uses immutable
        // ids. The metadata fetch resolves them, so merge by its returned ids.
        fetched.metas.retain(|meta| held.insert(meta.id.clone()));
        if self.local_threads() {
            if !fetched.metas.is_empty() {
                let (stored, touched) = self
                    .db
                    .write(move |c| fetched.store_missing(c, account_id))
                    .await?;
                metas.extend(stored);
                self.emit_threads(touched);
            }
            metas.sort_by(|a, b| b.date.cmp(&a.date).then_with(|| a.id.cmp(&b.id)));
            return Ok(metas);
        }
        let mut kept = self.listed.lock().expect("listed threads poisoned");
        kept.retain(|_, listed| listed.at.elapsed() < KEPT_FOR);
        for whole in fetched.whole {
            if let Some(first) = whole.first() {
                kept.insert(
                    first.thread_id.clone(),
                    Listed {
                        at: Instant::now(),
                        state: fetched.asked_at.clone(),
                        placing: fetched.placing.clone(),
                        metas: whole,
                    },
                );
            }
        }
        drop(kept);
        metas.extend(fetched.metas);
        metas.sort_by(|a, b| b.date.cmp(&a.date).then_with(|| a.id.cmp(&b.id)));
        Ok(metas)
    }

    /// One metadata page of an explicit server mailbox. It uses the same
    /// missing-metadata and local-threading path as a search, but announces
    /// no mail change: the listing itself shows these rows, and announcing
    /// them would reload the listing that is still being built. The normal
    /// window pruning still applies to the stored metadata.
    pub(crate) async fn browse_mailbox(
        &self,
        mailbox: &str,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<crate::Backfill, SyncError> {
        let _moving = self.mail.hold_moves().await;
        let mut page = self.mail.mailbox_page(mailbox, cursor, limit).await?;
        let account_id = self.account_id;
        let ids: Vec<_> = page.refs.iter().map(|r| r.id.clone()).collect();
        let held: HashSet<String> = self
            .db
            .read(move |c| messages::by_ids(c, account_id, &ids))
            .await?
            .into_iter()
            .map(|m| m.id)
            .collect();
        let wants = page
            .refs
            .iter()
            .filter(|r| !held.contains(&r.id))
            .cloned()
            .map(Want::from)
            .collect();
        let mut fetched = self.fetch(wants).await?;
        if !fetched.metas.is_empty() {
            let mailbox = mailbox.to_string();
            let renamed = self
                .db
                .write(move |c| {
                    let renamed = fetched.remap_renumbered(c, account_id, &mailbox)?;
                    fetched.store_missing(c, account_id)?;
                    Ok(renamed)
                })
                .await?;
            for hit in &mut page.refs {
                if let Some(id) = renamed.get(&hit.id) {
                    hit.id = id.clone();
                    hit.thread_id = id.clone();
                }
            }
        }
        Ok(page)
    }

    /// Opens a thread: stores the copy a search fetched whole, when it is
    /// still current, then does what [`AccountSync::ensure_thread`] does.
    /// With the copy stored, a recent history replay lets `ensure_thread`
    /// trust it and ask Gmail nothing.
    ///
    /// The copy is current while the history cursor has not moved since
    /// before Gmail was asked: a replay that moved it could have passed
    /// over a change to this thread, which the store did not hold then.
    pub async fn open_thread(&self, thread_id: &str) -> Result<(), SyncError> {
        let kept = self
            .listed
            .lock()
            .expect("listed threads poisoned")
            .remove(thread_id);
        if let Some(listed) = kept {
            let (account_id, thread) = (self.account_id, thread_id.to_string());
            let stored = self
                .db
                .write(move |c| {
                    let cursor = accounts::sync_cursor(c, account_id)?;
                    if cursor.state.is_none()
                        || cursor.state != listed.state
                        || !messages::thread_messages(c, account_id, &thread)?.is_empty()
                    {
                        return Ok(false);
                    }
                    let mut changes: Vec<Change> = listed
                        .metas
                        .iter()
                        .map(|meta| listed.placing.upsert(account_id, meta, cursor.sync_gen))
                        .collect();
                    changes.push(Change::MarkWhole {
                        thread_id: thread.clone(),
                    });
                    messages::apply(c, account_id, &changes)?;
                    listed.placing.write_refs(c, account_id)?;
                    Ok(true)
                })
                .await?;
            if stored {
                self.emit_threads(BTreeSet::from([thread_id.to_string()]));
            }
        }
        self.ensure_thread(thread_id).await
    }
}
