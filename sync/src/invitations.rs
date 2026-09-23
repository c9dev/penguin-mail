//! The invitation in one message: what it says, what it changes about an
//! event the user already has, and the answer they send back.
//!
//! Only this module knows that an organizer numbers each change to an
//! event with a sequence under one UID, that a higher sequence makes the
//! answer the user gave stale, and that Google Calendar takes the answer
//! while Gmail does not. The card in the window and its tests both go
//! through here, so neither works out any of that for itself.
//!
//! An answer reaches the organizer one of two ways. Google Calendar is
//! the better one where it works, since a single call tells the organizer
//! and marks the user's own calendar; it works only for an event Google
//! already holds, which leaves out an invitation from Exchange, one
//! forwarded by hand, and one that arrived at an address the calendar does
//! not belong to. The other is RFC 5546's: mail the organizer a
//! `METHOD:REPLY` object. That needs nobody's permission, so it is what an
//! answer falls back to.

mod mail;

use std::sync::{Arc, Mutex};

use mailrs_domain::invitation::{self, Answer, Invitation, Method, Scope, When};
use mailrs_domain::{AccountId, Address, EpochMillis};
use mailrs_gmail::{Answered, GmailError, limiter};
use mailrs_store::{Db, invitations as store};

use crate::{AccountSync, Accounts, BackendError, SyncError};

/// What a message does to an event the user already has. `None` alongside
/// it means the message is the first word on this event, or an older
/// version of one the user has already seen change.
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

impl Change {
    /// The word the store keeps for this change.
    fn word(self) -> &'static str {
        match self {
            Change::Moved { .. } => "moved",
            Change::Updated => "updated",
            Change::Cancelled => "cancelled",
        }
    }
}

/// One invitation as the window shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opened {
    pub invitation: Invitation,
    pub change: Option<Change>,
    /// The answer the user gave this version of the event, if they have.
    pub answer: Option<Answer>,
}

/// Where the user's answer went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Told {
    /// Google Calendar recorded it, which tells the organizer and marks
    /// the event on the user's own calendar.
    Calendar,
    /// Mailed to the organizer as an iTIP reply.
    Organizer,
    /// Nowhere: the event is on no calendar of the user's and the
    /// invitation names no organizer to write to.
    Nobody,
}

/// What answering an invitation did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sent {
    pub told: Told,
    /// Google turned the calendar call down for want of the permission.
    /// The answer went out all the same; the caller offers to ask for the
    /// permission so that the user's own calendar keeps up from here on.
    pub needs_permission: bool,
    /// The Google Cloud project has the Calendar API switched off, so the
    /// answer went by mail instead and no permission would change that.
    pub api_off: Option<ApiOff>,
}

/// An API the Google Cloud project has switched off, and the page that
/// turns it on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiOff {
    pub service: String,
    pub enable_url: String,
}

/// How long an event with no end of its own is taken to run for, when
/// asking what else it clashes with. An organizer who leaves `DTEND` out
/// means a meeting, not a day.
const ASSUMED_LENGTH: EpochMillis = 60 * 60 * 1_000;

/// The last invitation this asked Google what clashes with, and what it
/// said. The window reads a message twice on the way in and again each
/// time the body lands, so without this the same question goes out three
/// times for one opening.
struct Asked {
    account_id: AccountId,
    uid: String,
    busy: Vec<String>,
}

pub struct Invitations<A: Accounts> {
    accounts: Arc<A>,
    db: Db,
    asked: Mutex<Option<Asked>>,
}

impl<A: Accounts> Invitations<A> {
    pub fn new(accounts: Arc<A>, db: Db) -> Self {
        Invitations {
            accounts,
            db,
            asked: Mutex::new(None),
        }
    }

    /// What else the user has on while this event runs, by title. One call
    /// to Google, the answer kept for as long as the message stays open,
    /// and nothing at all without the calendar permission: a clash is
    /// worth saying, not worth a permission prompt of its own.
    ///
    /// The call goes out at background priority. It answers a question
    /// nobody asked, so it waits behind whatever the user is doing.
    pub async fn busy(
        &self,
        account_id: AccountId,
        invitation: &Invitation,
    ) -> Result<Vec<String>, SyncError> {
        let Some(When::At { starts_at, ends_at }) = invitation.when else {
            return Ok(Vec::new());
        };
        if let Some(held) = self.remembered(account_id, &invitation.uid) {
            return Ok(held);
        }
        let sync = self.sync(account_id)?;
        let ends_at = ends_at.unwrap_or(starts_at + ASSUMED_LENGTH);
        let busy = match limiter::background(sync.busy_between(starts_at, ends_at)).await {
            Ok(busy) => busy,
            // Without the permission there is nothing to say, and the user
            // is answering an invitation rather than asking about their
            // calendar. The empty answer is remembered like any other.
            Err(SyncError::Backend(BackendError::NeedsPermission)) => Vec::new(),
            Err(err) => return Err(err),
        };
        let busy: Vec<String> = busy
            .into_iter()
            .filter(|held| !held.uid.eq_ignore_ascii_case(&invitation.uid))
            .map(|held| held.summary)
            .collect();
        *self.asked.lock().expect("invitations poisoned") = Some(Asked {
            account_id,
            uid: invitation.uid.clone(),
            busy: busy.clone(),
        });
        Ok(busy)
    }

