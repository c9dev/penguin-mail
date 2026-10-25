//! The window, ids, metadata and whole threads. A listing is `Email/query`,
//! newest first, a page or an id batch a request. Each page goes on after
//! the last id the one before it named, so a message another client
//! removes meanwhile does not shift the rest past the listing.

use mailrs_domain::mailbox::keyword::SEEN;
use mailrs_domain::{Address, Memberships, MessageMeta, Role};
use mailrs_jmap::{Email, EmailAddress, EmailQuery, HELD, Hit, JmapError, META, Queried};
use serde_json::{Value, json};

use super::{Jmap, JmapApi, backend};
use crate::BackendError;
use crate::services::{Backfill, Found, RemoteRef, Want};

/// One page of the window.
const PAGE: usize = 200;

/// The most ids a window or inbox listing returns.
#[cfg(not(test))]
pub(crate) const MOST_IDS: usize = 20_000;
/// Small in tests, so a listing past the cap costs a few hundred messages.
#[cfg(test)]
pub(crate) const MOST_IDS: usize = 300;

/// Ids one call of an id listing asks for.
const IDS_PER_CALL: usize = 1000;

/// How many hits a listing goes back when it cannot go on from the last id
/// it named: a few repeats beat a skipped message.
const OVERLAP: u64 = 50;

/// The most messages of one thread a whole-thread fetch keeps.
const MOST_IN_THREAD: usize = 500;

pub(crate) fn received_millis(email: &Email) -> i64 {
    email
        .received_at
        .as_deref()
        .and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok())
        .map(|at| at.timestamp_millis())
        .unwrap_or_default()
}

/// The window's filters, listed one after the other: mail received in the
/// last `days` days, then the older mail in `inbox`, which the window holds
/// whatever its age. One `OR` would say it in one filter, but Apache James
/// refuses a mailbox under `OR`. The two never overlap (`after` takes the
/// instant itself, `before` does not), and in this order the whole listing
/// stays newest first. A window of `mailrs_sync::ALL_MAIL` (0 days) takes
/// every message, which no filter says best.
pub(super) fn window_filters(days: i64, inbox: Option<&str>) -> Vec<Option<Value>> {
    let Some(after) = window_start(days) else {
        return vec![None];
    };
    let mut filters = vec![Some(json!({ "after": after }))];
    if let Some(inbox) = inbox {
        filters.push(Some(json!({ "inMailbox": inbox, "before": after })));
    }
    filters
}

/// When a window of `days` days starts, or `None` for every message.
fn window_start(days: i64) -> Option<String> {
    (days > 0).then(|| {
        (chrono::Utc::now() - chrono::Duration::days(days))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    })
}

/// Where a listing stands: how many hits in, and the id of the last one,
/// which the next page goes on after.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct Mark {
    pub position: u64,
    pub last: Option<String>,
}

/// The hits of `found` and the mark after them.
fn marked(found: Queried, mark: &Mark) -> (Vec<Hit>, Mark) {
    let next = Mark {
        position: found.position + found.hits.len() as u64,
        last: found
            .hits
            .last()
            .map(|h| h.id.clone())
            .or_else(|| mark.last.clone()),
    };
    (found.hits, next)
}

/// A backfill cursor: which of the window's filters, and the mark in its
/// listing, `filter:position:last`. A JMAP id holds no colon (RFC 8620
/// section 1.2).
fn cursor_of(filter: usize, mark: &Mark) -> String {
    format!(
        "{filter}:{}:{}",
        mark.position,
        mark.last.as_deref().unwrap_or_default()
    )
}

fn parse_cursor(text: &str) -> Option<(usize, Mark)> {
    let mut fields = text.splitn(3, ':');
    let filter = fields.next()?.parse().ok()?;
    let position = fields.next()?.parse().ok()?;
    let last = fields.next()?;
    Some((
        filter,
        Mark {
            position,
            last: (!last.is_empty()).then(|| last.to_string()),
        },
    ))
}

