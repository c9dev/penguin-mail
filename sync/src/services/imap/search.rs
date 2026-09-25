//! The neutral query tree as IMAP SEARCH keys (RFC 3501 section 6.4.4).
//! IMAP searches one mailbox at a time and has no key for an attachment
//! or a category, so a query naming one cannot be said, and the caller
//! answers it from the store alone.

use chrono::NaiveDate;
use mailrs_domain::query::{Query, Term, plain};
use mailrs_domain::{Location, MailSet, Role};

use super::keywords::flag_of;
use super::mailboxes::display_name;
use super::syntax::{imap_date, string};
use super::{Imap, ImapApi, Submit, window};
use crate::BackendError;
use crate::services::{MailBackend, RemoteRef, SearchQuery};

/// A query IMAP has no words for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Unsayable;

/// The one mailbox a query's top level says to search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Place {
    /// A role mailbox, or one mailbox by the server's id.
    Set(MailSet),
    /// A mailbox by the name a person typed, as a smart mailbox's label
    /// condition holds it.
    Named(String),
}

/// A query as SEARCH keys, and the mailbox its top level names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Printed {
    pub within: Option<Place>,
    pub keys: String,
}

/// `query` as SEARCH keys on `today`, the day `NewerThan` counts back
/// from. A mailbox named at the top level becomes the place searched;
/// anywhere deeper it cannot be said, since one SEARCH looks in one
/// mailbox.
pub(super) fn print(query: &Query, today: NaiveDate) -> Result<Printed, Unsayable> {
    let parts: Vec<&Query> = match query {
        Query::And(all) => all.iter().collect(),
        other => vec![other],
    };
    let mut within = None;
    let mut keys = Vec::new();
    for part in parts {
        let place = match part {
            Query::Term(Term::In(set @ (MailSet::Role(_) | MailSet::Mailbox(_)))) => {
                Some(Place::Set(set.clone()))
            }
            Query::Term(Term::MailboxNamed(name)) => Some(Place::Named(plain(name))),
            _ => None,
        };
        match place {
            Some(place) => {
                if within.replace(place).is_some() {
                    return Err(Unsayable);
                }
            }
            None => keys.extend(key(part, today)?),
        }
    }
    let keys = match keys.is_empty() {
        true => "ALL".to_string(),
        false => keys.join(" "),
    };
    Ok(Printed { within, keys })
}

/// `query` as one SEARCH key, or `None` when it drops out: a text term
/// that `plain` leaves empty says nothing, as in the store's reader and
/// in Gmail's text, and takes a `Not` around it with it.
fn key(query: &Query, today: NaiveDate) -> Result<Option<String>, Unsayable> {
    Ok(match query {
        Query::Term(term) => term_key(term, today)?,
        // An empty And holds for every message.
        Query::And(all) if all.is_empty() => Some("ALL".to_string()),
        Query::And(all) => {
            let keys = keys_of(all, today)?;
            match keys.len() {
                0 => None,
                1 => keys.into_iter().next(),
                _ => Some(format!("({})", keys.join(" "))),
            }
        }
        // An empty Or holds for none, which SEARCH has no key for.
        Query::Or(any) if any.is_empty() => return Err(Unsayable),
        Query::Or(any) => or_key(&keys_of(any, today)?),
        Query::Not(inner) => key(inner, today)?.map(|k| format!("NOT {k}")),
    })
}

/// The keys of `queries` that did not drop out.
fn keys_of(queries: &[Query], today: NaiveDate) -> Result<Vec<String>, Unsayable> {
    Ok(queries
        .iter()
        .map(|q| key(q, today))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .collect())
}

/// IMAP's OR takes two keys, so a longer list nests: `OR a OR b c`.
fn or_key(keys: &[String]) -> Option<String> {
    match keys {
        [] => None,
        [one] => Some(one.clone()),
        [first, rest @ ..] => Some(format!("OR {first} {}", or_key(rest)?)),
    }
}

fn term_key(term: &Term, today: NaiveDate) -> Result<Option<String>, Unsayable> {
    let text = |name: &str, value: &str| {
        let value = plain(value);
        (!value.is_empty()).then(|| format!("{name} {}", string(&value)))
    };
    Ok(match term {
        Term::From(who) => text("FROM", who),
        Term::To(who) => text("TO", who),
        Term::Subject(words) => text("SUBJECT", words),
        Term::Words(words) => text("TEXT", words),
        Term::Since(day) => Some(format!("SINCE {}", imap_date(*day))),
        Term::Before(day) => Some(format!("BEFORE {}", imap_date(*day))),
        // SEARCH counts whole days, so this takes in the rest of the first
        // day too, where Gmail and the store count back to the hour.
        Term::NewerThan(days) => {
            let first = today
                .checked_sub_days(chrono::Days::new(u64::from(*days)))
                .unwrap_or(NaiveDate::MIN);
            Some(format!("SINCE {}", imap_date(first)))
        }
        Term::Unread | Term::In(MailSet::Unseen) => Some("UNSEEN".to_string()),
        Term::Flagged => Some("FLAGGED".to_string()),
        Term::Larger(bytes) => Some(format!(
            "LARGER {}",
            u32::try_from((*bytes).max(0)).unwrap_or(u32::MAX)
        )),
        Term::In(MailSet::Keyword(keyword)) => Some(keyword_key(keyword)),
        // One SEARCH looks in one mailbox, so a mailbox below the top level
        // has no key, and IMAP has none for an attachment or a category.
        Term::HasAttachment
        | Term::MailboxNamed(_)
        | Term::In(MailSet::Role(_) | MailSet::Mailbox(_) | MailSet::Category(_)) => {
            return Err(Unsayable);
        }
    })
}

