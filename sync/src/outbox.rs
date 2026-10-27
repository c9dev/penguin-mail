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
//!
//! A server that holds mail itself (JMAP with delayed submission) takes a
//! Send Later message and sends it at its hour with the laptop shut. Its
//! row keeps the server's hold in `held`, and nothing here sends that
//! message until the server has given it back. A double send is worse
//! than a late one: whenever the server may hold or have sent a message,
//! the outbox asks it first, and when no answer settles that, the person
//! is asked to check Sent.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use mailrs_domain::translate::gettext;
use mailrs_domain::{AccountId, EpochMillis, Target};
use mailrs_gmail::GmailError;
use mailrs_store::Db;
use mailrs_store::messages;
use mailrs_store::outbox::{self, Queued};

use crate::account::message_id_header;
use crate::backoff::{MOST_TRIES, retry_delay};
use crate::{
    AccountSync, Accounts, BackendError, Claimed, Held, SavedDraft, Settled, SyncError, Unheld,
    Want, now_millis, outbox_id,
};

/// How long a claim on a waiting message holds. A send that takes longer
/// than this belongs to a run that died, and the message is free again.
const CLAIM_LASTS: EpochMillis = 10 * 60 * 1000;

/// How long the outbox waits before asking about a hold again, once its
/// hour has come and the server has not sent it yet.
const ASK_AGAIN: EpochMillis = 60_000;

/// How often a hold still hours from its hour is asked about, so a cancel
/// made in another app shows here before the hour.
const CHECK_HELD: EpochMillis = 60 * 60_000;

/// How `held` marks a hold whose answer never came back and that nobody
/// has found on the server yet: this, the time the answer was lost, and
/// the message's `Message-ID`. No server's handle starts with it.
const UNCONFIRMED: &str = "unconfirmed:";

/// The mark for a hold whose answer was lost at `since`.
fn mark(since: EpochMillis, message_id: &str) -> String {
    format!("{UNCONFIRMED}{since}:{message_id}")
}

/// What the row of a message the server may or may not have sent says.
fn check_sent() -> String {
    gettext("Penguin Mail could not confirm whether this message went out. Check Sent.")
}

/// When a row's hold lost its answer, and the `Message-ID` it waits to
/// find the hold by, when it does.
fn unconfirmed(message: &Queued) -> Option<(EpochMillis, &str)> {
    let rest = message.held.as_deref()?.strip_prefix(UNCONFIRMED)?;
    let parsed = rest
        .split_once(':')
        .and_then(|(since, id)| Some((since.parse().ok()?, id)));
    Some(parsed.unwrap_or((0, rest)))
}

/// Whether the server has taken the message and sends it at its hour. A
/// row whose hold lost its answer may still go from here, so it does not
/// count until the outbox finds the server's copy.
pub fn server_holds(message: &Queued) -> bool {
    message.held.is_some() && unconfirmed(message).is_none()
}

/// Whether the row is one whose hold another app canceled, copy and all.
/// Its stored problem is the only mark it has, so a row written in
/// another language reads as an ordinary stuck one.
pub fn canceled_elsewhere_row(message: &Queued) -> bool {
    message.held.is_none() && message.problem.as_deref() == Some(canceled_elsewhere().as_str())
}

/// Whether the row is one `unconfirmable` wrote: a hold no answer could
/// place. Only that writes a problem beside a hold. A settle that fails
/// keeps asking, and every other problem goes on a row without a hold, so
/// the pair is the marker, whatever language the problem's words are in.
fn past_knowing(message: &Queued) -> bool {
    message.held.is_some() && message.problem.is_some()
}

/// What a row says when another app canceled its hold and deleted the copy.
fn canceled_elsewhere() -> String {
    gettext("This message was canceled in another app and is no longer in Drafts.")
}

/// Why a held message opens read-only: the server could not be asked to
/// let go of it.
fn still_with_server() -> String {
    gettext(
        "The server still holds this message and sends it at its hour. Try again later, or use Cancel Send.",
    )
}

/// Why changes to a message the server sent already went nowhere.
fn already_went() -> String {
    gettext("The server had already sent this message.")
}

/// Why a held message cannot be deleted now.
fn still_holds() -> SyncError {
    SyncError::Backend(BackendError::Refused(gettext(
        "The server still holds this message and could not be asked to let it go. Try again later.",
    )))
}

/// Gives the row the server's hold and the copy waiting in Drafts.
fn adopt(message: &mut Queued, held: Held) {
    message.held = Some(held.handle);
    message.draft_id = Some(held.message_id.clone());
    message.message_id = Some(held.message_id);
    message.thread_id = Some(held.thread_id);
}

/// What handing a message to the server to hold left.
enum Handed {
    Held(Held),
    /// The server cannot hold it, or never took it: the outbox sends it.
    Kept,
    /// No answer came back, so the server may hold it. The mark goes in
    /// `held`, and a later pass looks for the server's copy.
    Unconfirmed(String),
}

/// What looking for a hold whose answer was lost found.
enum Looked {
    Found(Held),
    /// Nothing, and long enough after the lost answer to trust that.
    Nothing,
    /// Nothing yet, but a server's index may lag behind its commit.
    TooSoon,
}

/// Why a held message is being called back.
#[derive(Clone, Copy)]
enum Purpose {
    SendNow,
    /// Holding it again at this hour.
    NewHour(EpochMillis),
    /// Opening it in the composer, which edits a draft the server must
    /// not send behind the person's back.
    Edit,
}

/// What calling a held message back for Send Now, a new hour or an edit
/// left.
enum Recalled {
    /// The message is an ordinary draft now, or never reached the server,
    /// and the row says so.
    Here(Box<Queued>),
    /// Another app canceled the hold and deleted the copy. The row is gone.
    Gone,
    /// The row is settled already, and this is the answer.
    Done(Posted),
}

/// Where a reopened message closed unchanged went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reheld {
    /// The server holds it again for its hour.
    Held,
    /// The server would not take it, so it waits here for its hour.
    Here,
}

/// What opening a held message in the composer found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reopened {
    /// The row holds no hold, so the draft opens as any draft does.
    Free,
    /// The server let go of it. The row now waits here on this draft, which
    /// holds the whole message again, Bcc and all, and keeps its hour.
    Draft(Box<Queued>),
    /// The server had sent it already. It is in Sent.
    AlreadySent,
    /// Another app canceled it and deleted the copy.
    Gone,
    /// The server still holds it, or nothing could say what became of it.
    /// The composer shows it read-only with `notice`, and offers Cancel
    /// Send when that can still stop it.
    Locked { notice: String, can_cancel: bool },
}

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
    /// Messages the server held and had already sent before the cancel
    /// reached it, and ones this computer was sending as the cancel came.
    /// They are in Sent, or on their way there.
    pub already_sent: usize,
    /// Messages the server holds that it could not be asked about now.
    /// They stay in Send Later, held.
    pub still_held: usize,
    /// Held messages that another app canceled and deleted, so nothing
    /// waits in Drafts.
    pub cancelled_elsewhere: usize,
    /// Held messages whose fate the server could not say. They wait in
    /// the Outbox, asking the person to check Sent.
    pub unconfirmed: usize,
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
    /// When to ask about each held row again, by row id. Only held rows
    /// past their hour, or still unconfirmed, have an entry, and a restart
    /// asks about each once more at once.
    next_look: Mutex<HashMap<i64, EpochMillis>>,
    /// When to look again at each held row still hours from its hour.
    checked: Mutex<HashMap<i64, EpochMillis>>,
    /// When to ask again about each held copy another install made, by
    /// the copy's id.
    adopt_looked: Mutex<HashMap<String, EpochMillis>>,
    /// One lock per account that has sent or been held this run. A send
    /// takes it to read; [`Outbox::hold`] takes it to write.
    sending: Mutex<HashMap<AccountId, Arc<tokio::sync::RwLock<()>>>>,
}

