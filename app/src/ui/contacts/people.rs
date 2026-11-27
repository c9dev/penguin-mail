//! Who the Contacts mode lists: contacts grouped into people, the lists
//! built from them, and how a search reads them. No widgets here.

use std::collections::{BTreeMap, HashMap};

use mailrs_domain::AccountId;
use mailrs_store::address_book::{Contact, Link};

/// A contact's place in the account that holds it.
pub type Key = (AccountId, String);

pub fn key_of(contact: &Contact) -> Key {
    (contact.account_id, contact.resource.clone())
}

/// One row of a list: a person, with every stored contact that is them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Person {
    /// Indices into the contacts they were built from, in account order.
    pub members: Vec<usize>,
    /// The name the row shows.
    pub name: String,
    /// The first address, or empty.
    pub email: String,
    /// Each account that holds them once, in member order.
    pub accounts: Vec<AccountId>,
}

impl Person {
    pub fn emails(&self, contacts: &[Contact]) -> Vec<String> {
        let mut all: Vec<String> = Vec::new();
        for &i in &self.members {
            for email in &contacts[i].emails {
                if !all.iter().any(|e| e.eq_ignore_ascii_case(email)) {
                    all.push(email.clone());
                }
            }
        }
        all
    }
}

fn person(contacts: &[Contact], members: Vec<usize>, shown: usize) -> Person {
    let first = &contacts[shown];
    let mut accounts = Vec::new();
    for &i in &members {
        if !accounts.contains(&contacts[i].account_id) {
            accounts.push(contacts[i].account_id);
        }
    }
    Person {
        name: first.display().to_string(),
        email: first.email().unwrap_or_default().to_string(),
        accounts,
        members,
    }
}

fn find(parent: &mut [usize], mut i: usize) -> usize {
    while parent[i] != i {
        parent[i] = parent[parent[i]];
        i = parent[i];
    }
    i
}

/// Contacts that share an address, in any account, or that a link joins,
/// are one person. A linked pair shows the name its link picked.
pub fn group(contacts: &[Contact], links: &[Link]) -> Vec<Person> {
    let mut parent: Vec<usize> = (0..contacts.len()).collect();
    let mut by_email: HashMap<String, usize> = HashMap::new();
    let union = |parent: &mut Vec<usize>, a: usize, b: usize| {
        let (a, b) = (find(parent, a), find(parent, b));
        if a != b {
            parent[b.max(a)] = b.min(a);
        }
    };
    for (i, contact) in contacts.iter().enumerate() {
        for email in &contact.emails {
            match by_email.get(&email.to_lowercase()) {
                Some(&j) => union(&mut parent, i, j),
                None => {
                    by_email.insert(email.to_lowercase(), i);
                }
            }
        }
    }
    let index: HashMap<Key, usize> = contacts
        .iter()
        .enumerate()
        .map(|(i, c)| (key_of(c), i))
        .collect();
    let mut shown: HashMap<usize, usize> = HashMap::new();
    for link in links {
        if let (Some(&a), Some(&b)) = (index.get(&link.a), index.get(&link.b)) {
            union(&mut parent, a, b);
            shown.insert(find(&mut parent, a), if link.shown_a { a } else { b });
        }
    }
    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for i in 0..contacts.len() {
        let root = find(&mut parent, i);
        groups.entry(root).or_default().push(i);
    }
    let mut people: Vec<Person> = groups
        .into_iter()
        .map(|(root, mut members)| {
            members.sort_by_key(|&i| (contacts[i].account_id, i));
            let name_from = shown
                .get(&root)
                .copied()
                .filter(|s| members.contains(s))
                .or_else(|| {
                    members
                        .iter()
                        .copied()
                        .find(|&i| contacts[i].name.as_deref().is_some_and(|n| !n.trim().is_empty()))
                })
                .unwrap_or(members[0]);
            person(contacts, members, name_from)
        })
        .collect();
    sort(&mut people);
    people
}

/// One row per contact of `account`, with no grouping.
pub fn of_account(contacts: &[Contact], account: AccountId) -> Vec<Person> {
    let mut people: Vec<Person> = contacts
        .iter()
        .enumerate()
        .filter(|(_, c)| c.account_id == account)
        .map(|(i, _)| person(contacts, vec![i], i))
        .collect();
    sort(&mut people);
    people
}

fn sort(people: &mut [Person]) {
    people.sort_by_cached_key(|p| p.name.to_lowercase());
}

/// Two contacts of one account that share an address: Work keeps `keep`
/// and deletes `drop`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Merge {
    pub account: AccountId,
    pub keep: usize,
    pub drop: usize,
}

fn weight(contact: &Contact) -> usize {
    usize::from(contact.organization.as_deref().is_some_and(|o| !o.is_empty()))
        + usize::from(contact.phone.is_some() || !contact.details.phones.is_empty())
        + contact.emails.len()
        + contact.details.addresses.len()
        + usize::from(!contact.details.notes.is_empty())
        + usize::from(!contact.details.birthday.is_empty())
}

