//! What the event card's buttons do: send an answer to the organizer,
//! offer the calendar permission that keeps the user's own calendar in
//! step, and hand the `.ics` to the desktop so GNOME Calendar files the
//! event.
//!
//! `mailrs_sync` decides how an answer goes out. The card says which way
//! it went, since an answer Google filed shows up on the user's own
//! calendar and one that left as mail does not.

use std::rc::Rc;

use gtk::prelude::WidgetExt;
use gtk::{gio, glib};
use mailrs_domain::invitation::{Answer, Invitation, Scope, When};
use mailrs_domain::calendar::Occurrence;
use mailrs_domain::{AccountId, Address, EpochMillis};
use mailrs_sync::{Told, now_millis};

use super::MainWindow;
use crate::permission::{Occasion, Permission};
use crate::ui::conversation::ConversationView;
use crate::ui::invitation::{Action, AddTo, Proposal};
use mailrs_domain::translate::{fill, gettext};

impl MainWindow {
    /// Offers Grant Access on the card when the account has a calendar
    /// and withheld the permission to read it. With the permission
    /// granted Show in Calendar covers the event, and an account with no
    /// calendar hands the `.ics` to the desktop, so neither gets a line.
    pub(super) fn offer_calendar_access(
        self: &Rc<Self>,
        view: &Rc<ConversationView>,
        account_id: AccountId,
    ) {
        if crate::permission::card_offers_calendar_access(
            self.offers(account_id),
            self.withheld(account_id),
        ) {
            view.offer_calendar_access();
        }
    }

    /// The event card's buttons, for the main window and a conversation in
    /// its own window alike.
    pub(super) fn invitation_action(self: &Rc<Self>, view: &Rc<ConversationView>, action: Action) {
        match action {
            Action::Answer(answer, scope, note) => self.answer_invitation(view, answer, scope, note),
            Action::Propose(proposal) => self.propose_time(view, proposal),
            Action::AddToCalendar => self.add_to_calendar(view),
            Action::Import(events, target) => self.import_events(view, events, target),
            Action::ShowInCalendar => self.show_in_calendar(view),
            Action::GrantAccess => self.grant_calendar_access(view),
        }
    }

    /// Sends the answer and says where it went.
    fn answer_invitation(
        self: &Rc<Self>,
        view: &Rc<ConversationView>,
        answer: Answer,
        scope: Scope,
        note: Option<String>,
    ) {
        let account_id = view.read(|open| open.account_id);
        let found =
            view.with_invitation(|showing| (showing.invitation.clone(), showing.answering_as()));
        let (Some(account_id), Some((invitation, Some(me)))) = (account_id, found) else {
            return;
        };
        if invitation.uid.trim().is_empty() {
            return self.toast(&gettext(
                "This invitation names no event, so there is nothing to answer",
            ));
        }
        let organizer = invitation
            .organizer
            .as_ref()
            .map(|who| who.display().to_string());
        let before = view.with_invitation(|showing| showing.answer).flatten();
        let uid = invitation.uid.clone();
        let invitations = self.core.invitations();
        let (this, view) = (Rc::clone(self), Rc::clone(view));
        glib::spawn_future_local(async move {
            let sent = this
                .core
                .call(async move {
                    invitations
                        .answer(account_id, &invitation, &me, answer, scope, note, now_millis())
                        .await
                })
                .await;
            match sent {
                Ok(sent) => {
                    match sent.told {
                        Told::Nobody => {
                            view.invitation_answered(&uid, before);
                            this.toast(&gettext(
                                "This invitation names no organizer, so there is nobody \
                                 to reply to",
                            ));
                        }
                        told => {
                            view.invitation_answered(&uid, Some(answer));
                            view.invitation_went(&uid, Some(went(told, organizer.as_deref())));
                            this.toast(&replied(answer, told));
                        }
                    }
                    // An answer on the calendar is already in its copy, so
                    // the event's block and "Waiting for your answer" show
                    // it now; the queue sends it on.
                    if sent.told == Told::Calendar {
                        this.calendar.reload();
                        this.calendar.push(account_id);
                    }
                    // The answer reached the organizer either way; the
                    // permission is what puts the event on the user's own
                    // calendar, so it comes as an offer.
                    if sent.needs_permission {
                        this.ask_permission(account_id, Permission::Calendar, Occasion::Offer);
                    }
                    if let Some(off) = &sent.api_off {
                        this.explain_api_off(&off.service, &off.enable_url);
                    }
                }
                Err(err) => {
                    view.invitation_answered(&uid, before);
                    this.failed(&gettext("Could not send your reply: {reason}"), &err);
                }
            }
        });
    }

