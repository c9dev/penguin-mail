//! Hide My Email on Gmail's plus addresses. Gmail delivers
//! `name+anything@domain` to `name@domain`, so each alias is the account
//! address with a random tag such as `kite.fern482`. The Gmail filters
//! behind an alias live in `mailrs_sync::AccountSettings`.

use mailrs_domain::EpochMillis;
use serde::{Deserialize, Serialize};

/// Short words for tags. Two of them and three digits give ten million
/// tags, so a site that knows one alias cannot guess the next.
const WORDS: [&str; 100] = [
    "acorn", "alder", "amber", "aspen", "basil", "bay", "beach", "birch", "bloom", "brook",
    "cedar", "clay", "cliff", "cloud", "clove", "coral", "cove", "crane", "creek", "dawn", "delta",
    "dew", "dove", "dune", "elm", "ember", "fern", "field", "finch", "fjord", "flint", "frost",
    "gale", "glen", "grove", "hazel", "heath", "heron", "hill", "holly", "iris", "ivy", "jade",
    "juniper", "kelp", "kite", "lake", "larch", "lark", "leaf", "lily", "linden", "maple", "marsh",
    "meadow", "mint", "moss", "oak", "olive", "opal", "orchid", "otter", "owl", "palm", "pearl",
    "pebble", "pine", "plum", "pond", "poppy", "quail", "rain", "reed", "ridge", "river", "robin",
    "rose", "rush", "sage", "sand", "shore", "sky", "slate", "snow", "sorrel", "spruce", "stone",
    "swan", "thyme", "tide", "vale", "wave", "willow", "wind", "wren", "yarrow", "yew", "zephyr",
    "tulip", "lotus",
];

/// One alias and the Gmail filters behind it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HiddenAddress {
    /// The account address the alias delivers to.
    pub account: String,
    pub address: String,
    /// Where the user gave the alias out.
    #[serde(default)]
    pub note: String,
    pub created: EpochMillis,
    /// False once deactivated: mail to it goes to the Trash.
    #[serde(default = "yes")]
    pub active: bool,
    /// The filter that labels mail to the alias.
    #[serde(default)]
    pub label_filter: Option<String>,
    /// The filter that trashes mail to the alias while it is deactivated.
    #[serde(default)]
    pub trash_filter: Option<String>,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotAnAddress;

impl std::fmt::Display for NotAnAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("that is not an email address")
    }
}

impl std::error::Error for NotAnAddress {}

/// Splits `address` into its local part, without any `+tag`, and its domain.
fn base(address: &str) -> Result<(&str, &str), NotAnAddress> {
    let address = address.trim();
    let (local, domain) = address.rsplit_once('@').ok_or(NotAnAddress)?;
    let local = local.split_once('+').map_or(local, |(name, _)| name);
    let valid = !local.is_empty()
        && !local.contains('@')
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !address.chars().any(|c| c.is_whitespace() || c.is_control());
    if valid {
        Ok((local, domain))
    } else {
        Err(NotAnAddress)
    }
}

/// The alias of `account` built from word indexes and a number below 1000.
pub fn alias_from(
    account: &str,
    first: usize,
    second: usize,
    number: u16,
) -> Result<String, NotAnAddress> {
    let (local, domain) = base(account)?;
    let word = |i: usize| WORDS[i % WORDS.len()];
    Ok(format!(
        "{local}+{}.{}{:03}@{}",
        word(first),
        word(second),
        number % 1000,
        domain.to_lowercase()
    ))
}

/// A new random alias of `account`.
pub fn generate(account: &str) -> Result<String, NotAnAddress> {
    let first = rand::random_range(0..WORDS.len());
    // Two different words read better than "fern.fern".
    let second = (first + rand::random_range(1..WORDS.len())) % WORDS.len();
    alias_from(account, first, second, rand::random_range(0..1000))
}

/// Whether `address` has a tag of the shape `generate` makes.
pub fn is_alias(address: &str) -> bool {
    let Some((local, _)) = address.trim().rsplit_once('@') else {
        return false;
    };
    let Some((_, tag)) = local.split_once('+') else {
        return false;
    };
    let Some((first, rest)) = tag.split_once('.') else {
        return false;
    };
    if rest.len() < 4 || !rest.is_char_boundary(rest.len() - 3) {
        return false;
    }
    let (second, digits) = rest.split_at(rest.len() - 3);
    let known = |w: &str| WORDS.iter().any(|word| word.eq_ignore_ascii_case(w));
    known(first) && known(second) && digits.bytes().all(|b| b.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(word: &str) -> usize {
        WORDS.iter().position(|w| *w == word).unwrap()
    }

    #[test]
    fn aliases_add_a_readable_tag() {
        assert_eq!(
            alias_from("dana@gmail.com", 0, 1, 7).unwrap(),
            "dana+acorn.alder007@gmail.com"
        );
    }

    #[test]
    fn an_existing_tag_is_replaced() {
        assert_eq!(
            alias_from(" dana+news@Example.COM ", pos("kite"), pos("fern"), 482).unwrap(),
            "dana+kite.fern482@example.com"
        );
    }

    #[test]
    fn non_addresses_are_rejected() {
        for bad in [
            "",
            "dana",
            "@gmail.com",
            "+tag@gmail.com",
            "dana@",
            "dana@localhost",
            "dana@.com",
            "da na@gmail.com",
            "a@b@gmail.com",
        ] {
            assert_eq!(generate(bad), Err(NotAnAddress), "{bad:?}");
        }
    }

    #[test]
    fn generated_aliases_are_recognized() {
        for _ in 0..200 {
            let alias = generate("dana@gmail.com").unwrap();
            assert!(alias.starts_with("dana+"), "{alias}");
            assert!(alias.ends_with("@gmail.com"), "{alias}");
            assert!(is_alias(&alias), "{alias}");
            let tag = alias.split(['+', '@']).nth(1).unwrap();
            let (first, second) = tag.split_once('.').unwrap();
            assert_ne!(first, &second[..second.len() - 3], "{alias}");
        }
    }

    #[test]
    fn other_addresses_are_not_aliases() {
        for other in [
            "dana@gmail.com",
            "dana+news@gmail.com",
            "dana+kite.fern@gmail.com",
            "dana+kite.fern48@gmail.com",
            "dana+kite.gizmo482@gmail.com",
            "dana+kite.fern48x@gmail.com",
            "not an address",
        ] {
            assert!(!is_alias(other), "{other}");
        }
    }
}
