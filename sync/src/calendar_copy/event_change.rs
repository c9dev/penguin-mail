//! One change to an event, made the same way by the calendar view and
//! the assistant: [`CalendarCopy::ask_before`] says what to ask the
//! person first, as data each caller words its own way, and
//! [`CalendarCopy::change`] writes it. The copy works out the writes a
//! repeat scope takes and the notify the account or a guest's own event
//! forces, so no caller builds steps or picks a notify itself.

use mailrs_domain::AccountId;
use mailrs_domain::calendar::series::{self, RepeatScope, Step};
use mailrs_domain::calendar::{Event, Guest, Notify, Occurrence};

use super::{CalendarCopy, Held};
use crate::settings::Permitted;
use crate::{Accounts, SyncError};

/// What changes.
#[derive(Debug, Clone, PartialEq)]
pub enum EventChange {
    /// A new event, written whole.
    New(Event),
    /// `occurrence` becomes `edited`: an edit in the editor, a drag or a
    /// keyboard nudge, or the assistant's change. `edited` on another
    /// calendar of the account moves the event there.
    Edit { occurrence: Occurrence, edited: Event, how: Edit },
    /// Takes `occurrence` off the calendar. For a guest of someone else's
    /// event, only the guest's own copy goes.
    Remove(Occurrence),
}

/// What an edit changes as its guests see it, which only the caller can
/// tell: the editor compares the draft it opened with the one it saves.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Edit {
    /// The edit gives the event a new time. A move always asks, so a
    /// drag that slipped can be taken back.
    pub moves: bool,
    /// The guests see the change: a title, a time, a place, the guest
    /// list and so on, rather than the account's own reminders or colour.
    pub seen: bool,
    /// A move that also changes something besides the time, so the
    /// question offers to keep the old time and write the rest.
    pub more_than_time: bool,
    /// Those other changes reach the guests on their own.
    pub rest_seen: bool,
    /// The repeat rule changed, which no single occurrence can take.
    pub rule_changed: bool,
}

/// Whether the change waits on an Undo toast.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Undo {
    /// Write it to the copy and hold it: the caller's toast closing calls
    /// [`CalendarCopy::commit`], Undo calls [`CalendarCopy::revert`].
    Offer,
    /// Queue it at once.
    Skip,
}

/// What [`CalendarCopy::change`] did.
#[derive(Debug, Clone, PartialEq)]
pub enum Changed {
    /// Written to the copy and waiting on its Undo toast.
    Held(Held),
    /// Written and queued, with the writes it took.
    Queued(Vec<Step>),
}

/// What the person chose, or what a change nobody was asked about takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Choice {
    /// The occurrences of a series it covers; `None` writes the event as
    /// it is.
    pub scope: Option<RepeatScope>,
    pub notify: Notify,
}

/// What comes before a change is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ask {
    /// Ask the person this first.
    Question(Question),
    /// Nothing to ask: write it with this.
    Settled(Choice),
}

/// What the person did to the event, as the question words it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// A drag, a keyboard nudge, or a new time in the editor.
    Move,
    /// Any other change in the editor.
    Edit,
    Delete,
    /// A guest's Yes, Maybe or No. Only the organizer hears of it, so
    /// it asks which occurrences it covers and nothing about the guests.
    Answer,
}

/// What to ask before a move, a change or a delete is written: whether
/// to go ahead, which occurrences of a repeating event it covers, and
/// whether the guests are mailed. The window puts it in one dialog; the
/// assistant puts it in its confirmation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    pub action: Action,
    /// The occurrences the change may cover, for a repeating event.
    pub scopes: Vec<RepeatScope>,
    /// Offer to send the guests an update or to send nothing.
    pub ask_guests: bool,
    /// The account mails the guests whatever the person says, so the
    /// question says so instead of offering a choice.
    pub mailed: bool,
    /// The guests hear of it whatever the person says, because the change
    /// adds guests and their invitation is that mail.
    pub told: bool,
    /// Turning the new time down still writes the rest: the save changed
    /// more than the time.
    pub keeps: bool,
    /// Whether the rest, written at the old time, reaches the guests.
    pub rest_seen: bool,
}

