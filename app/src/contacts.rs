//! Matching typed text against known correspondents, saying which account
//! each one comes from, and the words of the offer to save new recipients
//! after a message goes out.

use mailrs_domain::translate::{fill, fill_plural, gettext};
use mailrs_domain::{AccountId, Address};
use mailrs_store::contacts::Suggestion;

use crate::format::AccountName;

/// The most account dots one suggestion shows. The tooltip names them all.
pub const MOST_DOTS: usize = 3;

/// People whose address or any word of whose name starts with `query`,
/// best first, leaving out addresses already in `entered`. The contacts of
/// `from`, the account the message goes out from, come before the rest;
/// each group keeps the list's own order.
pub fn suggest<'a>(
    contacts: &'a [Suggestion],
    query: &str,
    entered: &[String],
    limit: usize,
    from: Option<AccountId>,
) -> Vec<&'a Suggestion> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return Vec::new();
    }
    // One pass over the shared list, holding at most `limit` of each group,
    // so a short query over a large address book copies nothing.
    let mut first = Vec::new();
    let mut rest = Vec::new();
    let found = contacts
        .iter()
        .filter(|c| !entered.iter().any(|e| e.eq_ignore_ascii_case(&c.email)))
        .filter(|c| matches(c, &query));
    for contact in found {
        let theirs = from.is_some_and(|id| contact.accounts.contains(&id));
        match theirs {
            true => first.push(contact),
            false if rest.len() < limit => rest.push(contact),
            false => {}
        }
        if first.len() == limit {
            break;
        }
    }
    first.extend(rest);
    first.truncate(limit);
    first
}

/// Whether `contact` answers `query`, already trimmed and lower case.
fn matches(contact: &Suggestion, query: &str) -> bool {
    let email = contact.email.to_lowercase();
    let name = contact.name.as_deref().unwrap_or_default().to_lowercase();
    email.starts_with(query)
        || name.starts_with(query)
        || name.split_whitespace().any(|w| w.starts_with(query))
        || email
            .split(['.', '_', '-', '@'])
            .any(|part| part.starts_with(query))
}

/// The part of a recipient field being typed: the text after the last
/// comma or semicolon, trimmed.
pub fn current_token(text: &str) -> (usize, &str) {
    let start = text.rfind([',', ';']).map_or(0, |i| i + 1);
    let token = &text[start..];
    let trimmed = token.trim_start();
    (start + token.len() - trimmed.len(), trimmed.trim_end())
}

/// What a suggestion row shows about where a person comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cue {
    /// The accounts to draw a colour dot for, the From account first.
    pub dots: Vec<AccountId>,
    /// The words beside the dots.
    pub text: String,
    /// Every account's name, when the words name only one of several.
    pub tooltip: Option<String>,
    /// What a screen reader adds to the person's name and address.
    pub spoken: String,
}

