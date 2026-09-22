//! What the event card's buttons do: send an answer to the organizer,
//! offer the calendar permission that keeps the user's own calendar in
//! step, and hand the `.ics` to the desktop so GNOME Calendar files the
//! event.
//!
//! `mailrs_sync` decides how an answer goes out. The card says which way
//! it went, since an answer Google filed shows up on the user's own
//! calendar and one that left as mail does not.

use std::rc::Rc;

use gtk::{gio, glib};
use mailrs_domain::invitation::{Answer, Invitation, Scope, When};
use mailrs_domain::{AccountId, EpochMillis};
use mailrs_sync::{Told, now_millis};

use super::MainWindow;
use crate::goa;
use crate::permission::{Occasion, Permission};
use crate::settings::Change;
use crate::ui::conversation::ConversationView;
use crate::ui::invitation::{Action, Proposal};
use mailrs_domain::translate::{fill, gettext};

impl MainWindow {
    /// Offers to put this account in GNOME Online Accounts, where GNOME
    /// Calendar and the shell clock can see its meetings. The offer goes
    /// up once an account: the answer is remembered whichever way it
    /// goes, and an account GNOME already has is never asked about.
    pub(super) fn offer_gnome(self: &Rc<Self>, view: &Rc<ConversationView>, account_id: AccountId) {
        let Some(account) = self.account(account_id) else {
            return;
        };
        let asked = self.settings_with(|s| {
            s.offered_to_gnome
                .iter()
                .any(|email| email.eq_ignore_ascii_case(&account.email))
        });
        if !asked && goa::worth_offering(&account.email) {
            view.card.offer_gnome();
        }
    }

    /// Records the answer to that offer, and opens Online Accounts when
    /// the answer was yes.
    fn answer_gnome_offer(self: &Rc<Self>, view: &Rc<ConversationView>, open: bool) {
        let Some(account_id) = view.read(|open| open.account_id) else {
            return;
        };
        if let (Some(app), Some(account)) = (self.app.upgrade(), self.account(account_id)) {
            app.change_settings(Change::OfferedToGnome(account.email));
        }
        if open && let Err(err) = goa::open_online_accounts() {
            self.toast(&fill(
                &gettext("Could not open Settings: {reason}"),
                &[("reason", &err.to_string())],
            ));
        }
    }

    /// The event card's buttons, for the main window and a conversation in
    /// its own window alike.
    pub(super) fn invitation_action(self: &Rc<Self>, view: &Rc<ConversationView>, action: Action) {
        match action {
            Action::Answer(answer, scope) => self.answer_invitation(view, answer, scope),
            Action::Propose(proposal) => self.propose_time(view, proposal),
            Action::AddToCalendar => self.add_to_calendar(view),
            Action::OnlineAccounts { open } => self.answer_gnome_offer(view, open),
        }
    }

    /// Sends the answer and says where it went.
    fn answer_invitation(
        self: &Rc<Self>,
        view: &Rc<ConversationView>,
        answer: Answer,
        scope: Scope,
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
                        .answer(account_id, &invitation, &me, answer, scope, now_millis())
                        .await
                })
                .await;
            match sent {
                Ok(sent) => {
                    match sent.told {
                        Told::Nobody => {
                            view.card.set_answer(&uid, before);
                            this.toast(&gettext(
                                "This invitation names no organizer, so there is nobody \
                                 to reply to",
                            ));
                        }
                        told => {
                            view.card.set_answer(&uid, Some(answer));
                            view.card
                                .set_went(&uid, Some(went(told, organizer.as_deref())));
                            this.toast(&replied(answer, told));
                        }
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
                    view.card.set_answer(&uid, before);
                    this.toast(&fill(
                        &gettext("Could not send your reply: {reason}"),
                        &[("reason", &err.to_string())],
                    ));
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
        let scope = view.card.scope();
        let uid = invitation.uid.clone();
        let organizer = invitation
            .organizer
            .as_ref()
            .map(|who| who.display().to_string());
        let invitations = self.core.invitations();
        let (this, view) = (Rc::clone(self), Rc::clone(view));
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
                        &gettext("Propose a New Time"),
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
                    view.card.set_went(
                        &uid,
                        Some(match &organizer {
                            Some(organizer) => fill(
                                &gettext("Proposed a new time to {organizer}"),
                                &[("organizer", organizer)],
                            ),
                            None => gettext("Proposed a new time"),
                        }),
                    );
                    this.toast(&gettext("New time proposed. The organizer decides."));
                }
                Err(err) => this.toast(&fill(
                    &gettext("Could not send your proposal: {reason}"),
                    &[("reason", &err.to_string())],
                )),
            }
        });
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
            return self.toast(&fill(
                &gettext("Could not save the invitation: {reason}"),
                &[("reason", &err.to_string())],
            ));
        }
        let file = gio::File::for_path(&path);
        let this = Rc::clone(self);
        gtk::FileLauncher::new(Some(&file)).launch(
            Some(&self.window),
            gio::Cancellable::NONE,
            move |result| {
                if let Err(err) = result {
                    this.toast(&fill(
                        &gettext("No app on this desktop opens invitations: {reason}"),
                        &[("reason", &err.to_string())],
                    ));
                }
            },
        );
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
