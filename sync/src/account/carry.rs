//! The carrying of a message between accounts, from this account's
//! side: what a message was where it lived, landing a copy that came
//! from elsewhere, and taking one back. The order of a carry, land
//! first and erase after, belongs to the action that runs it; nothing
//! here removes a message from anywhere.

use mailrs_domain::Target;
use mailrs_store::messages;

use super::AccountSync;
use crate::services::{Landing, MailBackend};
use crate::{BackendError, RawMessage, SyncError};

/// One message read out to be carried, and where it sat: enough to
/// file it back through the same door. The bytes ride separately, read
/// from the server when the carry is ready for them.
#[derive(Debug, Clone)]
pub(crate) struct CopyOut {
    /// The store's id for the message.
    pub id: String,
    /// The mailboxes the message sat in, by this account's ids. A label
    /// account lists all it was filed under; a folder account, the one.
    pub mailboxes: Vec<String>,
    pub keywords: Vec<String>,
    /// The hour the message arrived, as this account's server kept it.
    pub at: i64,
}

impl AccountSync {
    /// The messages a carry takes with it: the whole conversation, or
    /// the one message `target` names, each with its place at home.
    /// The store must know them; a conversation the person only ever
    /// saw listed has to be fetched before a message can be read out
    /// of it, which a carry will not do silently.
    pub(crate) async fn copies_to_carry(
        &self,
        target: &Target,
    ) -> Result<Vec<CopyOut>, SyncError> {
        let account_id = self.account_id;
        let thread = target.thread_id.clone();
        let only = target.message_id.clone();
        let read = self
            .db
            .read(move |c| {
                Ok(
                    messages::thread_messages(c, account_id, &thread)?
                        .into_iter()
                        .filter(|m| only.as_ref().is_none_or(|id| *id == m.id))
                        .map(|m| CopyOut {
                            id: m.id,
                            mailboxes: m.held.mailboxes,
                            keywords: m.held.keywords,
                            at: m.date,
                        })
                        .collect::<Vec<CopyOut>>(),
                )
            })
            .await?;
        if read.is_empty() {
            return Err(BackendError::NotFound.into());
        }
        Ok(read)
    }

    /// Erases one message here, the server first and the store's rows
    /// after: the last step of a carry, once a copy has landed. When
    /// the server refuses, the rows stay, and the carry says the mail
    /// is in both places.
    pub(crate) async fn erase_carried(&self, target: &Target, id: &str) -> Result<(), SyncError> {
        let moves = self.mail.hold_moves().await;
        self.erase_ids(std::slice::from_ref(target), vec![vec![id.to_string()]], &moves, None)
            .await?
            .into_iter()
            .next()
            .unwrap_or_else(|| Err(SyncError::from(BackendError::NotFound)))
    }

    /// The messages as they arrived, each under its store id: what a
    /// carry sends to the other account.
    pub(crate) async fn fetch_raw(
        &self,
        ids: &[String],
    ) -> Result<Vec<RawMessage>, SyncError> {
        self.mail.fetch_raw(ids).await
    }

    /// Lands `bytes` as this account's own message: filed where `at`
    /// says, keeping its keywords and its arrival hour. Answers the
    /// copy's name here, which is the name that takes it back.
    pub(crate) async fn land(&self, bytes: &[u8], at: &Landing<'_>) -> Result<String, SyncError> {
        Ok(self.mail.untranslated().land_copy(bytes, at).await?)
    }

    /// The bytes a copy landed here carried, asked by the name the
    /// landing gave. The name is the server's, so the store is not
    /// asked: a landed copy has no row until this account's next look.
    pub(crate) async fn landed_bytes(&self, name: &str) -> Result<Vec<u8>, SyncError> {
        Ok(self
            .mail
            .untranslated()
            .fetch_raw(&[name.to_string()])
            .await?
            .pop()
            .map(|raw| raw.bytes)
            .ok_or(BackendError::NotFound)?)
    }

    /// Takes back a copy this account landed, by its landing name.
    pub(crate) async fn take_back(&self, name: &str) -> Result<(), SyncError> {
        Ok(self
            .mail
            .untranslated()
            .destroy_checked(&[name.to_string()], "")
            .await
            .into_iter()
            .next()
            .unwrap_or(Err(BackendError::NotFound))?)
    }
}
