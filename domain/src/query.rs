//! A search in words every provider shares. Folders and smart mailboxes
//! are trees of these; the store runs a tree over its own copy of the
//! mail, and each adapter prints one in its server's syntax.

use chrono::NaiveDate;

use crate::MailSet;

mod typed;

pub use typed::{MAX_DEPTH, parse, resolve_names};

/// Bytes in the megabyte a smart mailbox's size condition counts in.
pub const MEGABYTE: i64 = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Query {
    Term(Term),
    /// Every one holds. An empty list holds for every message.
    And(Vec<Query>),
    /// At least one holds. An empty list holds for none.
    Or(Vec<Query>),
    Not(Box<Query>),
}

/// One condition on a message. Text terms keep the text as the person
/// typed it, trimmed; each reader takes out what its syntax cannot carry.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Term {
    From(String),
    To(String),
    Subject(String),
    Words(String),
    /// On or after this day, in the local zone.
    Since(NaiveDate),
    /// Before this day, in the local zone.
    Before(NaiveDate),
    /// Received within this many days of now.
    NewerThan(u32),
    HasAttachment,
    Unread,
    Flagged,
    In(MailSet),
    /// In the server mailbox a person named, by its name rather than its
    /// id, as a smart mailbox's label condition holds it.
    MailboxNamed(String),
    /// Bigger than this many bytes.
    Larger(i64),
}

impl Query {
    pub fn term(term: Term) -> Query {
        Query::Term(term)
    }

    /// Mail in the set.
    pub fn is_in(set: MailSet) -> Query {
        Query::Term(Term::In(set))
    }

    /// Mail outside the set.
    pub fn not_in(set: MailSet) -> Query {
        Query::Not(Box::new(Query::is_in(set)))
    }

    /// Whether the tree names `set` outside a `Not`, as a folder that
    /// lists the Trash does.
    pub fn asks_for(&self, set: &MailSet) -> bool {
        match self {
            Query::Term(Term::In(named)) => named == set,
            Query::Term(_) | Query::Not(_) => false,
            Query::And(items) | Query::Or(items) => items.iter().any(|q| q.asks_for(set)),
        }
    }
}

/// `text` without double quotes or parentheses and without the spaces at
/// either end: what a text term means once the characters Gmail's syntax
/// reserves are gone. Empty when nothing else was there.
pub fn plain(text: &str) -> String {
    text.chars()
        .filter(|c| !matches!(c, '"' | '(' | ')'))
        .collect::<String>()
        .trim()
        .to_string()
}

/// `query`'s conditions in plain English words, comma-joined, such as
/// "from ana, unread, last 7 days". Written for an account that reads no
/// search syntax of its own, so a person or a model can tell what it
/// fetches without knowing any provider's operators.
pub fn describe(query: &Query) -> String {
    match query {
        Query::Term(term) => describe_term(term),
        Query::And(items) => descriptions(items).join(", "),
        Query::Or(items) => descriptions(items).join(" or "),
        Query::Not(inner) => {
            let described = describe(inner);
            if described.is_empty() {
                String::new()
            } else {
                format!("not {described}")
            }
        }
    }
}

fn descriptions(items: &[Query]) -> Vec<String> {
    items.iter().map(describe).filter(|d| !d.is_empty()).collect()
}

fn describe_term(term: &Term) -> String {
    match term {
        Term::From(text) => format!("from {text}"),
        Term::To(text) => format!("to {text}"),
        Term::Subject(text) => format!("subject {text}"),
        Term::Words(text) => text.clone(),
        Term::Since(day) => format!("since {}", day.format("%Y-%m-%d")),
        Term::Before(day) => format!("before {}", day.format("%Y-%m-%d")),
        Term::NewerThan(days) => format!("last {days} days"),
        Term::HasAttachment => "has an attachment".into(),
        Term::Unread => "unread".into(),
        Term::Flagged => "flagged".into(),
        Term::In(set) => describe_set(set),
        Term::MailboxNamed(name) => format!("in {name}"),
        Term::Larger(bytes) if *bytes % MEGABYTE == 0 => {
            format!("larger than {} MB", bytes / MEGABYTE)
        }
        Term::Larger(bytes) => format!("larger than {bytes} bytes"),
    }
}

fn describe_set(set: &MailSet) -> String {
    match set {
        MailSet::Role(crate::Role::Inbox) => "in the inbox".into(),
        MailSet::Role(crate::Role::Sent) => "sent".into(),
        MailSet::Role(crate::Role::Drafts) => "a draft".into(),
        MailSet::Role(crate::Role::Trash) => "in the trash".into(),
        MailSet::Role(crate::Role::Junk) => "in spam".into(),
        MailSet::Role(crate::Role::Archive) => "archived".into(),
        MailSet::Role(crate::Role::All) => "in all mail".into(),
        MailSet::Role(crate::Role::Important) => "important".into(),
        MailSet::Mailbox(name) => format!("in {name}"),
        MailSet::Keyword(word) => format!("keyword {word}"),
        MailSet::Unseen => "unread".into(),
        MailSet::Category(name) => format!("in the {name} category"),
    }
}

/// Gmail's spelling of a label name in its search: lower case, with each
/// space and slash as a dash, without double quotes or parentheses. The
/// search box suggests `label:` in this spelling, so a typed label is
/// matched to a mailbox by it.
pub fn label_spelling(name: &str) -> String {
    name.trim()
        .to_lowercase()
        .replace(|c: char| c.is_whitespace() || c == '/', "-")
        .chars()
        .filter(|c| !matches!(c, '"' | '(' | ')'))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Role;

    #[test]
    fn not_in_wraps_the_set_in_a_not() {
        let junk = MailSet::Role(Role::Junk);
        assert_eq!(
            Query::not_in(junk.clone()),
            Query::Not(Box::new(Query::Term(Term::In(junk))))
        );
    }

    #[test]
    fn plain_text_loses_quotes_parentheses_and_outer_spaces() {
        assert_eq!(plain("  \"Ann\" (work) "), "Ann work");
        assert_eq!(plain("\"()"), "");
        assert_eq!(plain("Zé Ninguém"), "Zé Ninguém");
    }

    #[test]
    fn describe_lists_conditions_in_words_a_person_reads() {
        let tree = Query::And(vec![
            Query::term(Term::From("ana".into())),
            Query::term(Term::Unread),
            Query::term(Term::NewerThan(7)),
        ]);
        assert_eq!(describe(&tree), "from ana, unread, last 7 days");
    }

    #[test]
    fn describe_reads_a_lone_term_without_a_separator() {
        assert_eq!(describe(&Query::term(Term::Flagged)), "flagged");
    }

    #[test]
    fn describe_joins_alternatives_with_or() {
        let tree = Query::Or(vec![
            Query::term(Term::From("ana".into())),
            Query::term(Term::From("bo".into())),
        ]);
        assert_eq!(describe(&tree), "from ana or from bo");
    }

    #[test]
    fn a_tree_asks_for_a_set_it_names_outside_a_not() {
        let trash = MailSet::Role(Role::Trash);
        assert!(Query::is_in(trash.clone()).asks_for(&trash));
        assert!(
            Query::Or(vec![Query::term(Term::Unread), Query::is_in(trash.clone())])
                .asks_for(&trash)
        );
        assert!(!Query::not_in(trash.clone()).asks_for(&trash));
        assert!(!Query::And(vec![]).asks_for(&trash));
    }
}
