//! Sending mail: the Undo Send delay, Send Later, the outbox a message
//! waits in when it cannot go out, and the loop that empties it.
//!
//! Everything a message needs to go out later lives in the store, so
//! closing the laptop mid-send loses nothing: the bytes as they were
//! built, and the draft the composer would reopen. `mailrs_sync::Outbox`
//! owns the sending and decides what is worth another try; this module
//! builds the message, hands it over, and says what happened.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::glib;
use mailrs_domain::AccountId;
use mailrs_store::outbox::Queued;
use mailrs_sync::{Posted, now_millis};

use super::{App, Signature};
use crate::compose::{Built, Draft, SendWhen, build, build_protected, new_message_id};
use crate::format::future_date;
use crate::protection::{self, Addressees, Standard};
use crate::ui::window::Notice;
use crate::unsubscribe::RequestSent;
use mailrs_domain::translate::{fill, gettext};

/// How often the scheduler looks for messages that are due.
const SCHEDULER_SECONDS: u32 = 30;

/// What a send says when the account it names is not syncing.
fn not_connected() -> String {
    gettext("That account is not connected. Check its status in the sidebar.")
}

impl App {
    /// Sends `draft`: now, after the Undo delay, or at a scheduled time.
    pub fn send(self: &Rc<Self>, draft: Draft, when: SendWhen, signature: Signature) {
        let draft = self.signed_when(draft, signature);
        self.dispatch(draft, None, when);
    }

    /// Sends a message a composer finished and built, as it was written.
    pub fn send_built(self: &Rc<Self>, draft: Draft, built: Built, when: SendWhen) {
        self.dispatch(draft, Some(built), when);
    }

    fn dispatch(self: &Rc<Self>, draft: Draft, built: Option<Built>, when: SendWhen) {
        match when {
            SendWhen::At(at) => self.schedule(draft, built, at),
            SendWhen::Now => {
                let delay = self.settings().undo_send.seconds();
                match self.window() {
                    Some(window) if delay > 0 => {
                        self.pending_sends.set(self.pending_sends.get() + 1);
                        // Undo and the timer share the one message, and
                        // whichever runs first takes it. Cloning it for
                        // Undo cost a copy of every attachment on every
                        // send.
                        let held = Rc::new(RefCell::new(Some((draft, built))));
                        let (undone, app) = (Rc::clone(&held), Rc::clone(self));
                        window.notice(Notice::UndoSend {
                            seconds: delay,
                            undo: Box::new(move || {
                                let Some((draft, _)) = undone.borrow_mut().take() else {
                                    return;
                                };
                                if let Some(composer) =
                                    app.open_composer(draft, Signature::AsWritten)
                                {
                                    composer.mark_unsaved();
                                }
                            }),
                        });
                        let app = Rc::clone(self);
                        // Nothing reaches the outbox until the delay runs
                        // out, so a message being undone is still only a
                        // message in hand.
                        glib::timeout_add_seconds_local_once(delay, move || {
                            app.pending_sends
                                .set(app.pending_sends.get().saturating_sub(1));
                            let taken = held.borrow_mut().take();
                            if let Some((draft, built)) = taken {
                                app.send_now(draft, built);
                            }
                        });
                    }
                    _ => self.send_now(draft, built),
                }
            }
        }
    }

