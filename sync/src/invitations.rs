//! The invitation in one message: what it says, what it changes about an
//! event the user already has, and the answer they send back.
//!
//! Only this module knows that an organizer numbers each change to an
//! event with a sequence under one UID, that a higher sequence makes the
//! answer the user gave stale, and that the calendar takes the answer
//! while mail does not. The card in the window and its tests both go
//! through here, so neither works out any of that for itself.
//!
//! Every calendar read here goes through the local copy: the clash line,
//! the series line, Show in Calendar and the event an answer lands on.
//! An answer reaches the organizer one of two ways. The calendar is the
//! better one where it works, since one queued change tells the organizer
//! and marks the user's own calendar; it works only for an event the copy
//! holds, which leaves out an invitation from a server that never put it
//! on the calendar, one forwarded by hand, and one that arrived at an
//! address the calendar does not belong to. The other is RFC 5546's: mail
//! the organizer a `METHOD:REPLY` object. That needs nobody's permission,
//! so it is what an answer falls back to.

pub(crate) mod mail;

use std::sync::Arc;

use mailrs_domain::calendar::series::{self, Picked, RepeatScope};
use mailrs_domain::calendar::{Event, Occurrence, Series};
use mailrs_domain::invitation::{self, Answer, Invitation, Method, Scope, When};
use mailrs_domain::{AccountId, Address, EpochMillis};
use mailrs_store::calendar as calendar_store;
use mailrs_store::{Db, invitations as store, messages};

use crate::calendar_copy::CalendarCopy;
use crate::settings::Permitted;
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
    /// The other events the same file lists, in order. A booking with an
    /// outbound and a return leg holds two; a request holds one.
    pub also: Vec<Invitation>,
    pub change: Option<Change>,
    /// The answer the user gave this version of the event, if they have.
    pub answer: Option<Answer>,
}

/// Where the user's answer went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Told {
    /// The calendar took it: the answer waits in the change queue, which
    /// tells the organizer and marks the event on the user's own calendar.
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
    /// The account withholds the calendar permission. The answer went
    /// out by mail all the same; the caller offers to ask for the
    /// permission so that the user's own calendar keeps up from here on.
    pub needs_permission: bool,
    /// The provider has the calendar switched off, such as a Google Cloud
    /// project without the Calendar API, so the answer went by mail
    /// instead and no permission would change that.
    pub api_off: Option<ApiOff>,
}

/// An API the Google Cloud project has switched off, and the page that
/// turns it on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiOff {
    pub service: String,
    pub enable_url: String,
}

/// What the calendar's copy says about the event an invitation names,
/// for an answer.
enum Held {
    /// The copy holds it, at this occurrence.
    On(Occurrence),
    /// The account has no calendar, or the copy has no such event.
    Missing,
    /// The account withholds the calendar permission.
    NeedsPermission,
    /// The provider has the calendar switched off.
    ApiOff(ApiOff),
}

/// Where the event an invitation names sits in the calendar's copy on
/// this computer: the account, the calendar, the event's id, and the
/// start of the occurrence to open. Show in Calendar opens it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spot {
    pub account_id: AccountId,
    pub calendar: String,
    pub id: String,
    pub start: EpochMillis,
}

/// One occurrence the account is a guest of, has not answered, and that
/// is not cancelled, for the calendar sidebar's "Waiting for your
/// answer" list. Its `account_id`, `calendar`, `id` and
/// `start` are exactly what `CalendarView::open` takes, since clicking
/// the card opens the occurrence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Waiting {
    pub account_id: AccountId,
    pub calendar: String,
    pub id: String,
    pub start: EpochMillis,
    pub title: String,
    pub all_day: bool,
    /// The thread this invitation arrived in, when the store still holds
    /// the message. `None` hides the card's "Open mail" door and keeps
    /// the card: the event still waits for an answer, and the card can
    /// still open it in the calendar.
    pub thread_id: Option<String>,
}

/// The most rows [`Invitations::waiting_for_answer`] answers, so a
/// person with many open invitations does not pull an unbounded read
/// into memory. The sidebar's own scrolled window shows the rest.
const MOST_WAITING: usize = 20;

/// How far either side of the time it wants Show in Calendar looks. The
/// copy holds a year back from its first read, so a little over a year
/// covers everything it could hold near that time.
const LOOK_AROUND: EpochMillis = 400 * 24 * 60 * 60 * 1_000;

