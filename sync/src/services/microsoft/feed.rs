//! The change feed: each synced folder's delta link, as JSON in the
//! account's sync state. A delta entry is the message as it stands, so each
//! becomes a gain of every place and mark it has and a loss of every mark it
//! lacks. A move shows as a removal in one folder and an addition in
//! another under the same id; a removal with no addition beside it is
//! asked about, since the message may have moved to a folder this look did
//! not read.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use mailrs_domain::mailbox::keyword::{FLAGGED, SEEN};
use mailrs_domain::{Membership, Role, category};
use mailrs_graph::{GraphError, Message};
use serde::{Deserialize, Serialize};

use super::{GraphApi, Microsoft, backend, tag_id};
use crate::BackendError;
use crate::services::{Changes, RemoteChange, SyncState};

/// The most delta pages one look reads from one folder: a thousand
/// messages, after which the next look carries on from the link.
const PAGES_PER_LOOK: usize = 20;

/// Where each synced folder's delta stands.
#[derive(Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct MicrosoftState {
    #[serde(default)]
    pub folders: BTreeMap<String, String>,
}

impl MicrosoftState {
    fn read(state: &SyncState) -> Result<MicrosoftState, BackendError> {
        serde_json::from_str(state.as_str()).map_err(|_| BackendError::StateLost)
    }

    fn written(&self) -> SyncState {
        SyncState::new(serde_json::to_string(self).unwrap_or_default())
    }
}

/// Where a folder's read stopped.
enum Left {
    /// The round ended; the next look starts a new one from here.
    Delta(String),
    /// Pages remain; the next look carries on from here.
    Next(String),
}

impl Left {
    fn link(self) -> String {
        match self {
            Left::Delta(link) | Left::Next(link) => link,
        }
    }
}

impl<G: GraphApi> Microsoft<G> {
    /// The window's start as Graph reads a time.
    pub(super) fn window_start(&self) -> String {
        let days = chrono::Duration::days(self.settings().window_days.max(1));
        (chrono::Utc::now() - days).to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    }

    pub(super) async fn feed(&self, since: Option<&SyncState>) -> Result<Changes, BackendError> {
        let synced = self.synced().await?;
        let Some(since) = since else {
            return self.feed_start(synced).await;
        };
        let mut state = MicrosoftState::read(since)?;
        let (due, slow) = self.due(synced);
        let inbox = self.known().roles.get(&Role::Inbox).cloned();
        let (mut removed, mut present) = (Vec::new(), Vec::new());
        for folder in due {
            let start = state.folders.get(&folder).cloned();
            match self.read_folder(&folder, start, &mut removed, &mut present).await {
                Ok(left) => {
                    state.folders.insert(folder, left.link());
                }
                // A folder deleted elsewhere: stop following it and list
                // the folders again, so the store drops it.
                Err(BackendError::NotFound) if Some(&folder) != inbox.as_ref() => {
                    tracing::warn!("a synced folder is gone; no longer following it");
                    state.folders.remove(&folder);
                    self.known().followed.remove(&folder);
                    self.list_mailboxes().await?;
                }
                Err(err) => return Err(err),
            }
        }
        if slow {
            self.slow_poll_done();
        }
        let changes = self.changes_from(removed, present).await?;
        Ok(Changes { changes, state: state.written() })
    }

    /// Where every synced folder stands now, with no changes: each first
    /// round read to its end and thrown away but its link.
    async fn feed_start(&self, synced: Vec<String>) -> Result<Changes, BackendError> {
        let mut state = MicrosoftState::default();
        for folder in synced {
            let mut link = None;
            loop {
                let (mut removed, mut present) = (Vec::new(), Vec::new());
                match self.read_folder(&folder, link, &mut removed, &mut present).await? {
                    Left::Delta(end) => {
                        state.folders.insert(folder, end);
                        break;
                    }
                    Left::Next(next) => link = Some(next),
                }
            }
        }
        Ok(Changes { changes: Vec::new(), state: state.written() })
    }

