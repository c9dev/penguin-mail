//! Rules in plain words, and building one from the rule form.

use mailrs_domain::translate::{fill, gettext};
use mailrs_domain::{Filter, FilterAction, FilterCriteria, MailSet, Role};

/// What the rule form collects.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuleForm {
    pub from: String,
    pub to: String,
    pub subject: String,
    pub has_words: String,
    pub not_words: String,
    pub has_attachment: bool,
    pub skip_inbox: bool,
    pub mark_read: bool,
    pub star: bool,
    pub label: Option<String>,
    pub never_spam: bool,
    pub trash: bool,
}

impl RuleForm {
    /// The Gmail filter the form describes, or what is missing.
    pub fn filter(&self) -> Result<Filter, &'static str> {
        let field = |text: &str| Some(text.trim().to_string()).filter(|t| !t.is_empty());
        let criteria = FilterCriteria {
            from: field(&self.from),
            to: field(&self.to),
            subject: field(&self.subject),
            query: field(&self.has_words),
            negated_query: field(&self.not_words),
            has_attachment: self.has_attachment,
        };
        if criteria == FilterCriteria::default() {
            return Err("Say which mail the rule is for");
        }
        let mut action = FilterAction::default();
        if self.star {
            action.add.push(MailSet::flagged());
        }
        if let Some(label) = &self.label {
            action.add.push(MailSet::Mailbox(label.clone()));
        }
        if self.trash {
            action.add.push(MailSet::Role(Role::Trash));
        }
        if self.skip_inbox || self.trash {
            action.remove.push(MailSet::Role(Role::Inbox));
        }
        if self.mark_read {
            action.remove.push(MailSet::Unseen);
        }
        if self.never_spam {
            action.remove.push(MailSet::Role(Role::Junk));
        }
        if action == FilterAction::default() {
            return Err("Choose what the rule does");
        }
        Ok(Filter {
            id: None,
            criteria,
            action,
        })
    }
}

/// "From ann@example.com, subject has “invoice”".
pub fn describe_criteria(criteria: &FilterCriteria) -> String {
    let mut parts = Vec::new();
    if let Some(from) = &criteria.from {
        parts.push(fill(&gettext("From {address}"), &[("address", from)]));
    }
    if let Some(to) = &criteria.to {
        parts.push(fill(&gettext("To {address}"), &[("address", to)]));
    }
    if let Some(subject) = &criteria.subject {
        parts.push(fill(
            &gettext("Subject has “{words}”"),
            &[("words", subject)],
        ));
    }
    if let Some(query) = &criteria.query {
        parts.push(fill(&gettext("Has “{words}”"), &[("words", query)]));
    }
    if let Some(query) = &criteria.negated_query {
        parts.push(fill(
            &gettext("Doesn't have “{words}”"),
            &[("words", query)],
        ));
    }
    if criteria.has_attachment {
        parts.push(gettext("Has an attachment"));
    }
    if parts.is_empty() {
        return gettext("All mail");
    }
    let mut text = parts.join(", ");
    // Only the first part starts with a capital.
    if let Some((first, rest)) = text.split_once(", ") {
        text = format!("{first}, {}", lower_first(rest));
    }
    text
}

/// "Skip the Inbox, apply Travel, mark as read". `label_name` turns a
/// label id into its name.
pub fn describe_action(
    action: &FilterAction,
    label_name: impl Fn(&str) -> Option<String>,
) -> String {
    let adds = |set: &MailSet| action.add.contains(set);
    let removes = |set: &MailSet| action.remove.contains(set);
    let mut parts: Vec<String> = Vec::new();
    if adds(&MailSet::Role(Role::Trash)) {
        parts.push(gettext("Delete it"));
    } else if removes(&MailSet::Role(Role::Inbox)) {
        parts.push(gettext("Skip the Inbox"));
    }
    for set in &action.add {
        if let MailSet::Mailbox(id) = set {
            parts.push(fill(
                &gettext("Apply {label}"),
                &[("label", &label_name(id).unwrap_or_else(|| id.clone()))],
            ));
        }
    }
    if adds(&MailSet::flagged()) {
        parts.push(gettext("Star it"));
    }
    if removes(&MailSet::Unseen) {
        parts.push(gettext("Mark as read"));
    }
    if adds(&MailSet::Role(Role::Important)) {
        parts.push(gettext("Mark as important"));
    }
    if removes(&MailSet::Role(Role::Important)) {
        parts.push(gettext("Never mark as important"));
    }
    if removes(&MailSet::Role(Role::Junk)) {
        parts.push(gettext("Never send to Spam"));
    }
    if let Some(to) = &action.forward {
        parts.push(fill(&gettext("Forward to {address}"), &[("address", to)]));
    }
    if parts.is_empty() {
        return gettext("Nothing");
    }
    let first = parts.remove(0);
    std::iter::once(first)
        .chain(parts.iter().map(|p| lower_first(p)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// `text` with its first letter lowered, for every part after the first.
/// Words further in, such as "Inbox" or a label name, keep their case.
fn lower_first(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_form_becomes_a_gmail_filter() {
        let form = RuleForm {
            from: " news@example.com ".into(),
            skip_inbox: true,
            mark_read: true,
            label: Some("Label_7".into()),
            ..RuleForm::default()
        };
        let filter = form.filter().unwrap();
        assert_eq!(filter.criteria.from.as_deref(), Some("news@example.com"));
        assert_eq!(filter.action.add, [MailSet::Mailbox("Label_7".into())]);
        assert_eq!(
            filter.action.remove,
            [MailSet::Role(Role::Inbox), MailSet::Unseen]
        );
        assert!(RuleForm::default().filter().is_err());
        let no_action = RuleForm {
            subject: "x".into(),
            ..RuleForm::default()
        };
        assert_eq!(no_action.filter(), Err("Choose what the rule does"));
    }

    #[test]
    fn filters_read_as_sentences() {
        let filter = RuleForm {
            from: "news@example.com".into(),
            subject: "Weekly".into(),
            skip_inbox: true,
            label: Some("Label_7".into()),
            star: true,
            ..RuleForm::default()
        }
        .filter()
        .unwrap();
        assert_eq!(
            describe_criteria(&filter.criteria),
            "From news@example.com, subject has “Weekly”"
        );
        let names = |id: &str| (id == "Label_7").then(|| "Newsletters".to_string());
        assert_eq!(
            describe_action(&filter.action, names),
            "Skip the Inbox, apply Newsletters, star it"
        );
        assert_eq!(
            describe_action(&Filter::block("x@y.com").action, |_| None),
            "Delete it"
        );
    }
}
