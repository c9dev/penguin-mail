//! What the event card's buttons do: send an answer to Google Calendar,
//! ask for the calendar permission when Google wants it first, and hand
//! the `.ics` to the desktop so GNOME Calendar files the event.

use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use mailrs_domain::AccountId;
use mailrs_domain::invitation::Answer;
use mailrs_gmail::{Answered, CALENDAR_SCOPE};
use mailrs_sync::{Permitted, now_millis};

use super::MainWindow;
use crate::ui::conversation::ConversationView;
use crate::ui::invitation::{Action, Showing};

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
        view.show_invitation(showing);
    }

    /// The event card's buttons, for the main window and a conversation in
    /// its own window alike.
    pub(super) fn invitation_action(self: &Rc<Self>, view: &Rc<ConversationView>, action: Action) {
        match action {
            Action::Answer(answer) => self.answer_invitation(view, answer),
            Action::AddToCalendar => self.add_to_calendar(view),
        }
    }

    /// Sends the answer to Google Calendar and says how it went.
    fn answer_invitation(self: &Rc<Self>, view: &Rc<ConversationView>, answer: Answer) {
        let account_id = view.with_open(|open| open.account_id);
        let uid = view.with_invitation(|showing| showing.invitation.uid.clone());
        let (Some(account_id), Some(uid)) = (account_id, uid) else {
            return;
        };
        if uid.trim().is_empty() {
            return self.toast("This invitation names no event, so there is nothing to answer");
        }
        let Some(me) = self.addresses_for(account_id).into_iter().next() else {
            return;
        };
        let before = view.with_invitation(|showing| showing.answer).flatten();
        let invitations = self.core.invitations();
        let (this, view) = (Rc::clone(self), Rc::clone(view));
        glib::spawn_future_local(async move {
            let sent = this
                .core
                .call(async move { invitations.answer(account_id, &uid, &me, answer).await })
                .await;
            match sent {
                Ok(Permitted::Done(Answered::Done)) => {
                    view.card.set_answer(Some(answer));
                    this.toast(match answer {
                        Answer::Yes => "Replied Yes. The organizer has been told.",
                        Answer::No => "Replied No. The organizer has been told.",
                        Answer::Maybe => "Replied Maybe. The organizer has been told.",
                    });
                }
                Ok(Permitted::Done(Answered::NotOnCalendar)) => {
                    view.card.set_answer(before);
                    this.toast(
                        "This meeting is not on your calendar, so there was nothing to answer",
                    );
                }
                Ok(Permitted::NeedsPermission) => {
                    view.card.set_answer(before);
                    this.ask_for_calendar_access(account_id);
                }
                Err(err) => {
                    view.card.set_answer(before);
                    this.toast(&format!("Could not send your reply: {err}"));
                }
            }
        });
    }

    /// Explains that answering an invitation needs one more permission, and
    /// offers to ask Google for it. Saying no leaves Google's own Yes, No
    /// and Maybe links in the message, which go on working.
    fn ask_for_calendar_access(self: &Rc<Self>, account_id: AccountId) {
        let Some(account) = self.account(account_id) else {
            return;
        };
        let dialog = adw::AlertDialog::new(
            Some("Allow Penguin Mail to Use Your Calendar"),
            Some(&format!(
                "Replying to an invitation needs permission to change events on the calendar for {}. Google asks you to confirm in your browser. Without it, the Yes, No and Maybe links in the message still work.",
                account.email
            )),
        );
        dialog.add_responses(&[("cancel", "Not Now"), ("grant", "Grant Access")]);
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
            return self.toast(&format!("Could not save the invitation: {err}"));
        }
        let file = gio::File::for_path(&path);
        let this = Rc::clone(self);
        gtk::FileLauncher::new(Some(&file)).launch(
            Some(&self.window),
            gio::Cancellable::NONE,
            move |result| {
                if let Err(err) = result {
                    this.toast(&format!("No app on this desktop opens invitations: {err}"));
                }
            },
        );
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
