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

    pub fn placed(&mut self, email: &str, moved: &[(String, String)], order: &[String]) {
        let root = format!("account:{}", email.to_ascii_lowercase());
        let prefix = format!("{root}:label:");
        let remap = |id: &mut String| {
            if let Some((_, to)) = moved.iter().find(|(from, _)| from == id) {
                *id = to.clone();
            }
        };
        let remap_key = |key: &mut String| {
            if let Some(id) = key.strip_prefix(&prefix) {
                let mut id = id.to_string();
                remap(&mut id);
                *key = format!("{prefix}{id}");
            }
        };
        if let Some(favorites) = &mut self.favorites {
            for favorite in favorites {
                remap_key(&mut favorite.key);
            }
        }
        for key in self.hidden.iter_mut().chain(&mut self.shown) {
            remap_key(key);
        }
        if let Some(keys) = self.account_roots.get_mut(&root) {
            for key in keys {
                remap_key(key);
            }
        }
        if let Some(ids) = self.label_order.get_mut(&root) {
            for id in ids.iter_mut() {
                remap(id);
            }
            *ids = reorder_siblings(std::mem::take(ids), order);
        }
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

pub(crate) fn reorder_siblings(mut ids: Vec<String>, siblings: &[String]) -> Vec<String> {
    for id in siblings {
        if !ids.contains(id) {
            ids.push(id.clone());
        }
    }
    let slots: Vec<usize> = ids
        .iter()
        .enumerate()
        .filter_map(|(i, id)| siblings.contains(id).then_some(i))
        .collect();
    for (slot, id) in slots.into_iter().zip(siblings) {
        ids[slot] = id.clone();
    }
    ids
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renamed_descendants_keep_favorites_visibility_and_sibling_order() {
        let root = "account:me@example.com";
        let key = |id: &str| format!("{root}:label:{id}");
        let mut layout = SidebarLayout {
            favorites: Some(vec![
                FavoriteMailbox {
                    key: key("Work/Child"),
                    name: "My folder".into(),
                },
                FavoriteMailbox {
                    key: "account:other@example.com:label:Work".into(),
                    name: String::new(),
                },
            ]),
            hidden: vec![key("Work"), key("WorkExtra")],
            shown: vec![key("Work/Child")],
            label_order: HashMap::from([(
                root.into(),
                vec!["Work".into(), "Work/Child".into(), "Home".into()],
            )]),
            account_roots: HashMap::from([(
                root.into(),
                vec![key("Work"), format!("{root}:inbox")],
            )]),
            ..Default::default()
        };
        let moved = vec![
            ("Work".into(), "Home/Work".into()),
            ("Work/Child".into(), "Home/Work/Child".into()),
        ];
        layout.placed(
            "Me@Example.com",
            &moved,
            &["Home".into(), "Home/Work".into()],
        );
        assert_eq!(
            layout.favorites()[0],
            FavoriteMailbox {
                key: key("Home/Work/Child"),
                name: "My folder".into()
            }
        );
        assert_eq!(
            layout.favorites()[1].key,
            "account:other@example.com:label:Work"
        );
        assert_eq!(layout.hidden, [key("Home/Work"), key("WorkExtra")]);
        assert_eq!(layout.shown, [key("Home/Work/Child")]);
        assert_eq!(
            layout.label_order[root],
            ["Home", "Home/Work/Child", "Home/Work"]
        );
        assert_eq!(
            layout.account_roots[root],
            [key("Home/Work"), format!("{root}:inbox")]
        );
        layout.placed("me@example.com", &[], &["Home/Work".into(), "Home".into()]);
        assert_eq!(
            layout.label_order[root],
            ["Home/Work", "Home/Work/Child", "Home"]
        );
        let mut defaults = SidebarLayout::default();
        defaults.placed("me@example.com", &moved, &[]);
        assert_eq!(defaults, SidebarLayout::default());
    }

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