/// A keyword as its SEARCH key: IMAP's own key for a system flag.
fn keyword_key(keyword: &str) -> String {
    let flag = flag_of(keyword);
    match flag.strip_prefix('\\') {
        Some(system) => system.to_ascii_uppercase(),
        None => format!("KEYWORD {keyword}"),
    }
}

impl<I: ImapApi, S: Submit> Imap<I, S> {
    /// At most `limit` messages the query tree matches on the server, the
    /// newest UIDs of each mailbox first. A search typed in Gmail's syntax
    /// never reaches an IMAP server, and a tree IMAP cannot say answers
    /// `Unsupported`, which the caller reads as "ask the store".
    pub(super) async fn search_server(
        &self,
        query: &SearchQuery,
        limit: usize,
    ) -> Result<Vec<RemoteRef>, BackendError> {
        let SearchQuery::Tree(tree) = query else {
            return Err(BackendError::Unsupported);
        };
        let today = chrono::Local::now().date_naive();
        let printed = print(tree, today).map_err(|Unsayable| BackendError::Unsupported)?;
        let mailboxes = match &printed.within {
            // A name no mailbox goes by finds nothing, as Gmail finds
            // nothing for a label nobody made.
            Some(place) => self.mailbox_of(place).await?.into_iter().collect(),
            None => self.searched().await?,
        };
        let keys = format!("{} UNDELETED", printed.keys);
        let mut found = Vec::new();
        for mailbox in mailboxes {
            if found.len() >= limit {
                break;
            }
            let selected = self.select(&mailbox, None).await?;
            let top = self.top_uid(&mailbox, &selected).await?;
            let room = limit - found.len();
            let uids = self.newest_matching(&mailbox, &keys, top, room).await?;
            found.extend(uids.into_iter().map(|uid| {
                let id = Location {
                    mailbox: mailbox.clone(),
                    uidvalidity: selected.uidvalidity,
                    uid,
                }
                .to_string();
                RemoteRef {
                    thread_id: id.clone(),
                    id,
                }
            }));
        }
        Ok(found)
    }

    /// At most `room` of the newest UIDs at or below `top` in `mailbox`
    /// that match `keys`, searched a [`window::WINDOW`] at a time from the
    /// top down: a search most mail answers from recent messages stops
    /// after the first window, and only one that matches nothing recent
    /// goes further down. Never one SEARCH over the whole mailbox, which a
    /// long-lived account could answer past the client's guard budget.
    async fn newest_matching(
        &self,
        mailbox: &str,
        keys: &str,
        top: u32,
        room: usize,
    ) -> Result<Vec<u32>, BackendError> {
        let mut matched = Vec::new();
        for window in window::windows(1, top)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
        {
            if matched.len() >= room {
                break;
            }
            let (start, end) = (*window.start(), *window.end());
            let mut uids = self
                .api
                .search(mailbox, &format!("UID {start}:{end} {keys}"))
                .await?;
            uids.retain(|uid| window.contains(uid));
            uids.sort_unstable_by(|a, b| b.cmp(a));
            matched.extend(uids);
        }
        matched.truncate(room);
        Ok(matched)
    }

    /// The mailbox a query's top level names, or `None` for a name no
    /// listed mailbox goes by. A role the server has no mailbox for cannot
    /// be searched there, and the store answers.
    async fn mailbox_of(&self, place: &Place) -> Result<Option<String>, BackendError> {
        self.ensure_listed().await?;
        match place {
            Place::Set(MailSet::Role(role)) => self
                .mailbox_for(*role)
                .map(Some)
                .ok_or(BackendError::Unsupported),
            Place::Set(MailSet::Mailbox(id)) => Ok(Some(id.clone())),
            Place::Set(_) => Err(BackendError::Unsupported),
            Place::Named(name) => {
                let wanted = name.to_lowercase();
                let known = self.known();
                Ok(known
                    .folders
                    .iter()
                    .find(|f| {
                        !f.parent_only
                            && display_name(&f.id, known.delimiter).to_lowercase() == wanted
                    })
                    .map(|f| f.id.clone()))
            }
        }
    }

