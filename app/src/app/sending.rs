//! Sending mail: the Undo Send delay, Send Later, and the loop that sends
//! scheduled drafts when they come due.

use std::rc::Rc;

use gtk::glib;
use mailrs_domain::system_label;
use mailrs_store::scheduled::{self, Scheduled};
use mailrs_sync::now_millis;

use super::App;
use crate::compose::{Draft, SendWhen, build_mime, new_message_id};
use crate::format::future_date;

/// How often the scheduler looks for messages that are due.
const SCHEDULER_SECONDS: u32 = 30;

impl App {
    /// Sends what a composer handed over: now, after the Undo delay, or at
    /// a scheduled time.
    pub fn send(self: &Rc<Self>, draft: Draft, when: SendWhen) {
        match when {
            SendWhen::At(at) => self.schedule(draft, at),
            SendWhen::Now => {
                let delay = self.settings().undo_send.seconds();
                match self.window() {
                    Some(window) if delay > 0 => {
                        self.pending_sends.set(self.pending_sends.get() + 1);
                        let cancelled = Rc::new(std::cell::Cell::new(false));
                        let (flag, app, undone) =
                            (Rc::clone(&cancelled), Rc::clone(self), draft.clone());
                        window.offer_undo_send(delay, move || {
                            flag.set(true);
                            if let Some(composer) = app.compose(undone.clone()) {
                                composer.mark_unsaved();
                            }
                        });
                        let app = Rc::clone(self);
                        glib::timeout_add_seconds_local_once(delay, move || {
                            app.pending_sends
                                .set(app.pending_sends.get().saturating_sub(1));
                            if !cancelled.get() {
                                app.send_now(draft, true);
                            }
                        });
                    }
                    _ => self.send_now(draft, true),
                }
            }
        }
    }

    /// Sends without the Undo delay, for messages the user never wrote,
    /// such as an unsubscribe request.
    pub fn send_immediately(self: &Rc<Self>, draft: Draft) {
        self.send_now(draft, false);
    }

