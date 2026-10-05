//! Rules in plain words, and building one from the rule form.

use mailrs_domain::translate::{fill, gettext};
use mailrs_domain::{Filter, FilterAction, FilterCriteria, MailSet, Role};
use mailrs_sync::RulesPlace;

use crate::offered::Filing;

/// The line under the rules list saying where they run.
pub fn place_line(place: RulesPlace, provider: &str) -> String {
    match place {
        RulesPlace::ThisComputer => {
            gettext("These rules run on this computer while Penguin Mail is open.")
        }
        RulesPlace::Server => fill(
            &gettext("{provider} runs these on new mail as it arrives, even when this computer is off."),
            &[("provider", provider)],
        ),
    }
}

/// The line that says the rules server is not answering.
pub fn waiting_line(provider: &str) -> String {
    fill(
        &gettext("{provider} is not answering. Changes wait here and go out when it does."),
        &[("provider", provider)],
    )
}

/// Whether the form offers "Never Send to Spam". A rule on a folder
/// server runs after the server's spam filter, so it cannot promise it.
pub fn offers_never_spam(filing: Filing) -> bool {
    filing == Filing::Labels
}

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

/// The parts of a rule the form has no field for, such as a size, a
/// forward, a category or a second label. Gmail's own settings can make
/// them, and an edit carries them over as they were.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Unshown {
    exclude_chats: bool,
    size: Option<u64>,
    size_comparison: Option<String>,
    add: Vec<MailSet>,
    remove: Vec<MailSet>,
    forward: Option<String>,
}

impl Unshown {
    /// Whether the form shows all of the rule.
    pub fn is_empty(&self) -> bool {
        *self == Unshown::default()
    }

    /// `filter` with these parts put back. A set the form already adds
    /// or removes stays listed once.
    fn put_back(&self, mut filter: Filter) -> Filter {
        filter.criteria.exclude_chats = self.exclude_chats;
        filter.criteria.size = self.size;
        filter.criteria.size_comparison = self.size_comparison.clone();
        let join = |sets: &mut Vec<MailSet>, more: &[MailSet]| {
            for set in more {
                if !sets.contains(set) {
                    sets.push(set.clone());
                }
            }
        };
        join(&mut filter.action.add, &self.add);
        join(&mut filter.action.remove, &self.remove);
        filter.action.forward = self.forward.clone();
        filter
    }
}

impl RuleForm {
    /// The form filled in from `filter`, and what it cannot show.
    /// `offered` says whether the Apply Label list has a label id; the
    /// first such label the rule adds goes in that list.
    pub fn read(filter: &Filter, offered: impl Fn(&str) -> bool) -> (RuleForm, Unshown) {
        let text = |field: &Option<String>| field.clone().unwrap_or_default();
        let criteria = &filter.criteria;
        let (add, remove) = (&filter.action.add, &filter.action.remove);
        let label = add.iter().find_map(|set| match set {
            MailSet::Mailbox(id) if offered(id) => Some(id.clone()),
            _ => None,
        });
        let form = RuleForm {
            from: text(&criteria.from),
            to: text(&criteria.to),
            subject: text(&criteria.subject),
            has_words: text(&criteria.query),
            not_words: text(&criteria.negated_query),
            has_attachment: criteria.has_attachment,
            skip_inbox: remove.contains(&MailSet::Role(Role::Inbox)),
            mark_read: remove.contains(&MailSet::Unseen),
            star: add.contains(&MailSet::flagged()),
            never_spam: remove.contains(&MailSet::Role(Role::Junk)),
            trash: add.contains(&MailSet::Role(Role::Trash)),
            label: label.clone(),
        };
        let shown_add = |set: &MailSet| {
            *set == MailSet::flagged()
                || *set == MailSet::Role(Role::Trash)
                || matches!(set, MailSet::Mailbox(id) if Some(id) == label.as_ref())
        };
        let shown_remove = |set: &MailSet| {
            matches!(
                set,
                MailSet::Role(Role::Inbox | Role::Junk) | MailSet::Unseen
            )
        };
        let unshown = Unshown {
            exclude_chats: criteria.exclude_chats,
            size: criteria.size,
            size_comparison: criteria.size_comparison.clone(),
            add: add.iter().filter(|s| !shown_add(s)).cloned().collect(),
            remove: remove.iter().filter(|s| !shown_remove(s)).cloned().collect(),
            forward: filter.action.forward.clone(),
        };
        (form, unshown)
    }