    /// Where a search that names no mailbox looks: the mailbox holding all
    /// mail where the server has one, else every mailbox that holds mail
    /// except Trash, Junk and a flagged view.
    async fn searched(&self) -> Result<Vec<String>, BackendError> {
        self.ensure_listed().await?;
        let known = self.known();
        if let Some(all) = known.folders.iter().find(|f| f.role == Some(Role::All)) {
            return Ok(vec![all.id.clone()]);
        }
        Ok(known
            .folders
            .iter()
            .filter(|f| {
                !f.parent_only && !f.flagged && !matches!(f.role, Some(Role::Trash | Role::Junk))
            })
            .map(|f| f.id.clone())
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;
    use mailrs_domain::query::{Query, Term};
    use mailrs_domain::{MailSet, Role};

    use super::{Place, Printed, Unsayable, print};

    fn term(term: Term) -> Query {
        Query::Term(term)
    }

    /// The day the tests print against.
    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 9, 24).unwrap()
    }

    fn keys(query: &Query) -> String {
        print(query, today()).unwrap().keys
    }

    #[test]
    fn each_term_has_its_key_and_an_and_lists_them() {
        let query = Query::And(vec![
            term(Term::From("ann@example.com".into())),
            term(Term::Subject("kites".into())),
            term(Term::Since(NaiveDate::from_ymd_opt(2026, 2, 1).unwrap())),
            term(Term::Before(NaiveDate::from_ymd_opt(2026, 3, 1).unwrap())),
            term(Term::Unread),
            term(Term::Larger(2_000_000)),
        ]);
        assert_eq!(
            print(&query, today()),
            Ok(Printed {
                within: None,
                keys: "FROM \"ann@example.com\" SUBJECT \"kites\" SINCE 1-Feb-2026 \
                       BEFORE 1-Mar-2026 UNSEEN LARGER 2000000"
                    .into(),
            })
        );
    }

    #[test]
    fn or_nests_two_at_a_time_and_not_and_and_nest_inside_it() {
        let query = Query::Or(vec![
            term(Term::From("a".into())),
            Query::Not(Box::new(term(Term::Flagged))),
            Query::And(vec![
                term(Term::To("b".into())),
                term(Term::Words("moss".into())),
            ]),
        ]);
        assert_eq!(
            keys(&query),
            "OR FROM \"a\" OR NOT FLAGGED (TO \"b\" TEXT \"moss\")"
        );
    }

    #[test]
    fn a_mailbox_at_the_top_picks_where_to_search() {
        let query = Query::And(vec![
            term(Term::In(MailSet::Role(Role::Sent))),
            term(Term::Words("kites".into())),
        ]);
        assert_eq!(
            print(&query, today()),
            Ok(Printed {
                within: Some(Place::Set(MailSet::Role(Role::Sent))),
                keys: "TEXT \"kites\"".into(),
            })
        );
        let alone = term(Term::In(MailSet::Mailbox("Receipts".into())));
        assert_eq!(print(&alone, today()).unwrap().keys, "ALL");
    }

    #[test]
    fn newer_than_counts_back_from_today_and_a_named_mailbox_picks_the_place() {
        assert_eq!(keys(&term(Term::NewerThan(7))), "SINCE 17-Sep-2026");
        assert_eq!(
            print(&term(Term::MailboxNamed("Work/Clients".into())), today()),
            Ok(Printed {
                within: Some(Place::Named("Work/Clients".into())),
                keys: "ALL".into(),
            })
        );
        let deeper = Query::Or(vec![
            term(Term::MailboxNamed("Work".into())),
            term(Term::Unread),
        ]);
        assert_eq!(print(&deeper, today()), Err(Unsayable));
    }

    #[test]
    fn keywords_are_flags_or_keywords_and_text_beyond_ascii_stays_quoted() {
        assert_eq!(keys(&term(Term::In(MailSet::flagged()))), "FLAGGED");
        assert_eq!(keys(&term(Term::In(MailSet::muted()))), "KEYWORD $muted");
        assert_eq!(keys(&term(Term::In(MailSet::Unseen))), "UNSEEN");
        // The client turns this string into a literal under CHARSET UTF-8.
        assert_eq!(keys(&term(Term::From("José".into()))), "FROM \"José\"");
    }

    #[test]
    fn text_is_cleaned_first_and_a_term_left_empty_drops_out_with_its_not() {
        assert_eq!(keys(&term(Term::From(" ann \" ".into()))), "FROM \"ann\"");
        let query = Query::And(vec![
            Query::Not(Box::new(term(Term::Subject("()".into())))),
            term(Term::Flagged),
        ]);
        assert_eq!(keys(&query), "FLAGGED");
    }

    #[test]
    fn what_imap_cannot_say_makes_the_whole_query_unsayable() {
        assert_eq!(print(&term(Term::HasAttachment), today()), Err(Unsayable));
        assert_eq!(
            print(
                &term(Term::In(MailSet::Category("CATEGORY_SOCIAL".into()))),
                today()
            ),
            Err(Unsayable)
        );
        let two_places = Query::Or(vec![
            term(Term::In(MailSet::Role(Role::Inbox))),
            term(Term::In(MailSet::Role(Role::Archive))),
        ]);
        assert_eq!(print(&two_places, today()), Err(Unsayable));
        assert_eq!(print(&Query::Or(vec![]), today()), Err(Unsayable));
    }
}
