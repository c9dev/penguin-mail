//! Searching on Graph with `$search`, in KQL.

mod kql;

use mailrs_domain::MailSet;
use mailrs_graph::{Fields, Listing};

use super::{GraphApi, Microsoft, backend};
use crate::BackendError;
use crate::services::{RemoteRef, SearchQuery};
use kql::Place;

impl<G: GraphApi> Microsoft<G> {
    pub(super) async fn find(&self, query: &SearchQuery, limit: usize) -> Result<Vec<RemoteRef>, BackendError> {
        let SearchQuery::Tree(tree) = query else {
            // A Microsoft account reads no search syntax of its own; the
            // caller parses what was typed into a tree first.
            return Err(BackendError::Unsupported);
        };
        let today = chrono::Local::now().date_naive();
        let printed = kql::print(tree, today).map_err(|_| BackendError::Unsupported)?;
        self.synced().await?;
        let folder = match printed.within {
            None => None,
            Some(Place::Set(MailSet::Role(role))) => Some(self.known().roles.get(&role).cloned().ok_or(BackendError::Unsupported)?),
            Some(Place::Set(MailSet::Mailbox(id))) => Some(id),
            Some(Place::Set(_)) => return Err(BackendError::Unsupported),
            Some(Place::Named(name)) => {
                let wanted = name.to_lowercase();
                let found = self.known().names.iter().find(|(_, path)| path.to_lowercase() == wanted).map(|(id, _)| id.clone());
                Some(found.ok_or(BackendError::Unsupported)?)
            }
        };
        let listing = Listing {
            folder,
            search: (!printed.kql.is_empty()).then_some(printed.kql),
            top: u32::try_from(limit.clamp(1, 250)).unwrap_or(250),
            fields: Fields::Ids,
            ..Listing::default()
        };
        let page = self.graph().list_messages(&listing, None).await.map_err(backend)?;
        Ok(page
            .value
            .into_iter()
            .take(limit)
            .map(|m| RemoteRef { thread_id: m.conversation_id.clone().unwrap_or_else(|| m.id.clone()), id: m.id })
            .collect())
    }
}