/// Pairs inside one account that share an address. The contact with more
/// filled in is the one to keep, until the person picks the other.
pub fn same_address(contacts: &[Contact]) -> Vec<Merge> {
    let mut seen: HashMap<(AccountId, String), usize> = HashMap::new();
    let mut pairs: Vec<Merge> = Vec::new();
    for (i, contact) in contacts.iter().enumerate() {
        for email in &contact.emails {
            let slot = (contact.account_id, email.to_lowercase());
            match seen.get(&slot) {
                Some(&j) if j != i => {
                    let (keep, drop) = if weight(&contacts[j]) >= weight(contact) {
                        (j, i)
                    } else {
                        (i, j)
                    };
                    let pair = Merge {
                        account: contact.account_id,
                        keep,
                        drop,
                    };
                    if !pairs.iter().any(|p| {
                        (p.keep, p.drop) == (keep, drop) || (p.keep, p.drop) == (drop, keep)
                    }) {
                        pairs.push(pair);
                    }
                }
                _ => {
                    seen.insert(slot, i);
                }
            }
        }
    }
    pairs.sort_by_cached_key(|p| contacts[p.keep].display().to_lowercase());
    pairs
}

/// Whether two names read as one person written two ways: the same first
/// name, and a last name that is the other's with the rest cut off, as in
/// "Owen Mercer" and "Owen M.".
pub fn close_names(a: &str, b: &str) -> bool {
    let words = |name: &str| -> Vec<String> {
        name.split_whitespace()
            .map(|w| w.trim_matches('.').to_lowercase())
            .filter(|w| !w.is_empty())
            .collect()
    };
    let (a, b) = (words(a), words(b));
    let (Some(a_first), Some(b_first)) = (a.first(), b.first()) else {
        return false;
    };
    let (Some(a_last), Some(b_last)) = (a.last(), b.last()) else {
        return false;
    };
    if a.len() < 2 || b.len() < 2 || a_first != b_first {
        return false;
    }
    a_last.starts_with(b_last.as_str()) || b_last.starts_with(a_last.as_str())
}

/// Two contacts in different accounts, which no address and no link joins
/// already, with close names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pair {
    pub a: usize,
    pub b: usize,
}

pub fn same_person(contacts: &[Contact], links: &[Link]) -> Vec<Pair> {
    let joined = group(contacts, links);
    let together = |a: usize, b: usize| {
        joined
            .iter()
            .any(|p| p.members.contains(&a) && p.members.contains(&b))
    };
    let mut pairs = Vec::new();
    for a in 0..contacts.len() {
        for b in a + 1..contacts.len() {
            let (x, y) = (&contacts[a], &contacts[b]);
            if x.account_id != y.account_id
                && close_names(x.display(), y.display())
                && !together(a, b)
            {
                pairs.push(Pair { a, b });
            }
        }
    }
    pairs.sort_by_cached_key(|p| contacts[p.a].display().to_lowercase());
    pairs
}

/// Whether the person matches every word of `query`, in a name, an
/// address, a phone number or an organization.
pub fn matches(person: &Person, contacts: &[Contact], query: &str) -> bool {
    let mut text = String::new();
    for &i in &person.members {
        let c = &contacts[i];
        text.push_str(&format!(
            "{} {} {} {} {} ",
            c.display(),
            c.emails.join(" "),
            c.details.phones.join(" "),
            c.phone.as_deref().unwrap_or_default(),
            c.organization.as_deref().unwrap_or_default()
        ));
    }
    let text = text.to_lowercase();
    query
        .to_lowercase()
        .split_whitespace()
        .all(|word| text.contains(word))
}

/// `text` as Pango markup with every match of a word of `query` marked.
pub fn highlight(text: &str, query: &str) -> String {
    let words: Vec<String> = query
        .to_lowercase()
        .split_whitespace()
        .map(str::to_string)
        .collect();
    let lower: Vec<(usize, char)> = text.char_indices().collect();
    let mut marked = vec![false; text.len()];
    for word in &words {
        let mut from = 0;
        let haystack = text.to_lowercase();
        // Lower-casing can change byte lengths; only ASCII-stable text is marked.
        if haystack.len() != text.len() {
            continue;
        }
        while let Some(at) = haystack[from..].find(word.as_str()) {
            let start = from + at;
            for slot in &mut marked[start..start + word.len()] {
                *slot = true;
            }
            from = start + word.len().max(1);
        }
    }
    let mut out = String::new();
    let mut open = false;
    for (at, ch) in lower {
        if marked[at] != open {
            out.push_str(if marked[at] {
                "<span background=\"#f6d3bc\" foreground=\"#1f1f23\">"
            } else {
                "</span>"
            });
            open = marked[at];
        }
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
    }
    if open {
        out.push_str("</span>");
    }
    out
}

