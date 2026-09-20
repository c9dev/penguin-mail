//! Running the OpenPGP engine over the message on screen.
//!
//! The bytes come from Gmail's `format=raw`, because a signature covers the
//! message as it was sent and the parts the API hands back have been
//! decoded since. gpg then runs on a thread of its own, since it may put a
//! pinentry in front of the person and wait as long as they take to type.

use std::rc::Rc;

use super::MainWindow;
use crate::pgp;
use crate::ui::conversation::ConversationView;

impl MainWindow {
    /// Checks or opens the protected message in `view` and puts what gpg
    /// said above it. A thread that has been through this keeps the answer,
    /// so redrawing never asks again.
    pub(super) async fn refresh_pgp(self: &Rc<Self>, view: &Rc<ConversationView>) {
        if !self.core.has_gpg() {
            return;
        }
        let found = view.with_open(|open| {
            if open.pgp.is_some() {
                return None;
            }
            let (message_id, opening) = {
                let (meta, opening) = open.protected()?;
                (meta.id.clone(), opening)
            };
            let body = open.bodies.get(&message_id)?.as_ref().ok()?.clone();
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
                tracing::info!(error = %err, "could not fetch the message OpenPGP protected");
                return;
            }
        };
        if !view.is_showing(account_id, &thread_id) {
            return;
        }
        let read = self
            .core
            .gpg(move |pgp| Ok(pgp::read(pgp, opening, &raw, &body)))
            .await;
        let read = match read {
            Ok(read) => read,
            Err(err) => {
                tracing::info!(error = %err, "gpg could not be asked about this message");
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
                open.bodies.insert(message_id.clone(), Ok(body));
            }
        });
        view.render(false);
    }
}