    /// Sends at once. With `announce`, says so in the window.
    fn send_now(self: &Rc<Self>, draft: Draft, announce: bool) {
        let raw = match build_mime(
            &draft,
            now_millis() / 1000,
            &new_message_id(&draft.from.email),
        ) {
            Ok(raw) => raw,
            Err(err) => return self.reopen(draft, &format!("Could not build the message: {err}")),
        };
        let Some(sync) = self.core.account(draft.account_id) else {
            return self.reopen(
                draft,
                "That account is not connected. Check its status in the sidebar.",
            );
        };
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let (thread, draft_id) = (draft.thread_id.clone(), draft.draft_id.clone());
            match this
                .core
                .call(async move { sync.send(raw, thread, draft_id).await })
                .await
            {
                Ok(_) => {
                    if let Some(draft_id) = draft.draft_id.clone() {
                        let account_id = draft.account_id;
                        this.core
                            .spawn_write(move |c| scheduled::remove(c, account_id, &draft_id));
                        this.scheduled_changed();
                    }
                    this.contacts_stale.set(true);
                    this.core.poke(draft.account_id);
                    if announce && let Some(window) = this.window() {
                        window.toast_sent();
                    }
                }
                Err(err) => this.reopen(draft, &format!("Not sent: {err}")),
            }
        });
    }

    /// Saves `draft` to Gmail and records when to send it.
    fn schedule(self: &Rc<Self>, draft: Draft, at: i64) {
        let raw = match build_mime(
            &draft,
            now_millis() / 1000,
            &new_message_id(&draft.from.email),
        ) {
            Ok(raw) => raw,
            Err(err) => return self.reopen(draft, &format!("Could not build the message: {err}")),
        };
        let Some(sync) = self.core.account(draft.account_id) else {
            return self.reopen(
                draft,
                "That account is not connected. Check its status in the sidebar.",
            );
        };
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let (thread, draft_id) = (draft.thread_id.clone(), draft.draft_id.clone());
            let saved = match this
                .core
                .call(async move { sync.save_draft(raw, thread, draft_id).await })
                .await
            {
                Ok(saved) => saved,
                Err(err) => return this.reopen(draft, &format!("Not scheduled: {err}")),
            };
            let recipients = draft
                .to
                .iter()
                .chain(&draft.cc)
                .map(|a| a.display().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            let item = Scheduled {
                account_id: draft.account_id,
                draft_id: saved.draft_id,
                message_id: saved.message_id,
                thread_id: saved.thread_id,
                subject: draft.subject.clone(),
                recipients,
                send_at: at,
            };
            match this
                .core
                .write(move |c| scheduled::schedule(c, &item))
                .await
            {
                Ok(()) => {
                    this.core.poke(draft.account_id);
                    this.scheduled_changed();
                    if let Some(window) = this.window() {
                        window.toast_text(&format!(
                            "Will send {}",
                            future_date(at, chrono::Local::now())
                        ));
                    }
                }
                Err(err) => this.reopen(draft, &format!("Not scheduled: {err}")),
            }
        });
    }

    /// Opens the composer again with a message that could not go out.
    fn reopen(self: &Rc<Self>, draft: Draft, problem: &str) {
        if let Some(composer) = self.compose(draft) {
            composer.mark_unsaved();
            composer.toast(problem);
        }
    }

    /// Checks for due messages every half minute. Anything that fell due
    /// while Penguin Mail was not running goes out on the first check.
    pub(super) fn start_scheduler(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        glib::timeout_add_seconds_local(SCHEDULER_SECONDS, move || {
            let Some(app) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            app.send_due();
            glib::ControlFlow::Continue
        });
    }

    fn send_due(self: &Rc<Self>) {
        if self.scheduler_running.replace(true) {
            return;
        }
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let due = this
                .core
                .read(|c| scheduled::due(c, now_millis()))
                .await
                .unwrap_or_default();
            let mut changed = false;
            for item in due {
                // The account may still be connecting; the next pass retries.
                let Some(sync) = this.core.account(item.account_id) else {
                    continue;
                };
                let draft_id = item.draft_id.clone();
                let sent = this
                    .core
                    .call(async move { sync.send_draft(&draft_id).await })
                    .await;
                match sent {
                    Ok(outcome) => {
                        let (account_id, draft_id) = (item.account_id, item.draft_id.clone());
                        let _ = this
                            .core
                            .write(move |c| scheduled::remove(c, account_id, &draft_id))
                            .await;
                        changed = true;
                        this.core.poke(item.account_id);
                        if outcome.is_some()
                            && let Some(window) = this.window()
                        {
                            let subject = if item.subject.is_empty() {
                                "your message".to_string()
                            } else {
                                format!("“{}”", item.subject)
                            };
                            window.toast_text(&format!("Sent {subject}"));
                        }
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, draft = %item.draft_id, "scheduled send failed; will retry")
                    }
                }
            }
            if this.return_reminders().await {
                changed = true;
            }
            if changed {
                this.scheduled_changed();
            }
            this.scheduler_running.set(false);
        });
    }

    /// Puts conversations whose reminder is due back in the inbox, unread,
    /// and announces them. Returns whether any came back.
    async fn return_reminders(self: &Rc<Self>) -> bool {
        let due = self
            .core
            .read(|c| mailrs_store::reminders::due(c, now_millis()))
            .await
            .unwrap_or_default();
        let mut returned = false;
        for item in due {
            let Some(sync) = self.core.account(item.account_id) else {
                continue;
            };
            let thread = item.thread_id.clone();
            let back = mailrs_sync::TriageAction::Relabel {
                add: vec![system_label::INBOX.into(), system_label::UNREAD.into()],
                remove: vec![],
            };
            if let Err(err) = self
                .core
                .call(async move { sync.triage_thread(&thread, &back).await })
                .await
            {
                tracing::warn!(error = %err, "a reminder could not return its conversation; will retry");
                continue;
            }
            let (account_id, thread) = (item.account_id, item.thread_id.clone());
            let newest = self
                .core
                .write(move |c| {
                    mailrs_store::reminders::remove(c, account_id, &thread)?;
                    Ok(
                        mailrs_store::messages::thread_messages(c, account_id, &thread)?
                            .into_iter()
                            .last(),
                    )
                })
                .await
                .ok()
                .flatten();
            returned = true;
            let settings = self.settings();
            if settings.notifications
                && let Some(message) = newest
            {
                crate::notify::announce(
                    vec![message],
                    settings.notification_previews,
                    settings.notification_buttons.clone(),
                    self.chosen.clone(),
                );
            }
        }
        returned
    }

    /// Tells the window the Send Later list changed.
    pub fn scheduled_changed(&self) {
        if let Some(window) = self.window() {
            window.scheduled_changed();
        }
    }
}
