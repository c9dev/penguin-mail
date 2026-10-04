//! The account's folders, their roles, and its Outlook categories as tags.

use std::collections::VecDeque;

use mailrs_domain::{MailboxKind, RemoteMailbox, Role};

use super::{GraphApi, Microsoft, backend, tag_id};
use crate::BackendError;

/// Graph's well-known names for the folders with a role.
const WELL_KNOWN: [(&str, Role); 6] = [
    ("inbox", Role::Inbox),
    ("sentitems", Role::Sent),
    ("drafts", Role::Drafts),
    ("deleteditems", Role::Trash),
    ("junkemail", Role::Junk),
    ("archive", Role::Archive),
];

/// The most folders one listing walks. A mailbox with more lists the first
/// thousand, breadth first, and says so in the log.
const MOST_FOLDERS: usize = 1000;

/// Outlook's category presets as `#rrggbb`, as Outlook on the web draws
/// them. `none` has no colour.
pub(super) const PRESETS: [(&str, &str); 25] = [
    ("preset0", "#e74856"), ("preset1", "#ff8c00"), ("preset2", "#ab620d"), ("preset3", "#fff100"),
    ("preset4", "#47d041"), ("preset5", "#30c6cc"), ("preset6", "#73aa24"), ("preset7", "#4a8ee0"),
    ("preset8", "#a473da"), ("preset9", "#ee5fb7"), ("preset10", "#7688a8"), ("preset11", "#4c596e"),
    ("preset12", "#abb3bb"), ("preset13", "#616b76"), ("preset14", "#474747"), ("preset15", "#750b1c"),
    ("preset16", "#ca5010"), ("preset17", "#6d4a1c"), ("preset18", "#c19c00"), ("preset19", "#0b6a0b"),
    ("preset20", "#038387"), ("preset21", "#5c7e0e"), ("preset22", "#004e8c"), ("preset23", "#5c2e91"),
    ("preset24", "#9b1c5a"),
];

pub(super) fn preset_color(preset: &str) -> Option<String> {
    PRESETS.iter().find(|(p, _)| p.eq_ignore_ascii_case(preset)).map(|(_, hex)| hex.to_string())
}

impl<G: GraphApi> Microsoft<G> {
    /// Every folder, walked breadth first, with its role, and every
    /// category as a tag. Refreshes what the adapter knows.
    pub(super) async fn list_mailboxes(&self) -> Result<Vec<RemoteMailbox>, BackendError> {
        let names: Vec<&str> = WELL_KNOWN.iter().map(|(n, _)| *n).collect();
        let found = self.graph().well_known(&names).await.map_err(backend)?;
        let roles: Vec<(Role, String)> = WELL_KNOWN
            .iter()
            .zip(found)
            .filter_map(|((_, role), folder)| folder.map(|f| (*role, f.id)))
            .collect();
        let mut listed = Vec::new();
        let (mut paths, mut totals) = (std::collections::HashMap::new(), std::collections::HashMap::new());
        let mut queue: VecDeque<(Option<String>, Option<String>)> = VecDeque::from([(None, None)]);
        'walk: while let Some((parent, path)) = queue.pop_front() {
            let mut next: Option<String> = None;
            loop {
                let page = self.graph().folders(parent.as_deref(), next.as_deref()).await.map_err(backend)?;
                for folder in page.value {
                    if listed.len() >= MOST_FOLDERS {
                        tracing::warn!("the mailbox has more than {MOST_FOLDERS} folders; listing the first");
                        break 'walk;
                    }
                    let name = path.as_ref().map_or_else(|| folder.display_name.clone(), |p| format!("{p}/{}", folder.display_name));
                    let role = roles.iter().find(|(_, id)| *id == folder.id).map(|(r, _)| *r);
                    if folder.child_folder_count > 0 {
                        queue.push_back((Some(folder.id.clone()), Some(name.clone())));
                    }
                    paths.insert(folder.id.clone(), name.clone());
                    totals.insert(folder.id.clone(), folder.total_item_count);
                    listed.push(RemoteMailbox {
                        id: folder.id,
                        name,
                        kind: if role.is_some() { MailboxKind::System } else { MailboxKind::Folder },
                        role,
                        color: None,
                        hidden: folder.is_hidden,
                    });
                }
                match page.next_link {
                    Some(link) => next = Some(link),
                    None => break,
                }
            }
        }
        let categories = self.graph().categories().await.map_err(backend)?;
        let (mut tags, mut category_ids) = (std::collections::BTreeMap::new(), std::collections::BTreeMap::new());
        for category in categories {
            let id = tag_id(&category.display_name);
            tags.insert(category.display_name.clone(), id.clone());
            category_ids.insert(category.display_name.clone(), category.id.clone());
            listed.push(RemoteMailbox {
                id,
                name: category.display_name,
                kind: MailboxKind::Tag,
                role: None,
                color: preset_color(&category.color),
                hidden: false,
            });
        }
        let mut known = self.known();
        known.roles = roles.into_iter().collect();
        known.names = paths;
        known.totals = totals;
        known.tags = tags;
        known.category_ids = category_ids;
        known.listed = true;
        Ok(listed)
    }

    /// The folders the feed reads: every folder with a role, and the ones
    /// a person opened. Lists the folders first if nothing has yet.
    pub(super) async fn synced(&self) -> Result<Vec<String>, BackendError> {
        if !self.known().listed {
            self.list_mailboxes().await?;
        }
        let known = self.known();
        let mut synced: Vec<String> = known.roles.values().cloned().collect();
        synced.extend(known.followed.iter().filter(|f| !synced.contains(f)).cloned().collect::<Vec<_>>());
        Ok(synced)
    }
}
