//! Inbox categories, after Apple Mail: Primary, Updates, Promotions, and
//! Social, built on the category labels Gmail puts on inbox mail.

use serde::{Deserialize, Serialize};

use crate::translate::gettext;

/// One slice of the inbox. `All` shows the whole inbox. Mail with no
/// category label other than Personal counts as `Primary`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Category {
    All,
    Primary,
    Updates,
    Promotions,
    Social,
}

/// The categories a message can be sorted into, as the store keeps them
/// in `message_categories`. Gmail is the only provider with categories,
/// and migration 26 stored them under Gmail's own names, so the names
/// stay Gmail's.
pub const PERSONAL: &str = "CATEGORY_PERSONAL";
pub const UPDATES: &str = "CATEGORY_UPDATES";
pub const PROMOTIONS: &str = "CATEGORY_PROMOTIONS";
pub const SOCIAL: &str = "CATEGORY_SOCIAL";
pub const FORUMS: &str = "CATEGORY_FORUMS";

/// Every category, Personal first.
pub const IDS: [&str; 5] = [PERSONAL, UPDATES, PROMOTIONS, SOCIAL, FORUMS];

/// What Primary leaves out: every category but Personal.
const NOT_PRIMARY: [&str; 4] = [UPDATES, PROMOTIONS, SOCIAL, FORUMS];

impl Category {
    pub const ALL: [Category; 5] = [
        Category::All,
        Category::Primary,
        Category::Updates,
        Category::Promotions,
        Category::Social,
    ];

    /// The name that actions and the assistant's tools pass around.
    pub fn key(self) -> &'static str {
        match self {
            Category::All => "all",
            Category::Primary => "primary",
            Category::Updates => "updates",
            Category::Promotions => "promotions",
            Category::Social => "social",
        }
    }

    pub fn from_key(key: &str) -> Option<Category> {
        Category::ALL.into_iter().find(|c| c.key() == key)
    }

    /// The name the category bar shows.
    pub fn name(self) -> String {
        match self {
            Category::All => gettext("All"),
            Category::Primary => gettext("Primary"),
            Category::Updates => gettext("Updates"),
            Category::Promotions => gettext("Promotions"),
            Category::Social => gettext("Social"),
        }
    }

    /// Categories a thread needs one of, and categories it must not have.
    /// Gmail files mailing lists under Forums; they show with Social here.
    pub fn categories(self) -> (&'static [&'static str], &'static [&'static str]) {
        match self {
            Category::All => (&[], &[]),
            Category::Primary => (&[], &NOT_PRIMARY),
            Category::Updates => (&[UPDATES], &[]),
            Category::Promotions => (&[PROMOTIONS], &[]),
            Category::Social => (&[SOCIAL, FORUMS], &[]),
        }
    }

    /// The category mail sorted into this one carries.
    pub fn id(self) -> &'static str {
        match self {
            Category::All | Category::Primary => PERSONAL,
            Category::Updates => UPDATES,
            Category::Promotions => PROMOTIONS,
            Category::Social => SOCIAL,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Category, FORUMS, IDS, PERSONAL, SOCIAL};

    #[test]
    fn keys_round_trip() {
        for category in Category::ALL {
            assert_eq!(Category::from_key(category.key()), Some(category));
        }
        assert_eq!(Category::from_key("junk"), None);
    }

    #[test]
    fn primary_excludes_every_other_category_but_not_personal() {
        let (any, none) = Category::Primary.categories();
        assert!(any.is_empty());
        assert!(!none.contains(&PERSONAL));
        assert_eq!(none.len(), 4);
    }

    #[test]
    fn all_puts_no_condition_on_labels() {
        let (any, none) = Category::All.categories();
        assert!(any.is_empty() && none.is_empty());
    }

    #[test]
    fn social_includes_forums() {
        let (any, none) = Category::Social.categories();
        assert_eq!(any, [SOCIAL, FORUMS]);
        assert!(none.is_empty());
    }

    #[test]
    fn sorting_uses_a_stored_category() {
        for category in Category::ALL {
            assert!(IDS.contains(&category.id()));
        }
        assert_eq!(Category::Primary.id(), PERSONAL);
    }
}
