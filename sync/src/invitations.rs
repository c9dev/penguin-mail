//! The invitation in one message: what it says, what it changes about an
//! event the user already has, and the answer they send back.
//!
//! Only this module knows that an organizer numbers each change to an
//! event with a sequence under one UID, that a higher sequence makes the
//! answer the user gave stale, and that Google Calendar takes the answer
//! while Gmail does not. The card in the window and its tests both go
//! through here, so neither works out any of that for itself.

use std::sync::Arc;

use mailrs_domain::invitation::{self, Answer, Invitation, When};
use mailrs_domain::{AccountId, EpochMillis};
use mailrs_gmail::{Answered, GmailError};
use mailrs_store::{Db, invitations as store};

use crate::settings::Permitted;
use crate::{AccountSync, Accounts, SyncError};

/// What a message does to an event the user already has. `None` alongside
/// it means the message is the first word on this event, or says nothing
/// the user has not seen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// The event moved. `was` is the start it had before.
    Moved {
        was: EpochMillis,
        /// Whether the start it had before was an all-day one.
        all_day: bool,
    },
    /// Something other than the start changed.
    Updated,
    /// The organizer called off an event the user already has.
    Cancelled,
}

/// One invitation as the window shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opened {
    pub invitation: Invitation,
    pub change: Option<Change>,
    /// The answer the user gave this version of the event, if they have.
    pub answer: Option<Answer>,
}

pub struct Invitations<A: Accounts> {
    accounts: Arc<A>,
    db: Db,
}

impl<A: Accounts> Invitations<A> {
    pub fn new(accounts: Arc<A>, db: Db) -> Self {
        Invitations { accounts, db }
    }

    /// Reads the `text/calendar` part of a message, records the version of
    /// the event it carries, and says what it changes and what the user
    /// already answered. `None` means the part held no event to show.
    pub async fn open(
        &self,
        account_id: AccountId,
        message_id: &str,
        ics: &str,
        now: EpochMillis,
    ) -> Result<Option<Opened>, SyncError> {
        let Some(invitation) = invitation::read(ics) else {
            return Ok(None);
        };
        // An invitation with no UID is one nothing can be matched against,
        // and answering it would write over every other such event.
        if invitation.uid.trim().is_empty() {
            return Ok(Some(Opened {
                invitation,
                change: None,
                answer: None,
            }));
        }
        let seen = row(&invitation, message_id);
        let uid = invitation.uid.clone();
        let (change, answer) = self
            .db
            .write(move |c| {
                let held = store::saved(c, account_id, &uid)?;
                let change = compare(held.as_ref(), &seen);
                store::remember(c, account_id, &seen, now)?;
                let answer = store::saved(c, account_id, &uid)?.and_then(|row| row.answer);
                Ok((change, answer))
            })
            .await?;
        Ok(Some(Opened {
            invitation,
            change,
            answer,
        }))
    }

    /// Sends the user's answer to Google Calendar and remembers it, so
    /// reopening the message shows it. `Permitted::NeedsPermission` means
    /// the account has not granted the calendar permission yet, and the
    /// caller offers to ask for it.
    pub async fn answer(
        &self,
        account_id: AccountId,
        uid: &str,
        me: &str,
        answer: Answer,
    ) -> Result<Permitted<Answered>, SyncError> {
        let sync = self.sync(account_id)?;
        let sent = match sync.answer_invitation(uid, me, answer).await {
            Ok(sent) => sent,
            Err(SyncError::Gmail(GmailError::MissingScope)) => {
                return Ok(Permitted::NeedsPermission);
            }
            Err(err) => return Err(err),
        };
        if sent == Answered::Done {
            let uid = uid.to_string();
            self.db
                .write(move |c| store::answer(c, account_id, &uid, answer))
                .await?;
        }
        Ok(Permitted::Done(sent))
    }

    fn sync(&self, account_id: AccountId) -> Result<Arc<AccountSync<A::Api>>, SyncError> {
        self.accounts
            .account(account_id)
            .ok_or(SyncError::UnknownAccount(account_id))
    }
}

/// The row to store for an invitation that just arrived.
fn row(invitation: &Invitation, message_id: &str) -> store::Saved {
    store::Saved {
        uid: invitation.uid.clone(),
        sequence: invitation.sequence,
        starts_at: invitation.when.as_ref().and_then(When::starts_at),
        all_day: invitation.when.as_ref().is_some_and(When::all_day),
        summary: invitation.summary.clone(),
        cancelled: invitation.cancelled(),
        answer: None,
        message_id: message_id.to_string(),
    }
}

/// What `seen` changes about the version the store holds.
fn compare(held: Option<&store::Saved>, seen: &store::Saved) -> Option<Change> {
    let held = held?;
    if seen.cancelled && !held.cancelled {
        return Some(Change::Cancelled);
    }
    if seen.sequence <= held.sequence {
        return None;
    }
    match (held.starts_at, seen.starts_at) {
        (Some(was), Some(now)) if was != now => Some(Change::Moved {
            was,
            all_day: held.all_day,
        }),
        _ => Some(Change::Updated),
    }
}
