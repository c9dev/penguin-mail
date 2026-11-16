//! Smart mailboxes: saved conditions that become a query tree.

use serde::{Deserialize, Serialize};

use crate::query::{self, MEGABYTE, Query, Term};
use crate::translate::gettext;
use crate::{MailSet, Role};

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
    #[serde(default)]
    pub unread_count: UnreadCount,
    /// Allow matching Sent messages to be trashed here without another question.
    #[serde(default)]
    pub trash_sent_without_asking: bool,
}

/// How a smart mailbox obtains its unread message count.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UnreadCount {
    #[default]
    Hidden,
    Local,
    Server,
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
    InMailbox,
    /// Received within this many days.
    NewerThanDays,
    /// Bigger than this many megabytes.
    LargerThanMb,
    HasAttachment,
    Unread,
    Flagged,
}

impl Field {
    pub const ALL: [Field; 11] = [
        Field::From,
        Field::To,
        Field::Subject,
        Field::Words,
        Field::Label,
        Field::InMailbox,
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
            Field::InMailbox => gettext("In mailbox"),
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

/// A saved folder choice. Older choices apply to every account in scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MailboxChoice {
    AllAccounts(String),
    InAccount { account: String, mailbox: String },
}

impl MailboxChoice {
    pub fn parse(value: &str) -> Vec<Self> {
        serde_json::from_str(value).unwrap_or_else(|_| {
            (!value.trim().is_empty())
                .then(|| Self::AllAccounts(value.to_string()))
                .into_iter()
                .collect()
        })
    }

    pub fn account(&self) -> Option<&str> {
        match self {
            Self::AllAccounts(_) => None,
            Self::InAccount { account, .. } => Some(account),
        }
    }

    pub fn mailbox(&self) -> &str {
        match self {
            Self::AllAccounts(mailbox) | Self::InAccount { mailbox, .. } => mailbox,
        }
    }
}

impl Condition {
    /// The condition as a query term, or `None` when the value is missing
    /// or unusable. Text keeps what the person typed, trimmed.
    fn query(&self, account: Option<&str>) -> Option<Query> {
        let value = self.value.trim();
        if self.field == Field::InMailbox {
            let selected = MailboxChoice::parse(value);
            let mut folders: Vec<Query> = selected
                .into_iter()
                .filter(|choice| {
                    account.is_none_or(|email| {
                        choice
                            .account()
                            .is_none_or(|a| a.eq_ignore_ascii_case(email))
                    })
                })
                .filter_map(|choice| {
                    let name = choice.mailbox().trim();
                    if name.is_empty() {
                        return None;
                    }
                    let term = match name.strip_prefix("role:") {
                        Some("inbox") => Term::In(MailSet::Role(Role::Inbox)),
                        Some("sent") => Term::In(MailSet::Role(Role::Sent)),
                        Some("drafts") => Term::In(MailSet::Role(Role::Drafts)),
                        Some("archive") => Term::In(MailSet::Role(Role::Archive)),
                        Some("junk") => Term::In(MailSet::Role(Role::Junk)),
                        Some("trash") => Term::In(MailSet::Role(Role::Trash)),
                        Some("all") => Term::In(MailSet::Role(Role::All)),
                        _ => Term::MailboxNamed(
                            name.strip_prefix("label:").unwrap_or(name).to_string(),
                        ),
                    };
                    Some(Query::Term(term))
                })
                .collect();
            return match folders.len() {
                0 => None,
                1 => folders.pop(),
                _ => Some(Query::Or(folders)),
            };
        }
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
        Some(Query::Term(match self.field {
            Field::From => Term::From(text()?),
            Field::To => Term::To(text()?),
            Field::Subject => Term::Subject(text()?),
            Field::Words => Term::Words(text()?),
            Field::Label => Term::MailboxNamed(label()?),
            Field::InMailbox => unreachable!(),
            Field::NewerThanDays => Term::NewerThan(number()?),
            Field::LargerThanMb => Term::Larger(i64::from(number()?) * MEGABYTE),
            Field::HasAttachment => Term::HasAttachment,
            Field::Unread => Term::Unread,
            Field::Flagged => Term::Flagged,
        }))
    }
}

impl SmartMailbox {
    /// A query without account restrictions, for validating conditions.
    /// Use `query_for` when searching an account.
    pub fn query(&self) -> Option<Query> {
        self.query_in(None)
    }

    /// Resolves folder choices before sending a query to one account.
    /// No query means this account cannot match the saved conditions.
    pub fn query_for(&self, account: &str) -> Option<Query> {
        if self
            .account
            .as_deref()
            .is_some_and(|a| !a.eq_ignore_ascii_case(account))
        {
            return None;
        }
        self.query_in(Some(account))
    }

