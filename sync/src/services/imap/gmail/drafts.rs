//! Drafts over Gmail's IMAP. A draft is a message in Drafts flagged
//! `\Draft`, and its id is the message's hex id. Gmail keeps every message
//! in All Mail too, so an old draft is moved to Trash and expunged there:
//! expunging it from Drafts would only archive it.

use mailrs_domain::Role;
use mailrs_imap::{UidSet, gm_labels};

use super::{Imap, ImapApi, Submit};
use crate::BackendError;
use crate::api::{DraftRef, SavedDraft};

const DRAFT: &str = "\\Draft";
const SEEN: &str = "\\Seen";
const DELETED: &str = "\\Deleted";

impl<I: ImapApi, S: Submit> Imap<I, S> {
    /// Saves `raw` as a draft, replacing the one `draft_id` names.
    pub(in crate::services::imap) async fn gm_save_draft(
        &self,
        draft_id: Option<&str>,
        raw: &[u8],
        thread_id: Option<&str>,
    ) -> Result<SavedDraft, BackendError> {
        self.ensure_listed().await?;
        let drafts = self
            .known()
            .folders
            .iter()
            .find(|f| f.role == Some(Role::Drafts))
            .map(|f| f.id.clone())
            .ok_or(BackendError::Unsupported)?;
        let appended = self
            .api
            .append(&drafts, &[DRAFT.to_string(), SEEN.to_string()], raw)
            .await?
            .ok_or_else(|| BackendError::Refused("Gmail gave no place for the draft".into()))?;
        let set = UidSet::from_uids([appended.uid]);
        let id = self
            .api
            .gmail_flags(&drafts, &set, None)
            .await?
            .into_iter()
            .find_map(|f| f.gmail)
            .map(|g| g.id())
            .ok_or(BackendError::NotFound)?;
        // Where the draft sits in All Mail, so opening it finds it.
        self.gm_find(std::slice::from_ref(&id)).await?;
        // The new copy is on the server, so an old one left behind is a
        // second draft, never a lost one.
        if let Some(old) = draft_id
            && old != id
            && let Err(err) = self.gm_delete_draft(old).await
            && !matches!(err, BackendError::NotFound)
        {
            tracing::warn!(%err, "saved the draft, but could not remove the copy it replaces");
        }
        Ok(SavedDraft {
            draft_id: id.clone(),
            message_id: id.clone(),
            thread_id: thread_id.map_or(id, str::to_string),
        })
    }

    /// Erases a draft: to Trash, then expunged there.
    pub(in crate::services::imap) async fn gm_delete_draft(
        &self,
        id: &str,
    ) -> Result<(), BackendError> {
        let boxes = self.gm_synced().await?;
        let at = self.gm_place(id).await?;
        let uids = UidSet::from_uids([at.uid]);
        let (mailbox, uids) = match boxes.place_of(&at.mailbox) {
            super::labels::GmailPlace::AllMail => {
                let copied = self.api.move_to(&at.mailbox, &uids, &boxes.trash).await?;
                self.known().places.forget(id);
                let uid = match copied.and_then(|c| c.pairs.first().map(|p| p.1)) {
                    Some(uid) => uid,
                    None => self
                        .gm_find(&[id.to_string()])
                        .await?
                        .remove(id)
                        .map(|at| at.uid)
                        .ok_or(BackendError::NotFound)?,
                };
                (boxes.trash.clone(), UidSet::from_uids([uid]))
            }
            _ => (at.mailbox.clone(), uids),
        };
        self.api
            .store(&mailbox, &uids, true, &[DELETED.to_string()])
            .await?;
        self.api.expunge(&mailbox, &uids).await?;
        self.known().places.forget(id);
        Ok(())
    }

    /// Sends the draft as the server holds it, then erases it.
    pub(in crate::services::imap) async fn gm_send_draft(
        &self,
        id: &str,
    ) -> Result<String, BackendError> {
        let raw = self
            .raw_messages(&[id.to_string()])
            .await?
            .into_iter()
            .next()
            .ok_or(BackendError::NotFound)?
            .bytes;
        let sent = self.submit(&raw).await?;
        if let Err(err) = self.gm_delete_draft(id).await {
            tracing::warn!(%err, "sent the draft, but could not remove it from Drafts");
        }
        Ok(sent)
    }

    /// Every draft, by hex id.
    pub(in crate::services::imap) async fn gm_list_drafts(
        &self,
    ) -> Result<Vec<DraftRef>, BackendError> {
        let boxes = self.gm_synced().await?;
        let keys = format!("{} UNDELETED", gm_labels(DRAFT));
        let selected = self.select(&boxes.all, None).await?;
        let uids = self.api.search(&boxes.all, &keys).await?;
        Ok(self
            .gm_pair(&boxes.all, selected.uidvalidity, &uids)
            .await?
            .into_iter()
            .filter_map(|f| f.gmail)
            .map(|g| DraftRef {
                draft_id: g.id(),
                message_id: g.id(),
            })
            .collect())
    }
}
