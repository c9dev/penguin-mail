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