    /// Reads `folder` from `start` for at most [`PAGES_PER_LOOK`] pages.
    /// `None` starts a first round, whose window comes back as present
    /// messages: that is how a folder a person just opened brings its mail.
    async fn read_folder(
        &self,
        folder: &str,
        start: Option<String>,
        removed: &mut Vec<(String, String)>,
        present: &mut Vec<(String, Message)>,
    ) -> Result<Left, BackendError> {
        let since = self.window_start();
        let mut link = start;
        for _ in 0..PAGES_PER_LOOK {
            let page = self.graph().message_delta(folder, link.as_deref(), &since).await.map_err(backend)?;
            for message in page.value {
                match message.removed.is_some() {
                    true => removed.push((folder.to_string(), message.id)),
                    false => present.push((folder.to_string(), message)),
                }
            }
            match (page.next_link, page.delta_link) {
                (Some(next), _) => link = Some(next),
                (None, Some(delta)) => return Ok(Left::Delta(delta)),
                (None, None) => return Err(BackendError::StateLost),
            }
        }
        link.map(Left::Next).ok_or(BackendError::StateLost)
    }

    async fn changes_from(
        &self,
        removed: Vec<(String, String)>,
        present: Vec<(String, Message)>,
    ) -> Result<Vec<RemoteChange>, BackendError> {
        let here: HashSet<&str> = present.iter().map(|(_, m)| m.id.as_str()).collect();
        let mut changes = Vec::new();
        let mut unpaired = Vec::new();
        for (folder, id) in removed {
            match here.contains(id.as_str()) {
                // Moved between two folders this look read.
                true => changes.push(RemoteChange::Lost { id, memberships: vec![Membership::Mailbox(folder)] }),
                false => unpaired.push((folder, id)),
            }
        }
        for (folder, message) in &present {
            changes.extend(self.restated(message, folder));
        }
        if unpaired.is_empty() {
            return Ok(changes);
        }
        let ids: Vec<String> = unpaired.iter().map(|(_, id)| id.clone()).collect();
        let answers = self.graph().messages(&ids).await.map_err(backend)?;
        for ((folder, id), answer) in unpaired.into_iter().zip(answers) {
            match answer {
                Ok(message) => {
                    let now_in = message.parent_folder_id.clone().unwrap_or_default();
                    // Still here: it left the round's window, nothing more.
                    if now_in == folder {
                        continue;
                    }
                    changes.push(RemoteChange::Lost { id, memberships: vec![Membership::Mailbox(folder)] });
                    changes.extend(self.restated(&message, &now_in));
                }
                Err(GraphError::NotFound) => changes.push(RemoteChange::Deleted { id }),
                Err(err) => return Err(backend(err)),
            }
        }
        Ok(changes)
    }

    /// `message` as it stands in `folder`: a gain of the folder and of each
    /// mark it carries, a loss of each mark it lacks.
    fn restated(&self, message: &Message, folder: &str) -> Vec<RemoteChange> {
        let known = self.known();
        let thread_id = message.conversation_id.clone().unwrap_or_else(|| message.id.clone());
        let mut gained = vec![Membership::Mailbox(folder.to_string())];
        let mut lost = Vec::new();
        let mut mark = |on: bool, membership: Membership| match on {
            true => gained.push(membership),
            false => lost.push(membership),
        };
        mark(message.is_read == Some(true), Membership::Keyword(SEEN.into()));
        mark(message.is_flagged(), Membership::Keyword(FLAGGED.into()));
        mark(message.is_other(), Membership::Category(category::OTHER.into()));
        let carried: BTreeSet<&str> = message.categories.iter().flatten().map(String::as_str).collect();
        for (name, id) in &known.tags {
            mark(carried.contains(name.as_str()), Membership::Mailbox(id.clone()));
        }
        // A category made since the last listing: the engine sees a
        // mailbox it has not listed and lists them again.
        for name in carried.iter().filter(|n| !known.tags.contains_key(**n)) {
            gained.push(Membership::Mailbox(tag_id(name)));
        }
        let mut changes = vec![RemoteChange::Gained { id: message.id.clone(), thread_id, memberships: gained }];
        if !lost.is_empty() {
            changes.push(RemoteChange::Lost { id: message.id.clone(), memberships: lost });
        }
        changes
    }
}
