//! Leaving mailing lists and blocking senders.

use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use mailrs_sync::{History, MailAction, Permitted, TriageAction};

use super::{MainWindow, Target};
use crate::compose::Draft;
use crate::ui::conversation::ConversationView;
use crate::unsubscribe::{Unsubscribe, choose};
use mailrs_domain::translate::{fill, gettext};

impl MainWindow {
    /// Unsubscribes from the open thread's mailing list, after asking.
    pub(super) fn unsubscribe(self: &Rc<Self>) {
        self.unsubscribe_from(Rc::clone(&self.conversation));
    }

    pub(super) fn unsubscribe_from(self: &Rc<Self>, view: Rc<ConversationView>) {
        let found = view.with_open(|open| {
            let (meta, body) = open.list_unsubscribe()?;
            let header = body.list_unsubscribe.clone()?;
            let sender = meta
                .from
                .as_ref()
                .map(|a| a.display().to_string())
                .unwrap_or_else(|| gettext("this list"));
            Some((
                open.account_id,
                sender,
                choose(&header, body.one_click_unsubscribe),
            ))
        });
        let Some(Some((account_id, sender, method))) = found else {
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
        let dialog = adw::AlertDialog::new(
            Some(&fill(
                &gettext("Unsubscribe from {sender}?"),
                &[("sender", &sender)],
            )),
            Some(&body),
        );
        dialog.add_responses(&[
            ("cancel", &gettext("Cancel")),
            ("unsubscribe", &gettext("Unsubscribe")),
        ]);
        dialog.set_response_appearance("unsubscribe", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("unsubscribe"));
        dialog.set_close_response("cancel");
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            if dialog.choose_future(Some(&this.window)).await != "unsubscribe" {
                return;
            }
            let done = match method {
                Unsubscribe::OneClick(url) => this
                    .core
                    .call(async move { mailrs_gmail::one_click_unsubscribe(&url).await })
                    .await
                    .map_err(|e| e.to_string()),
                Unsubscribe::Email { to, subject, body } => {
                    let Some(app) = this.app.upgrade() else {
                        return;
                    };
                    let mut draft = Draft::new(account_id, app.identity(account_id));
                    draft.to = crate::compose::parse_recipients(&to);
                    draft.subject = subject;
                    draft.markdown = body;
                    app.send_immediately(draft);
                    Ok(())
                }
                Unsubscribe::Page(url) => {
                    gtk::UriLauncher::new(&url).launch(
                        Some(&this.window),
                        gio::Cancellable::NONE,
                        |_| {},
                    );
                    Ok(())
                }
            };
            match done {
                Ok(()) => {
                    view.with_open(|o| o.unsubscribed = true);
                    view.render_buttons();
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

    /// Sends future mail from the open thread's sender to the Trash with a
    /// Gmail filter, and moves this thread there too.
    pub(super) fn block_sender(self: &Rc<Self>) {
        self.block_sender_from(Rc::clone(&self.conversation));
    }

    pub(super) fn block_sender_from(self: &Rc<Self>, view: Rc<ConversationView>) {
        let found = view.with_open(|open| {
            let me = open.me.clone();
            let sender = open
                .messages
                .iter()
                .rev()
                .filter_map(|m| m.from.clone())
                .find(|a| !me.iter().any(|mine| mine.eq_ignore_ascii_case(&a.email)))?;
            let target = Target {
                account_id: open.account_id,
                thread_id: open.thread_id.clone(),
                message_id: open.only_message.clone(),
            };
            Some((open.account_id, sender, target))
        });
        let Some(Some((account_id, sender, target))) = found else {
            return self.toast(&gettext("Open a message from the sender to block"));
        };
        let email = sender.email.clone();
        let dialog = adw::AlertDialog::new(
            Some(&fill(
                &gettext("Block {sender}?"),
                &[("sender", sender.display())],
            )),
            Some(&fill(
                &gettext(
                    "New mail from {email} goes straight to the Trash. This conversation \
                     moves there now. Remove the rule under Rules to unblock.",
                ),
                &[("email", &email)],
            )),
        );
        dialog.add_responses(&[("cancel", &gettext("Cancel")), ("block", &gettext("Block"))]);
        dialog.set_response_appearance("block", adw::ResponseAppearance::Destructive);
        dialog.set_close_response("cancel");
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            if dialog.choose_future(Some(&this.window)).await != "block" {
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
                    if view.with_open(|o| o.thread_id == target.thread_id) == Some(true) {
                        view.clear();
                    }
                    this.perform(
                        vec![target],
                        MailAction::Triage(TriageAction::Trash),
                        History::Record,
                        Some(fill(&gettext("Blocked {sender}"), &[("sender", &email)])),
                    );
                }
                Ok(Permitted::NeedsPermission) => this.ask_for_settings_access(account_id),
                Err(err) => this.toast(&fill(
                    &gettext("Could not block the sender: {reason}"),
                    &[("reason", &err.to_string())],
                )),
            }
        });
    }

    /// Explains that Gmail settings need one more permission, and offers to
    /// ask Google for it.
    pub(super) fn ask_for_settings_access(self: &Rc<Self>, account_id: mailrs_domain::AccountId) {
        let Some(account) = self.account(account_id) else {
            return;
        };
        let dialog = adw::AlertDialog::new(
            Some(&gettext("Allow Changes to Gmail Settings")),
            Some(&fill(
                &gettext(
                    "Penguin Mail needs permission to change Gmail settings for {account}. \
                     Google asks you to confirm in your browser.",
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
                this.authorize(Some(account.email));
            }
        });
    }
}