    fn query_in(&self, account: Option<&str>) -> Option<Query> {
        let mut terms = Vec::new();
        for condition in &self.conditions {
            if let Some(query) = condition.query(account) {
                terms.push(query);
            } else if self.match_all
                && condition.field == Field::InMailbox
                && condition.query(None).is_some()
            {
                return None;
            }
        }
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
            trash_sent_without_asking: false,
            unread_count: Default::default(),
            id: "s1".into(),
            name: "Test".into(),
            account: None,
            match_all,
            conditions,
        }
    }

    #[test]
    fn consent_defaults_to_asking_and_is_saved_per_mailbox() {
        let old = r#"{"id":"one","name":"One","conditions":[]}"#;
        let mut one: SmartMailbox = serde_json::from_str(old).unwrap();
        let two = one.clone();
        assert!(!one.trash_sent_without_asking);
        one.trash_sent_without_asking = true;
        let restored: SmartMailbox =
            serde_json::from_str(&serde_json::to_string(&one).unwrap()).unwrap();
        assert!(restored.trash_sent_without_asking);
        assert!(!two.trash_sent_without_asking);
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
    fn an_account_can_match_its_received_and_sent_mail() {
        let mut smart = mailbox(
            true,
            vec![
                cond(
                    Field::InMailbox,
                    "[\"role:inbox\",\"role:sent\",\"label:Work/Clients\"]",
                ),
                cond(Field::Unread, ""),
            ],
        );
        smart.account = Some("ann@example.com".into());
        assert_eq!(
            smart.query(),
            Some(Query::And(vec![
                Query::Or(vec![
                    Query::is_in(MailSet::Role(Role::Inbox)),
                    Query::is_in(MailSet::Role(Role::Sent)),
                    Query::Term(Term::MailboxNamed("Work/Clients".into())),
                ]),
                Query::Term(Term::Unread),
            ]))
        );
    }

    #[test]
    fn a_folder_name_keeps_its_own_label_prefix() {
        assert_eq!(
            mailbox(true, vec![cond(Field::InMailbox, "[\"label:label:Work\"]")],).query(),
            Some(Query::Term(Term::MailboxNamed("label:Work".into())))
        );
    }

    #[test]
    fn folder_choices_keep_their_accounts_in_all_and_any_conditions() {
        let folders = cond(
            Field::InMailbox,
            r#"[{"account":"ann@example.com","mailbox":"role:inbox"},{"account":"bob@example.com","mailbox":"role:sent"},{"account":"ann@example.com","mailbox":"label:Clients"}]"#,
        );
        let mut smart = mailbox(true, vec![folders, cond(Field::Unread, "")]);
        assert_eq!(
            smart.query_for("ANN@example.com"),
            Some(Query::And(vec![
                Query::Or(vec![
                    Query::is_in(MailSet::Role(Role::Inbox)),
                    Query::term(Term::MailboxNamed("Clients".into()))
                ]),
                Query::term(Term::Unread),
            ]))
        );
        assert_eq!(
            smart.query_for("bob@example.com"),
            Some(Query::And(vec![
                Query::is_in(MailSet::Role(Role::Sent)),
                Query::term(Term::Unread)
            ]))
        );
        assert_eq!(smart.query_for("other@example.com"), None);
        smart.match_all = false;
        assert_eq!(
            smart.query_for("other@example.com"),
            Some(Query::term(Term::Unread))
        );
        smart.account = Some("ann@example.com".into());
        assert_eq!(smart.query_for("bob@example.com"), None);
    }

    #[test]
    fn separate_folder_conditions_do_not_drop_a_nonmatching_account() {
        let smart = mailbox(
            true,
            vec![
                cond(
                    Field::InMailbox,
                    r#"[{"account":"ann@example.com","mailbox":"role:inbox"}]"#,
                ),
                cond(
                    Field::InMailbox,
                    r#"[{"account":"bob@example.com","mailbox":"role:inbox"}]"#,
                ),
            ],
        );
        assert_eq!(smart.query_for("ann@example.com"), None);
        assert_eq!(smart.query_for("bob@example.com"), None);
        let legacy = mailbox(
            true,
            vec![cond(Field::InMailbox, r#"["role:inbox","label:Clients"]"#)],
        );
        assert_eq!(
            legacy.query_for("ann@example.com"),
            legacy.query_for("bob@example.com")
        );
        assert!(legacy.query_for("ann@example.com").is_some());
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
        assert_eq!(
            mailbox(true, vec![cond(Field::Words, "\"()")]).query(),
            None
        );
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

#[cfg(test)]
mod unread_tests {
    use super::*;

    #[test]
    fn old_settings_hide_counts_and_each_mode_round_trips() {
        let old = r#"{"id":"s1","name":"Work","conditions":[]}"#;
        let mut smart: SmartMailbox = serde_json::from_str(old).unwrap();
        assert_eq!(smart.unread_count, UnreadCount::Hidden);
        for mode in [UnreadCount::Hidden, UnreadCount::Local, UnreadCount::Server] {
            smart.unread_count = mode;
            assert_eq!(
                serde_json::from_str::<SmartMailbox>(&serde_json::to_string(&smart).unwrap())
                    .unwrap(),
                smart
            );
        }
    }
}