    /// Asks the organizer for another time. The proposal is a question,
    /// not an answer, so it leaves the answer buttons where they were:
    /// nothing is settled until the organizer says so.
    fn propose_time(self: &Rc<Self>, view: &Rc<ConversationView>, proposal: Proposal) {
        let account_id = view.read(|open| open.account_id);
        let found =
            view.with_invitation(|showing| (showing.invitation.clone(), showing.answering_as()));
        let (Some(account_id), Some((invitation, Some(me)))) = (account_id, found) else {
            return;
        };
        let scope = view.invitation_scope();
        let uid = invitation.uid.clone();
        let view = Rc::clone(view);
        self.propose(account_id, invitation, me, scope, proposal, move |went| {
            view.invitation_went(&uid, Some(went));
        });
    }

    /// Propose New Time from the calendar's event popover: the same
    /// question and the same mail as the card's, for the invitation the
    /// event stands for.
    pub(super) fn propose_for_event(self: &Rc<Self>, account_id: AccountId, occurrence: Occurrence) {
        let invitations = self.core.invitations();
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let found = this
                .core
                .call(async move { invitations.for_event(account_id, &occurrence).await })
                .await;
            match found {
                Ok((invitation, me, scope)) => {
                    this.propose(account_id, invitation, me, scope, Proposal::Pick, |_| {})
                }
                Err(err) => this.failed(&gettext("Could not send your proposal: {reason}"), &err),
            }
        });
    }

    /// Asks for the time when `proposal` leaves it open, mails the
    /// organizer the proposal, and hands `went` the line that says where
    /// it went. The card and the calendar both come here.
    fn propose(
        self: &Rc<Self>,
        account_id: AccountId,
        invitation: Invitation,
        me: Address,
        scope: Scope,
        proposal: Proposal,
        went: impl Fn(String) + 'static,
    ) {
        let organizer = invitation
            .organizer
            .as_ref()
            .map(|who| who.display().to_string());
        let invitations = self.core.invitations();
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let starts_at = match proposal {
                Proposal::At(at) => Some(at),
                Proposal::Pick => {
                    let hears = match organizer.as_deref() {
                        Some(organizer) => organizer.to_string(),
                        None => gettext("Nobody"),
                    };
                    crate::ui::when::pick_time(
                        &this.window,
                        &gettext("Propose New Time"),
                        &fill(
                            &gettext("The organizer decides. {organizer} hears what you suggest."),
                            &[("organizer", &hears)],
                        ),
                        &gettext("Propose"),
                    )
                    .await
                }
            };
            let Some(when) = starts_at.and_then(|at| moved(&invitation, at)) else {
                return;
            };
            let sent = this
                .core
                .call(async move {
                    invitations
                        .propose(account_id, &invitation, &me, &when, scope, now_millis())
                        .await
                })
                .await;
            match sent {
                Ok(Told::Nobody) => this.toast(&gettext(
                    "This invitation names no organizer, so there is nobody to ask",
                )),
                Ok(_) => {
                    went(match &organizer {
                        Some(organizer) => fill(
                            &gettext("Proposed a new time to {organizer}"),
                            &[("organizer", organizer)],
                        ),
                        None => gettext("Proposed a new time"),
                    });
                    this.toast(&gettext("New time proposed. The organizer decides."));
                }
                Err(err) => this.failed(&gettext("Could not send your proposal: {reason}"), &err),
            }
        });
    }

    /// Sends the account through consent again for the calendar
    /// permission, the same path as the Grant Access banner. The line
    /// stays up, since the person may close the consent page unanswered.
    fn grant_calendar_access(self: &Rc<Self>, view: &Rc<ConversationView>) {
        if let Some(account_id) = view.read(|open| open.account_id) {
            self.grant_access(account_id);
        }
    }

    /// Writes the invitation to a file and opens it with the desktop's
    /// handler, which on GNOME is Calendar. The event then shows up in the
    /// shell clock like any other.
    fn add_to_calendar(self: &Rc<Self>, view: &Rc<ConversationView>) {
        let found = view.find(|open| open.invitation().map(|(_, ics)| ics.to_string()));
        let Some(ics) = found else {
            return;
        };
        let name = view
            .with_invitation(|showing| file_name(&showing.invitation.summary))
            .unwrap_or_else(|| "invitation.ics".to_string());
        let dir = glib::user_cache_dir()
            .join("penguin-mail")
            .join("invitations");
        let path = dir.join(&name);
        if let Err(err) = std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&path, &ics)) {
            return self.failed(&gettext("Could not save the invitation: {reason}"), &err);
        }
        let file = gio::File::for_path(&path);
        let this = Rc::clone(self);
        gtk::FileLauncher::new(Some(&file)).launch(
            Some(&self.window),
            gio::Cancellable::NONE,
            move |result| {
                if let Err(err) = result {
                    this.failed(
                        &gettext("No app on this desktop opens invitations: {reason}"),
                        &err,
                    );
                }
            },
        );
    }

    /// Adds the events of a calendar file to the calendar the card's
    /// picker named, and says on the card where they went.
    fn import_events(self: &Rc<Self>, view: &Rc<ConversationView>, events: Vec<Invitation>, target: AddTo) {
        let Some(uid) = view.with_invitation(|showing| showing.invitation.uid.clone()) else {
            return;
        };
        let calendar = self.core.calendar();
        let (this, view) = (Rc::clone(self), Rc::clone(view));
        glib::spawn_future_local(async move {
            let account_id = target.account_id;
            let done = this
                .core
                .call(async move { calendar.import(account_id, Some(&target.calendar), &events).await })
                .await;
            match done {
                Ok(mailrs_sync::Permitted::Done(added)) if added.spots.is_empty() => this.toast(&gettext(
                    "Nothing in this file can be added, since its events have no id or start time",
                )),
                Ok(mailrs_sync::Permitted::Done(added)) => {
                    view.events_added(&uid, &added.calendar, &added.spots);
                    if added.skipped > 0 {
                        this.toast(&gettext(
                            "Some events were left out, since they have no id or start time",
                        ));
                    }
                    this.calendar.reload();
                }
                Ok(mailrs_sync::Permitted::NeedsPermission) => {
                    this.ask_permission(account_id, Permission::Calendar, Occasion::Offer);
                }
                Err(err) => this.failed(&gettext("Could not add to your calendar: {reason}"), &err),
            }
        });
    }

    /// Switches the main window to the calendar on the day of the event
    /// the card shows, with its popover open. A conversation in a window
    /// of its own raises the main window for it.
    fn show_in_calendar(self: &Rc<Self>, view: &Rc<ConversationView>) {
        let Some(Some(spot)) = view.with_invitation(|showing| showing.on_calendar.clone()) else {
            return;
        };
        self.show_spot(&spot);
    }

    /// Reads the calendar again, after something added events to the copy
    /// behind the window's back.
    pub fn calendar_changed(&self) {
        self.calendar.reload();
    }

    /// Raises the window and opens the calendar on the event at `spot`.
    /// A calendar file opened from Files comes here too.
    pub fn show_spot(self: &Rc<Self>, spot: &mailrs_sync::Spot) {
        self.present();
        let _ = WidgetExt::activate_action(&self.window, "win.show-calendar", None);
        self.calendar
            .open(spot.account_id, &spot.calendar, &spot.id, spot.start);
    }
}

