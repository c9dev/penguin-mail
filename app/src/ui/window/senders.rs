//! Leaving mailing lists and blocking senders.

use std::rc::Rc;

use gtk::{gio, glib};
use mailrs_sync::{History, Leave, MailAction, Permitted, TriageAction};

use super::MainWindow;
use crate::permission::{Occasion, Permission};
use crate::ui::confirm::{Tone, confirm};
use crate::ui::conversation::ConversationView;
use crate::unsubscribe::{Unsubscribe, choose};
use mailrs_domain::translate::{fill, gettext};

impl MainWindow {
    /// Unsubscribes from the mailing list of the thread `view` shows,
    /// after asking.
    pub(super) fn unsubscribe(self: &Rc<Self>, view: Rc<ConversationView>) {
        let found = view.find(|open| {
            let (meta, body) = open.list_unsubscribe()?;
            let header = body.list_unsubscribe.clone()?;
            let sender = meta
                .from
                .as_ref()
                .map(|a| a.display().to_string())
                .unwrap_or_else(|| gettext("this list"));
            Some((
                open.target(),
                sender,
                choose(&header, body.one_click_unsubscribe),
            ))
        });
        let Some((asked_on, sender, method)) = found else {
            return self.toast(&gettext("This message has no unsubscribe link"));
        };
        let Some(method) = method else {
            return self.toast(&gettext(
                "This message's unsubscribe link is not one Penguin Mail can use",
            ));
        };
        let body = match &method {
            Unsubscribe::OneClick(_) => {
                gettext("Penguin Mail asks the sender to take you off the list.")
            }
            Unsubscribe::Email { .. } => {
                gettext("Penguin Mail sends the list an unsubscribe request from your account.")
            }
            Unsubscribe::Page(_) => gettext("The sender's unsubscribe page opens in your browser."),
        };
        let question = confirm(
            &fill(
                &gettext("Unsubscribe from {sender}?"),
                &[("sender", &sender)],
            ),
            &body,
            &gettext("Unsubscribe"),
            Tone::Suggested,
        )
        .by_default();
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            if !question.ask(&this.window).await {
                return;
            }
            match this.leave_list(asked_on.account_id, method).await {
                Ok(()) => {
                    // The request can take a while; mark only the thread it
                    // came from, if that is still the one on screen.
                    if view.is_showing(&asked_on) {
                        view.mark_unsubscribed();
                    }
                    this.toast(&fill(
                        &gettext("Unsubscribed from {sender}"),
                        &[("sender", &sender)],
                    ));
                }
                Err(err) => this.toast(&fill(
                    &gettext("Could not unsubscribe: {reason}"),
                    &[("reason", &err.to_string())],
                )),
            }
        });
    }

    /// Leaves a mailing list the way `how` says, from the account, and
    /// does what `mailrs_sync` leaves to the app: sending the request or
    /// opening the page. The Unsubscribe button and the assistant both end
    /// here.
    pub(super) async fn leave_list(
        self: &Rc<Self>,
        account_id: mailrs_domain::AccountId,
        how: Unsubscribe,
    ) -> Result<(), String> {
        let actions = self.core.actions();
        let leave = self
            .core
            .call(async move { actions.unsubscribe(account_id, how).await })
            .await
            .map_err(|e| e.to_string())?;
        match leave {
            Leave::Done => Ok(()),
            Leave::Send { to, subject, body } => {
                let app = self
                    .app
                    .upgrade()
                    .ok_or_else(|| gettext("The app is closing."))?;
                app.send_request(account_id, &to, subject, body);
                Ok(())
            }
            Leave::Open(url) => {
                gtk::UriLauncher::new(&url).launch(
                    Some(&self.window),
                    gio::Cancellable::NONE,
                    |_| {},
                );
                Ok(())
            }
        }
    }

    /// Sends future mail from the sender of the thread `view` shows to the
    /// Trash with a Gmail filter, and moves this thread there too.
    pub(super) fn block_sender(self: &Rc<Self>, view: Rc<ConversationView>) {
        let found = view.find(|open| Some((open.other_sender()?.clone(), open.target())));
        let Some((sender, target)) = found else {
            return self.toast(&gettext("Open a message from the sender to block"));
        };
        let (account_id, email) = (target.account_id, sender.email.clone());
        let question = confirm(
            &fill(&gettext("Block {sender}?"), &[("sender", sender.display())]),
            &fill(
                &gettext(
                    "New mail from {email} goes straight to the Trash. This conversation \
                     moves there now. Remove the rule under Rules to unblock.",
                ),
                &[("email", &email)],
            ),
            &gettext("Block"),
            Tone::Destructive,
        );
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            if !question.ask(&this.window).await {
                return;
            }
            if this.core.account(account_id).is_none() {
                return this.toast(&gettext("That account is not connected"));
            }
            let settings = this.core.gmail_settings();
            let blocking = {
                let email = email.clone();
                this.core
                    .call(async move { settings.block_sender(account_id, &email).await })
                    .await
            };
            match blocking {
                Ok(Permitted::Done(_)) => {
                    if view.is_showing(&target) {
                        view.clear();
                    }
                    this.perform(
                        vec![target],
                        MailAction::Triage(TriageAction::Trash),
                        History::Record,
                        Some(fill(&gettext("Blocked {sender}"), &[("sender", &email)])),
                    );
                }
                Ok(Permitted::NeedsPermission) => {
                    this.ask_permission(account_id, Permission::Settings, Occasion::Needed)
                }
                Err(err) => this.toast(&fill(
                    &gettext("Could not block the sender: {reason}"),
                    &[("reason", &err.to_string())],
                )),
            }
        });
    }
}
