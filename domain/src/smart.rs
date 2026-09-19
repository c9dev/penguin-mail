//! Smart mailboxes: saved conditions that become a Gmail search.

use serde::{Deserialize, Serialize};

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

    pub fn label(self) -> &'static str {
        match self {
            Field::From => "From contains",
            Field::To => "To contains",
            Field::Subject => "Subject contains",
            Field::Words => "Message contains",
            Field::Label => "Has label",
            Field::NewerThanDays => "Received in the last (days)",
            Field::LargerThanMb => "Larger than (MB)",
            Field::HasAttachment => "Has an attachment",
            Field::Unread => "Is unread",
            Field::Flagged => "Is flagged",
        }
    }

    /// Whether the condition needs text next to it.
    pub fn takes_value(self) -> bool {
        !matches!(self, Field::HasAttachment | Field::Unread | Field::Flagged)
    }
}

impl Condition {
    /// The Gmail search term, or `None` when the value is missing or unusable.
    fn term(&self) -> Option<String> {
        let value = self.value.trim();
        let quoted = || {
            let clean: String = value
                .chars()
                .filter(|c| !matches!(c, '"' | '(' | ')'))
                .collect();
            (!clean.trim().is_empty()).then(|| {
                if clean.contains(char::is_whitespace) {
                    format!("\"{}\"", clean.trim())
                } else {
                    clean.trim().to_string()
                }
            })
        };
        let number = || value.parse::<u32>().ok().filter(|n| *n > 0);
        Some(match self.field {
            Field::From => format!("from:{}", quoted()?),
            Field::To => format!("to:{}", quoted()?),
            Field::Subject => format!("subject:{}", quoted()?),
            Field::Words => quoted()?,
            // Gmail writes spaces and slashes in label names as dashes.
            Field::Label => format!(
                "label:{}",
                value
                    .trim()
                    .to_lowercase()
                    .replace(|c: char| c.is_whitespace() || c == '/', "-")
            )
            .chars()
            .filter(|c| !matches!(c, '"' | '(' | ')'))
            .collect::<String>(),
            Field::NewerThanDays => format!("newer_than:{}d", number()?),
            Field::LargerThanMb => format!("larger:{}M", number()?),
            Field::HasAttachment => "has:attachment".into(),
            Field::Unread => "is:unread".into(),
            Field::Flagged => "is:starred".into(),
        })
        .filter(|term| term != "label:")
    }
}

impl SmartMailbox {
    /// The Gmail search for this mailbox, or `None` with no usable condition.
    pub fn query(&self) -> Option<String> {
        let terms: Vec<String> = self.conditions.iter().filter_map(Condition::term).collect();
        match terms.len() {
            0 => None,
            1 => terms.into_iter().next(),
            _ if self.match_all => Some(terms.join(" ")),
            _ => Some(format!("{{{}}}", terms.join(" "))),
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
    fn all_conditions_join_and_any_uses_braces() {
        let conditions = vec![
            cond(Field::From, "ann@example.com"),
            cond(Field::Subject, "quarterly report"),
            cond(Field::Unread, "ignored"),
            cond(Field::NewerThanDays, "7"),
        ];
        assert_eq!(
            mailbox(true, conditions.clone()).query().as_deref(),
            Some("from:ann@example.com subject:\"quarterly report\" is:unread newer_than:7d")
        );
        assert_eq!(
            mailbox(false, conditions).query().as_deref(),
            Some("{from:ann@example.com subject:\"quarterly report\" is:unread newer_than:7d}")
        );
    }

    #[test]
    fn empty_or_bad_values_drop_out() {
        let conditions = vec![
            cond(Field::From, "  "),
            cond(Field::LargerThanMb, "lots"),
            cond(Field::Label, "Work/Clients"),
        ];
        assert_eq!(
            mailbox(true, conditions).query().as_deref(),
            Some("label:work-clients")
        );
        assert_eq!(
            mailbox(true, vec![cond(Field::Words, "\"()")]).query(),
            None
        );
    }
}
