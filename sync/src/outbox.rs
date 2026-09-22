//! The outbox: every message waiting to go out, and the pass that sends
//! whatever is due. A message that cannot reach Gmail now waits here with
//! the bytes built for it, so closing the laptop mid-send loses nothing.
//! Send Later waits here too, with the hour it chose and, once Gmail has
//! taken it, a draft holding its bytes. The window and the assistant send
//! through this module, so the rule about what waits here and what goes
//! back to the person is written once.
//!
//! Nothing waits on a timer of its own. The caller comes round every so
//! often and calls [`Outbox::send_due`]; a message that failed carries the
//! time of its next try in `send_at`, and the wait doubles from half a
//! minute to half an hour. After about a day of tries the outbox stops and
//! leaves the message to the person, and so does one failure nothing would
//! fix. The network coming back skips the rest of a wait, through
//! [`Outbox::try_now`].

use std::sync::Arc;

use mailrs_domain::{AccountId, EpochMillis, Target};
use mailrs_gmail::GmailError;
use mailrs_store::Db;
use mailrs_store::outbox::{self, Queued};

use crate::backoff::{MOST_TRIES, retry_delay};
use crate::{Accounts, SavedDraft, SyncError, now_millis, outbox_id};

/// What became of a message handed to the outbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Posted {
    /// It went out. Gmail's id for the sent message comes with it.
    Sent(String),
    /// It is waiting in the outbox, under this row id, and the outbox will
    /// try again.
    Waiting(i64),
    /// Nothing another try would fix. The person has to see this.
    Refused(String),
}

/// What one pass over the outbox did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Drained {
    /// The messages that went out.
    pub sent: Vec<Queued>,
    /// The messages the outbox has stopped trying, which now wait on the
    /// person.
    pub stuck: Vec<Queued>,
    /// Whether the table changed, so the lists reading it are stale.
    pub changed: bool,
}

/// What cancelling Send Later did with the messages it stopped.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Cancelled {
    /// The messages that are in Gmail's Drafts now: the ones Gmail already
    /// held as drafts, and the ones scheduled while Gmail was out of reach,
    /// which cancelling saved there.
    pub in_drafts: usize,
    /// Messages Gmail never had that it could not take as drafts either.
    /// They stay in the table, since their bytes are the only copy, and
    /// the app reopens each in a composer before it drops the row.
    pub unsaved: Vec<Queued>,
}

/// Sends the messages waiting to go out. See the module docs.
pub struct Outbox<A: Accounts> {
    accounts: Arc<A>,
    db: Db,
}

impl<A: Accounts> Outbox<A> {
    pub fn new(accounts: Arc<A>, db: Db) -> Self {
        Outbox { accounts, db }
    }

    /// Sends `message` now, and keeps it for later when the reason it would
    /// not go is one that passes. The bytes travel in `message.raw`, so a
    /// message that never reaches Gmail is still whole on this computer.
    pub async fn post(&self, mut message: Queued) -> Result<Posted, SyncError> {
        match self.attempt(&message).await {
            Ok(sent) => {
                self.forget(&message).await?;
                Ok(Posted::Sent(sent))
            }
            // An account that is not connected has told us nothing about
            // the message, so a message already waiting waits on as it is.
            Err(SyncError::UnknownAccount(_)) if message.id > 0 => Ok(Posted::Waiting(message.id)),
            Err(err) if err.worth_retrying() => {
                message.attempts += 1;
                Ok(Posted::Waiting(self.keep(message, &err).await?.id))
            }
            Err(err) => {
                // A message already waiting keeps its place, so the Outbox
                // goes on showing it and saying what is wrong with it.
                if message.id > 0 {
                    message.attempts = MOST_TRIES;
                    self.keep(message, &err).await?;
                }
                Ok(Posted::Refused(err.to_string()))
            }
        }
    }

