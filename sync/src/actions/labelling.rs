//! Labelling mail by name across accounts. A name stands for a separate
//! label in each account, and an account without one needs a new label in
//! Gmail before its mail can carry the name. The person decides whether
//! that happens, once for the whole change. This module works out what the
//! question covers and which mail a "no" still labels; the window and the
//! assistant each put the question their own way and then run
//! `MailAction::Label`.

use std::collections::{BTreeMap, BTreeSet};

use mailrs_domain::translate::fill_plural;
use mailrs_domain::{AccountId, Target};

/// The labels a change by name would have to create.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewLabels {
    /// Each account among the targets that lacks an added name, with the
    /// names it lacks, in the order the change gave them.
    missing: BTreeMap<AccountId, Vec<String>>,
    /// How many names the change adds.
    adding: usize,
    /// Whether the change removes labels too, which gives every target
    /// something to do without new labels.
    removing: bool,
}

impl NewLabels {
    /// Works out what adding `add` to the targets needs, from the label
    /// names each account holds. Names match ignoring case, as
    /// `MailActions` finds them.
    pub fn plan(
        targets: &[Target],
        add: &[String],
        remove: &[String],
        names_in: impl Fn(AccountId) -> Vec<String>,
    ) -> NewLabels {
        let accounts: BTreeSet<AccountId> = targets.iter().map(|t| t.account_id).collect();
        let mut missing = BTreeMap::new();
        for account_id in accounts {
            let known = names_in(account_id);
            let lacking: Vec<String> = add
                .iter()
                .filter(|name| !known.iter().any(|k| k.eq_ignore_ascii_case(name)))
                .cloned()
                .collect();
            if !lacking.is_empty() {
                missing.insert(account_id, lacking);
            }
        }
        NewLabels {
            missing,
            adding: add.len(),
            removing: !remove.is_empty(),
        }
    }

    /// True when every account holds every added name, so there is nothing
    /// to ask.
    pub fn is_empty(&self) -> bool {
        self.missing.is_empty()
    }

    /// The accounts that would get a new label, in id order.
    pub fn accounts(&self) -> impl Iterator<Item = AccountId> + '_ {
        self.missing.keys().copied()
    }

    /// The names to create, each once, whatever the case of its spellings.
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        for name in self.missing.values().flatten() {
            if !names.iter().any(|n| n.eq_ignore_ascii_case(name)) {
                names.push(name.clone());
            }
        }
        names
    }

    /// The targets the change still reaches when the person declines new
    /// labels: those in an account that holds at least one added name, and
    /// every target when the change also removes labels.
    pub fn kept(&self, targets: &[Target]) -> Vec<Target> {
        targets
            .iter()
            .filter(|t| {
                self.removing
                    || self
                        .missing
                        .get(&t.account_id)
                        .is_none_or(|lacking| lacking.len() < self.adding)
            })
            .cloned()
            .collect()
    }

    /// The question, naming the labels: "Create the label “Receipts”?"
    pub fn heading(&self) -> String {
        let names = self.names();
        let quoted: Vec<String> = names.iter().map(|n| format!("“{n}”")).collect();
        fill_plural(
            "Create the label {names}?",
            "Create the labels {names}?",
            names.len(),
            &[("names", &quoted.join(", "))],
        )
    }

    /// Which accounts get a new label, named by `email_of`.
    pub fn who(&self, email_of: impl Fn(AccountId) -> String) -> String {
        let accounts: Vec<String> = self.accounts().map(email_of).collect();
        fill_plural(
            "{accounts} will get a new label.",
            "{accounts} will get new labels.",
            self.names().len(),
            &[("accounts", &accounts.join(", "))],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|n| n.to_string()).collect()
    }

    /// Account 1 has Receipts and Travel, account 2 has Travel, account 3
    /// has no labels.
    fn known(account_id: AccountId) -> Vec<String> {
        match account_id {
            1 => names(&["Receipts", "Travel"]),
            2 => names(&["travel"]),
            _ => vec![],
        }
    }

    fn targets() -> Vec<Target> {
        vec![
            Target::thread(1, "a"),
            Target::thread(2, "b"),
            Target::thread(3, "c"),
            Target::thread(1, "d"),
        ]
    }

    #[test]
    fn a_label_every_account_holds_needs_no_question() {
        let plan = NewLabels::plan(&targets()[..2], &names(&["TRAVEL"]), &[], known);
        assert!(plan.is_empty());
        assert_eq!(plan.kept(&targets()[..2]), &targets()[..2]);
    }

    #[test]
    fn the_accounts_without_the_label_are_the_ones_asked_about() {
        let plan = NewLabels::plan(&targets(), &names(&["Receipts"]), &[], known);
        assert_eq!(plan.accounts().collect::<Vec<_>>(), [2, 3]);
        assert_eq!(plan.names(), ["Receipts"]);
        assert_eq!(plan.heading(), "Create the label “Receipts”?");
        assert_eq!(
            plan.who(|id| format!("{id}@example.com")),
            "2@example.com, 3@example.com will get a new label."
        );
    }

    #[test]
    fn declining_keeps_the_mail_in_accounts_that_have_the_label() {
        let plan = NewLabels::plan(&targets(), &names(&["Receipts"]), &[], known);
        assert_eq!(
            plan.kept(&targets()),
            [Target::thread(1, "a"), Target::thread(1, "d")]
        );
    }

    #[test]
    fn an_account_with_one_of_two_names_keeps_its_mail() {
        let plan = NewLabels::plan(&targets(), &names(&["Receipts", "Travel"]), &[], known);
        assert_eq!(plan.names(), ["Receipts", "Travel"]);
        assert_eq!(plan.heading(), "Create the labels “Receipts”, “Travel”?");
        let kept: Vec<AccountId> = plan.kept(&targets()).iter().map(|t| t.account_id).collect();
        assert_eq!(kept, [1, 2, 1], "account 3 has neither");
    }

    #[test]
    fn a_change_that_also_removes_keeps_every_target() {
        let plan = NewLabels::plan(&targets(), &names(&["Receipts"]), &names(&["Old"]), known);
        assert!(!plan.is_empty());
        assert_eq!(plan.kept(&targets()), targets());
    }
}