/// Keeps an account's mail from going out until it drops. See
/// [`Outbox::hold`].
pub struct OutboxHold {
    _lock: tokio::sync::OwnedRwLockWriteGuard<()>,
}

impl<A: Accounts> Outbox<A> {
    pub fn new(accounts: Arc<A>, db: Db) -> Self {
        Outbox {
            accounts,
            db,
            next_look: Mutex::new(HashMap::new()),
            checked: Mutex::default(),
            adopt_looked: Mutex::default(),
            sending: Mutex::default(),
        }
    }

    fn lock_of(&self, account_id: AccountId) -> Arc<tokio::sync::RwLock<()>> {
        let mut locks = self.sending.lock().unwrap_or_else(PoisonError::into_inner);
        Arc::clone(locks.entry(account_id).or_default())
    }

    /// Stops `account_id`'s mail going out until the answer drops, once a
    /// send already under way has finished. A switch to JMAP holds the
    /// account this way: a message that falls due meanwhile waits for the
    /// account the switch leaves, and no try counts against it.
    pub async fn hold(&self, account_id: AccountId) -> OutboxHold {
        OutboxHold {
            _lock: self.lock_of(account_id).write_owned().await,
        }
    }

    /// Leave to send for `account_id`, or None while it is held.
    fn sending(&self, account_id: AccountId) -> Option<tokio::sync::OwnedRwLockReadGuard<()>> {
        self.lock_of(account_id).try_read_owned().ok()
    }

