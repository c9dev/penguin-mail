//! Search: the query tree compiled into an `Email/query` filter (RFC 8621
//! section 4.4.1) with `AND`, `OR` and `NOT` operators. A tree that can
//! match nothing goes nowhere, and one the filter cannot say answers
//! `Unsupported`, as Graph's does.

use chrono::{DateTime, Local, NaiveDate, SecondsFormat, Utc};
use mailrs_domain::MailSet;
use mailrs_domain::query::{Query, Term};
use mailrs_jmap::JmapError;
use serde_json::{Value, json};

use super::fetch::Mark;
use super::{Jmap, JmapApi, Known, backend};
use crate::BackendError;
use crate::services::{RemoteRef, SearchQuery};

/// The most hits a search returns, and the most one request asks for.
const MOST_HITS: usize = 500;

/// What a tree comes to: a filter (`None` matches every message), or
/// nothing at all.
pub(super) enum Compiled {
    Filter(Option<Value>),
    Nothing,
}

fn stamp(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Midnight of `day` in this computer's zone, as JMAP's UTC date.
fn midnight(day: NaiveDate) -> Option<String> {
    let local = day
        .and_hms_opt(0, 0, 0)?
        .and_local_timezone(Local)
        .earliest()?;
    Some(stamp(local.with_timezone(&Utc)))
}

fn in_set(set: &MailSet, known: &Known) -> Result<Value, BackendError> {
    Ok(match set {
        MailSet::Role(role) => {
            json!({ "inMailbox": known.roles.get(role).ok_or(BackendError::Unsupported)? })
        }
        MailSet::Mailbox(id) => json!({ "inMailbox": id }),
        MailSet::Keyword(keyword) => json!({ "hasKeyword": keyword }),
        MailSet::Unseen => json!({ "notKeyword": "$seen" }),
        MailSet::Category(_) => return Err(BackendError::Unsupported),
    })
}

fn condition(term: &Term, known: &Known, now: DateTime<Utc>) -> Result<Value, BackendError> {
    Ok(match term {
        Term::From(who) => json!({ "from": who }),
        Term::To(who) => json!({ "to": who }),
        Term::Subject(words) => json!({ "subject": words }),
        Term::Words(words) => json!({ "text": words }),
        Term::Since(day) => json!({ "after": midnight(*day).ok_or(BackendError::Unsupported)? }),
        Term::Before(day) => json!({ "before": midnight(*day).ok_or(BackendError::Unsupported)? }),
        Term::NewerThan(days) => {
            json!({ "after": stamp(now - chrono::Duration::days(i64::from(*days))) })
        }
        Term::HasAttachment => json!({ "hasAttachment": true }),
        Term::Unread => json!({ "notKeyword": "$seen" }),
        Term::Flagged => json!({ "hasKeyword": "$flagged" }),
        Term::In(within) => in_set(within, known)?,
        Term::MailboxNamed(name) => {
            let wanted = name.to_lowercase();
            let id = known
                .paths
                .iter()
                .find(|(_, path)| path.to_lowercase() == wanted)
                .map(|(id, _)| id);
            json!({ "inMailbox": id.ok_or(BackendError::Unsupported)? })
        }
        Term::Larger(bytes) => json!({ "minSize": bytes.saturating_add(1).max(0) }),
    })
}

/// Joins the filters of `parts` under `operator`: none is no filter, one
/// stands alone.
fn join(operator: &str, mut conditions: Vec<Value>) -> Option<Value> {
    match conditions.len() {
        0 => None,
        1 => conditions.pop(),
        _ => Some(json!({ "operator": operator, "conditions": conditions })),
    }
}

pub(super) fn compile(
    query: &Query,
    known: &Known,
    now: DateTime<Utc>,
) -> Result<Compiled, BackendError> {
    Ok(match query {
        Query::Term(term) => Compiled::Filter(Some(condition(term, known, now)?)),
        Query::And(parts) => {
            let mut conditions = Vec::new();
            for part in parts {
                match compile(part, known, now)? {
                    Compiled::Nothing => return Ok(Compiled::Nothing),
                    Compiled::Filter(None) => {}
                    Compiled::Filter(Some(filter)) => conditions.push(filter),
                }
            }
            Compiled::Filter(join("AND", conditions))
        }
        Query::Or(parts) => {
            let mut conditions = Vec::new();
            for part in parts {
                match compile(part, known, now)? {
                    Compiled::Nothing => {}
                    Compiled::Filter(None) => return Ok(Compiled::Filter(None)),
                    Compiled::Filter(Some(filter)) => conditions.push(filter),
                }
            }
            match join("OR", conditions) {
                Some(filter) => Compiled::Filter(Some(filter)),
                None => Compiled::Nothing,
            }
        }
        Query::Not(inner) => match compile(inner, known, now)? {
            Compiled::Nothing => Compiled::Filter(None),
            Compiled::Filter(None) => Compiled::Nothing,
            Compiled::Filter(Some(filter)) => {
                Compiled::Filter(Some(json!({ "operator": "NOT", "conditions": [filter] })))
            }
        },
    })
}

/// A filter or sort the server cannot take leaves the search to the store,
/// as Apache James answers a mailbox under `NOT`. Only a search reads it
/// so: a window listing that fails this way must fail loudly.
fn unsayable(err: JmapError) -> BackendError {
    match &err {
        JmapError::Method { kind, .. }
            if kind == "unsupportedFilter" || kind == "unsupportedSort" =>
        {
            BackendError::Unsupported
        }
        _ => backend(err),
    }
}

impl<J: JmapApi> Jmap<J> {
    pub(super) async fn count_matches(&self, query: &Query) -> Result<i64, BackendError> {
        self.listed().await?;
        let filter = {
            let known = self.known();
            let Compiled::Filter(filter) = compile(query, &known, Utc::now())? else {
                return Ok(0);
            };
            let mut conditions: Vec<Value> = filter.into_iter().collect();
            for role in [mailrs_domain::Role::Junk, mailrs_domain::Role::Trash] {
                if !query.asks_for(&MailSet::Role(role))
                    && let Some(id) = known.roles.get(&role)
                {
                    conditions
                        .push(json!({ "operator": "NOT", "conditions": [{ "inMailbox": id }] }));
                }
            }
            join("AND", conditions)
        };
        let answer = self
            .api()
            .query(&mailrs_jmap::EmailQuery {
                filter,
                position: 0,
                limit: 0,
                total: true,
            })
            .await
            .map_err(unsayable)?;
        answer
            .total
            .and_then(|n| i64::try_from(n).ok())
            .ok_or(BackendError::Unsupported)
    }

    pub(super) async fn find(
        &self,
        query: &SearchQuery,
        limit: usize,
    ) -> Result<Vec<RemoteRef>, BackendError> {
        let SearchQuery::Tree(tree) = query else {
            // A JMAP account reads no search syntax of its own; the caller
            // parses what was typed into a tree first.
            return Err(BackendError::Unsupported);
        };
        self.listed().await?;
        let compiled = compile(tree, &self.known(), Utc::now())?;
        let Compiled::Filter(filter) = compiled else {
            return Ok(Vec::new());
        };
        let wanted = limit.min(MOST_HITS);
        let mut found: Vec<RemoteRef> = Vec::new();
        let mut mark = Mark::default();
        // A server may answer fewer hits than asked, so only an empty page
        // ends the listing early.
        while found.len() < wanted {
            let (hits, next) = self
                .page_after(&filter, &mark, wanted - found.len())
                .await
                .map_err(unsayable)?;
            if hits.is_empty() {
                break;
            }
            mark = next;
            let fresh: Vec<_> = hits
                .into_iter()
                .filter(|h| !found.iter().any(|f| f.id == h.id))
                .collect();
            found.extend(fresh.into_iter().map(|h| RemoteRef {
                id: h.id,
                thread_id: h.thread_id,
            }));
        }
        found.truncate(wanted);
        Ok(found)
    }
}