    /// Saves `message` as a Gmail draft and records the hour to send it.
    /// With no way through to Gmail the bytes wait here instead and the
    /// message goes out from them at its hour, so Send Later works on a
    /// laptop that is offline when the writer picks the time.
    pub async fn schedule(&self, mut message: Queued) -> Result<Posted, SyncError> {
        match self.save_draft(&message).await {
            Ok(saved) => {
                message.draft_id = Some(saved.draft_id);
                message.message_id = Some(saved.message_id);
                message.thread_id = Some(saved.thread_id);
                // Gmail holds the bytes now, so sending the draft sends
                // whatever the writer changed on another device.
                message.raw = None;
            }
            // Nothing has failed yet: the message keeps its hour and goes
            // out from these bytes, so it stays in Send Later.
            Err(err) if err.worth_retrying() => {
                tracing::info!(error = %err, "scheduled here; Gmail holds no draft for it yet");
            }
            Err(err) => return Ok(Posted::Refused(err.to_string())),
        }
        let id = self.db.write(move |c| outbox::put(c, &message)).await?;
        Ok(Posted::Waiting(id))
    }

    /// Tries every message whose time has come, soonest first. One the
    /// outbox has given up on is left alone until the person asks for it.
    pub async fn send_due(&self, now: EpochMillis) -> Result<Drained, SyncError> {
        let mut drained = Drained::default();
        for mut message in self.db.read(move |c| outbox::due(c, now)).await? {
            if retry_delay(message.attempts).is_none() {
                continue;
            }
            match self.attempt(&message).await {
                // The account may still be connecting, or may have gone
                // for good. Either way the message has not been tried, so
                // nothing is counted against it and the next pass asks
                // again.
                Err(SyncError::UnknownAccount(account_id)) => {
                    tracing::debug!(account = account_id, "not connected yet; the outbox waits");
                    continue;
                }
                Ok(_) => {
                    self.forget(&message).await?;
                    drained.sent.push(message);
                }
                Err(err) => {
                    message.attempts = if err.worth_retrying() {
                        message.attempts + 1
                    } else {
                        MOST_TRIES
                    };
                    let gave_up = retry_delay(message.attempts).is_none();
                    let kept = self.keep(message, &err).await?;
                    if gave_up {
                        drained.stuck.push(kept);
                    }
                }
            }
            drained.changed = true;
        }
        Ok(drained)
    }

    /// Sends one waiting message now, whatever its interval says.
    pub async fn send_one(&self, id: i64) -> Result<Posted, SyncError> {
        match self.db.read(move |c| outbox::find(c, id)).await? {
            Some(message) => self.post(message).await,
            None => Ok(Posted::Refused("That message is no longer waiting".into())),
        }
    }

    /// Brings every stuck message forward, for when the network comes back
    /// and sitting out the rest of a wait would serve nobody.
    pub async fn try_now(&self) -> Result<(), SyncError> {
        let now = now_millis();
        self.db.write(move |c| outbox::try_now(c, now)).await?;
        Ok(())
    }

    /// Drops a waiting message. A Send Later message leaves its Gmail
    /// draft behind, as cancelling one always has.
    pub async fn drop_one(&self, id: i64) -> Result<(), SyncError> {
        self.db.write(move |c| outbox::remove(c, id)).await?;
        Ok(())
    }

    pub async fn find(&self, id: i64) -> Result<Option<Queued>, SyncError> {
        Ok(self.db.read(move |c| outbox::find(c, id)).await?)
    }

    /// The waiting message that sends the Gmail draft `draft_id`, which is
    /// how a reopened draft finds the hour Send Later gave it.
    pub async fn find_draft(
        &self,
        account_id: AccountId,
        draft_id: &str,
    ) -> Result<Option<Queued>, SyncError> {
        let draft_id = draft_id.to_string();
        Ok(self
            .db
            .read(move |c| outbox::find_draft(c, account_id, &draft_id))
            .await?)
    }

    /// Records that the writer saved a draft again. A Send Later message
    /// waiting on that draft keeps its hour and now names the draft's new
    /// message and thread, so the list and a later cancel still find it.
    /// A draft nothing waits on changes nothing here.
    pub async fn draft_saved(
        &self,
        account_id: AccountId,
        saved: SavedDraft,
    ) -> Result<(), SyncError> {
        self.db
            .write(move |c| {
                outbox::set_message(
                    c,
                    account_id,
                    &saved.draft_id,
                    &saved.message_id,
                    &saved.thread_id,
                )
            })
            .await?;
        Ok(())
    }

