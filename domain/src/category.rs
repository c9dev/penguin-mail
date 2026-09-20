//! Inbox categories, after Apple Mail: Primary, Updates, Promotions, and
//! Social, built on the category labels Gmail puts on inbox mail.

use crate::system_label::{
    CATEGORY_FORUMS, CATEGORY_PERSONAL, CATEGORY_PROMOTIONS, CATEGORY_SOCIAL, CATEGORY_UPDATES,
};
use crate::translate::gettext;

/// One slice of the inbox. `All` shows the whole inbox. Mail with no
/// category label other than Personal counts as `Primary`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Category {
    All,
    Primary,
    Updates,
    Promotions,
    Social,
}

/// What Primary leaves out: every category label but Personal.
const NOT_PRIMARY: [&str; 4] = [
    CATEGORY_UPDATES,
    CATEGORY_PROMOTIONS,
    CATEGORY_SOCIAL,
    CATEGORY_FORUMS,
];

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

    /// Labels a thread needs one of, and labels it must not have. Gmail
    /// files mailing lists under Forums; they show with Social here.
    pub fn labels(self) -> (&'static [&'static str], &'static [&'static str]) {
        match self {
            Category::All => (&[], &[]),
            Category::Primary => (&[], &NOT_PRIMARY),
            Category::Updates => (&[CATEGORY_UPDATES], &[]),
            Category::Promotions => (&[CATEGORY_PROMOTIONS], &[]),
            Category::Social => (&[CATEGORY_SOCIAL, CATEGORY_FORUMS], &[]),
        }
    }

    /// The label Gmail gives mail sorted into this category.
    pub fn gmail_label(self) -> &'static str {
        match self {
            Category::All | Category::Primary => CATEGORY_PERSONAL,
            Category::Updates => CATEGORY_UPDATES,
            Category::Promotions => CATEGORY_PROMOTIONS,
            Category::Social => CATEGORY_SOCIAL,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Category;
    use crate::system_label::{self, CATEGORIES};

    #[test]
    fn keys_round_trip() {
        for category in Category::ALL {
            assert_eq!(Category::from_key(category.key()), Some(category));
        }
        assert_eq!(Category::from_key("junk"), None);
    }

    #[test]
    fn primary_excludes_every_other_category_but_not_personal() {
        let (any, none) = Category::Primary.labels();
        assert!(any.is_empty());
        assert!(!none.contains(&system_label::CATEGORY_PERSONAL));
        assert_eq!(none.len(), 4);
    }

    #[test]
    fn all_puts_no_condition_on_labels() {
        let (any, none) = Category::All.labels();
        assert!(any.is_empty() && none.is_empty());
    }

    #[test]
    fn social_includes_forums() {
        let (any, none) = Category::Social.labels();
        assert_eq!(
            any,
            [system_label::CATEGORY_SOCIAL, system_label::CATEGORY_FORUMS]
        );
        assert!(none.is_empty());
    }

    #[test]
    fn sorting_uses_a_gmail_category_label() {
        for category in Category::ALL {
            assert!(CATEGORIES.contains(&category.gmail_label()));
        }
        assert_eq!(
            Category::Primary.gmail_label(),
            system_label::CATEGORY_PERSONAL
        );
    }
}
