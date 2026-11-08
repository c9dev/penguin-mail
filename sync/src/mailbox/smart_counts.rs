//! Smart mailbox badges. Local reads never wait for a server search.

use super::*;
use mailrs_domain::query::{Query, Term};
use mailrs_domain::smart::UnreadCount;

pub(super) struct Cached {
    at: tokio::time::Instant,
    changes: u64,
    answer: Option<i64>,
    pending: bool,
    request: u64,
    task: tokio::task::AbortHandle,
}

impl Drop for Cached {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl<A: Accounts> Mailboxes<A> {
    pub(super) async fn smart_counts(
        &self,
        sidebar: &[Mailbox],
        refresh: bool,
        now: DateTime<Local>,
    ) -> Result<(HashMap<Mailbox, i64>, HashMap<Mailbox, SmartCountState>), SyncError> {
        let wanted: HashSet<SmartMailbox> = sidebar
            .iter()
            .filter_map(|mailbox| match mailbox {
                Mailbox::Smart(smart) if smart.unread_count != UnreadCount::Hidden => {
                    Some(smart.clone())
                }
                _ => None,
            })
            .collect();
        if wanted.is_empty() {
            self.smart_unread
                .lock()
                .expect("smart counts poisoned")
                .clear();
            return Ok((HashMap::new(), HashMap::new()));
        }
        let (local, jobs) = self
            .db
            .read(move |c| {
                let accounts =
                    mailrs_domain::mail_accounts(&mailrs_store::accounts::list_accounts(c)?);
                let mut local = HashMap::new();
                let mut jobs = HashMap::new();
                for smart in wanted {
                    let mut count = 0;
                    let mut queries = Vec::new();
                    for account in &accounts {
                        let Some(query) = smart.query_for(&account.email) else {
                            continue;
                        };
                        match smart.unread_count {
                            UnreadCount::Local => {
                                count +=
                                    mailrs_store::query::unread_count(c, account.id, &query, &now)?
                            }
                            UnreadCount::Server => queries.push((account.id, with_unread(query))),
                            UnreadCount::Hidden => {}
                        }
                    }
                    match smart.unread_count {
                        UnreadCount::Local => {
                            local.insert(Mailbox::Smart(smart), count);
                        }
                        UnreadCount::Server => {
                            jobs.insert(smart, queries);
                        }
                        UnreadCount::Hidden => {}
                    }
                }
                Ok((local, jobs))
            })
            .await?;
        let mut states: HashMap<_, _> = local
            .keys()
            .cloned()
            .map(|mailbox| (mailbox, SmartCountState::Local))
            .collect();
        let mut counts = local;
        let mut cached = self.smart_unread.lock().expect("smart counts poisoned");
        cached.retain(|(smart, id), _| {
            jobs.get(smart)
                .is_some_and(|queries| queries.iter().any(|(account, _)| account == id))
        });
        for (smart, queries) in jobs {
            let mut total = 0;
            let mut status = SmartCountState::Server;
            for (id, query) in queries {
                let key = (smart.clone(), id);
                let Some(sync) = self.accounts.account(id) else {
                    cached.remove(&key);
                    status = SmartCountState::Unavailable;
                    continue;
                };
                let changes = sync.mail_changes();
                let fresh = cached.get(&key).is_some_and(|kept| {
                    kept.changes == changes && kept.at.elapsed() < REMOTE_FRESH
                });
                if fresh && let Some(kept) = cached.get(&key) {
                    if kept.pending {
                        if status != SmartCountState::Unavailable {
                            status = SmartCountState::Pending;
                        }
                    } else if let Some(answer) = kept.answer {
                        total += answer;
                    } else {
                        status = SmartCountState::Unavailable;
                    }
                    continue;
                }
                if status != SmartCountState::Unavailable {
                    status = SmartCountState::Pending;
                }
                // Finish a running search before replacing it during a sync burst.
                if !refresh || cached.get(&key).is_some_and(|kept| kept.pending) {
                    continue;
                }
                let request = self.unread_request.fetch_add(1, Ordering::Relaxed);
                let state = Arc::downgrade(&self.smart_unread);
                let changed = self.unread_changed.clone();
                let mut moved = sync.watch_mail_changes();
                let task_key = key.clone();
                let task = tokio::spawn(async move {
                    let answer = tokio::time::timeout(
                        Duration::from_secs(120),
                        sync.services().mail.count_query(&query),
                    )
                    .await
                    .ok()
                    .and_then(Result::ok);
                    drop(sync);
                    let Some(current) = state.upgrade() else {
                        return;
                    };
                    {
                        let mut cached = current.lock().expect("smart counts poisoned");
                        let Some(kept) = cached
                            .get_mut(&task_key)
                            .filter(|kept| kept.request == request)
                        else {
                            return;
                        };
                        kept.answer = answer;
                        kept.pending = false;
                        kept.at = tokio::time::Instant::now();
                    }
                    changed.send_replace(());
                    drop(current);
                    tokio::select! {
                        () = tokio::time::sleep(REMOTE_FRESH) => {}
                        Ok(()) = moved.changed() => {}
                    }
                    if let Some(current) = state.upgrade()
                        && current
                            .lock()
                            .expect("smart counts poisoned")
                            .get(&task_key)
                            .is_some_and(|kept| kept.request == request)
                    {
                        changed.send_replace(());
                    }
                });
                cached.insert(
                    key,
                    Cached {
                        at: tokio::time::Instant::now(),
                        changes,
                        answer: None,
                        pending: true,
                        request,
                        task: task.abort_handle(),
                    },
                );
            }
            let mailbox = Mailbox::Smart(smart);
            if status == SmartCountState::Server {
                counts.insert(mailbox.clone(), total);
            }
            states.insert(mailbox, status);
        }
        Ok((counts, states))
    }
}

fn with_unread(query: Query) -> Query {
    let mut parts = match query {
        Query::And(parts) => parts,
        other => vec![other],
    };
    parts.push(Query::term(Term::Unread));
    Query::And(parts)
}
