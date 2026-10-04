//! Inbox categories, after Apple Mail: Primary, Updates, Promotions, and
//! Social, built on the category labels Gmail puts on inbox mail.

use serde::{Deserialize, Serialize};

use crate::translate::{gettext, pgettext};

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
    /// Focused Inbox's first tab, over a Microsoft account's inbox.
    Focused,
    /// Focused Inbox's second tab.
    Other,
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

/// The category Microsoft's Focused Inbox gives the inbox mail it files
/// under Other. The store keeps it beside Gmail's; no Gmail message ever
/// carries it, so Gmail's slices never name it.
pub const OTHER: &str = "FOCUS_OTHER";

/// Every Gmail category, Personal first.
pub const IDS: [&str; 5] = [PERSONAL, UPDATES, PROMOTIONS, SOCIAL, FORUMS];

/// What Primary leaves out: every category but Personal.
const NOT_PRIMARY: [&str; 4] = [UPDATES, PROMOTIONS, SOCIAL, FORUMS];

impl Category {
    /// Gmail's five, in the order its category bar shows them.
    pub const ALL: [Category; 5] = [
        Category::All,
        Category::Primary,
        Category::Updates,
        Category::Promotions,
        Category::Social,
    ];

    /// Focused Inbox's two tabs, over a Microsoft account's inbox.
    pub const FOCUS: [Category; 2] = [Category::Focused, Category::Other];

    /// Every slice the store counts unread mail for.
    pub const COUNTED: [Category; 7] = [
        Category::All,
        Category::Primary,
        Category::Updates,
        Category::Promotions,
        Category::Social,
        Category::Focused,
        Category::Other,
    ];

    /// The name that actions and the assistant's tools pass around.
    pub fn key(self) -> &'static str {
        match self {
            Category::All => "all",
            Category::Primary => "primary",
            Category::Updates => "updates",
            Category::Promotions => "promotions",
            Category::Social => "social",
            Category::Focused => "focused",
            Category::Other => "other",
        }
    }

    pub fn from_key(key: &str) -> Option<Category> {
        Category::COUNTED.into_iter().find(|c| c.key() == key)
    }

    /// The name the category bar shows.
    pub fn name(self) -> String {
        match self {
            Category::All => gettext("All"),
            Category::Primary => gettext("Primary"),
            Category::Updates => gettext("Updates"),
            Category::Promotions => gettext("Promotions"),
            Category::Social => gettext("Social"),
            // Outlook's two inboxes. "Focused" alone already names a
            // calendar event kind, so these carry a context of their own.
            Category::Focused => pgettext("inbox", "Focused"),
            Category::Other => pgettext("inbox", "Other"),
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
            Category::Focused => (&[], &[OTHER]),
            Category::Other => (&[OTHER], &[]),
        }
    }
}

/// What sorting mail into a slice changes on it: the category it gains,
/// if any, and the ones it loses. Focused has no category of its own: it
/// is inbox mail without Other.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sorting {
    pub gains: Option<&'static str>,
    pub loses: Vec<&'static str>,
}

impl Category {
    pub fn sorting(self) -> Sorting {
        let gmail = |id: &'static str| Sorting {
            gains: Some(id),
            loses: IDS.iter().copied().filter(|c| *c != id).collect(),
        };
        match self {
            Category::All | Category::Primary => gmail(PERSONAL),
            Category::Updates => gmail(UPDATES),
            Category::Promotions => gmail(PROMOTIONS),
            Category::Social => gmail(SOCIAL),
            Category::Focused => Sorting { gains: None, loses: vec![OTHER] },
            Category::Other => Sorting { gains: Some(OTHER), loses: Vec::new() },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Category, FORUMS, IDS, OTHER, PERSONAL, SOCIAL};

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
            assert!(IDS.contains(&category.sorting().gains.unwrap()));
        }
    }

    #[test]
    fn gmails_list_leaves_out_the_focused_inbox_slices() {
        for category in Category::FOCUS {
            assert!(!Category::ALL.contains(&category));
        }
    }

    #[test]
    fn other_holds_focus_other_and_focused_holds_none_of_it() {
        assert_eq!(Category::Other.categories(), (&[OTHER][..], &[][..]));
        assert_eq!(Category::Focused.categories(), (&[][..], &[OTHER][..]));
    }

    #[test]
    fn gmails_slices_never_name_other() {
        for category in Category::ALL {
            let (any, none) = category.categories();
            assert!(!any.contains(&OTHER) && !none.contains(&OTHER), "{category:?}");
        }
    }

    #[test]
    fn every_counted_slice_round_trips_its_key() {
        for category in Category::COUNTED {
            assert_eq!(Category::from_key(category.key()), Some(category));
        }
        assert_eq!(Category::COUNTED.len(), Category::ALL.len() + Category::FOCUS.len());
    }

    #[test]
    fn sorting_into_focused_takes_other_off_and_into_other_puts_it_on() {
        let focused = Category::Focused.sorting();
        assert_eq!((focused.gains, focused.loses), (None, vec![OTHER]));
        let other = Category::Other.sorting();
        assert_eq!((other.gains, other.loses), (Some(OTHER), vec![]));
    }

    #[test]
    fn sorting_into_a_gmail_category_leaves_the_others() {
        let social = Category::Social.sorting();
        assert_eq!(social.gains, Some(SOCIAL));
        assert!(!social.loses.contains(&SOCIAL));
        assert_eq!(social.loses.len(), IDS.len() - 1);
        assert_eq!(Category::Primary.sorting().gains, Some(PERSONAL));
    }
}