/// The cue for someone held by `accounts`, named by `label`. The From
/// account leads when it is one of them. The row shows short names; the
/// tooltip and a screen reader get the full ones.
pub fn cue(
    accounts: &[AccountId],
    from: Option<AccountId>,
    label: impl Fn(AccountId) -> AccountName,
) -> Cue {
    if accounts.is_empty() {
        let words = gettext("from mail");
        return Cue {
            dots: Vec::new(),
            text: words.clone(),
            tooltip: None,
            spoken: words,
        };
    }
    let mut ordered = accounts.to_vec();
    if let Some(at) = from.and_then(|id| ordered.iter().position(|a| *a == id)) {
        let lead = ordered.remove(at);
        ordered.insert(0, lead);
    }
    let names: Vec<AccountName> = ordered.iter().map(|id| label(*id)).collect();
    let all = names
        .iter()
        .map(|n| n.full.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let text = match names.len() {
        1 => names[0].short.clone(),
        count => fill_plural(
            "{account} and {count} more",
            "{account} and {count} more",
            count - 1,
            &[("account", &names[0].short), ("count", &(count - 1).to_string())],
        ),
    };
    // A short name that already says everything needs no tooltip.
    let tooltip = (names.len() > 1 || names[0].short != names[0].full).then(|| all.clone());
    ordered.truncate(MOST_DOTS);
    Cue {
        dots: ordered,
        text,
        tooltip,
        spoken: fill(&gettext("contact in {accounts}"), &[("accounts", &all)]),
    }
}

/// Everyone a draft goes to, in the order the fields hold them.
pub fn recipients_of(draft: &crate::compose::Draft) -> Vec<Address> {
    draft
        .to
        .iter()
        .chain(&draft.cc)
        .chain(&draft.bcc)
        .cloned()
        .collect()
}

/// The question the sent toast asks about `people`, the new recipients,
/// for the account named `account`.
pub fn offer_title(people: &[Address], account: &str) -> String {
    match people {
        [one] => fill(
            &gettext("Save {name} to contacts in {account}?"),
            &[("name", one.display()), ("account", account)],
        ),
        _ => fill_plural(
            "Save {count} person to contacts in {account}?",
            "Save {count} people to contacts in {account}?",
            people.len(),
            &[("count", &people.len().to_string()), ("account", account)],
        ),
    }
}

/// What the window says once Save has made contacts of `people`.
pub fn saved_title(people: &[Address], account: &str) -> String {
    match people {
        [one] => fill(
            &gettext("Saved {name} to contacts in {account}"),
            &[("name", one.display()), ("account", account)],
        ),
        _ => fill_plural(
            "Saved {count} person to contacts in {account}",
            "Saved {count} people to contacts in {account}",
            people.len(),
            &[("count", &people.len().to_string()), ("account", account)],
        ),
    }
}

/// What the window says when the provider would not take `people`.
pub fn failed_title(people: &[Address], account: &str) -> String {
    match people {
        [one] => fill(
            &gettext("Could not save {name} to contacts in {account}"),
            &[("name", one.display()), ("account", account)],
        ),
        _ => fill_plural(
            "Could not save {count} person to contacts in {account}",
            "Could not save {count} people to contacts in {account}",
            people.len(),
            &[("count", &people.len().to_string()), ("account", account)],
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contact(name: Option<&str>, email: &str) -> Suggestion {
        Suggestion {
            name: name.map(str::to_string),
            email: email.into(),
            organization: None,
            photo_file: None,
            accounts: Vec::new(),
            score: 1,
            last_seen: 0,
        }
    }

    fn held(name: &str, email: &str, accounts: &[AccountId]) -> Suggestion {
        Suggestion {
            accounts: accounts.to_vec(),
            ..contact(Some(name), email)
        }
    }

    fn emails(found: Vec<&Suggestion>) -> Vec<String> {
        found.into_iter().map(|c| c.email.clone()).collect()
    }

    #[test]
    fn names_and_addresses_match_by_prefix() {
        let all = [
            contact(Some("Ann Lee"), "ann@example.com"),
            contact(Some("Priya Raman"), "priya.raman@work.example"),
            contact(None, "leeroy@example.com"),
        ];
        assert_eq!(
            emails(suggest(&all, "lee", &[], 5, None)),
            ["ann@example.com", "leeroy@example.com"]
        );
        assert_eq!(
            emails(suggest(&all, "RAMAN", &[], 5, None)),
            ["priya.raman@work.example"]
        );
        assert_eq!(
            emails(suggest(&all, "work", &[], 5, None)),
            ["priya.raman@work.example"]
        );
        assert!(suggest(&all, " ", &[], 5, None).is_empty());
        assert_eq!(
            emails(suggest(&all, "lee", &["ANN@example.com".into()], 5, None)),
            ["leeroy@example.com"]
        );
    }

    #[test]
    fn the_from_accounts_contacts_come_first() {
        let (home, work) = (1, 2);
        let all = [
            held("Ana Lima", "ana@home.example", &[home]),
            held("Ana Sousa", "ana.sousa@work.example", &[work]),
            contact(Some("Anabela"), "anabela@example.org"),
            held("Anders", "anders@both.example", &[home, work]),
        ];
        assert_eq!(
            emails(suggest(&all, "an", &[], 5, Some(work))),
            [
                "ana.sousa@work.example",
                "anders@both.example",
                "ana@home.example",
                "anabela@example.org"
            ]
        );
        assert_eq!(
            emails(suggest(&all, "an", &[], 5, Some(home))),
            [
                "ana@home.example",
                "anders@both.example",
                "ana.sousa@work.example",
                "anabela@example.org"
            ]
        );
    }

    #[test]
    fn the_from_accounts_contacts_fill_a_short_list_first() {
        let all = [
            held("Ana Lima", "ana@home.example", &[1]),
            held("Ana Sousa", "ana.sousa@work.example", &[2]),
        ];
        assert_eq!(
            emails(suggest(&all, "an", &[], 1, Some(2))),
            ["ana.sousa@work.example"]
        );
    }

    fn label(id: AccountId) -> AccountName {
        let (short, full) = match id {
            1 => ("Home", "Home"),
            2 => ("Work", "Work"),
            _ => ("Club", "dana@club.example"),
        };
        AccountName {
            short: short.into(),
            full: full.into(),
        }
    }

    #[test]
    fn a_contact_of_one_account_shows_its_dot_and_name() {
        let shown = cue(&[3], Some(1), label);
        assert_eq!(shown.dots, [3]);
        assert_eq!(shown.text, "Club");
        assert_eq!(shown.tooltip.as_deref(), Some("dana@club.example"));
        assert_eq!(shown.spoken, "contact in dana@club.example");
    }

    #[test]
    fn a_contact_of_several_accounts_names_the_from_account_and_counts_the_rest() {
        let shown = cue(&[1, 2, 3], Some(2), label);
        assert_eq!(shown.dots, [2, 1, 3]);
        assert_eq!(shown.text, "Work and 2 more");
        assert_eq!(shown.tooltip.as_deref(), Some("Work, Home, dana@club.example"));
        assert_eq!(shown.spoken, "contact in Work, Home, dana@club.example");
    }

    #[test]
    fn no_more_than_three_dots_show() {
        let shown = cue(&[1, 2, 3, 4], None, label);
        assert_eq!(shown.dots.len(), MOST_DOTS);
        assert_eq!(shown.text, "Home and 3 more");
    }

    #[test]
    fn someone_found_only_in_mail_says_so() {
        let shown = cue(&[], Some(1), label);
        assert!(shown.dots.is_empty());
        assert_eq!(shown.text, "from mail");
        assert_eq!(shown.spoken, "from mail");
    }

    fn address(name: Option<&str>, email: &str) -> Address {
        Address {
            name: name.map(str::to_string),
            email: email.into(),
        }
    }

    #[test]
    fn the_offer_names_one_new_recipient() {
        assert_eq!(
            offer_title(&[address(Some("Ana Lima"), "ana@example.pt")], "Work"),
            "Save Ana Lima to contacts in Work?"
        );
        assert_eq!(
            offer_title(&[address(None, "ana@example.pt")], "Work"),
            "Save ana@example.pt to contacts in Work?"
        );
    }

    #[test]
    fn the_offer_counts_several_new_recipients() {
        let people = [
            address(Some("Ana Lima"), "ana@example.pt"),
            address(None, "rui@example.pt"),
            address(None, "eva@example.pt"),
        ];
        assert_eq!(
            offer_title(&people, "Work"),
            "Save 3 people to contacts in Work?"
        );
    }

    #[test]
    fn saving_says_who_went_where() {
        let ana = address(Some("Ana Lima"), "ana@example.pt");
        let rui = address(None, "rui@example.pt");
        assert_eq!(
            saved_title(std::slice::from_ref(&ana), "Work"),
            "Saved Ana Lima to contacts in Work"
        );
        assert_eq!(
            saved_title(&[ana.clone(), rui.clone()], "Work"),
            "Saved 2 people to contacts in Work"
        );
        assert_eq!(
            failed_title(std::slice::from_ref(&rui), "Work"),
            "Could not save rui@example.pt to contacts in Work"
        );
        assert_eq!(
            failed_title(&[ana, rui], "Work"),
            "Could not save 2 people to contacts in Work"
        );
    }

    #[test]
    fn every_field_of_a_draft_is_offered() {
        let mut draft = crate::compose::Draft::new(1, address(None, "dana@example.com"));
        draft.to = vec![address(Some("Ana Lima"), "ana@example.pt")];
        draft.cc = vec![address(None, "rui@example.pt")];
        draft.bcc = vec![address(None, "eva@example.pt")];
        let all: Vec<String> = recipients_of(&draft).into_iter().map(|a| a.email).collect();
        assert_eq!(all, ["ana@example.pt", "rui@example.pt", "eva@example.pt"]);
    }

    #[test]
    fn the_token_is_whatever_follows_the_last_separator() {
        assert_eq!(current_token("Ann <ann@x.com>, pri"), (17, "pri"));
        assert_eq!(current_token("pri"), (0, "pri"));
        assert_eq!(current_token("a@x.com;  "), (10, ""));
    }
}