    /// Sends `message` now, and keeps it for later when the reason it would
    /// not go is one that passes. The bytes travel in `message.raw`, so a
    /// message that never reaches Gmail is still whole on this computer.
    /// A message already waiting that another caller is sending right now
    /// answers `Waiting` and is left to that caller.
    pub async fn post(&self, mut message: Queued) -> Result<Posted, SyncError> {
        // A composer opened on a held copy sends under the copy's draft.
        // The hold is called back first, or the server sends it as well.
        let took_over = match self.held_row_of(&message).await? {
            Some(row) => {
                message = taken_over(message, row);
                true
            }
            None => false,
        };
        if message.id > 0 && self.claim(message.id).await?.is_none() {
            return Ok(Posted::Waiting(message.id));
        }
        // Taken before a held message is called back, since that also
        // talks to the account a switch may be moving.
        let _sending = match self.sending(message.account_id) {
            Some(sending) => sending,
            None if message.id > 0 => return self.wait_out_hold(message).await,
            // A new message waits for a switch to end rather than write a
            // row now: the swap may already have committed, and the row
            // would keep the IMAP ids it replaced.
            None => self.lock_of(message.account_id).read_owned().await,
        };
        if message.held.is_some() {
            message = match self.recall(message, Purpose::SendNow).await? {
                Recalled::Here(message) => *message,
                Recalled::Gone => return Ok(Posted::Refused(canceled_elsewhere())),
                // What went is the version the server held, not this one.
                Recalled::Done(Posted::Sent(_)) if took_over => {
                    return Ok(Posted::Refused(already_went()));
                }
                Recalled::Done(posted) => return Ok(posted),
            };
        }
        match self.attempt(&message).await {
            Ok(sent) => {
                self.forget_sent(&message).await;
                Ok(Posted::Sent(sent))
            }
            // An account that is not connected has told us nothing about
            // the message, so a message already waiting waits on as it is.
            Err(SyncError::UnknownAccount(_)) if message.id > 0 => {
                self.release(message.id).await?;
                Ok(Posted::Waiting(message.id))
            }
            Err(err) if worth_retrying(&err) => {
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

    /// A server that holds mail itself takes the message, and sends it at
    /// its hour with the laptop shut. The bytes stay here too, since the
    /// server's copy leaves out the Bcc that Send Now and a new hour need.
    /// A server that will not hold it leaves it to the rest of this.
    ///
    /// Otherwise saves `message` as a Gmail draft and records the hour to
    /// send it. With no way through to Gmail the bytes wait here instead
    /// and the message goes out from them at its hour, so Send Later works
    /// on a laptop that is offline when the writer picks the time.
    pub async fn schedule(&self, mut message: Queued) -> Result<Posted, SyncError> {
        // A composer opened on a held copy schedules under the copy's
        // draft: the hold is called back and held again at the new hour,
        // with the new words.
        if let Some(row) = self.held_row_of(&message).await? {
            let at = message.send_at;
            let Some(_) = self.claim(row.id).await? else {
                return Ok(Posted::Refused(still_with_server()));
            };
            return Ok(match self.rehold(taken_over(message, row), at).await {
                Ok(Some(kept)) => Posted::Waiting(kept.id),
                Ok(None) => Posted::Refused(already_went()),
                Err(err) => Posted::Refused(err.to_string()),
            });
        }
        // Written before the server hears of it, so a crash or a failed
        // write later leaves a row that looks for the server's copy, never
        // a plain one the app would send as well.
        if let Some(id) = message.raw.as_deref().and_then(message_id_header) {
            message.held = Some(mark(now_millis(), &id));
            let stored = message.clone();
            message.id = self.db.write(move |c| outbox::put(c, &stored)).await?;
        }
        let handed = self.hand_over(&message, message.send_at).await;
        message.held = None;
        match handed {
            Ok(Handed::Held(held)) => {
                // The draft the composer saved would stay in Drafts beside
                // the server's copy.
                self.drop_old_copy(
                    message.account_id,
                    message.draft_id.as_deref(),
                    &held.message_id,
                )
                .await;
                adopt(&mut message, held);
                return self.put_waiting(message).await;
            }
            Ok(Handed::Unconfirmed(mark)) => {
                message.held = Some(mark);
                return self.put_waiting(message).await;
            }
            Ok(Handed::Kept) => {}
            // As when a hold is taken again: the message stays in Send
            // Later and goes from this computer at its hour.
            Err(err) => {
                tracing::info!(error = %quiet_text(err), "the server would not hold the message; the app sends it")
            }
        }
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
            Err(err) if worth_retrying(&err) => {
                tracing::info!(error = %err, "scheduled here; Gmail holds no draft for it yet");
            }
            Err(err) => {
                self.drop_written(&message).await?;
                return Ok(Posted::Refused(err.to_string()));
            }
        }
        let id = self.db.write(move |c| outbox::put(c, &message)).await?;
        Ok(Posted::Waiting(id))
    }

    /// Tries every message whose time has come, soonest first. One the
    /// outbox has given up on is left alone until the person asks for it,
    /// and one another caller is sending is left to that caller.
    ///
    /// A held message is the server's to send: the pass asks where it
    /// stands, once a minute past its hour, and never sends it here. One
    /// whose hold's answer was lost is looked for once a minute. An empty
    /// look counts only a minute or more after the lost answer, and only
    /// at the hour, right before the message goes from here.
    pub async fn send_due(&self, now: EpochMillis) -> Result<Drained, SyncError> {
        let mut drained = Drained::default();
        if let Err(err) = self.adopt_holds(now, &mut drained).await {
            tracing::info!(error = %err, "could not look for holds another install made");
        }
        // A hold still hours from its hour is asked about once an hour, so
        // one another app called back leaves Send Later before its hour.
        let early = move |m: &Queued| m.send_at > now && m.problem.is_none() && server_holds(m);
        let listed = self.db.read(outbox::list).await?;
        let waiting: Vec<Queued> = listed
            .into_iter()
            .filter(|m| {
                m.send_at <= now
                    || unconfirmed(m).is_some()
                    || (early(m) && self.check_due(m.id, now))
            })
            .collect();
        for due in waiting {
            if retry_delay(due.attempts).is_none()
                || (due.held.is_some() && !early(&due) && !self.look_due(due.id, now))
            {
                continue;
            }
            if early(&due) {
                self.checked
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(due.id, now + CHECK_HELD);
            }
            // The row as it stands once claimed, which another caller may
            // have sent, dropped or moved since the list was read.
            let Some(mut message) = self.claim(due.id).await? else {
                continue;
            };
            // A switch to JMAP holds the account: nothing is sent, and no
            // held message is asked about, until it lets go.
            let Some(_sending) = self.sending(message.account_id) else {
                self.release(message.id).await?;
                continue;
            };
            if message.held.is_some() {
                match self.settle(message, now, &mut drained).await? {
                    Some(here) => message = here,
                    None => continue,
                }
            }
            if message.send_at > now
                || retry_delay(message.attempts).is_none()
                || message.held.is_some()
            {
                self.release(message.id).await?;
                continue;
            }
            match self.attempt(&message).await {
                // The account may still be connecting, or may have gone
                // for good. Either way the message has not been tried, so
                // nothing is counted against it and the next pass asks
                // again.
                Err(SyncError::UnknownAccount(account_id)) => {
                    tracing::debug!(account = account_id, "not connected yet; the outbox waits");
                    self.release(message.id).await?;
                    continue;
                }
                Ok(_) => {
                    self.forget_sent(&message).await;
                    drained.sent.push(message);
                }
                Err(err) => {
                    message.attempts = if worth_retrying(&err) {
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
    /// draft behind, as cancelling one always has. A message the server
    /// holds is called back first, so the server does not send it; when
    /// the server cannot be asked, the message stays. Only one whose row
    /// asks the person to check Sent goes when the server cannot be asked
    /// at all, since nothing would change that.
    pub async fn drop_one(&self, id: i64) -> Result<(), SyncError> {
        let Some(message) = self.find(id).await? else {
            return Ok(());
        };
        if message.held.is_some() {
            let Some(message) = self.claim(id).await? else {
                return Err(still_holds());
            };
            let unknown = past_knowing(&message);
            match self.let_go(&message).await {
                Ok(()) => {}
                Err(err) if unknown && !worth_retrying(&err) => {}
                Err(err) => {
                    tracing::info!(error = %err, "could not call a held message back to delete it");
                    self.release(id).await?;
                    return Err(still_holds());
                }
            }
        }
        self.looked(id);
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

    /// Records that the writer saved the draft `old` again. A Send Later
    /// message waiting on that draft keeps its hour and now names the
    /// draft's new id, message and thread, so the list, a later cancel and
    /// the send at its hour still find it. A server that saves a draft as a
    /// new message (JMAP, IMAP) gives it a new id. The draft holds the
    /// person's latest words from now on, so the row lets go of any bytes
    /// it kept. A draft nothing waits on changes nothing here.
    pub async fn draft_saved(
        &self,
        account_id: AccountId,
        old: Option<String>,
        saved: SavedDraft,
    ) -> Result<(), SyncError> {
        let old = old.unwrap_or_else(|| saved.draft_id.clone());
        self.db
            .write(move |c| {
                outbox::set_message(
                    c,
                    account_id,
                    &old,
                    &saved.draft_id,
                    &saved.message_id,
                    &saved.thread_id,
                )
            })
            .await?;
        Ok(())
    }

    /// Hands the Send Later message `id` back to the server for its hour,
    /// for a composer that [`Outbox::reopen_held`] opened and the person
    /// closed without a change: the server sends it with the laptop shut,
    /// as before. The draft the reopen left goes. `Here` when the server
    /// would not take it: the message stays in Send Later and goes from
    /// this computer at its hour.
    pub async fn hold_unchanged(&self, id: i64) -> Result<Reheld, SyncError> {
        let Some(mut message) = self.claim(id).await? else {
            return Ok(Reheld::Here);
        };
        if message.held.is_some() || message.problem.is_some() || message.raw.is_none() {
            self.release(id).await?;
            return Ok(match server_holds(&message) {
                true => Reheld::Held,
                false => Reheld::Here,
            });
        }
        let Some(_sending) = self.sending(message.account_id) else {
            self.release(id).await?;
            return Ok(Reheld::Here);
        };
        // Marked first, as `schedule` does, so a crash mid-way leaves a row
        // that looks for the hold before anything goes from here.
        let unmarked = message.clone();
        message.held = message
            .raw
            .as_deref()
            .and_then(message_id_header)
            .map(|id| mark(now_millis(), &id));
        let stored = message.clone();
        self.db.write(move |c| outbox::put(c, &stored)).await?;
        let handed = self.hand_over(&message, message.send_at).await;
        let reheld = match handed {
            Ok(Handed::Held(held)) => {
                self.drop_old_copy(
                    message.account_id,
                    message.draft_id.as_deref(),
                    &held.message_id,
                )
                .await;
                adopt(&mut message, held);
                Reheld::Held
            }
            Ok(Handed::Unconfirmed(mark)) => {
                message.held = Some(mark);
                Reheld::Held
            }
            Ok(Handed::Kept) => {
                message = unmarked;
                Reheld::Here
            }
            Err(err) => {
                tracing::info!(error = %quiet_text(err), "the server would not hold a reopened message again; the app sends it");
                message = unmarked;
                Reheld::Here
            }
        };
        self.db.write(move |c| outbox::put(c, &message)).await?;
        Ok(reheld)
    }

    /// Opens the Send Later message `id` for the composer. A held one is
    /// called back first and goes back in Drafts whole, so whatever the
    /// person does next (send, a new hour, an edit) sends it once. See
    /// [`Reopened`].
    pub async fn reopen_held(&self, id: i64) -> Result<Reopened, SyncError> {
        let Some(found) = self.find(id).await? else {
            return Ok(Reopened::Free);
        };
        if found.held.is_none() {
            return Ok(Reopened::Free);
        }
        let locked = |notice: String, can_cancel| Reopened::Locked { notice, can_cancel };
        if past_knowing(&found) {
            return Ok(locked(check_sent(), false));
        }
        let Some(message) = self.claim(id).await? else {
            return Ok(locked(still_with_server(), true));
        };
        let Some(_sending) = self.sending(message.account_id) else {
            self.release(id).await?;
            return Ok(locked(still_with_server(), true));
        };
        Ok(match self.recall(message, Purpose::Edit).await? {
            Recalled::Here(message) => {
                Reopened::Draft(Box::new(self.back_as_draft(*message).await?))
            }
            Recalled::Gone => Reopened::Gone,
            Recalled::Done(Posted::Sent(_)) => Reopened::AlreadySent,
            Recalled::Done(Posted::Waiting(_)) => locked(still_with_server(), true),
            Recalled::Done(Posted::Refused(reason)) => {
                let row = self.find(id).await?;
                let can_cancel = row
                    .as_ref()
                    .is_some_and(|row| row.held.is_some() && !past_knowing(row));
                locked(
                    if can_cancel {
                        still_with_server()
                    } else {
                        reason
                    },
                    can_cancel,
                )
            }
        })
    }

    /// Stops the Send Later messages the targets name and says what became
    /// of them. Each Gmail draft stays in Drafts. A message scheduled while
    /// Gmail was out of reach has no draft, so stopping it deletes it. A
    /// list row names a scheduled message by its Gmail thread, by its
    /// draft's message, or, for a message Gmail has never seen, by its own
    /// place in the table, so a target matching any of the three stops it.
    pub async fn cancel_scheduled(&self, targets: &[Target]) -> Result<Cancelled, SyncError> {
        let named = self.named(targets).await?;
        let mut cancelled = Cancelled::default();
        // Held messages are called back here, and those the server never
        // took come back for the rest of the loop.
        for item in named.into_iter().filter(|item| item.problem.is_none()) {
            let item = match item.held {
                Some(_) => match self.cancel_held(item, &mut cancelled).await? {
                    Some(here) => here,
                    None => continue,
                },
                // Claimed like a send, so a pass sending it at its hour
                // cannot send it while it turns into a draft. One already
                // claimed is going out, and is left to that pass.
                None => match self.claim(item.id).await? {
                    Some(claimed) => claimed,
                    None => {
                        cancelled.already_sent += 1;
                        continue;
                    }
                },
            };
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

    /// How many messages of `account_id` the server holds for Send Later,
    /// which removing the account would leave the server to send.
    pub async fn held_count(&self, account_id: AccountId) -> Result<usize, SyncError> {
        Ok(self
            .db
            .read(move |c| {
                Ok(outbox::list(c)?
                    .iter()
                    .filter(|m| m.account_id == account_id && m.held.is_some())
                    .count())
            })
            .await?)
    }

    /// Calls back every message the server holds for `account_id`, before
    /// the account goes: each returns to the server's Drafts, Bcc and all,
    /// and nothing sends it. Answers how many could not be called back,
    /// which the server may still send at their hour. A message the server
    /// never took goes to Drafts too, since its bytes here go with the
    /// account; one that cannot is counted with the rest.
    pub async fn call_back_account(&self, account_id: AccountId) -> Result<usize, SyncError> {
        let held: Vec<Queued> = self
            .db
            .read(move |c| {
                Ok(outbox::list(c)?
                    .into_iter()
                    .filter(|m| m.account_id == account_id && m.held.is_some())
                    .collect())
            })
            .await?;
        let mut cancelled = Cancelled::default();
        let mut failed = 0;
        for item in held {
            if past_knowing(&item) {
                failed += 1;
                continue;
            }
            let Some(here) = self.cancel_held(item, &mut cancelled).await? else {
                continue;
            };
            if let Err(err) = self.save_draft(&here).await {
                tracing::info!(error = %err, "could not put a message the server never held in Drafts");
                failed += 1;
            }
        }
        Ok(failed + cancelled.still_held + cancelled.unconfirmed + cancelled.unsaved.len())
    }

    /// The waiting messages, in Send Later and the Outbox alike, that the
    /// list rows `targets` stand for, soonest first. A row names one the
    /// way [`Outbox::cancel_scheduled`] describes.
    pub async fn named(&self, targets: &[Target]) -> Result<Vec<Queued>, SyncError> {
        let targets = targets.to_vec();
        Ok(self
            .db
            .read(move |c| {
                Ok(outbox::list(c)?
                    .into_iter()
                    .filter(|item| targets.iter().any(|t| names(t, item)))
                    .collect())
            })
            .await?)
    }

    /// Moves a Send Later message to the hour `at`. Gmail holds only the
    /// draft, so the new hour lives here alone. A message the server holds
    /// is called back and held again at the new hour. `None` when the
    /// message has gone (the server may have sent it), or has hit a
    /// problem and waits in the Outbox, where the hour is the next try and
    /// not the writer's to choose.
    pub async fn reschedule(&self, id: i64, at: EpochMillis) -> Result<Option<Queued>, SyncError> {
        if let Some(message) = self
            .find(id)
            .await?
            .filter(|m| m.problem.is_none() && m.held.is_some())
        {
            return self.hold_again(message, at).await;
        }
        Ok(self
            .db
            .write(move |c| {
                let Some(mut message) = outbox::find(c, id)?.filter(|m| m.problem.is_none()) else {
                    return Ok(None);
                };
                message.send_at = at;
                outbox::put(c, &message)?;
                Ok(Some(message))
            })
            .await?)
    }

    /// Takes the waiting message `id` for this caller, or `None` when it
    /// has gone or another caller is sending it.
    async fn claim(&self, id: i64) -> Result<Option<Queued>, SyncError> {
        let now = now_millis();
        Ok(self
            .db
            .write(move |c| outbox::claim(c, id, now, now - CLAIM_LASTS))
            .await?)
    }

    async fn release(&self, id: i64) -> Result<(), SyncError> {
        self.db.write(move |c| outbox::release(c, id)).await?;
        Ok(())
    }

    /// Leaves a waiting message posted while its account is held as it
    /// is, with no try counted, so the next pass after the hold sends it.
    async fn wait_out_hold(&self, message: Queued) -> Result<Posted, SyncError> {
        self.release(message.id).await?;
        Ok(Posted::Waiting(message.id))
    }

    /// One try at Gmail: from the bytes this computer holds, or from the
    /// Gmail draft when they are Gmail's. Sending bytes that came out of a
    /// draft deletes that draft, so Drafts is not left holding a copy of
    /// what just went out.
    ///
    /// A message tried before may have reached Gmail on a try whose answer
    /// was lost, so its bytes are looked for in Sent first. A draft is
    /// checked by the provider: Gmail deletes a draft it sends, so sending
    /// one that has gone answers that it is gone, which counts as sent;
    /// IMAP looks in Sent for the draft's Message-ID, and Graph sees that
    /// the message is no longer a draft.
    async fn attempt(&self, message: &Queued) -> Result<String, SyncError> {
        let sync = self
            .accounts
            .account(message.account_id)
            .ok_or(SyncError::UnknownAccount(message.account_id))?;
        // An account running without mail, as a Gmail account does while
        // Online Accounts is out, has heard nothing of the message: it
        // waits as an account still connecting does, with no try counted,
        // and goes once the account starts again with its mail.
        if !sync.services().serves_mail() {
            return Err(SyncError::UnknownAccount(message.account_id));
        }
        // The try counts before anything goes, so one the app never hears
        // back from, because it stopped, makes the next look in Sent. The
        // caller counts it again on a failure, which writes the same number.
        if message.id > 0 {
            let id = message.id;
            self.db.write(move |c| outbox::count_try(c, id)).await?;
        }
        match (&message.raw, &message.draft_id) {
            (Some(raw), draft_id) => {
                if message.attempts > 0
                    && let Some(sent) = sync.sent_copy(raw).await?
                {
                    tracing::info!(
                        account = message.account_id,
                        "a message tried before is already in Sent; not sending it again"
                    );
                    // A server that keeps one copy of the same bytes sent
                    // the draft itself: the copy found is the draft, and
                    // deleting it would delete the sent message.
                    if let Some(draft_id) = draft_id
                        && *draft_id != sent
                        && let Err(err) = sync.delete_draft(draft_id).await
                    {
                        tracing::warn!(error = %err, "sent, but could not delete the draft");
                    }
                    return Ok(sent);
                }
                sync.send(raw.clone(), message.thread_id.clone(), draft_id.clone())
                    .await
            }
            (None, Some(draft_id)) => Ok(sync
                .send_draft(draft_id)
                .await?
                .unwrap_or_else(|| draft_id.clone())),
            (None, None) => Err(BackendError::NotFound.into()),
        }
    }

    async fn save_draft(&self, message: &Queued) -> Result<SavedDraft, SyncError> {
        // A held account is as good as not connected: Send Later keeps the
        // bytes here, and a cancel leaves the message to the person.
        let _sending = self
            .sending(message.account_id)
            .ok_or(SyncError::UnknownAccount(message.account_id))?;
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
        self.looked(message.id);
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

    /// Forgets a message that went out. A failure here must not read as a
    /// failed send, or the message would go again: the row stays claimed,
    /// counts its try, and the pass after the claim lapses finds it in Sent.
    async fn forget_sent(&self, message: &Queued) {
        if let Err(err) = self.forget(message).await {
            tracing::warn!(account = message.account_id, error = %err, "sent, but could not take the message out of the outbox");
        }
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

    /// Stores a message that waits for its hour.
    async fn put_waiting(&self, message: Queued) -> Result<Posted, SyncError> {
        Ok(Posted::Waiting(
            self.db.write(move |c| outbox::put(c, &message)).await?,
        ))
    }

    /// Removes the row `schedule` wrote before a refusal.
    async fn drop_written(&self, message: &Queued) -> Result<(), SyncError> {
        let id = message.id;
        if id > 0 {
            self.db.write(move |c| outbox::remove(c, id)).await?;
        }
        Ok(())
    }

    /// Whether it is time for the hourly look at the held row `id`, before
    /// its hour.
    fn check_due(&self, id: i64, now: EpochMillis) -> bool {
        let checked = self.checked.lock().unwrap_or_else(PoisonError::into_inner);
        checked.get(&id).is_none_or(|at| *at <= now)
    }

    /// Whether it is time to ask the server about the held row `id`.
    fn look_due(&self, id: i64, now: EpochMillis) -> bool {
        let looks = self
            .next_look
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        looks.get(&id).is_none_or(|at| *at <= now)
    }

    fn look_at(&self, id: i64, at: EpochMillis) {
        self.next_look
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id, at);
    }

    fn looked(&self, id: i64) {
        self.next_look
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&id);
        self.checked
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&id);
    }

    /// Hands `message` to its server to send at `at`, when the server holds
    /// mail and the bytes are here. When no answer comes back the server
    /// may hold it anyway, so the row is marked for a later pass to look
    /// for the server's copy. A message without a `Message-ID` could not
    /// be looked for, so it stays here.
    async fn hand_over(&self, message: &Queued, at: EpochMillis) -> Result<Handed, SyncError> {
        let (Some(raw), Some(sync)) = (&message.raw, self.accounts.account(message.account_id))
        else {
            return Ok(Handed::Kept);
        };
        let Some(id) = message_id_header(raw) else {
            return Ok(Handed::Kept);
        };
        match sync.hold(raw, at).await {
            Ok(Some(held)) => Ok(Handed::Held(held)),
            Ok(None) => Ok(Handed::Kept),
            Err(err) if worth_retrying(&err) => {
                tracing::info!(error = %err, "no answer to a hold; the outbox looks for it later");
                Ok(Handed::Unconfirmed(mark(now_millis(), &id)))
            }
            Err(err) => Err(err),
        }
    }

    /// Looks for the hold of a row whose hold's answer was lost.
    async fn look(
        &self,
        sync: &AccountSync,
        message: &Queued,
        now: EpochMillis,
    ) -> Result<Looked, SyncError> {
        let Some((since, id)) = unconfirmed(message) else {
            return Ok(Looked::Nothing);
        };
        Ok(match sync.find_hold(id).await? {
            Some(held) => Looked::Found(held),
            None if now >= since.saturating_add(ASK_AGAIN) => Looked::Nothing,
            None => Looked::TooSoon,
        })
    }

    /// Gives the row the hold a look found. A copy the row waited on
    /// before goes from Drafts.
    async fn take_found(&self, sync: &AccountSync, message: &mut Queued, held: Held) {
        if let Some(old) = message
            .draft_id
            .clone()
            .filter(|old| *old != held.message_id)
            && let Err(err) = sync.delete_draft(&old).await
        {
            tracing::warn!(error = %err, "held, but the old copy stayed in Drafts");
        }
        adopt(message, held);
        self.looked(message.id);
    }

    /// Records that nothing can say whether the server sent `message`. It
    /// keeps its hold and waits in the Outbox, and nothing here sends it.
    async fn unconfirmable(&self, mut message: Queued) -> Result<Queued, SyncError> {
        self.looked(message.id);
        message.problem = Some(check_sent());
        message.attempts = MOST_TRIES;
        let stored = message.clone();
        message.id = self.db.write(move |c| outbox::put(c, &stored)).await?;
        Ok(message)
    }

    /// Where a held message stands once its hour came: still waiting, sent
    /// and filed, called back elsewhere, or past knowing. A row whose
    /// hold's answer was lost is looked for, and comes back to be sent
    /// from here when it is due and the server has nothing.
    /// `message` is claimed.
    async fn settle(
        &self,
        mut message: Queued,
        now: EpochMillis,
        drained: &mut Drained,
    ) -> Result<Option<Queued>, SyncError> {
        let Some(sync) = self.accounts.account(message.account_id) else {
            self.release(message.id).await?;
            return Ok(None);
        };
        if unconfirmed(&message).is_some() {
            match self.look(&sync, &message, now).await {
                Ok(Looked::Found(held)) => {
                    self.take_found(&sync, &mut message, held).await;
                    // The next pass settles the hold.
                    self.db.write(move |c| outbox::put(c, &message)).await?;
                    drained.changed = true;
                }
                // Looked for right before it goes, so a send from here
                // never goes beside the server's.
                Ok(Looked::Nothing) if message.send_at <= now => {
                    self.looked(message.id);
                    message.held = None;
                    message.attempts = message.attempts.max(1);
                    return Ok(Some(message));
                }
                // An empty look that counts settles nothing until the hour,
                // so the next one waits until the hour is near.
                Ok(Looked::Nothing) => {
                    self.look_at(
                        message.id,
                        (now + ASK_AGAIN).max(message.send_at - ASK_AGAIN),
                    );
                    self.release(message.id).await?;
                }
                Ok(Looked::TooSoon) => {
                    self.look_at(message.id, now + ASK_AGAIN);
                    self.release(message.id).await?;
                }
                Err(err) if worth_retrying(&err) => {
                    self.look_at(message.id, now + ASK_AGAIN);
                    self.release(message.id).await?;
                }
                Err(_) => {
                    drained.stuck.push(self.unconfirmable(message).await?);
                    drained.changed = true;
                }
            }
            return Ok(None);
        }
        let handle = message.held.clone().unwrap_or_default();
        let copy = message.message_id.clone().unwrap_or_default();
        match sync.settle_hold(&handle, &copy, message.send_at).await {
            Ok(Settled::Waiting(until)) => {
                self.look_at(message.id, until.max(now + ASK_AGAIN));
                self.release(message.id).await?;
            }
            Ok(Settled::Sent) => {
                self.forget(&message).await?;
                drained.sent.push(message);
                drained.changed = true;
            }
            // Another install's Send Now calls the hold back and sends the
            // message itself, so a canceled hold may still have gone.
            Ok(Settled::Canceled) if self.went_out(&sync, &message).await => {
                self.forget(&message).await?;
                drained.sent.push(message);
                drained.changed = true;
            }
            Ok(Settled::Canceled) if self.copy_left(&sync, &message).await => {
                self.forget(&message).await?;
                drained.changed = true;
            }
            // The bytes stay, so the person can still send it from the
            // Outbox or delete it.
            Ok(Settled::Canceled) => {
                self.looked(message.id);
                message.held = None;
                message.draft_id = None;
                message.problem = Some(canceled_elsewhere());
                message.attempts = MOST_TRIES;
                let stored = message.clone();
                self.db.write(move |c| outbox::put(c, &stored)).await?;
                drained.stuck.push(message);
                drained.changed = true;
            }
            Ok(Settled::Unknown) => {
                drained.stuck.push(self.unconfirmable(message).await?);
                drained.changed = true;
            }
            // The server still holds it whatever went wrong here, so the row
            // stays held in Send Later, with Send Now and Cancel, and the
            // outbox asks again in a minute, as for a late hold.
            Err(err) => {
                if !worth_retrying(&err) {
                    tracing::info!(error = %err, "the server would not say where a hold stands; asking again later");
                }
                self.look_at(message.id, now + ASK_AGAIN);
                self.release(message.id).await?;
            }
        }
        Ok(None)
    }

    /// Calls the server's hold on `message` back, for Send Now or a new
    /// hour. `Here` gives the row as an ordinary draft, written and claimed
    /// again; for a new hour it is written with that hour and marked, so a
    /// failure from there on leaves a row that looks for the next hold.
    /// `Done` gives the answer when the server sent it, cannot say, or
    /// cannot be asked. `message` is claimed.
    async fn recall(&self, mut message: Queued, purpose: Purpose) -> Result<Recalled, SyncError> {
        let id = message.id;
        let Some(sync) = self.accounts.account(message.account_id) else {
            self.release(id).await?;
            return Ok(Recalled::Done(Posted::Waiting(id)));
        };
        if unconfirmed(&message).is_some() {
            match (self.look(&sync, &message, now_millis()).await, purpose) {
                (Ok(Looked::Found(held)), _) => self.take_found(&sync, &mut message, held).await,
                (Ok(Looked::Nothing), Purpose::NewHour(_) | Purpose::Edit) => {
                    message.held = None;
                    return self.store_claimed(message, purpose).await;
                }
                (Ok(Looked::TooSoon), Purpose::NewHour(_) | Purpose::Edit) => {
                    self.release(id).await?;
                    return Ok(Recalled::Done(Posted::Waiting(id)));
                }
                // The server may hold it where a look cannot see yet, so
                // Send Now sends nothing and asks the person.
                (Ok(Looked::Nothing | Looked::TooSoon), Purpose::SendNow) => {
                    self.unconfirmable(message).await?;
                    return Ok(Recalled::Done(Posted::Refused(check_sent())));
                }
                (Err(err), _) if worth_retrying(&err) => {
                    self.release(id).await?;
                    return Ok(Recalled::Done(Posted::Waiting(id)));
                }
                (Err(_), _) => {
                    self.unconfirmable(message).await?;
                    return Ok(Recalled::Done(Posted::Refused(check_sent())));
                }
            }
        }
        let handle = message.held.clone().unwrap_or_default();
        let copy = message.message_id.clone().unwrap_or_default();
        match sync.release_hold(&handle, &copy, message.send_at).await {
            Ok(Unheld::InDrafts) => {
                message.held = None;
                self.looked(id);
                if self.went_out(&sync, &message).await {
                    self.forget(&message).await?;
                    return Ok(Recalled::Done(Posted::Sent(copy)));
                }
                if !self.copy_left(&sync, &message).await {
                    self.forget(&message).await?;
                    return Ok(Recalled::Gone);
                }
                self.store_claimed(message, purpose).await
            }
            Ok(Unheld::AlreadySent) => {
                self.forget(&message).await?;
                Ok(Recalled::Done(Posted::Sent(copy)))
            }
            Ok(Unheld::Unknown) => {
                self.unconfirmable(message).await?;
                Ok(Recalled::Done(Posted::Refused(check_sent())))
            }
            Err(err) if worth_retrying(&err) => {
                self.release(id).await?;
                Ok(Recalled::Done(Posted::Waiting(id)))
            }
            Err(err) => {
                self.release(id).await?;
                Ok(Recalled::Done(Posted::Refused(err.to_string())))
            }
        }
    }

    /// Writes a row the server let go of, and claims it again for the
    /// caller. It counts one try, so a crash after a send from here looks
    /// in Sent before it sends again. For a new hour the row takes that
    /// hour and a mark, since the next step hands it to the server.
    async fn store_claimed(
        &self,
        mut message: Queued,
        purpose: Purpose,
    ) -> Result<Recalled, SyncError> {
        message.attempts = message.attempts.max(1);
        if let Purpose::NewHour(at) = purpose {
            message.send_at = at;
            message.held = message
                .raw
                .as_deref()
                .and_then(message_id_header)
                .map(|id| mark(now_millis(), &id));
        }
        let stored = message.clone();
        let id = self.db.write(move |c| outbox::put(c, &stored)).await?;
        Ok(match self.claim(id).await? {
            Some(_) => Recalled::Here(Box::new(Queued { id, ..message })),
            None => Recalled::Done(Posted::Waiting(id)),
        })
    }

    /// Whether a hold someone called back went out all the same: another
    /// install's Send Now cancels the hold and sends the message itself,
    /// so its `Message-ID` is in Sent. A look that fails says no.
    async fn went_out(&self, sync: &AccountSync, message: &Queued) -> bool {
        let Some(raw) = &message.raw else {
            return false;
        };
        match sync.sent_copy(raw).await {
            Ok(found) => found.is_some(),
            Err(err) => {
                tracing::info!(error = %quiet_text(err), "could not look in Sent for a called-back hold");
                false
            }
        }
    }

    /// Whether the copy a called-back hold left is still on the server.
    /// Another app that canceled the hold may have deleted it. A server
    /// that cannot say leaves the copy assumed there.
    async fn copy_left(&self, sync: &AccountSync, message: &Queued) -> bool {
        let Some(copy) = message.draft_id.clone() else {
            return true;
        };
        match sync.mail().fetch(vec![Want::message(copy)]).await {
            Ok(found) => !found.metas.is_empty() || found.gone.is_empty(),
            Err(err) => {
                tracing::info!(error = %err, "could not look for a called-back copy");
                true
            }
        }
    }

    /// Calls the hold on `message` back so the server does not send it,
    /// for Delete. Any answer will do; an error says the server could not
    /// be asked, or a lost hold could not be looked for yet.
    async fn let_go(&self, message: &Queued) -> Result<(), SyncError> {
        let sync = self
            .accounts
            .account(message.account_id)
            .ok_or(SyncError::UnknownAccount(message.account_id))?;
        let mut message = message.clone();
        if unconfirmed(&message).is_some() {
            match self.look(&sync, &message, now_millis()).await? {
                Looked::Found(held) => adopt(&mut message, held),
                Looked::Nothing => return Ok(()),
                Looked::TooSoon => return Err(BackendError::Offline(String::new()).into()),
            }
        }
        let handle = message.held.clone().unwrap_or_default();
        let copy = message.message_id.clone().unwrap_or_default();
        sync.release_hold(&handle, &copy, message.send_at)
            .await
            .map(|_| ())
    }

    /// Calls the hold on a Send Later message back for Cancel Send, and
    /// counts what became of it. The copy left in Drafts gets the row's
    /// bytes back, Bcc and all. `Some` gives the row back when the server
    /// never took the message, for the cancel to treat as its own.
    async fn cancel_held(
        &self,
        item: Queued,
        cancelled: &mut Cancelled,
    ) -> Result<Option<Queued>, SyncError> {
        let Some(mut item) = self.claim(item.id).await? else {
            cancelled.still_held += 1;
            return Ok(None);
        };
        let id = item.id;
        let Some(sync) = self.accounts.account(item.account_id) else {
            self.release(id).await?;
            cancelled.still_held += 1;
            return Ok(None);
        };
        if unconfirmed(&item).is_some() {
            match self.look(&sync, &item, now_millis()).await {
                Ok(Looked::Found(held)) => self.take_found(&sync, &mut item, held).await,
                Ok(Looked::Nothing) => {
                    self.looked(id);
                    item.held = None;
                    let stored = item.clone();
                    self.db.write(move |c| outbox::put(c, &stored)).await?;
                    if item.draft_id.is_none() {
                        return Ok(Some(item));
                    }
                    // A new hour's lost hold leaves the old copy in Drafts,
                    // which the server stored without the Bcc.
                    return self.back_in_drafts(&sync, item, cancelled).await;
                }
                Ok(Looked::TooSoon) => {
                    self.release(id).await?;
                    cancelled.still_held += 1;
                    return Ok(None);
                }
                Err(err) if worth_retrying(&err) => {
                    self.release(id).await?;
                    cancelled.still_held += 1;
                    return Ok(None);
                }
                Err(_) => {
                    self.unconfirmable(item).await?;
                    cancelled.unconfirmed += 1;
                    return Ok(None);
                }
            }
        }
        let handle = item.held.clone().unwrap_or_default();
        let copy = item.message_id.clone().unwrap_or_default();
        match sync.release_hold(&handle, &copy, item.send_at).await {
            Ok(Unheld::InDrafts) if self.copy_left(&sync, &item).await => {
                self.looked(id);
                item.held = None;
                return self.back_in_drafts(&sync, item, cancelled).await;
            }
            Ok(Unheld::InDrafts) if self.went_out(&sync, &item).await => {
                cancelled.already_sent += 1
            }
            Ok(Unheld::InDrafts) => cancelled.cancelled_elsewhere += 1,
            Ok(Unheld::AlreadySent) => cancelled.already_sent += 1,
            Ok(Unheld::Unknown) => {
                self.unconfirmable(item).await?;
                cancelled.unconfirmed += 1;
                return Ok(None);
            }
            Err(err) => {
                tracing::info!(error = %err, "could not call a held message back");
                self.release(id).await?;
                cancelled.still_held += 1;
                return Ok(None);
            }
        }
        self.forget(&item).await?;
        Ok(None)
    }

    /// Puts the whole message back over the copy a cancelled hold left in
    /// Drafts, and drops the row. When that fails the copy lacks the Bcc,
    /// so the row stays, without a hold, for the app to reopen the whole
    /// message in a composer.
    async fn back_in_drafts(
        &self,
        sync: &AccountSync,
        item: Queued,
        cancelled: &mut Cancelled,
    ) -> Result<Option<Queued>, SyncError> {
        if let Err(err) = self.restore_draft(sync, &item).await {
            tracing::info!(error = %err, "could not put the whole message back in Drafts");
            let stored = item.clone();
            self.db.write(move |c| outbox::put(c, &stored)).await?;
            cancelled.unsaved.push(item);
            return Ok(None);
        }
        self.forget(&item).await?;
        cancelled.in_drafts += 1;
        Ok(None)
    }

    /// Writes the row's bytes over the copy a called-back hold left in
    /// Drafts, which the server stored without the Bcc.
    async fn restore_draft(&self, sync: &AccountSync, item: &Queued) -> Result<(), SyncError> {
        let (Some(raw), Some(copy)) = (&item.raw, &item.draft_id) else {
            return Ok(());
        };
        sync.put_back_draft(raw.clone(), item.thread_id.clone(), Some(copy.clone()))
            .await?;
        Ok(())
    }

    /// Gives a row to every copy the server holds for Send Later that no
    /// row here stands for: another install of the app held it. The row
    /// shows in Send Later as the server's, with Cancel Send, and the pass
    /// settles and files it like any other. A copy is asked about at most
    /// once a minute. One the server sent or someone called back is filed
    /// as such, and one whose fate nothing can tell stays where it is.
    async fn adopt_holds(&self, now: EpochMillis, drained: &mut Drained) -> Result<(), SyncError> {
        let copies = self
            .db
            .read(|c| outbox::unclaimed_holds(c, mailrs_jmap::HELD))
            .await?;
        for (account_id, copy) in copies {
            if !self.adopt_due(&copy, now) {
                continue;
            }
            let (Some(sync), Some(_sending)) =
                (self.accounts.account(account_id), self.sending(account_id))
            else {
                continue;
            };
            self.adopt_looked
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(copy.clone(), now + ASK_AGAIN);
            match sync.hold_of_copy(&copy).await {
                Ok(Some(Claimed::Held { held, send_at, raw })) => {
                    let wanted = vec![copy.clone()];
                    let meta = self
                        .db
                        .read(move |c| messages::by_ids(c, account_id, &wanted))
                        .await?
                        .into_iter()
                        .next();
                    let mut row = Queued {
                        account_id,
                        send_at,
                        raw: Some(raw),
                        ..Queued::default()
                    };
                    if let Some(meta) = meta {
                        row.subject = meta.subject;
                        row.recipients = meta
                            .to
                            .iter()
                            .chain(&meta.cc)
                            .map(|a| a.display().to_string())
                            .collect::<Vec<_>>()
                            .join(", ");
                    }
                    adopt(&mut row, held);
                    self.db.write(move |c| outbox::put(c, &row)).await?;
                    drained.changed = true;
                }
                Ok(Some(Claimed::Unknown)) => tracing::info!(
                    account = account_id,
                    "a held copy no submission sends; left as it is"
                ),
                Ok(Some(Claimed::Settled) | None) => {}
                Err(err) => {
                    tracing::info!(account = account_id, error = %err, "could not ask about a hold another install made")
                }
            }
        }
        Ok(())
    }

    /// Whether to ask about the held copy `copy` now.
    fn adopt_due(&self, copy: &str, now: EpochMillis) -> bool {
        let looked = self
            .adopt_looked
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        looked.get(copy).is_none_or(|at| *at <= now)
    }

    /// The held row whose draft `message` names, when `message` has no row
    /// of its own: what a composer opened on the server's copy hands over.
    async fn held_row_of(&self, message: &Queued) -> Result<Option<Queued>, SyncError> {
        let (Some(draft), true) = (message.draft_id.clone(), message.id <= 0) else {
            return Ok(None);
        };
        let account_id = message.account_id;
        Ok(self
            .db
            .read(move |c| outbox::find_draft(c, account_id, &draft))
            .await?
            .filter(|row| row.held.is_some()))
    }

    /// Deletes the draft `old` once the server holds its words as `copy`.
    async fn drop_old_copy(&self, account_id: AccountId, old: Option<&str>, copy: &str) {
        let (Some(old), Some(sync)) = (
            old.filter(|old| *old != copy),
            self.accounts.account(account_id),
        ) else {
            return;
        };
        if let Err(err) = sync.delete_draft(old).await {
            tracing::warn!(error = %err, "held, but the old copy stayed in Drafts");
        }
    }

    /// Puts the whole message back over the copy a called-back hold left
    /// in Drafts, for the composer to open, and writes the row as one that
    /// waits for its hour here. When the save fails the composer opens on
    /// the row's bytes, Bcc and all.
    async fn back_as_draft(&self, mut row: Queued) -> Result<Queued, SyncError> {
        if let (Some(sync), Some(raw)) = (self.accounts.account(row.account_id), row.raw.clone()) {
            match sync
                .put_back_draft(raw, row.thread_id.clone(), row.draft_id.clone())
                .await
            {
                // The row keeps the bytes, so a composer closed unchanged
                // can hand them back to the server; saving an edit drops
                // them (`draft_saved`), and the edit goes.
                Ok(saved) => {
                    row.draft_id = Some(saved.draft_id);
                    row.message_id = Some(saved.message_id);
                    row.thread_id = Some(saved.thread_id);
                }
                Err(err) => {
                    tracing::info!(error = %err, "called back, but the whole message could not go back in Drafts")
                }
            }
        }
        let stored = row.clone();
        self.db.write(move |c| outbox::put(c, &stored)).await?;
        Ok(row)
    }

    /// Calls the hold back and holds the message again at `at`. Once the
    /// server lets go, the row is written with the new hour and a mark, so
    /// a crash or a failed write from there on leaves a row that looks for
    /// the new hold before anything goes from here.
    async fn hold_again(
        &self,
        message: Queued,
        at: EpochMillis,
    ) -> Result<Option<Queued>, SyncError> {
        let Some(message) = self.claim(message.id).await? else {
            return Ok(None);
        };
        self.rehold(message, at).await
    }

    /// [`Outbox::hold_again`] for a row already claimed. `message` may
    /// carry new words, which go to the server in place of the old.
    async fn rehold(&self, message: Queued, at: EpochMillis) -> Result<Option<Queued>, SyncError> {
        let mut message = match self.recall(message, Purpose::NewHour(at)).await? {
            Recalled::Here(message) => *message,
            Recalled::Gone => {
                return Err(SyncError::Backend(BackendError::Refused(
                    canceled_elsewhere(),
                )));
            }
            Recalled::Done(Posted::Waiting(_)) => {
                return Err(SyncError::Backend(BackendError::Offline(gettext(
                    "The server could not be asked to let go of the message.",
                ))));
            }
            Recalled::Done(Posted::Refused(reason)) => {
                return Err(SyncError::Backend(BackendError::Refused(reason)));
            }
            Recalled::Done(Posted::Sent(_)) => return Ok(None),
        };
        let old = message.draft_id.clone();
        let handed = self.hand_over(&message, at).await;
        message.held = None;
        match handed {
            Ok(Handed::Held(held)) => {
                self.drop_old_copy(message.account_id, old.as_deref(), &held.message_id)
                    .await;
                adopt(&mut message, held);
            }
            Ok(Handed::Unconfirmed(mark)) => message.held = Some(mark),
            Ok(Handed::Kept) => {}
            Err(err) => {
                tracing::info!(error = %quiet_text(err), "the server would not hold the message again; the app sends it")
            }
        }
        let kept = message.clone();
        self.db.write(move |c| outbox::put(c, &message)).await?;
        Ok(Some(kept))
    }
}

/// `err` for the log, without the words a server put in it, which can
/// name an address.
fn quiet_text(err: SyncError) -> String {
    match err {
        SyncError::Backend(err) => crate::account::quiet(err).to_string(),
        other => other.to_string(),
    }
}

/// `message`, from a composer opened on the server's copy, in the place of
/// the held `row`: the row's id, hold and copy, with the new words.
fn taken_over(message: Queued, row: Queued) -> Queued {
    Queued {
        id: row.id,
        held: row.held,
        draft_id: row.draft_id,
        message_id: row.message_id,
        thread_id: row.thread_id,
        send_at: row.send_at,
        attempts: row.attempts,
        ..message
    }
}

/// Whether the list row `target` stands for the waiting message `item`.
fn names(target: &Target, item: &Queued) -> bool {
    target.account_id == item.account_id
        && (outbox_id(&target.thread_id) == Some(item.id)
            || item.thread_id.as_deref() == Some(target.thread_id.as_str())
            || (item.message_id.is_some() && target.message_id == item.message_id))
}

/// Whether a message that would not go out is worth trying again. No
/// network, a 5xx and a rate limit are; a refused recipient, a message
/// over Gmail's size limit and a revoked token are not, because the
/// same bytes fail the same way however long the outbox waits, and the
/// person is the only one who can fix any of them.
fn worth_retrying(err: &SyncError) -> bool {
    match err {
        // Gmail answers a request that took too long with 408 and
        // nothing else; every other 4xx is about the message.
        SyncError::Backend(BackendError::Gmail(GmailError::Http { status: 408, .. })) => true,
        SyncError::Backend(err) => err.is_transient(),
        // This computer's own trouble, not the message's, and it
        // usually clears on its own.
        SyncError::Store(_) => true,
        // The account is still connecting.
        SyncError::UnknownAccount(_) => true,
        SyncError::NoLabel(_) | SyncError::ReservedLabel(_) => false,
        // A mail action never names a calendar, so the outbox never
        // meets this one; the same id fails the same way regardless.
        SyncError::NoCalendar(_) | SyncError::MadeInGoogle(_) => false,
        // The bytes could not be written at all, so the same draft
        // would fail the same way on every try.
        SyncError::Mime(_) => false,
        SyncError::NotAnAddress(_) => false,
        // An export's file; the outbox exports nothing.
        SyncError::Write(_) => false,
        // The outbox sends nothing to a list's server.
        SyncError::OneClick(_) => false,
        // The keyring's own trouble, not the message's; it usually clears
        // on its own, as a locked keyring does once the person unlocks it.
        SyncError::Password(_) | SyncError::NoAnswer(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn http(status: u16) -> SyncError {
        SyncError::from(GmailError::Http {
            status,
            body: String::new(),
        })
    }

    #[test]
    fn a_message_waits_out_no_network_a_rate_limit_and_gmail_falling_over() {
        for err in [
            SyncError::from(GmailError::Network("connection refused".into())),
            SyncError::from(GmailError::RateLimited {
                retry_after: Some(Duration::from_secs(3)),
            }),
            http(500),
            http(503),
            http(408),
            SyncError::UnknownAccount(1),
        ] {
            assert!(worth_retrying(&err), "{err} is worth another try");
        }
    }

    #[test]
    fn a_refused_recipient_an_oversized_message_and_a_revoked_token_go_to_the_person() {
        for err in [
            http(400),
            http(413),
            http(403),
            SyncError::from(GmailError::NeedsReauth),
            SyncError::from(GmailError::MissingScope),
            SyncError::from(GmailError::NotFound),
        ] {
            assert!(!worth_retrying(&err), "{err} needs the person");
        }
    }
}
