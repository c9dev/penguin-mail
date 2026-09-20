//! Running the engine the message on screen needs, OpenPGP or S/MIME.
//!
//! The bytes come from Gmail's `format=raw`, because a signature covers the
//! message as it was sent and the parts the API hands back have been
//! decoded since. The engine then runs on a thread of its own, since it may
//! put a pinentry in front of the person and wait as long as they take to
//! type.

use std::rc::Rc;

use gtk::glib;

use super::MainWindow;
use crate::smime::Engine;
use crate::ui::conversation::ConversationView;
use crate::{pgp, smime};

impl MainWindow {
    /// Starts the engine on the message `view` shows, without holding up
    /// whatever the caller does next. gpg can sit on a pinentry for as long
    /// as the person takes to type, and the rest of opening a thread has no
    /// reason to wait for that.
    pub(super) fn start_pgp(self: &Rc<Self>, view: &Rc<ConversationView>) {
        let (this, view) = (Rc::clone(self), Rc::clone(view));
        glib::spawn_future_local(async move { this.refresh_pgp(&view).await });
    }

    /// Checks or opens the protected message in `view` and puts what the
    /// engine said above it. A thread that has been through this keeps the
    /// answer, so redrawing never asks again.
    async fn refresh_pgp(self: &Rc<Self>, view: &Rc<ConversationView>) {
        if !self.core.has_gpg() && !self.core.has_gpgsm() {
            return;
        }
        let found = view.with_open(|open| {
            if open.pgp_asked {
                return None;
            }
            let (message_id, opening) = {
                let (meta, opening) = open.protected()?;
                (meta.id.clone(), opening)
            };
            // The message names its standard, and the engine that reads it
            // may be the one this computer lacks.
            match opening {
                Engine::Pgp(_) if !self.core.has_gpg() => return None,
                Engine::Smime(_) if !self.core.has_gpgsm() => return None,
                _ => {}
            }
            let body = open.bodies.get(&message_id)?.as_ref().ok()?.clone();
            open.pgp_asked = true;
            Some((
                open.account_id,
                open.thread_id.clone(),
                message_id,
                opening,
                body,
            ))
        });
        let Some(Some((account_id, thread_id, message_id, opening, body))) = found else {
            return;
        };
        let Some(sync) = self.core.account(account_id) else {
            return;
        };
        let key = message_id.clone();
        let raw = match self
            .core
            .call(async move { sync.raw_message(&key).await })
            .await
        {
            Ok(raw) => raw,
            Err(err) => {
                tracing::info!(error = %err, "could not fetch the message to check how it was signed");
                return;
            }
        };
        if !view.is_showing(account_id, &thread_id) {
            return;
        }
        let read = match opening {
            Engine::Pgp(opening) => {
                self.core
                    .gpg(move |pgp| Ok(pgp::read(pgp, opening, &raw, &body)))
                    .await
            }
            Engine::Smime(opening) => {
                self.core
                    .gpgsm(move |smime| Ok(smime::read(smime, opening, &raw)))
                    .await
            }
        };
        let read = match read {
            Ok(read) => read,
            Err(err) => {
                tracing::info!(error = %err, "the engine could not be asked about this message");
                return;
            }
        };
        if !view.is_showing(account_id, &thread_id) {
            return;
        }
        view.with_open(|open| {
            open.pgp = Some(read.mark);
            // What was inside the encryption is what the reader wanted, and
            // it goes no further than this window: the store keeps the
            // message as Gmail holds it, ciphertext and all.
            if let Some(body) = read.body {
                open.bodies.insert(message_id, Ok(body));
            }
        });
        view.render(false);
    }
}
