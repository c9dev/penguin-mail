//! What the event card's buttons do: send an answer to the organizer,
//! offer the calendar permission that keeps the user's own calendar in
//! step, and hand the `.ics` to the desktop so GNOME Calendar files the
//! event.
//!
//! `mailrs_sync` decides how an answer goes out. The card says which way
//! it went, since an answer Google filed shows up on the user's own
//! calendar and one that left as mail does not.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use mailrs_domain::invitation::{Answer, Invitation, Method, Scope, When};
use mailrs_domain::{AccountId, EpochMillis};
use mailrs_gmail::CALENDAR_SCOPE;
use mailrs_sync::{Told, now_millis};

use super::MainWindow;
use crate::goa;
use crate::settings::Change;
use crate::ui::conversation::ConversationView;
use crate::ui::invitation::{Action, Proposal, Showing};
use mailrs_domain::translate::{fill, gettext};

thread_local! {
    /// The accounts this run has already offered the calendar permission.
    /// An answer reaches the organizer by mail without it, so the offer
    /// comes once and then stays out of the way.
    static ASKED_FOR_CALENDAR: RefCell<HashSet<AccountId>> = RefCell::new(HashSet::new());
}

impl MainWindow {
    /// Reads the invitation in the message `view` shows and puts it on the
    /// card, or takes the card away when the message carries none.
    pub(super) async fn refresh_invitation(self: &Rc<Self>, view: &Rc<ConversationView>) {
        let found = view
            .with_open(|open| {
                open.invitation()
                    .map(|(meta, ics)| (open.account_id, meta.id.clone(), ics.to_string()))
            })
            .flatten();
        let Some((account_id, message_id, ics)) = found else {
            view.show_invitation(None);
            return;
        };
        let me = self.addresses_for(account_id);
        let invitations = self.core.invitations();
        let opened = self
            .core
            .call(async move {
                invitations
                    .open(account_id, &message_id, &ics, now_millis())
                    .await
            })
            .await;
        let showing = match opened {
            Ok(Some(opened)) => Some(Showing {
                invitation: opened.invitation,
                change: opened.change,
                answer: opened.answer,
                me,
            }),
            Ok(None) => None,
            Err(err) => {
                tracing::info!(error = %err, "could not read the invitation");
                None
            }
        };
        view.show_invitation(showing.clone());
        if let Some(showing) = showing.filter(waiting_on_an_answer) {
            self.offer_gnome(view, account_id);
            self.show_clashes(view, account_id, showing.invitation);
        }
    }

    /// Offers to put this account in GNOME Online Accounts, where GNOME
    /// Calendar and the shell clock can see its meetings. The offer goes
    /// up once an account: the answer is remembered whichever way it
    /// goes, and an account GNOME already has is never asked about.
    fn offer_gnome(self: &Rc<Self>, view: &Rc<ConversationView>, account_id: AccountId) {
        let Some(account) = self.account(account_id) else {
            return;
        };
        let asked = self
            .settings()
            .offered_to_gnome
            .iter()
            .any(|email| email.eq_ignore_ascii_case(&account.email));
        if !asked && goa::worth_offering(&account.email) {
            view.card.offer_gnome();
        }
    }