    /// Sends a request the person never wrote, the mail that leaves a
    /// mailing list, from `from` when it is one of the account's
    /// addresses. It has no signature and no Undo delay, since nothing in
    /// it is theirs to take back.
    ///
    /// The answer waits for the outbox, so "Unsubscribed" is said only
    /// once the mail has left. A request that fails reopens no composer:
    /// the person never wrote it, and the caller says why it failed.
    pub async fn send_request(
        self: &Rc<Self>,
        account_id: AccountId,
        from: &str,
        to: &str,
        subject: String,
        body: String,
    ) -> Result<RequestSent, String> {
        if self.core.account(account_id).is_none() {
            return Err(not_connected());
        }
        let mut draft = self.blank_draft(account_id);
        // The list knows the person by the address it writes to, and a
        // request from another of their addresses may not match anyone
        // on it.
        let alias = self.identities().into_iter().find(|identity| {
            identity.account_id == account_id && identity.address.email.eq_ignore_ascii_case(from)
        });
        if let Some(identity) = alias {
            draft.from = identity.address;
        }
        draft.to = crate::compose::parse_recipients(to);
        draft.subject = subject;
        draft.markdown = body;
        let raw = self.raw_for(&draft, None).await?;
        let outbox = self.core.outbox();
        let message = queued(&draft, raw, now_millis());
        let posted = self
            .core
            .call(async move { outbox.post(message).await })
            .await;
        match posted {
            Ok(Posted::Sent(_)) => {
                self.core.poke(account_id);
                self.scheduled_changed();
                Ok(RequestSent::Sent)
            }
            Ok(Posted::Waiting(_)) => {
                self.scheduled_changed();
                Ok(RequestSent::Waiting)
            }
            Ok(Posted::Refused(problem)) => Err(problem),
            Err(err) => Err(err.to_string()),
        }
    }

    /// The bytes to send: the message as it was written, or what the
    /// engine the draft names made of it when the writer asked to sign or
    /// encrypt. Either engine may put a pinentry in front of them and
    /// wait, so this runs off the GTK thread and holds nothing up but this
    /// message.
    async fn raw_for(&self, draft: &Draft, built: Option<Built>) -> Result<Vec<u8>, String> {
        let message_id = new_message_id(&draft.from.email);
        let date = now_millis() / 1000;
        let failed = |what: &str| {
            fill(
                &gettext("Could not build the message: {reason}"),
                &[("reason", what)],
            )
        };
        let part = match prepared(draft, built, date, &message_id).map_err(|err| failed(&err))? {
            Built::Message(raw) => return Ok(raw),
            Built::Body(part) => part,
        };
        let from = draft.from.email.clone();
        let addressees = Addressees::of(draft);
        let (sign, encrypt) = (draft.sign, draft.encrypt);
        if encrypt && draft.standard == Standard::Smime && addressees.has_blind_copy() {
            // The composer never offers this, but a draft that comes back
            // from the outbox was chosen before anyone checked.
            return Err(fill(
                &gettext("Not encrypted, so not sent: {reason}"),
                &[("reason", &protection::smime_names_everyone())],
            ));
        }
        let entity = match draft.standard {
            Standard::Pgp => {
                self.core
                    .gpg(move |pgp| {
                        if !encrypt {
                            return pgp.sign(&part, &from);
                        }
                        // The sender reads their own copy in Sent only if
                        // gpg encrypts to them as well, which it can when
                        // it holds a key of theirs.
                        let own = pgp
                            .keys_for(std::slice::from_ref(&from))?
                            .iter()
                            .any(|held| held.key.is_some());
                        // A signature goes inside the encryption, which
                        // is the only place one on encrypted mail means
                        // anything.
                        pgp.encrypt(
                            &part,
                            &addressees.readers(own),
                            sign.then_some(from.as_str()),
                        )
                    })
                    .await
            }
            Standard::Smime => {
                self.core
                    .gpgsm(move |smime| {
                        if !encrypt {
                            return smime.sign(&part, &from);
                        }
                        let own = smime
                            .certificates_for(std::slice::from_ref(&from))?
                            .iter()
                            .any(|held| held.certificate.is_some());
                        smime.encrypt(
                            &part,
                            &addressees.certificates(own),
                            sign.then_some(from.as_str()),
                        )
                    })
                    .await
            }
        };
        let entity = entity.map_err(|err| {
            let values = [("reason", err.to_string())];
            let values = [("reason", values[0].1.as_str())];
            match encrypt {
                true => fill(&gettext("Not encrypted, so not sent: {reason}"), &values),
                false => fill(&gettext("Not signed, so not sent: {reason}"), &values),
            }
        })?;
        build_protected(draft, date, &message_id, entity).map_err(|err| failed(&err))
    }