/// What the question rule reads about a change, beyond its action.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Facts {
    /// The guests see the change. A move and a delete reach them whatever
    /// this says.
    pub seen: bool,
    /// The change invites someone new.
    pub adds_guests: bool,
    /// A move that also changes something besides the time.
    pub more_than_time: bool,
    /// Those other changes reach the guests on their own.
    pub rest_seen: bool,
    /// The account mails the guests of every change it writes and has no
    /// way to send nobody (`Offers::quiet_changes` is false).
    pub always_mails: bool,
}

/// Whether anyone but the account itself is on the event's guest list.
pub fn has_other_guests(guests: &[Guest]) -> bool {
    guests.iter().any(|g| !g.me)
}

/// Whether `after` holds an address `before` does not.
pub fn adds_guests(before: &[Guest], after: &[Guest]) -> bool {
    after.iter().any(|a| !before.iter().any(|b| b.email.eq_ignore_ascii_case(&a.email)))
}

/// The question for `action` on an event with `guests`, offering
/// `scopes` (from `series::scopes`; empty for an event that does not
/// repeat), or `None` when nothing needs asking. Every move asks. A
/// delete, and an edit the guests see, ask when the event repeats or has
/// guests; any other edit only when it repeats. A change that adds guests
/// tells every guest without a choice, since the new ones need their
/// invitation.
pub fn question(action: Action, scopes: &[RepeatScope], guests: &[Guest], facts: Facts) -> Option<Question> {
    let seen = action != Action::Edit || facts.seen;
    let guests_hear = action != Action::Answer && has_other_guests(guests) && seen;
    let ask_guests = guests_hear && !facts.adds_guests && !facts.always_mails;
    let mailed = guests_hear && !facts.adds_guests && facts.always_mails;
    let needed = match action {
        Action::Move => true,
        Action::Delete | Action::Edit | Action::Answer => !scopes.is_empty() || ask_guests || mailed,
    };
    needed.then(|| Question {
        action,
        scopes: scopes.to_vec(),
        ask_guests,
        mailed,
        told: guests_hear && facts.adds_guests,
        keeps: action == Action::Move && facts.more_than_time,
        rest_seen: facts.rest_seen,
    })
}

/// What a change nobody was asked about sends: nothing for an edit the
/// guests do not see, an update otherwise. An account that always mails
/// cannot send nothing, so it says `Guests`.
pub fn unasked(action: Action, facts: Facts) -> Choice {
    let quiet = action == Action::Edit && !facts.seen && !facts.always_mails;
    Choice { scope: None, notify: if quiet { Notify::Nobody } else { Notify::Guests } }
}

/// What to ask before `change`, on an account that mails the guests of
/// every change when `always_mails`.
pub fn asking(change: &EventChange, always_mails: bool) -> Ask {
    let base = Facts { always_mails, ..Facts::default() };
    let (action, scopes, guests, facts) = match change {
        // A new event's guests get their invitation.
        EventChange::New(_) => return Ask::Settled(unasked(Action::Edit, Facts { seen: true, ..base })),
        EventChange::Edit { occurrence, edited, how } => {
            let before = &occurrence.event;
            let action = if how.moves { Action::Move } else { Action::Edit };
            let facts = Facts {
                seen: how.seen,
                adds_guests: adds_guests(&before.guests, &edited.guests),
                more_than_time: how.moves && how.more_than_time,
                rest_seen: how.rest_seen,
                ..base
            };
            // A guest the edit removed still hears of it.
            let guests = if has_other_guests(&before.guests) { &before.guests } else { &edited.guests };
            (action, edit_scopes(before, edited, how.rule_changed), guests.as_slice(), facts)
        }
        EventChange::Remove(occurrence) => {
            let event = &occurrence.event;
            // A guest's removal takes only their own copy, so the other
            // guests hear nothing of it.
            let guests: &[Guest] = if event.limited() { &[] } else { &event.guests };
            (Action::Delete, own_scopes(event, series::scopes(event, false)), guests, base)
        }
    };
    match question(action, &scopes, guests, facts) {
        Some(question) => Ask::Question(question),
        None => Ask::Settled(unasked(action, facts)),
    }
}

