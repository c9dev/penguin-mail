//! The newsletters of one account: the senders the store already knows,
//! and the few fetches that fill in what it does not.
//!
//! Every metadata fetch asks Gmail for the unsubscribe headers, so mail
//! synced since that change answers for nothing. Mail stored before it has
//! empty columns, and this is where they are filled: for a sender that
//! looks like a list and has no header stored, one `messages.get` on their
//! newest message, at background priority, and the answer is kept.

use std::sync::Arc;

use mailrs_domain::{AccountId, EpochMillis};
use mailrs_store::Db;
use mailrs_store::messages;
use mailrs_store::newsletters::{self, Sender};

use crate::{Accounts, SyncError, now_millis};

/// How far back the list counts a sender's mail. A list the owner has not
/// heard from in three months is one they have already left, or one that
/// left them.
pub const WINDOW: EpochMillis = 90 * 24 * 60 * 60 * 1000;

/// Messages a sender needs before a fetch goes out to find their header.
/// One or two messages in Promotions is as often a shop's receipt as a
/// list, and each fetch costs five quota units.
const ENOUGH_TO_ASK: u32 = 3;

pub struct Newsletters<A: Accounts> {
    accounts: Arc<A>,
    db: Db,
}

impl<A: Accounts> Newsletters<A> {
    pub fn new(accounts: Arc<A>, db: Db) -> Self {
        Newsletters { accounts, db }
    }

    /// The account's newsletter senders, newest first.
    ///
    /// The store answers for every sender. A sender with no stored header
    /// and enough mail to look like a list costs one metadata fetch, which
    /// goes out at background priority and is kept, so the next listing
    /// asks nothing. A fetch that fails leaves the sender as the store had
    /// them: the listing is worth having without it. A sender whose newest
    /// message turns out to carry no header is asked about again next
    /// time, because the store cannot tell that from never having asked.
    pub async fn list(&self, account_id: AccountId) -> Result<Vec<Sender>, SyncError> {
        let since = now_millis() - WINDOW;
        let mut senders = self
            .db
            .read(move |c| newsletters::list(c, account_id, since))
            .await?;
        let wanted: Vec<String> = senders
            .iter()
            .filter(|s| s.header.is_none() && s.messages >= ENOUGH_TO_ASK)
            .map(|s| s.message_id.clone())
            .collect();
        if wanted.is_empty() {
            return Ok(senders);
        }
        let sync = self
            .accounts
            .account(account_id)
            .ok_or(SyncError::UnknownAccount(account_id))?;
        let fetched = match crate::background(sync.fetch_metadata(&wanted)).await {
            Ok(fetched) => fetched,
            Err(err) => {
                tracing::debug!(account = account_id, error = %err, "could not look up newsletter headers");
                return Ok(senders);
            }
        };
        for meta in fetched {
            let Some(sender) = senders.iter_mut().find(|s| s.message_id == meta.id) else {
                continue;
            };
            sender.header.clone_from(&meta.list_unsubscribe);
            sender.one_click = meta.one_click;
            let stored = self
                .db
                .write(move |c| {
                    messages::set_unsubscribe(
                        c,
                        account_id,
                        &meta.id,
                        meta.list_unsubscribe.as_deref(),
                        meta.one_click,
                    )
                })
                .await;
            if let Err(err) = stored {
                tracing::warn!(account = account_id, error = %err, "could not keep a newsletter header");
            }
        }
        Ok(senders)
    }
}