fn address(from: &EmailAddress) -> Option<Address> {
    (!from.email.is_empty()).then(|| Address {
        name: from.name.clone().filter(|n| !n.is_empty()),
        email: from.email.clone(),
    })
}

fn remote_ref(hit: mailrs_jmap::Hit) -> RemoteRef {
    RemoteRef {
        id: hit.id,
        thread_id: hit.thread_id,
    }
}

impl<J: JmapApi> Jmap<J> {
    /// A message's metadata. The roles come from the last mailbox listing,
    /// so call `listed()` first.
    pub(super) fn meta_of(&self, email: &Email) -> MessageMeta {
        let mailboxes = self.filed_in(email);
        let roles: Vec<Role> = self
            .known()
            .roles
            .iter()
            .filter(|(_, id)| mailboxes.contains(id))
            .map(|(role, _)| *role)
            .collect();
        let keywords: Vec<String> = email
            .keywords
            .iter()
            .filter(|(_, on)| **on)
            .map(|(k, _)| k.clone())
            .collect();
        let people = |list: &Option<Vec<EmailAddress>>| {
            list.iter()
                .flatten()
                .filter_map(address)
                .collect::<Vec<_>>()
        };
        MessageMeta {
            account_id: 0,
            id: email.id.clone(),
            thread_id: email.thread_id.clone(),
            rfc822_msgid: email
                .message_id
                .as_ref()
                .and_then(|ids| ids.first())
                .map(|m| m.trim().trim_matches(['<', '>']).to_string()),
            from: email.from.iter().flatten().find_map(address),
            to: people(&email.to),
            cc: people(&email.cc),
            subject: email.subject.clone().unwrap_or_default(),
            date: received_millis(email),
            snippet: email
                .preview
                .as_deref()
                .unwrap_or_default()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" "),
            size: i64::try_from(email.size).unwrap_or(i64::MAX),
            has_attachments: email.has_attachment,
            held: Memberships {
                mailboxes,
                keywords,
                categories: Vec::new(),
            },
            roles,
            list_unsubscribe: email
                .list_unsubscribe
                .as_deref()
                .map(|v| v.trim().to_string()),
            one_click: email
                .list_unsubscribe_post
                .as_deref()
                .is_some_and(|v| v.to_ascii_lowercase().contains("one-click")),
        }
    }

    /// The mailboxes the store files `email` in. A copy held for Send
    /// Later sits in Drafts on the server, but it is the outbox's, not a
    /// draft: opened as one, it would go out twice. So the store files it
    /// in no mailbox until the hold ends, on every install of the app.
    pub(super) fn filed_in(&self, email: &Email) -> Vec<String> {
        let held = email.keywords.get(HELD).copied().unwrap_or(false);
        let drafts = if held {
            self.known().roles.get(&Role::Drafts).cloned()
        } else {
            None
        };
        email
            .mailbox_ids
            .iter()
            .filter(|(id, on)| **on && drafts.as_ref() != Some(*id))
            .map(|(id, _)| id.clone())
            .collect()
    }

    fn inbox_id(&self) -> Option<String> {
        self.known().roles.get(&Role::Inbox).cloned()
    }

    /// The next page of `filter`'s listing after `mark`, at most `limit`
    /// hits, and the mark after it. The page goes on right after the last
    /// id the mark names. When that message is gone, or the server takes no
    /// anchor (Apache James 3.9), the page starts `OVERLAP` hits before the
    /// mark's position and drops what came up to the last id, so it skips
    /// none and repeats a few only when the last id itself went.
    ///
    /// A refusal as `invalidArguments` turns anchors off for the session
    /// when it names the anchor, or when the same page without one passes.
    pub(super) async fn page_after(
        &self,
        filter: &Option<Value>,
        mark: &Mark,
        limit: usize,
    ) -> Result<(Vec<Hit>, Mark), JmapError> {
        let query = EmailQuery {
            filter: filter.clone(),
            position: mark.position,
            limit,
            total: false,
        };
        let Some(last) = &mark.last else {
            return Ok(marked(self.api().query(&query).await?, mark));
        };
        if self.known().no_anchor {
            return self.page_near(&query, mark, last).await;
        }
        match self.api().query_after(&query, last).await {
            Err(JmapError::Method { kind, .. }) if kind == "anchorNotFound" => {
                self.page_near(&query, mark, last).await
            }
            Err(JmapError::Method { kind, description }) if kind == "invalidArguments" => {
                if description.to_ascii_lowercase().contains("anchor") {
                    self.known().no_anchor = true;
                    return self.page_near(&query, mark, last).await;
                }
                let page = self.page_near(&query, mark, last).await?;
                self.known().no_anchor = true;
                Ok(page)
            }
            answer => Ok(marked(answer?, mark)),
        }
    }

    /// The page after `mark` found without an anchor: from `OVERLAP` hits
    /// before the mark's position, with what came up to `last` dropped.
    async fn page_near(
        &self,
        query: &EmailQuery,
        mark: &Mark,
        last: &str,
    ) -> Result<(Vec<Hit>, Mark), JmapError> {
        let limit = query.limit;
        let back = mark.position.saturating_sub(OVERLAP);
        let wider =
            limit.saturating_add(usize::try_from(mark.position - back).unwrap_or(usize::MAX));
        let found = self
            .api()
            .query(&EmailQuery {
                position: back,
                limit: wider,
                ..query.clone()
            })
            .await?;
        let Some(at) = found.hits.iter().position(|h| h.id == last) else {
            // A server that answers fewer hits than asked may stop short of
            // the mark; going on from there would never move forward.
            if found.position + (found.hits.len() as u64) < mark.position {
                return Ok(marked(self.api().query(query).await?, mark));
            }
            return Ok(marked(found, mark));
        };
        let position = found.position + at as u64 + 1;
        let mut hits = found.hits;
        hits.drain(..=at);
        if hits.is_empty() {
            // A server may answer fewer hits than asked, so a page that ends
            // at the last id does not end the listing.
            return Ok(marked(
                self.api()
                    .query(&EmailQuery {
                        position,
                        ..query.clone()
                    })
                    .await?,
                mark,
            ));
        }
        Ok(marked(
            Queried {
                hits,
                position,
                total: None,
                query_state: found.query_state,
            },
            mark,
        ))
    }

    /// A complete mailbox can be paged by position only while its query state holds.
    pub(super) async fn folder_page(
        &self,
        mailbox: &str,
        cursor: Option<&str>,
        limit: usize,
        unread: bool,
    ) -> Result<Backfill, BackendError> {
        self.listed().await?;
        let (state, position) = match cursor {
            Some(cursor) => {
                let (folder, state, position): (String, String, u64) =
                    serde_json::from_str(cursor).map_err(|_| BackendError::StateLost)?;
                if folder != mailbox {
                    return Err(BackendError::StateLost);
                }
                (Some(state), position)
            }
            None => (None, 0),
        };
        let found = self
            .api()
            .query(&EmailQuery {
                filter: Some(if unread {
                    json!({ "inMailbox": mailbox, "notKeyword": SEEN })
                } else {
                    json!({ "inMailbox": mailbox })
                }),
                position,
                limit: limit.clamp(1, IDS_PER_CALL),
                total: false,
            })
            .await
            .map_err(backend)?;
        if found.position != position || state.is_some_and(|state| state != found.query_state) {
            return Err(BackendError::StateLost);
        }
        let next = if found.hits.is_empty() {
            None
        } else {
            let position = position
                .checked_add(found.hits.len() as u64)
                .ok_or(BackendError::StateLost)?;
            Some(
                serde_json::to_string(&(mailbox, found.query_state, position))
                    .expect("a mailbox cursor serializes"),
            )
        };
        Ok(Backfill {
            refs: found.hits.into_iter().map(remote_ref).collect(),
            next,
        })
    }

    pub(super) async fn backfill_page(
        &self,
        days: i64,
        cursor: Option<&str>,
    ) -> Result<Backfill, BackendError> {
        self.listed().await?;
        let (mut at, mut mark) = match cursor {
            Some(text) => parse_cursor(text).ok_or(BackendError::StateLost)?,
            None => (0, Mark::default()),
        };
        let filters = window_filters(days, self.inbox_id().as_deref());
        let mut refs = Vec::new();
        // A page fills from one filter and then the next, so a short window
        // still brings the Inbox's older mail in the first page.
        while let Some(filter) = filters.get(at) {
            if refs.len() >= PAGE {
                return Ok(Backfill {
                    refs,
                    next: Some(cursor_of(at, &mark)),
                });
            }
            let (hits, next) = self
                .page_after(filter, &mark, PAGE - refs.len())
                .await
                .map_err(backend)?;
            // A server may answer fewer hits than asked, so only an empty
            // answer ends a filter's listing.
            match hits.is_empty() {
                true => (at, mark) = (at + 1, Mark::default()),
                false => {
                    mark = next;
                    refs.extend(hits.into_iter().map(remote_ref));
                }
            }
        }
        Ok(Backfill { refs, next: None })
    }

    /// Every id `filter` matches, `IDS_PER_CALL` a call, refused as
    /// `Unsupported` past `most`. A cut list would read as mail gone from
    /// the server to a caller that sweeps what it lacks.
    async fn every_id(
        &self,
        filter: Option<Value>,
        most: usize,
    ) -> Result<Vec<RemoteRef>, BackendError> {
        self.every_id_of(filter, most)
            .await
            .map_err(backend)?
            .ok_or(BackendError::Unsupported)
    }

    /// Every id `filter` matches, or `None` when there are more than `most`.
    async fn every_id_of(
        &self,
        filter: Option<Value>,
        most: usize,
    ) -> Result<Option<Vec<RemoteRef>>, JmapError> {
        let mut out: Vec<RemoteRef> = Vec::new();
        let mut mark = Mark::default();
        // One past `most` tells a full list from a cut one.
        while out.len() <= most {
            let limit = IDS_PER_CALL.min(most + 1 - out.len());
            let (hits, next) = self.page_after(&filter, &mark, limit).await?;
            if hits.is_empty() {
                break;
            }
            mark = next;
            // A page that went back after a lost anchor repeats only what
            // the last `OVERLAP` hits named.
            let recent = out.len().saturating_sub(OVERLAP as usize);
            let fresh: Vec<Hit> = hits
                .into_iter()
                .filter(|h| !out[recent..].iter().any(|r| r.id == h.id))
                .collect();
            out.extend(fresh.into_iter().take(most + 1 - out.len()).map(remote_ref));
        }
        Ok((out.len() <= most).then_some(out))
    }

    /// The ids the window holds, in `mailbox` when given. The Inbox is held
    /// whole whatever its age, so a window never narrows it.
    pub(super) async fn ids_in(
        &self,
        days: Option<i64>,
        mailbox: Option<&str>,
    ) -> Result<Vec<RemoteRef>, BackendError> {
        self.listed().await?;
        let inbox = self.inbox_id();
        let start = days.and_then(window_start);
        let (mailbox, start) = match (mailbox, start) {
            (Some(mailbox), Some(start)) if Some(mailbox) != inbox.as_deref() => (mailbox, start),
            (Some(mailbox), _) => {
                return self
                    .every_id(Some(json!({ "inMailbox": mailbox })), MOST_IDS)
                    .await;
            }
            (None, _) => {
                let mut out = Vec::new();
                for filter in
                    days.map_or_else(|| vec![None], |days| window_filters(days, inbox.as_deref()))
                {
                    let left = MOST_IDS - out.len();
                    out.extend(self.every_id(filter, left).await?);
                }
                return Ok(out);
            }
        };
        // A mailbox other than the Inbox lists what the window holds of it:
        // its recent mail, and its older mail that is also in the Inbox,
        // which is stored whatever its age.
        let mut out = self
            .every_id(
                Some(json!({ "inMailbox": mailbox, "after": start })),
                MOST_IDS,
            )
            .await?;
        let Some(inbox) = inbox else {
            return Ok(out);
        };
        let left = MOST_IDS - out.len();
        if !self.known().one_mailbox_a_filter {
            let both = json!({ "operator": "AND", "conditions": [{ "inMailbox": mailbox, "before": start }, { "inMailbox": inbox }] });
            match self.every_id_of(Some(both), left).await {
                Err(JmapError::Method { kind, .. }) if kind == "unsupportedFilter" => {
                    self.known().one_mailbox_a_filter = true
                }
                listed => {
                    out.extend(listed.map_err(backend)?.ok_or(BackendError::Unsupported)?);
                    return Ok(out);
                }
            }
        }
        // Apache James takes one mailbox condition a filter, so the older
        // part lists the Inbox's older mail and keeps what also sits in
        // `mailbox`.
        let older = self
            .every_id(Some(json!({ "inMailbox": inbox, "before": start })), left)
            .await?;
        let ids: Vec<String> = older.iter().map(|r| r.id.clone()).collect();
        let got = self
            .api()
            .emails(&ids, &["id", "mailboxIds"])
            .await
            .map_err(backend)?;
        let held: std::collections::HashSet<String> = got
            .list
            .into_iter()
            .filter(|e| e.mailbox_ids.get(mailbox).copied().unwrap_or(false))
            .map(|e| e.id)
            .collect();
        out.extend(older.into_iter().filter(|r| held.contains(&r.id)));
        Ok(out)
    }

    /// Every message in the Inbox, refused as `Unsupported` past
    /// `MOST_IDS`, which leaves nothing to compare.
    pub(super) async fn inbox_listing(&self) -> Result<Vec<RemoteRef>, BackendError> {
        self.listed().await?;
        let inbox = self.inbox_id().ok_or(BackendError::NotFound)?;
        let total = self
            .known()
            .mailboxes
            .get(&inbox)
            .map_or(0, |m| m.total_emails);
        if usize::try_from(total).unwrap_or(usize::MAX) > MOST_IDS {
            return Err(BackendError::Unsupported);
        }
        self.every_id(Some(json!({ "inMailbox": inbox })), MOST_IDS)
            .await
    }

    pub(super) async fn fetch_metas(&self, wants: Vec<Want>) -> Result<Found, BackendError> {
        self.listed().await?;
        let ids: Vec<String> = wants.into_iter().map(|w| w.id).collect();
        let got = self.api().emails(&ids, META).await.map_err(backend)?;
        Ok(Found {
            metas: got.list.iter().map(|e| self.meta_of(e)).collect(),
            gone: got.not_found,
            ..Found::default()
        })
    }

    pub(super) async fn fetch_threads(&self, threads: Vec<String>) -> Result<Found, BackendError> {
        self.listed().await?;
        let got = self.api().threads(&threads).await.map_err(backend)?;
        let mut found = Found {
            gone_threads: got.not_found,
            ..Found::default()
        };
        for thread in &got.threads {
            let mut metas: Vec<MessageMeta> = got
                .emails
                .iter()
                .filter(|e| e.thread_id == thread.id)
                .map(|e| self.meta_of(e))
                .collect();
            metas.sort_by_key(|m| m.date);
            metas.truncate(MOST_IN_THREAD);
            found.metas.extend(metas.iter().cloned());
            found.whole.push(metas);
        }
        Ok(found)
    }

    pub(super) async fn sent_with(&self, message_id: &str) -> Result<Option<String>, BackendError> {
        self.listed().await?;
        let sent = self.known().roles.get(&Role::Sent).cloned();
        let filter = super::outgoing::sent_copies(message_id, sent.as_deref());
        let query = EmailQuery {
            filter: Some(filter),
            position: 0,
            limit: 1,
            total: false,
        };
        let found = self.api().query(&query).await.map_err(backend)?;
        Ok(found.hits.into_iter().next().map(|h| h.id))
    }
}