    /// The Gmail filter the form describes, or what is missing.
    pub fn filter(&self) -> Result<Filter, &'static str> {
        self.filter_keeping(&Unshown::default())
    }

    /// The filter the form describes with `unshown` put back, or what is
    /// missing. A rule that says which mail through its size alone
    /// counts, though the form shows no field for it.
    pub fn filter_keeping(&self, unshown: &Unshown) -> Result<Filter, &'static str> {
        let filter = unshown.put_back(self.built());
        if filter.criteria == FilterCriteria::default() {
            return Err("Say which mail the rule is for");
        }
        if filter.action == FilterAction::default() {
            return Err("Choose what the rule does");
        }
        Ok(filter)
    }

    /// The filter the form's own fields describe, unchecked.
    fn built(&self) -> Filter {
        let field = |text: &str| Some(text.trim().to_string()).filter(|t| !t.is_empty());
        let criteria = FilterCriteria {
            from: field(&self.from),
            to: field(&self.to),
            subject: field(&self.subject),
            query: field(&self.has_words),
            negated_query: field(&self.not_words),
            has_attachment: self.has_attachment,
            ..FilterCriteria::default()
        };
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
        Filter {
            id: None,
            criteria,
            action,
            ..Filter::default()
        }
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
    if let Some(size) = criteria.size {
        let size = crate::format::human_size(i64::try_from(size).unwrap_or(i64::MAX));
        let words = match criteria.size_comparison.as_deref() {
            Some("smaller") => gettext("Smaller than {size}"),
            _ => gettext("Larger than {size}"),
        };
        parts.push(fill(&words, &[("size", &size)]));
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
        if let MailSet::Mailbox(id) | MailSet::Category(id) = set {
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
    fn the_rules_say_where_they_run() {
        assert_eq!(
            place_line(RulesPlace::ThisComputer, "Fastmail"),
            "These rules run on this computer while Penguin Mail is open."
        );
        assert_eq!(
            place_line(RulesPlace::Server, "mailbox.org"),
            "mailbox.org runs these on new mail as it arrives, even when this computer is off."
        );
        assert_eq!(
            waiting_line("mailbox.org"),
            "mailbox.org is not answering. Changes wait here and go out when it does."
        );
    }

    #[test]
    fn never_send_to_spam_is_for_label_accounts() {
        assert!(offers_never_spam(Filing::Labels));
        assert!(!offers_never_spam(Filing::Folders));
    }

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

    /// Label ids the form's Apply Label list offers in these tests.
    fn offered(id: &str) -> bool {
        matches!(id, "Label_7" | "Label_8")
    }

    #[test]
    fn a_rule_the_form_made_reads_back_whole() {
        let form = RuleForm {
            from: "news@example.com".into(),
            has_words: "unsubscribe".into(),
            has_attachment: true,
            skip_inbox: true,
            mark_read: true,
            star: true,
            label: Some("Label_7".into()),
            never_spam: true,
            ..RuleForm::default()
        };
        let filter = form.filter().unwrap();
        let (read, unshown) = RuleForm::read(&filter, offered);
        assert_eq!(read, form);
        assert!(unshown.is_empty(), "{unshown:?}");
    }

    #[test]
    fn a_deleting_rule_reads_as_delete_it() {
        let (form, unshown) = RuleForm::read(&Filter::block("x@y.com"), offered);
        assert!(form.trash);
        assert_eq!(form.from, "x@y.com");
        assert!(unshown.is_empty(), "{unshown:?}");
    }

    /// A filter made in Gmail's own settings, with a size, a forward, a
    /// category, a second label and the important marker.
    fn made_in_gmail() -> Filter {
        Filter {
            id: Some("f1".into()),
            criteria: FilterCriteria {
                from: Some("shop@example.com".into()),
                size: Some(5_000_000),
                size_comparison: Some("larger".into()),
                exclude_chats: true,
                ..FilterCriteria::default()
            },
            action: FilterAction {
                add: vec![
                    MailSet::Mailbox("Label_7".into()),
                    MailSet::Mailbox("Label_8".into()),
                    MailSet::Category("CATEGORY_PROMOTIONS".into()),
                    MailSet::Role(Role::Important),
                ],
                remove: vec![MailSet::Role(Role::Inbox)],
                forward: Some("me@example.org".into()),
            },
            ..Filter::default()
        }
    }

    #[test]
    fn a_rule_from_gmail_fills_what_the_form_can_show() {
        let (form, unshown) = RuleForm::read(&made_in_gmail(), offered);
        assert_eq!(form.from, "shop@example.com");
        assert_eq!(form.label.as_deref(), Some("Label_7"));
        assert!(form.skip_inbox);
        assert!(!unshown.is_empty());
    }

    #[test]
    fn saving_an_edit_keeps_what_the_form_cannot_show() {
        let (mut form, unshown) = RuleForm::read(&made_in_gmail(), offered);
        form.from = "deals@example.com".into();
        form.mark_read = true;
        let saved = form.filter_keeping(&unshown).unwrap();
        let mut expected = made_in_gmail();
        expected.id = None;
        expected.criteria.from = Some("deals@example.com".into());
        expected.action.remove.push(MailSet::Unseen);
        assert_eq!(sorted(saved), sorted(expected));
    }

    #[test]
    fn choosing_no_label_drops_only_the_one_the_form_showed() {
        let (mut form, unshown) = RuleForm::read(&made_in_gmail(), offered);
        form.label = None;
        let saved = form.filter_keeping(&unshown).unwrap();
        assert!(!saved.action.add.contains(&MailSet::Mailbox("Label_7".into())));
        assert!(saved.action.add.contains(&MailSet::Mailbox("Label_8".into())));
    }

    #[test]
    fn picking_a_label_the_rule_already_adds_lists_it_once() {
        let (mut form, unshown) = RuleForm::read(&made_in_gmail(), offered);
        form.label = Some("Label_8".into());
        let saved = form.filter_keeping(&unshown).unwrap();
        let eights = saved
            .action
            .add
            .iter()
            .filter(|set| **set == MailSet::Mailbox("Label_8".into()))
            .count();
        assert_eq!(eights, 1);
    }

    #[test]
    fn a_size_alone_is_enough_to_say_which_mail() {
        let mut filter = made_in_gmail();
        filter.criteria.from = None;
        let (form, unshown) = RuleForm::read(&filter, offered);
        assert!(form.filter_keeping(&unshown).is_ok());
        assert_eq!(form.filter(), Err("Say which mail the rule is for"));
    }

    #[test]
    fn a_size_rule_names_the_size() {
        let criteria = FilterCriteria {
            size: Some(5 * 1024 * 1024),
            size_comparison: Some("larger".into()),
            ..FilterCriteria::default()
        };
        assert_eq!(describe_criteria(&criteria), "Larger than 5.0 MB");
        let smaller = FilterCriteria {
            from: Some("a@b.c".into()),
            size_comparison: Some("smaller".into()),
            ..criteria
        };
        assert_eq!(
            describe_criteria(&smaller),
            "From a@b.c, smaller than 5.0 MB"
        );
    }

    /// The filter with its sets in a fixed order, since Gmail treats the
    /// lists as sets.
    fn sorted(mut filter: Filter) -> Filter {
        let key = |set: &MailSet| format!("{set:?}");
        filter.action.add.sort_by_key(key);
        filter.action.remove.sort_by_key(key);
        filter
    }

    #[test]
    fn a_category_rule_names_the_category() {
        let action = FilterAction {
            add: vec![MailSet::Category("CATEGORY_SOCIAL".into())],
            ..FilterAction::default()
        };
        assert_eq!(describe_action(&action, |_| None), "Apply CATEGORY_SOCIAL");
    }
}