/// How long an event with no end of its own is taken to run for, when
/// asking what else it clashes with. An organizer who leaves `DTEND` out
/// means a meeting, not a day.
const ASSUMED_LENGTH: EpochMillis = 60 * 60 * 1_000;


pub struct Invitations<A: Accounts> {
    accounts: Arc<A>,
    db: Db,
    /// The calendar's local copy, which every read here goes through.
    copy: Arc<CalendarCopy<A>>,
}

impl<A: Accounts> Invitations<A> {
    pub fn new(accounts: Arc<A>, db: Db, copy: Arc<CalendarCopy<A>>) -> Self {
        Invitations {
            accounts,
            db,
            copy,
        }
    }

    /// What else the user has on while this event runs, by title, from
    /// the local copy, on the calendars the account owns. Nothing at all
    /// without a calendar, without the calendar permission or with the
    /// provider's calendar switched off: a clash is worth saying, not
    /// worth a permission prompt of its own. Never remembered, since a
    /// store read costs nothing and remembering it would hide a refresh
    /// that landed while the message stayed open.
    ///
    /// `Event::busy` is the provider's transparency alone, so
    /// `Event::blocks_time` also leaves out a cancelled, declined or
    /// all-day event.
    pub async fn busy(
        &self,
        account_id: AccountId,
        invitation: &Invitation,
    ) -> Result<Vec<String>, SyncError> {
        let Some(When::At { starts_at, ends_at }) = invitation.when else {
            return Ok(Vec::new());
        };
        if !self.copy_ready(account_id, crate::now_millis()).await? {
            return Ok(Vec::new());
        }
        let ends_at = ends_at.unwrap_or(starts_at + ASSUMED_LENGTH);
        let uid = invitation.uid.clone();
        Ok(self
            .db
            .read(move |c| {
                let found = calendar_store::occurrences(
                    c,
                    &[account_id],
                    starts_at,
                    ends_at,
                    calendar_store::CalendarScope::Owned,
                )?;
                Ok(found
                    .into_iter()
                    .filter(|o| o.event.blocks_time())
                    .filter(|o| !o.event.uid.eq_ignore_ascii_case(&uid))
                    .map(|o| o.event.title.clone())
                    .collect())
            })
            .await?)
    }

    /// How the series behind an invitation to one of its occurrences runs,
    /// in words: "Every Tuesday, 6 left". The invitation carries no rule of
    /// its own, so this reads the series the local copy holds under the
    /// invitation's UID. `None` leaves the card as it was: for an
    /// invitation to a whole event, for a series the copy does not hold,
    /// and when the calendar cannot be read for want of the permission or
    /// of the API.
    pub async fn series(
        &self,
        account_id: AccountId,
        invitation: &Invitation,
        now: EpochMillis,
    ) -> Result<Option<String>, SyncError> {
        if invitation.occurrence.is_none() || invitation.uid.trim().is_empty() {
            return Ok(None);
        }
        if !self.copy_ready(account_id, now).await? {
            return Ok(None);
        }
        let uid = invitation.uid.clone();
        let held = self
            .db
            .read(move |c| calendar_store::series_with_uid(c, account_id, &uid))
            .await?;
        Ok(held
            .and_then(|event| Series::of(&event, now))
            .and_then(|series| invitation.series_in_words(&series.rule, series.left)))
    }

    /// Whether the local copy can answer for the account's calendar,
    /// after waiting for its first read when it has none yet. `false` for
    /// an account with no calendar, one that withholds it, and one whose
    /// provider has the calendar switched off.
    async fn copy_ready(&self, account_id: AccountId, now: EpochMillis) -> Result<bool, SyncError> {
        let services = self
            .accounts
            .services(account_id)
            .ok_or(SyncError::UnknownAccount(account_id))?;
        if services.calendar.is_none() {
            return Ok(false);
        }
        match self.copy.ready(account_id, now).await {
            Ok(Permitted::Done(())) => Ok(true),
            Ok(Permitted::NeedsPermission)
            | Err(SyncError::Backend(BackendError::ApiDisabled { .. })) => Ok(false),
            Err(err) => Err(err),
        }
    }