/// The event moved to `starts_at`, keeping the length the organizer gave
/// it. An event with no end of its own is proposed as an hour, which is
/// what the calendar on the other side will draw.
fn moved(invitation: &Invitation, starts_at: EpochMillis) -> Option<When> {
    let Some(When::At {
        starts_at: was,
        ends_at,
    }) = invitation.when
    else {
        return None;
    };
    let length = ends_at.map_or(60 * 60 * 1_000, |ends_at| ends_at - was);
    Some(When::At {
        starts_at,
        ends_at: Some(starts_at + length),
    })
}

/// The toast an answer leaves: the answer the user gave, and that the
/// organizer now knows it.
fn replied(answer: Answer, told: Told) -> String {
    let said = match answer {
        Answer::Yes => gettext("Replied Yes"),
        Answer::No => gettext("Replied No"),
        Answer::Maybe => gettext("Replied Maybe"),
    };
    let pattern = match told {
        Told::Calendar => gettext("{answer}. The organizer has been told."),
        _ => gettext("{answer}. Your reply is on its way to the organizer."),
    };
    fill(&pattern, &[("answer", &said)])
}

/// The line under the buttons: where the answer went. Google files the
/// answer on the user's own calendar as well, so the two roads leave the
/// user in different places and the card says which.
fn went(told: Told, organizer: Option<&str>) -> String {
    match (told, organizer) {
        (Told::Calendar, _) => gettext("Answered on your calendar"),
        (_, Some(organizer)) => fill(
            &gettext("Replied by email to {organizer}"),
            &[("organizer", organizer)],
        ),
        (_, None) => gettext("Replied by email"),
    }
}

/// A file name for the event, so the calendar app shows something better
/// than a random string while it imports.
fn file_name(summary: &str) -> String {
    let stem: String = summary
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    let stem = stem.trim_matches('-').to_lowercase();
    let stem: String = stem.chars().take(48).collect();
    if stem.is_empty() {
        "invitation.ics".into()
    } else {
        format!("{stem}.ics")
    }
}

#[cfg(test)]
mod tests {
    use super::file_name;

    #[test]
    fn an_event_becomes_a_readable_file_name() {
        assert_eq!(file_name("Q4 roadmap review"), "q4-roadmap-review.ics");
        assert_eq!(file_name("  "), "invitation.ics");
        assert_eq!(file_name("Budget review, Q1"), "budget-review--q1.ics");
        assert!(file_name(&"x".repeat(200)).len() <= 52);
    }
}