/// The occurrences an edit of `before` into `edited` may cover. Google
/// moves a series to another calendar whole, so a new calendar leaves
/// only "All events".
fn edit_scopes(before: &Event, edited: &Event, rule_changed: bool) -> Vec<RepeatScope> {
    let offered = series::scopes(before, rule_changed);
    if edited.calendar != before.calendar {
        return offered.into_iter().filter(|s| *s == RepeatScope::All).collect();
    }
    own_scopes(before, offered)
}

/// `offered`, less "This and following" on a guest's copy of someone
/// else's series: it would start a new series the guest organizes.
fn own_scopes(event: &Event, offered: Vec<RepeatScope>) -> Vec<RepeatScope> {
    match event.limited() {
        true => offered.into_iter().filter(|s| *s != RepeatScope::Following).collect(),
        false => offered,
    }
}

/// The notify a write of `event` goes out with. A guest changes and
/// removes only their own copy, which nobody else hears of, and Google
/// marks a guest who removes an invitation as declined itself. An account
/// that mails the guests of every change cannot keep one quiet.
fn forced(event: Option<&Event>, quiet_changes: bool, chosen: Notify) -> Notify {
    if event.is_some_and(Event::limited) {
        Notify::Nobody
    } else if !quiet_changes {
        Notify::Guests
    } else {
        chosen
    }
}

impl<A: Accounts> CalendarCopy<A> {
    /// What to ask the person before `change` is written on `account_id`,
    /// or the choice it takes when nothing needs asking.
    pub fn ask_before(&self, account_id: AccountId, change: &EventChange) -> Result<Ask, SyncError> {
        Ok(asking(change, !self.quiet_changes(account_id)?))
    }

    /// Writes `change` with what the person chose: the writes `choice`'s
    /// scope takes, sent with the notify the account and the event allow.
    /// With [`Undo::Offer`] the change waits on its toast, and holding it
    /// commits one held before. An account with no calendar answers
    /// `Unsupported`, and one whose calendar permission is withheld
    /// `NeedsPermission`, so the queue never takes a change it cannot send.
    pub async fn change(
        &self,
        account_id: AccountId,
        change: EventChange,
        choice: Choice,
        undo: Undo,
    ) -> Result<Permitted<Changed>, SyncError> {
        let quiet = self.quiet_changes(account_id)?;
        let (steps, notify) = match change {
            EventChange::New(event) => (vec![Step::Save(event)], forced(None, quiet, choice.notify)),
            EventChange::Edit { occurrence, edited, .. } => {
                let notify = forced(Some(&occurrence.event), quiet, choice.notify);
                (self.change_steps(account_id, &occurrence, edited, choice.scope).await?, notify)
            }
            EventChange::Remove(occurrence) => {
                let notify = forced(Some(&occurrence.event), quiet, choice.notify);
                (self.delete_steps(account_id, &occurrence, choice.scope).await?, notify)
            }
        };
        let written = match undo {
            Undo::Offer => self.hold_with(account_id, steps, notify).await?.done().map(Changed::Held),
            Undo::Skip => {
                let queued = steps.clone();
                self.apply_with(account_id, steps, notify).await?.done().map(|()| Changed::Queued(queued))
            }
        };
        Ok(written.map_or(Permitted::NeedsPermission, Permitted::Done))
    }

    fn quiet_changes(&self, account_id: AccountId) -> Result<bool, SyncError> {
        Ok(self
            .accounts
            .services(account_id)
            .ok_or(SyncError::UnknownAccount(account_id))?
            .offers()
            .quiet_changes)
    }
}