    /// Where the event this invitation names sits in the calendar's copy,
    /// for the card's Show in Calendar. For an invitation to one
    /// occurrence, that occurrence, wherever the organizer moved it;
    /// otherwise the first occurrence that has not ended by the
    /// invitation's start or by `now`, whichever is later.
    ///
    /// Reads the store and nothing else. `None` for a cancellation, for an
    /// account whose copy has never been read, and for an event on no
    /// calendar the person shows.
    pub async fn on_calendar(
        &self,
        account_id: AccountId,
        invitation: &Invitation,
        now: EpochMillis,
    ) -> Result<Option<Spot>, SyncError> {
        if invitation.cancelled() || invitation.uid.trim().is_empty() {
            return Ok(None);
        }
        let services = self
            .accounts
            .services(account_id)
            .ok_or(SyncError::UnknownAccount(account_id))?;
        // Without a calendar there is nowhere for the event to sit.
        if !services.offers().calendar {
            return Ok(None);
        }
        Ok(self.found_on_copy(account_id, invitation, now, Near::Nearest).await?.map(|o| Spot {
            account_id,
            calendar: o.event.calendar.clone(),
            id: o.event.id.clone(),
            start: o.start,
        }))
    }

    /// The occurrence of the invitation's event the copy holds: the one
    /// the invitation is about, or the next to come. `None` for an
    /// invitation with no UID and for a copy never read.
    async fn found_on_copy(
        &self,
        account_id: AccountId,
        invitation: &Invitation,
        now: EpochMillis,
        near: Near,
    ) -> Result<Option<Occurrence>, SyncError> {
        if invitation.uid.trim().is_empty() {
            return Ok(None);
        }
        let occurrence = invitation.occurrence.as_ref().and_then(|o| o.at);
        let from = invitation
            .when
            .as_ref()
            .and_then(When::starts_at)
            .filter(|at| *at > now)
            .unwrap_or(now);
        let around = occurrence.unwrap_or(from);
        let uid = invitation.uid.clone();
        let found = self
            .db
            .read(move |c| {
                if !mailrs_store::calendar::synced(c, account_id)? {
                    return Ok(Vec::new());
                }
                mailrs_store::calendar::with_uid(c, account_id, &uid, around - LOOK_AROUND, around + LOOK_AROUND)
            })
            .await?;
        Ok(pick(found, occurrence, from, near))
    }

    /// The invitations still waiting for the account's answer, each at
    /// its next occurrence, nearest first, at most [`MOST_WAITING`], for
    /// the calendar sidebar's "Waiting for your answer" list. Pass only
    /// accounts that offer a calendar and have not withheld it: the copy
    /// keeps an account's rows after it withdraws the calendar
    /// permission, and a list built from them would offer events the
    /// person can no longer answer here.
    ///
    /// [`calendar_store::waiting`] filters before it caps, so a busy
    /// calendar cannot push an invitation out of the list. Reads the
    /// store and nothing else.
    pub async fn waiting_for_answer(
        &self,
        accounts: &[AccountId],
        now: EpochMillis,
    ) -> Result<Vec<Waiting>, SyncError> {
        let accounts = accounts.to_vec();
        Ok(self
            .db
            .read(move |c| {
                let found = calendar_store::waiting(c, &accounts, now, MOST_WAITING)?;
                let mut waiting = Vec::with_capacity(found.len());
                for occurrence in found {
                    let thread_id = mail_thread_of(c, occurrence.account_id, &occurrence.event.uid)?;
                    waiting.push(Waiting {
                        account_id: occurrence.account_id,
                        calendar: occurrence.event.calendar.clone(),
                        id: occurrence.event.id.clone(),
                        start: occurrence.start,
                        title: occurrence.event.title.clone(),
                        all_day: occurrence.event.all_day,
                        thread_id,
                    });
                }
                Ok(waiting)
            })
            .await?)
    }

