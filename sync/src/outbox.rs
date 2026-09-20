//! The outbox: every message waiting to go out, and the pass that sends
//! whatever is due. A message that cannot reach Gmail now waits here with
//! the bytes built for it, so closing the laptop mid-send loses nothing.
//! Send Later waits here too, with the hour it chose and, once Gmail has
//! taken it, a draft holding its bytes. The window and the assistant send
//! through this module, so only it decides what is worth another try.
//!
//! Nothing waits on a timer of its own. The caller comes round every so
//! often and calls [`Outbox::send_due`]; a message that failed carries the
//! time of its next try in `send_at`, and the wait doubles from half a
//! minute to half an hour. After about a day of tries the outbox stops and
//! leaves the message to the person, and so does one failure nothing would
//! fix. The network coming back skips the rest of a wait, through
//! [`Outbox::try_now`].

use std::sync::Arc;

use mailrs_domain::EpochMillis;
use mailrs_gmail::GmailError;
use mailrs_store::Db;
use mailrs_store::outbox::{self, Queued};

use crate::backoff::{MOST_TRIES, retry_delay};
use crate::{Accounts, SavedDraft, SyncError, now_millis};

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