    /// How the series behind an invitation to one of its occurrences runs,
    /// in words: "Every Tuesday, 6 left". The invitation carries no rule of
    /// its own, so this asks the calendar, at background priority as
    /// [`Self::busy`] does. `None` leaves the card as it was: for an
    /// invitation to a whole event, for a series the calendar does not
    /// hold, and when the calendar cannot be read for want of the
    /// permission or of the API.
    pub async fn series(
        &self,
        account_id: AccountId,
        invitation: &Invitation,
        now: EpochMillis,
    ) -> Result<Option<String>, SyncError> {
        if invitation.occurrence.is_none() || invitation.uid.trim().is_empty() {
            return Ok(None);
        }
        let sync = self.sync(account_id)?;
        let series = match limiter::background(sync.series(&invitation.uid, now)).await {
            Ok(series) => series,
            Err(SyncError::Backend(BackendError::NeedsPermission | BackendError::Gmail(GmailError::ApiDisabled { .. }))) => {
                None
            }
            Err(err) => return Err(err),
        };
        Ok(series.and_then(|series| invitation.series_in_words(&series.rule, series.left)))
    }

    /// What the last look said, when it was about this same invitation.
    fn remembered(&self, account_id: AccountId, uid: &str) -> Option<Vec<String>> {
        let asked = self.asked.lock().expect("invitations poisoned");
        let asked = asked.as_ref()?;
        (asked.account_id == account_id && asked.uid == uid).then(|| asked.busy.clone())
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
        let mut seen = row(&invitation, message_id);
        let uid = invitation.uid.clone();
        let sequence = invitation.sequence;
        let (change, answer) = self
            .db
            .write(move |c| {
                let held = store::saved(c, account_id, &uid)?;
                if let Some(change) = compare(held.as_ref(), &seen) {
                    seen.news = Some(change.word().to_string());
                    seen.moved_from = match change {
                        Change::Moved { was, .. } => Some(was),
                        _ => None,
                    };
                }
                store::remember(c, account_id, &seen, now)?;
                // The row that comes back speaks for the version it holds.
                // An older message than that one gets no news of its own:
                // what changed after it arrived is not its doing.
                let held =
                    store::saved(c, account_id, &uid)?.filter(|row| row.sequence == sequence);
                Ok((
                    held.as_ref().and_then(change_of),
                    held.and_then(|row| row.answer),
                ))
            })
            .await?;
        Ok(Some(Opened {
            invitation,
            change,
            answer,
        }))
    }

    /// Sends the user's answer to the organizer and remembers it, so
    /// reopening the message shows it. Google Calendar takes the answer
    /// where it can, since one call tells the organizer and marks the
    /// user's own calendar; where it cannot, the answer goes to the
    /// organizer as mail. The answer says which of the two happened.
    pub async fn answer(
        &self,
        account_id: AccountId,
        invitation: &Invitation,
        me: &Address,
        answer: Answer,
        scope: Scope,
        now: EpochMillis,
    ) -> Result<Sent, SyncError> {
        let sync = self.sync(account_id)?;
        let mut sent = Sent {
            told: Told::Nobody,
            needs_permission: false,
            api_off: None,
        };
        // Google needs an instant to find one occurrence of a series by.
        // An occurrence whose zone this app could not work out leaves it
        // nothing to go on, and only the emailed reply, which copies the
        // organizer's own `RECURRENCE-ID` back, can name that one.
        let google = match (scope, &invitation.occurrence) {
            (Scope::Occurrence, Some(occurrence)) => occurrence.at.map(Some),
            _ => Some(None),
        };
        if let Some(occurrence) = google {
            match sync
                .answer_invitation(&invitation.uid, &me.email, answer, occurrence)
                .await
            {
                Ok(Answered::Done) => sent.told = Told::Calendar,
                Ok(Answered::NotOnCalendar) => {}
                // The answer still has to reach the organizer, so it goes
                // by mail and the caller offers to ask for the permission,
                // which keeps the user's own calendar in step from here on.
                Err(SyncError::Backend(BackendError::NeedsPermission)) => sent.needs_permission = true,
                Err(SyncError::Backend(BackendError::Gmail(GmailError::ApiDisabled {
                    service,
                    enable_url,
                }))) => {
                    sent.api_off = Some(ApiOff {
                        service,
                        enable_url,
                    })
                }
                Err(err) => return Err(err),
            }
        }
        if sent.told == Told::Nobody {
            sent.told = self
                .mail_reply(&sync, invitation, me, answer, scope, now)
                .await?;
        }
        if sent.told != Told::Nobody {
            let uid = invitation.uid.clone();
            self.db
                .write(move |c| store::answer(c, account_id, &uid, answer))
                .await?;
        }
        Ok(sent)
    }