    /// Stops the Send Later messages the targets name and says what became
    /// of them. Each Gmail draft stays in Drafts. A message scheduled while
    /// Gmail was out of reach has no draft, so stopping it deletes it. A
    /// list row names a scheduled message by its Gmail thread, by its
    /// draft's message, or, for a message Gmail has never seen, by its own
    /// place in the table, so a target matching any of the three stops it.
    pub async fn cancel_scheduled(&self, targets: &[Target]) -> Result<Cancelled, SyncError> {
        let targets = targets.to_vec();
        let named: Vec<Queued> = self
            .db
            .read(move |c| {
                Ok(outbox::scheduled(c)?
                    .into_iter()
                    .filter(|item| targets.iter().any(|t| names(t, item)))
                    .collect())
            })
            .await?;
        let mut cancelled = Cancelled::default();
        for item in named {
            // A message Gmail never had goes to Drafts now, so cancelling
            // does not throw away the only copy.
            if item.draft_id.is_none()
                && let Err(err) = self.save_draft(&item).await
            {
                tracing::info!(error = %err, "could not save a cancelled message to Drafts");
                cancelled.unsaved.push(item);
                continue;
            }
            let id = item.id;
            self.db.write(move |c| outbox::remove(c, id)).await?;
            cancelled.in_drafts += 1;
        }
        Ok(cancelled)
    }

    /// One try at Gmail: from the bytes this computer holds, or from the
    /// Gmail draft when they are Gmail's. Sending bytes that came out of a
    /// draft deletes that draft, so Drafts is not left holding a copy of
    /// what just went out.
    async fn attempt(&self, message: &Queued) -> Result<String, SyncError> {
        let sync = self
            .accounts
            .account(message.account_id)
            .ok_or(SyncError::UnknownAccount(message.account_id))?;
        match (&message.raw, &message.draft_id) {
            (Some(raw), draft_id) => {
                sync.send(raw.clone(), message.thread_id.clone(), draft_id.clone())
                    .await
            }
            (None, Some(draft_id)) => Ok(sync
                .send_draft(draft_id)
                .await?
                .unwrap_or_else(|| draft_id.clone())),
            (None, None) => Err(GmailError::NotFound.into()),
        }
    }

    async fn save_draft(&self, message: &Queued) -> Result<SavedDraft, SyncError> {
        let sync = self
            .accounts
            .account(message.account_id)
            .ok_or(SyncError::UnknownAccount(message.account_id))?;
        let raw = message.raw.clone().unwrap_or_default();
        sync.save_draft(raw, message.thread_id.clone(), message.draft_id.clone())
            .await
    }

    /// Takes a message that has gone out off the table, whether it was
    /// waiting under its own row or under the Gmail draft it occupied.
    async fn forget(&self, message: &Queued) -> Result<(), SyncError> {
        let (id, account_id, draft_id) = (message.id, message.account_id, message.draft_id.clone());
        self.db
            .write(move |c| match (id, draft_id) {
                (id, _) if id > 0 => outbox::remove(c, id),
                (_, Some(draft_id)) => outbox::remove_draft(c, account_id, &draft_id),
                _ => Ok(()),
            })
            .await?;
        Ok(())
    }

    /// Keeps a message that would not go out, with why and when to try
    /// again, and gives back the row as it now stands. `message.attempts`
    /// already counts this try.
    async fn keep(&self, mut message: Queued, err: &SyncError) -> Result<Queued, SyncError> {
        message.problem = Some(err.to_string());
        message.send_at = now_millis()
            + retry_delay(message.attempts)
                .unwrap_or_default()
                .as_millis() as EpochMillis;
        message.id = self
            .db
            .write({
                let message = message.clone();
                move |c| outbox::put(c, &message)
            })
            .await?;
        Ok(message)
    }
}

/// Whether the list row `target` stands for the waiting message `item`.
fn names(target: &Target, item: &Queued) -> bool {
    target.account_id == item.account_id
        && (outbox_id(&target.thread_id) == Some(item.id)
            || item.thread_id.as_deref() == Some(target.thread_id.as_str())
            || (item.message_id.is_some() && target.message_id == item.message_id))
}
