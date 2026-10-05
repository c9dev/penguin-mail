//! The neutral query tree as KQL, the syntax Graph's `$search` reads on
//! messages. Graph's KQL has no property for read or flagged state, so a
//! query naming either cannot be said, and the caller answers it from the
//! store alone, as with IMAP. A folder named at the top level is where the
//! search looks; anywhere deeper it cannot be said.

use chrono::NaiveDate;
use mailrs_domain::MailSet;
use mailrs_domain::query::{Query, Term, plain};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Unsayable;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Place {
    Set(MailSet),
    Named(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Printed {
    pub within: Option<Place>,
    pub kql: String,
}

pub(crate) fn print(query: &Query, today: NaiveDate) -> Result<Printed, Unsayable> {
    let parts: Vec<&Query> = match query {
        Query::And(all) => all.iter().collect(),
        other => vec![other],
    };
    let mut within = None;
    let mut said = Vec::new();
    for part in parts {
        let place = match part {
            Query::Term(Term::In(set @ (MailSet::Role(_) | MailSet::Mailbox(_)))) => Some(Place::Set(set.clone())),
            Query::Term(Term::MailboxNamed(name)) => Some(Place::Named(plain(name))),
            _ => None,
        };
        match place {
            Some(place) if within.is_none() => within = Some(place),
            Some(_) => return Err(Unsayable),
            None => said.extend(clause(part, today)?),
        }
    }
    Ok(Printed { within, kql: said.join(" AND ") })
}

fn clause(query: &Query, today: NaiveDate) -> Result<Option<String>, Unsayable> {
    Ok(match query {
        Query::Term(term) => term_clause(term, today)?,
        Query::And(all) => joined(all, " AND ", today)?,
        Query::Or(any) if any.is_empty() => return Err(Unsayable),
        Query::Or(any) => joined(any, " OR ", today)?,
        Query::Not(inner) => clause(inner, today)?.map(|c| format!("NOT {c}")),
    })
}

fn joined(queries: &[Query], with: &str, today: NaiveDate) -> Result<Option<String>, Unsayable> {
    let parts: Vec<String> = queries
        .iter()
        .map(|q| clause(q, today))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .collect();
    Ok(match parts.len() {
        0 => None,
        1 => parts.into_iter().next(),
        _ => Some(format!("({})", parts.join(with))),
    })
}

/// `plain` already drops the double quotes that would end the value early.
fn quoted(value: &str) -> Option<String> {
    let value = plain(value);
    (!value.is_empty()).then(|| format!("\"{value}\""))
}

fn term_clause(term: &Term, today: NaiveDate) -> Result<Option<String>, Unsayable> {
    let date = |d: NaiveDate| d.format("%Y-%m-%d").to_string();
    Ok(match term {
        Term::From(who) => quoted(who).map(|v| format!("from:{v}")),
        Term::To(who) => quoted(who).map(|v| format!("to:{v}")),
        Term::Subject(text) => quoted(text).map(|v| format!("subject:{v}")),
        Term::Words(text) => quoted(text),
        Term::Since(day) => Some(format!("received>={}", date(*day))),
        Term::Before(day) => Some(format!("received<{}", date(*day))),
        Term::NewerThan(days) => Some(format!("received>={}", date(today - chrono::Days::new(u64::from(*days))))),
        Term::HasAttachment => Some("hasattachment:true".into()),
        Term::Larger(bytes) => Some(format!("size>{bytes}")),
        Term::Unread | Term::Flagged | Term::In(_) | Term::MailboxNamed(_) => return Err(Unsayable),
    })
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;
    use mailrs_domain::query::{Query, Term};
    use mailrs_domain::{MailSet, Role};

    use super::*;

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 9, 27).unwrap()
    }

    fn said(query: Query) -> String {
        print(&query, today()).unwrap().kql
    }

    #[test]
    fn each_term_prints_as_graphs_kql() {
        assert_eq!(said(Query::term(Term::From("ann@example.com".into()))), "from:\"ann@example.com\"");
        assert_eq!(said(Query::term(Term::Subject("weekly report".into()))), "subject:\"weekly report\"");
        assert_eq!(said(Query::term(Term::Words("lunch".into()))), "\"lunch\"");
        assert_eq!(said(Query::term(Term::HasAttachment)), "hasattachment:true");
        assert_eq!(said(Query::term(Term::NewerThan(7))), "received>=2026-09-20");
        assert_eq!(said(Query::term(Term::Before(NaiveDate::from_ymd_opt(2026, 1, 1).unwrap()))), "received<2026-01-01");
        assert_eq!(said(Query::term(Term::Since(NaiveDate::from_ymd_opt(2026, 1, 1).unwrap()))), "received>=2026-01-01");
        assert_eq!(said(Query::term(Term::Larger(1_000_000))), "size>1000000");
    }

    #[test]
    fn and_or_and_not_nest_in_parentheses() {
        let query = Query::And(vec![
            Query::term(Term::From("ann".into())),
            Query::Or(vec![Query::term(Term::Subject("a".into())), Query::term(Term::Subject("b".into()))]),
            Query::Not(Box::new(Query::term(Term::HasAttachment))),
        ]);
        assert_eq!(said(query), "from:\"ann\" AND (subject:\"a\" OR subject:\"b\") AND NOT hasattachment:true");
    }

    #[test]
    fn read_and_flagged_are_unsayable() {
        assert!(print(&Query::term(Term::Unread), today()).is_err());
        assert!(print(&Query::And(vec![Query::term(Term::From("a".into())), Query::term(Term::Flagged)]), today()).is_err());
    }

    #[test]
    fn a_folder_at_the_top_is_where_to_search() {
        let query = Query::And(vec![Query::is_in(MailSet::Role(Role::Sent)), Query::term(Term::To("bo".into()))]);
        let printed = print(&query, today()).unwrap();
        assert_eq!(printed.within, Some(Place::Set(MailSet::Role(Role::Sent))));
        assert_eq!(printed.kql, "to:\"bo\"");
        assert!(print(&Query::Or(vec![Query::is_in(MailSet::Role(Role::Sent))]), today()).is_err());
    }

    #[test]
    fn a_quote_in_a_value_is_left_out() {
        assert_eq!(said(Query::term(Term::Subject("the \"big\" day".into()))), "subject:\"the big day\"");
    }
}