    /// Sends at once, or puts the message in the outbox when it cannot go,
    /// and says so in the window.
    fn send_now(self: &Rc<Self>, draft: Draft, built: Option<Built>) {
        if self.core.account(draft.account_id).is_none() {
            return self.reopen(draft, &not_connected());
        }
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let raw = match this.raw_for(&draft, built).await {
                Ok(raw) => raw,
                Err(problem) => return this.reopen(draft, &problem),
            };
            let outbox = this.core.outbox();
            let message = queued(&draft, raw, now_millis());
            let posted = this
                .core
                .call(async move { outbox.post(message).await })
                .await;
            match posted {
                Ok(Posted::Sent(_)) => {
                    this.contacts_stale.set(true);
                    this.core.poke(draft.account_id);
                    this.scheduled_changed();
                    this.tell_window(Notice::Sent);
                }
                Ok(Posted::Waiting(_)) => {
                    this.scheduled_changed();
                    this.tell_window(Notice::Toast(gettext(
                        "Waiting in the Outbox. It goes out as soon as it can.",
                    )));
                }
                Ok(Posted::Refused(problem)) => this.reopen(
                    draft,
                    &fill(&gettext("Not sent: {reason}"), &[("reason", &problem)]),
                ),
                Err(err) => this.reopen(
                    draft,
                    &fill(
                        &gettext("Not sent: {reason}"),
                        &[("reason", &err.to_string())],
                    ),
                ),
            }
        });
    }

    /// Saves `draft` to Gmail and records when to send it.
    fn schedule(self: &Rc<Self>, draft: Draft, built: Option<Built>, at: i64) {
        if self.core.account(draft.account_id).is_none() {
            return self.reopen(draft, &not_connected());
        }
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            // A scheduled message is signed now rather than at its hour,
            // since the person is here to answer the pinentry now.
            let raw = match this.raw_for(&draft, built).await {
                Ok(raw) => raw,
                Err(problem) => return this.reopen(draft, &problem),
            };
            let outbox = this.core.outbox();
            let message = queued(&draft, raw, at);
            let posted = this
                .core
                .call(async move { outbox.schedule(message).await })
                .await;
            match posted {
                Ok(Posted::Refused(problem)) => this.reopen(
                    draft,
                    &fill(&gettext("Not scheduled: {reason}"), &[("reason", &problem)]),
                ),
                Ok(_) => {
                    this.core.poke(draft.account_id);
                    this.scheduled_changed();
                    this.tell_window(Notice::Toast(fill(
                        &gettext("Will send {when}"),
                        &[("when", &future_date(at, chrono::Local::now()))],
                    )));
                }
                Err(err) => this.reopen(
                    draft,
                    &fill(
                        &gettext("Not scheduled: {reason}"),
                        &[("reason", &err.to_string())],
                    ),
                ),
            }
        });
    }

    /// Opens the composer again with a message that could not go out.
    fn reopen(self: &Rc<Self>, draft: Draft, problem: &str) {
        if let Some(composer) = self.open_composer(draft, Signature::AsWritten) {
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

    /// Empties the outbox as far as Gmail will let it, and brings back the
    /// conversations whose reminder is due.
    pub(super) fn send_due(self: &Rc<Self>) {
        if self.scheduler_running.replace(true) {
            return;
        }
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let outbox = this.core.outbox();
            let drained = this
                .core
                .call(async move { outbox.send_due(now_millis()).await })
                .await;
            let mut changed = match drained {
                Ok(drained) => {
                    for message in &drained.sent {
                        this.core.poke(message.account_id);
                    }
                    for message in &drained.sent {
                        this.tell_window(Notice::Toast(fill(
                            &gettext("Sent {message}"),
                            &[("message", &named(&message.subject))],
                        )));
                    }
                    for message in &drained.stuck {
                        this.tell_window(Notice::Toast(fill(
                            &gettext("Still in the Outbox: {message}"),
                            &[("message", &named(&message.subject))],
                        )));
                    }
                    drained.changed
                }
                Err(err) => {
                    tracing::warn!(error = %err, "the outbox could not be emptied; will retry");
                    false
                }
            };
            if this.return_reminders().await {
                changed = true;
            }
            if changed {
                this.scheduled_changed();
            }
            this.scheduler_running.set(false);
        });
    }

    /// Tries the outbox again without waiting out the rest of an interval,
    /// which is what the network coming back calls for.
    pub(super) fn wake_outbox(self: &Rc<Self>) {
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let outbox = this.core.outbox();
            if let Err(err) = this.core.call(async move { outbox.try_now().await }).await {
                return tracing::warn!(error = %err, "could not bring the outbox forward");
            }
            this.send_due();
        });
    }

    /// Puts conversations whose reminder is due back in the inbox, unread,
    /// and announces them. Returns whether any came back.
    async fn return_reminders(self: &Rc<Self>) -> bool {
        let actions = self.core.actions();
        let returned = match self
            .core
            .call(async move { actions.return_due(now_millis()).await })
            .await
        {
            Ok(returned) => returned,
            Err(err) => {
                tracing::warn!(error = %err, "could not read the reminders that are due");
                return false;
            }
        };
        let settings = self.settings();
        if settings.notifications {
            for message in returned.iter().filter_map(|r| r.newest.clone()) {
                crate::notify::announce(
                    vec![message],
                    settings.notification_previews,
                    settings.notification_buttons.clone(),
                    self.chosen.clone(),
                );
            }
        }
        !returned.is_empty()
    }

    /// Tells the window the Send Later and Outbox lists changed.
    fn scheduled_changed(&self) {
        self.tell_window(Notice::OutboxChanged);
    }
}

