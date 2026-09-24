//! Smart mailboxes: saved conditions that become a query tree.

use serde::{Deserialize, Serialize};

use crate::query::{self, MEGABYTE, Query, Term};
use crate::translate::gettext;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SmartMailbox {
    /// Stable identity, so renaming keeps the sidebar selection.
    pub id: String,
    pub name: String,
    /// One account's address, or every account when unset.
    #[serde(default)]
    pub account: Option<String>,
    /// Every condition must hold, rather than any one.
    #[serde(default = "yes")]
    pub match_all: bool,
    #[serde(default)]
    pub conditions: Vec<Condition>,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Condition {
    pub field: Field,
    /// Text for the fields that take some; ignored by the rest.
    #[serde(default)]
    pub value: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Field {
    From,
    To,
    Subject,
    Words,
    Label,
    /// Received within this many days.
    NewerThanDays,
    /// Bigger than this many megabytes.
    LargerThanMb,
    HasAttachment,
    Unread,
    Flagged,
}

impl Field {
    pub const ALL: [Field; 10] = [
        Field::From,
        Field::To,
        Field::Subject,
        Field::Words,
        Field::Label,
        Field::NewerThanDays,
        Field::LargerThanMb,
        Field::HasAttachment,
        Field::Unread,
        Field::Flagged,
    ];

    /// What the condition's own row calls it.
    pub fn label(self) -> String {
        match self {
            Field::From => gettext("From contains"),
            Field::To => gettext("To contains"),
            Field::Subject => gettext("Subject contains"),
            Field::Words => gettext("Message contains"),
            Field::Label => gettext("Has label"),
            Field::NewerThanDays => gettext("Received in the last (days)"),
            Field::LargerThanMb => gettext("Larger than (MB)"),
            Field::HasAttachment => gettext("Has an attachment"),
            Field::Unread => gettext("Is unread"),
            Field::Flagged => gettext("Is flagged"),
        }
    }

    /// Whether the condition needs text next to it.
    pub fn takes_value(self) -> bool {
        !matches!(self, Field::HasAttachment | Field::Unread | Field::Flagged)
    }
}

impl Condition {
    /// The condition as a query term, or `None` when the value is missing
    /// or unusable. Text keeps what the person typed, trimmed.
    fn term(&self) -> Option<Term> {
        let value = self.value.trim();
        // Quotes and parentheses alone name nothing to search for.
        let text = || (!query::plain(value).is_empty()).then(|| value.to_string());
        // A label name keeps its inner spaces, which Gmail spells as
        // dashes, so only a name of quotes and parentheses alone is empty.
        let label = || {
            value
                .chars()
                .any(|c| !matches!(c, '"' | '(' | ')'))
                .then(|| value.to_string())
        };
        let number = || value.parse::<u32>().ok().filter(|n| *n > 0);
        Some(match self.field {
            Field::From => Term::From(text()?),
            Field::To => Term::To(text()?),
            Field::Subject => Term::Subject(text()?),
            Field::Words => Term::Words(text()?),
            Field::Label => Term::MailboxNamed(label()?),
            Field::NewerThanDays => Term::NewerThan(number()?),
            Field::LargerThanMb => Term::Larger(i64::from(number()?) * MEGABYTE),
            Field::HasAttachment => Term::HasAttachment,
            Field::Unread => Term::Unread,
            Field::Flagged => Term::Flagged,
        })
    }
}

impl SmartMailbox {
    /// The query for this mailbox, or `None` with no usable condition.
    pub fn query(&self) -> Option<Query> {
        let mut terms: Vec<Query> = self
            .conditions
            .iter()
            .filter_map(Condition::term)
            .map(Query::Term)
            .collect();
        match terms.len() {
            0 => None,
            1 => terms.pop(),
            _ if self.match_all => Some(Query::And(terms)),
            _ => Some(Query::Or(terms)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cond(field: Field, value: &str) -> Condition {
        Condition {
            field,
            value: value.into(),
        }
    }

    fn mailbox(match_all: bool, conditions: Vec<Condition>) -> SmartMailbox {
        SmartMailbox {
            id: "s1".into(),
            name: "Test".into(),
            account: None,
            match_all,
            conditions,
        }
    }

    #[test]
    fn all_conditions_make_an_and_and_any_makes_an_or() {
        let conditions = vec![
            cond(Field::From, "ann@example.com"),
            cond(Field::Unread, "ignored"),
            cond(Field::NewerThanDays, "7"),
        ];
        let terms = vec![
            Query::Term(Term::From("ann@example.com".into())),
            Query::Term(Term::Unread),
            Query::Term(Term::NewerThan(7)),
        ];
        assert_eq!(
            mailbox(true, conditions.clone()).query(),
            Some(Query::And(terms.clone()))
        );
        assert_eq!(mailbox(false, conditions).query(), Some(Query::Or(terms)));
    }

    #[test]
    fn one_usable_condition_stands_alone() {
        let one = vec![cond(Field::Subject, "  quarterly report ")];
        let subject = Some(Query::Term(Term::Subject("quarterly report".into())));
        assert_eq!(mailbox(true, one.clone()).query(), subject);
        assert_eq!(mailbox(false, one).query(), subject);
    }

    #[test]
    fn empty_or_bad_values_drop_out() {
        let conditions = vec![
            cond(Field::From, "  "),
            cond(Field::LargerThanMb, "lots"),
            cond(Field::NewerThanDays, "0"),
            cond(Field::Label, "Work/Clients"),
        ];
        assert_eq!(
            mailbox(true, conditions).query(),
            Some(Query::Term(Term::MailboxNamed("Work/Clients".into())))
        );
        assert_eq!(mailbox(true, vec![cond(Field::Words, "\"()")]).query(), None);
        assert_eq!(mailbox(true, vec![cond(Field::Label, "()")]).query(), None);
        assert_eq!(mailbox(true, vec![]).query(), None);
    }

    #[test]
    fn a_size_counts_in_megabytes_and_fits_the_largest_value() {
        let largest = vec![cond(Field::LargerThanMb, "4294967295")];
        assert_eq!(
            mailbox(true, largest).query(),
            Some(Query::Term(Term::Larger(4_294_967_295 * MEGABYTE)))
        );
    }
}
