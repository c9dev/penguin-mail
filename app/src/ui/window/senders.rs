//! Leaving mailing lists and blocking senders.

use std::rc::Rc;

use gtk::{gio, glib};
use mailrs_sync::{History, Leave, MailAction, Permitted, TriageAction};

use super::MainWindow;
use crate::permission::{Occasion, Permission};
use crate::ui::confirm::{Tone, confirm};
use crate::ui::conversation::ConversationView;
use crate::ui::unsubscribe::{self, ListLine, Way, summary};
use crate::unsubscribe::{Unsubscribe, choose_with_body};
use crate::unsubscribe_page::{Adviser, Outcome, WebkitBrowser, finish, model_adviser, prepare};
use mailrs_domain::translate::{fill, gettext};

impl MainWindow {
    /// Unsubscribes from the mailing list of the thread `view` shows,
    /// after asking.
    ///
    /// A list that lets go through a page has its page read out of sight
    /// while the dialog is already on screen, so the question names the
    /// button and the address rather than sending the reader off to do
    /// it themselves. A page nobody could read still opens in their
    /// browser, as it always did.
    pub(super) fn unsubscribe(self: &Rc<Self>, view: Rc<ConversationView>) {
        let found = view.find(|open| {
            let (meta, body) = open.list_unsubscribe()?;
            let sender = meta
                .from
                .as_ref()
                .map(|a| a.display().to_string())
                .unwrap_or_else(|| gettext("this list"));
            let sent_to = meta
                .to
                .iter()
                .chain(meta.cc.iter())
                .map(|a| a.email.clone())
                .collect::<Vec<String>>();
            Some((
                open.target(),
                sender,
                choose_with_body(
                    body.list_unsubscribe.as_deref(),
                    body.one_click_unsubscribe,
                    body.html.as_deref(),
                ),
                sent_to,
            ))
        });
        let Some((asked_on, sender, method, sent_to)) = found else {
            return self.toast(&gettext("This message has no unsubscribe link"));
        };
        let Some(method) = method else {
            return self.toast(&gettext(
                "This message's unsubscribe link is not one Penguin Mail can use",
            ));
        };
        let account = self
            .account(asked_on.account_id)
            .map(|account| account.email)
            .unwrap_or_default();
        let (way, page) = match &method {
            Unsubscribe::OneClick(_) => (Way::OneClick, None),
            Unsubscribe::Email { .. } => (
                Way::Mail {
                    from: account.clone(),
                },
                None,
            ),
            Unsubscribe::Page(url) | Unsubscribe::BodyLink(url) => {
                (Way::Reading, Some(url.clone()))
            }
        };
        let lines = vec![ListLine {
            name: sender.clone(),
            way,
        }];

        // One hidden view for the run, held by both halves: the read that
        // fills the dialog in, and the submission after the yes.
        let browser = page.as_ref().map(|_| Rc::new(WebkitBrowser::new()));
        let (tell, hear) = async_channel::bounded(1);
        match (page, browser.clone()) {
            (Some(url), Some(browser)) => {
                let senders = self.settings_with(|s| s.senders(&account));
                let mine: Vec<String> = senders.into_iter().map(|a| a.email).collect();
                let address = unsubscribe::sent_to(&sent_to, &mine, &account);
                let ai = self.settings_with(|s| s.ai.clone());
                let adviser = model_adviser(&ai, self.core.runtime());
                glib::spawn_future_local(async move {
                    let adviser = adviser.as_ref().map(|a| a as &dyn Adviser);
                    let prepared = prepare(&*browser, adviser, &url, &address).await;
                    let _ = tell.send((0, Way::Page(prepared))).await;
                });
            }
            // Nothing will arrive, and the dialog waits for a line to
            // settle until the last sender is gone.
            _ => drop(tell),
        }

        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let Some(ticked) = unsubscribe::confirm(&this.window, lines, hear).await else {
                return;
            };
            let Some((_, way)) = ticked.into_iter().next() else {
                return;
            };
            let (outcome, page) = match way {
                Way::Page(prepared) => match &browser {
                    Some(browser) => (
                        finish(&**browser, &prepared).await,
                        Some(prepared.url.clone()),
                    ),
                    None => return,
                },
                // Unsubscribe is insensitive while a line still reads, so
                // nothing unread reaches here.
                Way::Reading => return,
                Way::OneClick | Way::Mail { .. } => (
                    match this.leave_list(asked_on.account_id, method).await {
                        Ok(()) => Outcome::Done,
                        Err(err) => Outcome::Failed(err),
                    },
                    None,
                ),
            };
            // The person said yes to this page, so opening it is the rest
            // of what they asked for, not something to offer in a toast.
            if let Outcome::OpenInBrowser(url) = &outcome {
                return this.open_page(url);
            }
            // The request can take a while; mark only the thread it came
            // from, if that is still the one on screen.
            if outcome == Outcome::Done && view.is_showing(&asked_on) {
                view.mark_unsubscribed();
            }
            let said = summary(&[(sender, outcome.clone())]);
            match (outcome, page) {
                // The form went in and the page said nothing either way.
                // Whoever wants to know can look at what it did say.
                (Outcome::Unclear, Some(url)) => this.toast_opening(&said, &url),
                _ => this.toast(&said),
            }
        });
    }

    /// Toasts `said` with a button that opens `url` in the person's own
    /// browser.
    fn toast_opening(self: &Rc<Self>, said: &str, url: &str) {
        let toast = adw::Toast::builder()
            .title(glib::markup_escape_text(said))
            .button_label(gettext("Open Page"))
            .timeout(8)
            .build();
        let this = Rc::downgrade(self);
        let url = url.to_string();
        toast.connect_button_clicked(move |_| {
            if let Some(window) = this.upgrade() {
                window.open_page(&url);
            }
        });
        self.toasts.add_toast(toast);
    }

    /// Opens `url` in the person's own browser.
    pub(super) fn open_page(&self, url: &str) {
        gtk::UriLauncher::new(url).launch(Some(&self.window), gio::Cancellable::NONE, |_| {});
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
                self.open_page(&url);
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
                Err(err) => this.failed(&gettext("Could not block the sender: {reason}"), &err),
            }
        });
    }
}