/// The letters of an avatar: the first name's, and the last name's unless
/// either is abbreviated with a full stop, as "O" for "Owen M.".
pub fn initials(name: &str) -> String {
    let words: Vec<&str> = name.split_whitespace().collect();
    let letter = |w: &str| {
        w.chars()
            .find(|c| c.is_alphanumeric())
            .map(|c| c.to_uppercase().collect::<String>())
    };
    let Some(first) = words.first() else {
        return String::new();
    };
    let mut out = letter(first).unwrap_or_default();
    if let Some(last) = words.last().filter(|_| words.len() > 1)
        && !first.ends_with('.')
        && !last.ends_with('.')
    {
        out.push_str(&letter(last).unwrap_or_default());
    }
    out
}

/// Which of the seven flat avatar colours (`.cts-hue-N` in `style.css`) a
/// name gets, from its first letter, so a person written two ways keeps
/// one colour.
pub fn hue(name: &str) -> usize {
    let letter = name
        .chars()
        .find(|c| c.is_alphabetic())
        .map_or(' ', |c| c.to_ascii_lowercase());
    match letter {
        'p' => 0,
        'o' => 1,
        'h' => 2,
        'j' => 3,
        'm' => 4,
        't' => 5,
        'c' => 6,
        other => other as usize % 7,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contact(account: AccountId, resource: &str, name: &str, emails: &[&str]) -> Contact {
        Contact {
            account_id: account,
            resource: resource.into(),
            name: Some(name.into()),
            emails: emails.iter().map(|e| e.to_string()).collect(),
            ..Contact::default()
        }
    }

    #[test]
    fn one_row_for_a_person_stored_in_two_accounts() {
        let all = [
            contact(1, "a", "Priya Raman", &["priya@fernwood.example"]),
            contact(2, "b", "Priya Raman", &["PRIYA@fernwood.example"]),
            contact(1, "c", "Harbor", &["h@harbor.example"]),
        ];
        let people = group(&all, &[]);
        assert_eq!(people.len(), 2);
        assert_eq!(people[1].accounts, [1, 2]);
    }

    #[test]
    fn two_records_of_one_account_with_one_address_are_one_row() {
        let all = [
            contact(1, "a", "Jonas Hale", &["jonas@fernwood.example"]),
            contact(1, "b", "J. Hale", &["jonas@fernwood.example"]),
        ];
        assert_eq!(group(&all, &[]).len(), 1);
        let pairs = same_address(&all);
        assert_eq!(pairs.len(), 1);
    }

    #[test]
    fn same_address_keeps_the_fuller_contact() {
        let mut full = contact(1, "a", "Jonas Hale", &["j@x.example"]);
        full.organization = Some("Fernwood".into());
        let bare = contact(1, "b", "J. Hale", &["j@x.example"]);
        let pairs = same_address(&[bare, full]);
        assert_eq!((pairs[0].keep, pairs[0].drop), (1, 0));
    }

    #[test]
    fn a_close_name_across_accounts_is_a_pair_until_linked() {
        let all = [
            contact(1, "a", "Owen Mercer", &["owen.mercer@example.com"]),
            contact(2, "b", "Owen M.", &["owen@mercer.example"]),
        ];
        assert_eq!(group(&all, &[]).len(), 2);
        assert_eq!(same_person(&all, &[]).len(), 1);
        let link = Link {
            a: (1, "a".into()),
            b: (2, "b".into()),
            shown_a: true,
        };
        let people = group(&all, std::slice::from_ref(&link));
        assert_eq!(people.len(), 1);
        assert_eq!(people[0].name, "Owen Mercer");
        assert!(same_person(&all, std::slice::from_ref(&link)).is_empty());
        let other = Link {
            shown_a: false,
            ..link
        };
        assert_eq!(group(&all, &[other])[0].name, "Owen M.");
    }

    #[test]
    fn names_that_only_start_alike_are_not_close() {
        assert!(close_names("Owen Mercer", "Owen M."));
        assert!(!close_names("Owen Mercer", "Owen Smith"));
        assert!(!close_names("Owen", "Owen"));
        assert!(!close_names("Ana Reyes", "Joan Reyes"));
    }

    #[test]
    fn search_marks_every_word_and_escapes_markup() {
        let marked = highlight("Harbor & Lane", "harbor lane");
        assert_eq!(marked.matches("<span").count(), 2);
        assert!(marked.contains("&amp;"));
        assert_eq!(highlight("Mara", ""), "Mara");
    }

    #[test]
    fn initials_drop_an_abbreviated_name() {
        assert_eq!(initials("Priya Raman"), "PR");
        assert_eq!(initials("Owen M."), "O");
        assert_eq!(initials("J. Hale"), "J");
        assert_eq!(initials("Trail"), "T");
        assert_eq!(hue("Jonas Hale"), hue("J. Hale"));
    }

    #[test]
    fn a_search_reads_name_address_and_organization() {
        let mut c = contact(1, "a", "Harbor Lane", &["lettings@harborlane.example"]);
        c.organization = Some("Lettings Ltd".into());
        let all = [c];
        let people = group(&all, &[]);
        assert!(matches(&people[0], &all, "harbor ltd"));
        assert!(!matches(&people[0], &all, "mara"));
    }
}