    /// Answers the invitation in message `message_id` as the account
    /// address `me`, for a caller that holds the message rather than an
    /// open card: the assistant. The answer goes out as [`Self::answer`]
    /// sends it. An invitation to one occurrence of a repeating event is
    /// answered for that occurrence, the smaller of the two things the
    /// answer could mean. `None` means the message holds no invitation
    /// that waits on an answer, such as a cancellation or somebody's reply.
    pub async fn answer_message(
        &self,
        account_id: AccountId,
        message_id: &str,
        me: &str,
        answer: Answer,
        now: EpochMillis,
    ) -> Result<Option<(Invitation, Sent)>, SyncError> {
        let body = self.sync(account_id)?.body(message_id).await?;
        let Some(ics) = body.calendar else {
            return Ok(None);
        };
        let Some(opened) = self.open(account_id, message_id, &ics, now).await? else {
            return Ok(None);
        };
        let invitation = opened.invitation;
        if invitation.uid.trim().is_empty()
            || invitation.method != Method::Request
            || invitation.cancelled()
        {
            return Ok(None);
        }
        let guest = invitation
            .me(&[me.to_string()])
            .map(|guest| guest.who.clone());
        let me = guest.unwrap_or_else(|| Address {
            name: None,
            email: me.to_string(),
        });
        let scope = match invitation.occurrence {
            Some(_) => Scope::Occurrence,
            None => Scope::Series,
        };
        let sent = self
            .answer(account_id, &invitation, &me, answer, scope, now)
            .await?;
        Ok(Some((invitation, sent)))
    }

    /// Proposes another time for the event and mails the organizer the
    /// proposal. iTIP calls this a counter proposal: it asks rather than
    /// decides, so nothing changes on anybody's calendar until the
    /// organizer answers, and Google Calendar has no part in it.
    pub async fn propose(
        &self,
        account_id: AccountId,
        invitation: &Invitation,
        me: &Address,
        when: &When,
        scope: Scope,
        now: EpochMillis,
    ) -> Result<Told, SyncError> {
        let sync = self.sync(account_id)?;
        let Some(organizer) = organizer_of(invitation) else {
            return Ok(Told::Nobody);
        };
        let raw = mail::itip(
            me,
            &organizer,
            &mail::counter_subject(&invitation.summary),
            &mail::counter_prose(me, &invitation.summary, when),
            "COUNTER",
            &invitation::counter(invitation, me, when, scope, now),
            now,
        )
        .map_err(SyncError::Mime)?;
        sync.send(raw, None, None).await?;
        Ok(Told::Organizer)
    }

    /// Mails the organizer the reply. An invitation that names no
    /// organizer has nobody to send it to, and says so.
    async fn mail_reply(
        &self,
        sync: &AccountSync<A::Api>,
        invitation: &Invitation,
        me: &Address,
        answer: Answer,
        scope: Scope,
        now: EpochMillis,
    ) -> Result<Told, SyncError> {
        let Some(organizer) = organizer_of(invitation) else {
            return Ok(Told::Nobody);
        };
        let raw = mail::itip(
            me,
            &organizer,
            &mail::reply_subject(answer, &invitation.summary),
            &mail::reply_prose(me, answer, &invitation.summary),
            "REPLY",
            &invitation::reply(invitation, me, answer, scope, now),
            now,
        )
        .map_err(SyncError::Mime)?;
        sync.send(raw, None, None).await?;
        Ok(Told::Organizer)
    }

    fn sync(&self, account_id: AccountId) -> Result<Arc<AccountSync<A::Api>>, SyncError> {
        self.accounts
            .account(account_id)
            .ok_or(SyncError::UnknownAccount(account_id))
    }
}

/// The organizer to write to, if the invitation names one worth writing
/// to.
fn organizer_of(invitation: &Invitation) -> Option<Address> {
    invitation
        .organizer
        .clone()
        .filter(|who| !who.email.trim().is_empty())
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
        news: None,
        moved_from: None,
    }
}

/// The change a stored row records, read back out of it.
fn change_of(row: &store::Saved) -> Option<Change> {
    match row.news.as_deref()? {
        "moved" => Some(Change::Moved {
            was: row.moved_from?,
            all_day: row.all_day,
        }),
        "updated" => Some(Change::Updated),
        "cancelled" => Some(Change::Cancelled),
        _ => None,
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