/// What the composer built of `draft`, or the same built here for a
/// message that came from somewhere else, such as the assistant.
fn prepared(
    draft: &Draft,
    built: Option<Built>,
    date_secs: i64,
    message_id: &str,
) -> Result<Built, String> {
    match built {
        Some(built) => Ok(built),
        None => build(draft, date_secs, message_id),
    }
}

/// A message for the outbox: the bytes that go out, and the draft the
/// composer reopens if the person wants to change it before it does.
fn queued(draft: &Draft, raw: Vec<u8>, send_at: i64) -> Queued {
    let recipients = draft
        .to
        .iter()
        .chain(&draft.cc)
        .map(|a| a.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    Queued {
        account_id: draft.account_id,
        draft_id: draft.draft_id.clone(),
        thread_id: draft.thread_id.clone(),
        subject: draft.subject.clone(),
        recipients,
        send_at,
        raw: Some(raw),
        composer: serde_json::to_string(draft).unwrap_or_default(),
        ..Queued::default()
    }
}

/// A message by its subject, or by name when it has none.
fn named(subject: &str) -> String {
    if subject.is_empty() {
        return gettext("your message");
    }
    fill(&gettext("“{subject}”"), &[("subject", subject)])
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailrs_domain::Address;

    fn draft() -> Draft {
        let mut draft = Draft::new(
            1,
            Address {
                name: None,
                email: "dana@example.com".into(),
            },
        );
        draft.to = vec![Address {
            name: None,
            email: "ann@example.com".into(),
        }];
        draft
    }

    #[test]
    fn bytes_the_composer_built_go_out_as_they_are() {
        let handed = Built::Message(b"the composer's own bytes".to_vec());
        assert_eq!(
            prepared(&draft(), Some(handed.clone()), 0, "id@example.com"),
            Ok(handed)
        );
    }

    #[test]
    fn a_message_nobody_built_is_built_here() {
        let Ok(Built::Message(raw)) = prepared(&draft(), None, 0, "id@example.com") else {
            panic!("a plain draft builds whole");
        };
        assert!(
            String::from_utf8(raw)
                .unwrap()
                .contains("To: <ann@example.com>")
        );
    }
}