    /// Records the answer to that offer, and opens Online Accounts when
    /// the answer was yes.
    fn answer_gnome_offer(self: &Rc<Self>, view: &Rc<ConversationView>, open: bool) {
        let Some(account_id) = view.with_open(|open| open.account_id) else {
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

    /// Asks the calendar what else the user has on while the event runs,
    /// and puts it on the card. Only for an invitation still waiting on an
    /// answer: a meeting the user has already answered is one they have
    /// thought about.
    fn show_clashes(
        self: &Rc<Self>,
        view: &Rc<ConversationView>,
        account_id: AccountId,
        invitation: Invitation,
    ) {
        let invitations = self.core.invitations();
        let (this, view) = (Rc::clone(self), Rc::clone(view));
        glib::spawn_future_local(async move {
            let uid = invitation.uid.clone();
            let busy = this
                .core
                .call(async move { invitations.busy(account_id, &invitation).await })
                .await;
            match busy {
                Ok(busy) => view.card.set_busy(&uid, &busy),
                Err(err) => tracing::info!(error = %err, "could not read the calendar"),
            }
        });
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
        let account_id = view.with_open(|open| open.account_id);
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
                            view.card.set_answer(before);
                            this.toast(&gettext(
                                "This invitation names no organizer, so there is nobody \
                                 to reply to",
                            ));
                        }
                        told => {
                            view.card.set_answer(Some(answer));
                            view.card.set_went(Some(went(told, organizer.as_deref())));
                            this.toast(&replied(answer, told));
                        }
                    }
                    if sent.needs_permission {
                        this.offer_calendar_access(account_id);
                    }
                }
                Err(err) => {
                    view.card.set_answer(before);
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
        let account_id = view.with_open(|open| open.account_id);
        let found =
            view.with_invitation(|showing| (showing.invitation.clone(), showing.answering_as()));
        let (Some(account_id), Some((invitation, Some(me)))) = (account_id, found) else {
            return;
        };
        let scope = view.card.scope();
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
                    view.card.set_went(Some(match &organizer {
                        Some(organizer) => fill(
                            &gettext("Proposed a new time to {organizer}"),
                            &[("organizer", organizer)],
                        ),
                        None => gettext("Proposed a new time"),
                    }));
                    this.toast(&gettext("New time proposed. The organizer decides."));
                }
                Err(err) => this.toast(&fill(
                    &gettext("Could not send your proposal: {reason}"),
                    &[("reason", &err.to_string())],
                )),
            }
        });
    }

    /// Explains what the calendar permission adds, and offers to ask
    /// Google for it. The answer has already reached the organizer either
    /// way; the permission is what puts the event on the user's own
    /// calendar. The offer comes once a run, so saying no ends it.
    fn offer_calendar_access(self: &Rc<Self>, account_id: AccountId) {
        let first = ASKED_FOR_CALENDAR.with(|asked| asked.borrow_mut().insert(account_id));
        if !first {
            return;
        }
        let Some(account) = self.account(account_id) else {
            return;
        };
        let dialog = adw::AlertDialog::new(
            Some(&gettext("Allow Penguin Mail to Use Your Calendar")),
            Some(&fill(
                &gettext(
                    "Your reply went to the organizer as mail. With permission to change \
                     events on the calendar for {account}, the meeting is marked on your \
                     own calendar too. Google asks you to confirm in your browser.",
                ),
                &[("account", &account.email)],
            )),
        );
        dialog.add_responses(&[
            ("cancel", &gettext("Not Now")),
            ("grant", &gettext("Grant Access")),
        ]);
        dialog.set_response_appearance("grant", adw::ResponseAppearance::Suggested);
        dialog.set_close_response("cancel");
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            if dialog.choose_future(Some(&this.window)).await == "grant" {
                this.authorize_with(Some(account.email), &[CALENDAR_SCOPE]);
            }
        });
    }

    /// Writes the invitation to a file and opens it with the desktop's
    /// handler, which on GNOME is Calendar. The event then shows up in the
    /// shell clock like any other.
    fn add_to_calendar(self: &Rc<Self>, view: &Rc<ConversationView>) {
        let found = view
            .with_open(|open| open.invitation().map(|(_, ics)| ics.to_string()))
            .flatten();
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

/// Whether the card is asking the user a question they have not answered.
/// A cancellation, somebody else's reply and a meeting already answered
/// are none of them worth reading the calendar for.
fn waiting_on_an_answer(showing: &Showing) -> bool {
    showing.answer.is_none()
        && showing.invitation.method == Method::Request
        && !showing.invitation.cancelled()
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
