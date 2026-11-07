//! Saved choices for the mail sidebar. Keys name mailboxes independently of
//! account ids, which can change when an account signs in again.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SidebarLayout {
    /// None keeps the original favorites; Some(empty) shows none.
    pub favorites: Option<Vec<FavoriteMailbox>>,
    /// Rows hidden in their ordinary section. Favorites stay independent.
    pub hidden: Vec<String>,
    /// Rows that were hidden by default but the person chose to show.
    pub shown: Vec<String>,
    /// Order of unified mailboxes; unlisted ones follow in their old order.
    pub mailboxes: Vec<String>,
    /// Order of each account's folders among their siblings, by label id.
    pub label_order: HashMap<String, Vec<String>>,
    /// Order of each account's standard mailboxes and folders.
    pub account_roots: HashMap<String, Vec<String>>,
}

impl SidebarLayout {
    pub fn favorites(&self) -> Vec<FavoriteMailbox> {
        self.favorites.clone().unwrap_or_else(|| {
            ["all:inbox", "vips", "all:flagged", "follow-up"]
                .into_iter()
                .map(|key| FavoriteMailbox {
                    key: key.into(),
                    name: String::new(),
                })
                .collect()
        })
    }

    pub fn visible(&self, key: &str, by_default: bool) -> bool {
        if self.shown.iter().any(|shown| shown == key) {
            true
        } else if self.hidden.iter().any(|hidden| hidden == key) {
            false
        } else {
            by_default
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FavoriteMailbox {
    pub key: String,
    /// Blank keeps the mailbox's own name.
    #[serde(default)]
    pub name: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn favorites_and_ordinary_rows_keep_independent_visibility() {
        let mut layout = SidebarLayout::default();
        assert_eq!(layout.favorites()[0].key, "all:inbox");
        assert!(!layout.visible("all:inbox", false));
        layout.hidden.push("account:a@example.com:inbox".into());
        layout.favorites = Some(vec![FavoriteMailbox {
            key: "account:a@example.com:inbox".into(),
            name: "Personal".into(),
        }]);
        assert!(!layout.visible("account:a@example.com:inbox", true));
        assert_eq!(layout.favorites()[0].name, "Personal");
        layout.shown.push("all:inbox".into());
        assert!(layout.visible("all:inbox", false));
        let saved = serde_json::to_string(&layout).unwrap();
        assert_eq!(
            serde_json::from_str::<SidebarLayout>(&saved).unwrap(),
            layout
        );
    }
}