    /// The thread of the mail an event's invitation arrived in, found by
    /// the event's uid, for "Open the invitation in Mail". `None` when no
    /// mail brought it, or the mail has left the store.
    pub async fn mail_thread(&self, account_id: AccountId, uid: &str) -> Result<Option<String>, SyncError> {
        let uid = uid.to_string();
        Ok(self.db.read(move |c| mail_thread_of(c, account_id, &uid)).await?)
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
        let mut events = invitation::read_all(ics).into_iter();
        let Some(invitation) = events.next() else {
            return Ok(None);
        };
        let also: Vec<Invitation> = events.collect();
        // An invitation with no UID is one nothing can be matched against,
        // and answering it would write over every other such event.
        if invitation.uid.trim().is_empty() {
            return Ok(Some(Opened {
                invitation,
                also,
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
        // A file that only describes an event asks nothing of the reader,
        // so a new version of it is not news.
        let change = change.filter(|_| invitation.method != Method::Publish);
        Ok(Some(Opened {
            invitation,
            also,
            change,
            answer,
        }))
    }

    /// Sends the user's answer to the organizer and remembers it, so
    /// reopening the message shows it. An event the calendar's copy holds
    /// is answered the way the calendar view answers it, through the
    /// queue ([`Self::answer_event`]), which tells the organizer and marks
    /// the user's own calendar, so the card and the calendar cannot
    /// disagree about how an answer goes out; the caller sends the queue.
    /// An event the copy lacks is answered by mail to the organizer. The
    /// answer says which of the two happened.
    #[expect(clippy::too_many_arguments, reason = "each is part of what the organizer is told")]
    pub async fn answer(
        &self,
        account_id: AccountId,
        invitation: &Invitation,
        me: &Address,
        answer: Answer,
        scope: Scope,
        note: Option<String>,
        now: EpochMillis,
    ) -> Result<Sent, SyncError> {
        let note = note.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
        let sync = self.sync(account_id)?;
        let mut sent = Sent {
            told: Told::Nobody,
            needs_permission: false,
            api_off: None,
        };
        // An occurrence whose instant this app could not work out cannot
        // be matched to one in the copy; only the emailed reply, which
        // copies the organizer's own `RECURRENCE-ID` back, can name it.
        let reach = match (scope, &invitation.occurrence) {
            (Scope::Occurrence, Some(occurrence)) => occurrence.at.map(|_| RepeatScope::This),
            _ => Some(RepeatScope::All),
        };
        if let Some(reach) = reach {
            // An answer to one occurrence the copy has not read yet, such
            // as one the organizer just added, would otherwise answer
            // another day's meeting; the mailed reply names the right one.
            let near = match reach {
                RepeatScope::This => Near::Exact,
                _ => Near::Nearest,
            };
            match self.held_on_calendar(account_id, invitation, now, near).await? {
                Held::On(found) => {
                    self.queue_answer(account_id, &found, reach, answer, note, me.email.clone())
                        .await?;
                    sent.told = Told::Calendar;
                    return Ok(sent);
                }
                // The answer still has to reach the organizer, so it goes
                // by mail and the caller offers to ask for the permission,
                // which keeps the user's own calendar in step from here on.
                Held::NeedsPermission => sent.needs_permission = true,
                Held::ApiOff(off) => sent.api_off = Some(off),
                Held::Missing => {}
            }
        }
        sent.told = self
            .mail_reply(&sync, invitation, me, answer, scope, note.as_deref(), now)
            .await?;
        // A mailed reply leaves the copy alone: the provider's calendar
        // has not changed.
        if sent.told != Told::Nobody {
            let uid = invitation.uid.clone();
            self.db
                .write(move |c| store::answer(c, account_id, &uid, answer))
                .await?;
        }
        Ok(sent)
    }

    /// Answers `occurrence`'s event from the calendar view: `This`
    /// answers that occurrence alone, any other scope the whole series (a
    /// one-off event is its own series). `note` goes with the answer for
    /// the organizer to read.
    ///
    /// The answer goes into the calendar's queue, as every other calendar
    /// write does, so it survives the network going and a restart; the
    /// copy and the invitation card (if the message ever opened one) show
    /// it at once. The caller sends the queue. An account with no calendar
    /// answers `Unsupported`, and one whose calendar permission is
    /// withheld `NeedsPermission`, so the queue never takes an answer it
    /// cannot send.
    pub async fn answer_event(
        &self,
        account_id: AccountId,
        occurrence: &Occurrence,
        answer: Answer,
        scope: RepeatScope,
        note: Option<String>,
    ) -> Result<Permitted<()>, SyncError> {
        let services = self
            .accounts
            .services(account_id)
            .ok_or(SyncError::UnknownAccount(account_id))?;
        if services.calendar.is_none() {
            return Err(SyncError::Backend(BackendError::Unsupported));
        }
        if services.withheld().calendar {
            return Ok(Permitted::NeedsPermission);
        }
        let me = occurrence
            .event
            .guests
            .iter()
            .find(|guest| guest.me)
            .map(|guest| guest.email.clone())
            .unwrap_or_default();
        self.queue_answer(account_id, occurrence, scope, answer, note, me).await?;
        Ok(Permitted::Done(()))
    }

    /// Writes the answer to the copy, marked waiting, and queues it. The
    /// one place an answer on the calendar is made, for the calendar view
    /// and the invitation card alike.
    async fn queue_answer(
        &self,
        account_id: AccountId,
        occurrence: &Occurrence,
        scope: RepeatScope,
        answer: Answer,
        note: Option<String>,
        me: String,
    ) -> Result<(), SyncError> {
        let event = Event::clone(&occurrence.event);
        let picked = Picked {
            original_start: event.original_start.unwrap_or(occurrence.start),
            start: occurrence.start,
        };
        let note = note.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
        let now = crate::now_millis();
        self.db
            .write(move |c| {
                let row = answered_row(c, account_id, &event, picked, scope, answer)?;
                let row = Event { pending: true, ..row };
                calendar_store::save_events(c, account_id, std::slice::from_ref(&row), now)?;
                // The series answers for every occurrence, the changed
                // ones stored apart from it included.
                if row.series.is_none() {
                    calendar_store::set_my_answer(c, account_id, &row.calendar, &row.id, answer)?;
                }
                let queued = calendar_store::QueuedAnswer { me, answer, note, title: row.title.clone() };
                calendar_store::enqueue_answer(c, account_id, &row.calendar, &row.id, &queued)?;
                store::answer(c, account_id, &row.uid, answer)
            })
            .await?;
        Ok(())
    }

    /// The event the invitation names, as the copy holds it: the
    /// occurrence the invitation is about, or the next one to come. The
    /// copy is read first when it never has been, so an answer in the
    /// first minute after an account is added still reaches the calendar.
    async fn held_on_calendar(
        &self,
        account_id: AccountId,
        invitation: &Invitation,
        now: EpochMillis,
        near: Near,
    ) -> Result<Held, SyncError> {
        let services = self
            .accounts
            .services(account_id)
            .ok_or(SyncError::UnknownAccount(account_id))?;
        if !services.offers().calendar {
            return Ok(Held::Missing);
        }
        if services.withheld().calendar {
            return Ok(Held::NeedsPermission);
        }
        match self.copy.ready(account_id, now).await {
            Ok(Permitted::Done(())) => {}
            Ok(Permitted::NeedsPermission) => return Ok(Held::NeedsPermission),
            Err(SyncError::Backend(BackendError::ApiDisabled { service, enable_url })) => {
                return Ok(Held::ApiOff(ApiOff { service, enable_url }));
            }
            Err(err) => return Err(err),
        }
        Ok(match self.found_on_copy(account_id, invitation, now, near).await? {
            Some(found) => Held::On(found),
            None => Held::Missing,
        })
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
            .answer(account_id, &invitation, &me, answer, scope, None, now)
            .await?;
        // The card's caller pushes the queue itself; this caller has no
        // window, so the answer goes out from here.
        if sent.told == Told::Calendar {
            self.copy.send_soon(account_id);
        }
        Ok(Some((invitation, sent)))
    }

    /// The invitation an event on the calendar stands for, the account's
    /// own guest entry, and whether an answer or proposal names that
    /// occurrence or the whole event: what the card holds for a message,
    /// built for an event the person opened in the calendar, so
    /// [`Self::propose`] serves both. The sequence is the newer of the
    /// last invitation mail read for the event and the calendar's copy,
    /// since an organizer may set aside a proposal for an older version.
    pub async fn for_event(
        &self,
        account_id: AccountId,
        occurrence: &Occurrence,
    ) -> Result<(Invitation, Address, Scope), SyncError> {
        let uid = occurrence.event.uid.clone();
        let sequence = self
            .db
            .read(move |c| Ok(store::saved(c, account_id, &uid)?.map_or(0, |saved| saved.sequence)))
            .await?
            .max(occurrence.event.sequence);
        let invitation = invitation::from_occurrence(occurrence, sequence);
        let me = occurrence
            .event
            .guests
            .iter()
            .find(|guest| guest.me)
            .map(|guest| Address { name: guest.name.clone(), email: guest.email.clone() })
            .unwrap_or(Address { name: None, email: String::new() });
        let scope = match invitation.occurrence {
            Some(_) => Scope::Occurrence,
            None => Scope::Series,
        };
        Ok((invitation, me, scope))
    }

    /// Proposes another time for the event and mails the organizer the
    /// proposal. iTIP calls this a counter proposal: it asks rather than
    /// decides, so nothing changes on anybody's calendar until the
    /// organizer answers, and the calendar has no part in it.
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
    #[expect(clippy::too_many_arguments, reason = "each is part of what the organizer is told")]
    async fn mail_reply(
        &self,
        sync: &AccountSync,
        invitation: &Invitation,
        me: &Address,
        answer: Answer,
        scope: Scope,
        note: Option<&str>,
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
            &invitation::reply_with_note(invitation, me, answer, scope, note, now),
            now,
        )
        .map_err(SyncError::Mime)?;
        sync.send(raw, None, None).await?;
        Ok(Told::Organizer)
    }

    fn sync(&self, account_id: AccountId) -> Result<Arc<AccountSync>, SyncError> {
        self.accounts
            .account(account_id)
            .ok_or(SyncError::UnknownAccount(account_id))
    }
}

/// Whether an invitation to one occurrence may settle for the nearest one
/// when the copy holds none at its instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Near {
    /// Show in Calendar: the nearest day is still the right place to look.
    Nearest,
    /// An answer: only the occurrence the invitation names will do.
    Exact,
}

/// The occurrence Show in Calendar opens or an answer goes to. For an
/// invitation to one occurrence, the one that replaced it or starts at it,
/// else the nearest when `near` allows it; otherwise the first that has
/// not ended by `from`, or the last one when every occurrence has.
fn pick(found: Vec<Occurrence>, occurrence: Option<EpochMillis>, from: EpochMillis, near: Near) -> Option<Occurrence> {
    if let Some(at) = occurrence {
        let exact = found.iter().position(|o| {
            o.event.original_start == Some(at) || (o.event.original_start.is_none() && o.start == at)
        });
        return match exact {
            Some(index) => found.into_iter().nth(index),
            None if near == Near::Exact => None,
            None => found.into_iter().min_by_key(|o| (o.start - at).abs()),
        };
    }
    let (ended, ahead): (Vec<_>, Vec<_>) = found.into_iter().partition(|o| o.end <= from);
    ahead
        .into_iter()
        .min_by_key(|o| o.start)
        .or_else(|| ended.into_iter().max_by_key(|o| o.start))
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

/// The row an answer writes: the series (or a one-off event) as the copy
/// holds it, or for `This` the picked occurrence as a changed occurrence
/// of its own. An occurrence whose series the copy lacks takes the answer
/// on the row the person opened.
fn answered_row(
    c: &rusqlite::Connection,
    account_id: AccountId,
    event: &Event,
    picked: Picked,
    scope: RepeatScope,
    answer: Answer,
) -> mailrs_store::Result<Event> {
    if !series::in_series(event) {
        return Ok(series::answered(event, &[], picked, RepeatScope::All, answer));
    }
    let series_id = event.series.clone().unwrap_or_else(|| event.id.clone());
    let Some(whole) = calendar_store::event(c, account_id, &event.calendar, &series_id)? else {
        return Ok(series::answered(event, &[], picked, RepeatScope::All, answer));
    };
    let changed = calendar_store::changed_occurrences(c, account_id, &event.calendar, &series_id)?;
    Ok(series::answered(&whole, &changed, picked, scope, answer))
}

/// The thread the mail that carried `uid`'s invitation sits in. A failed
/// thread lookup counts as no mail: the caller offers no way to it then.
fn mail_thread_of(
    c: &rusqlite::Connection,
    account_id: AccountId,
    uid: &str,
) -> mailrs_store::Result<Option<String>> {
    let Some(saved) = store::saved(c, account_id, uid)? else {
        return Ok(None);
    };
    Ok(messages::thread_id_of(c, account_id, &saved.message_id).ok().flatten())
}
