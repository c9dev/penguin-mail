//! The window behind the engine run. `crate::protection::run` decides what
//! happens to a protected message; this file is the adapter that gives it
//! the thread on screen and makes the calls it asks for, on the GTK thread.

use std::rc::{Rc, Weak};

use gtk::glib;
use mailrs_domain::{AccountId, MessageBody};

use super::MainWindow;
use crate::core::Core;
use crate::protection::run::{Answer, Claimed, Desk, Effects, Engines, Installed};
use crate::protection::{Engine, Read};
use crate::ui::conversation::ConversationView;
use crate::{pgp, smime};

impl MainWindow {
    /// Starts the engine on the message `view` shows, without holding up
    /// whatever the caller does next. gpg can sit on a pinentry for as long
    /// as the person takes to type, and the rest of opening a thread has no
    /// reason to wait for that.
    pub(super) fn start_pgp(self: &Rc<Self>, view: &Rc<ConversationView>) {
        let engines = self.engines(view);
        glib::spawn_future_local(async move { engines.run().await });
    }

    /// The engines, with this window behind both ports.
    fn engines(self: &Rc<Self>, view: &Rc<ConversationView>) -> Engines {
        let ports = Rc::new(Ports {
            window: Rc::downgrade(self),
            core: Rc::clone(&self.core),
            view: Rc::clone(view),
        });
        Engines::new(Rc::clone(&ports) as Rc<dyn Desk>, ports as Rc<dyn Effects>)
    }
}

/// The window as the engine run sees it.
struct Ports {
    window: Weak<MainWindow>,
    core: Rc<Core>,
    view: Rc<ConversationView>,
}

impl Desk for Ports {
    fn installed(&self) -> Installed {
        Installed {
            pgp: self.core.has_gpg(),
            smime: self.core.has_gpgsm(),
        }
    }

    fn claim(&self, installed: Installed) -> Option<Claimed> {
        self.view.take_protected(installed)
    }

    fn is_showing(&self, account_id: AccountId, thread_id: &str) -> bool {
        self.view.is_showing(account_id, thread_id)
    }
}

impl Effects for Ports {
    fn raw_message(
        &self,
        account_id: AccountId,
        message_id: String,
    ) -> Answer<'_, Result<Vec<u8>, String>> {
        Box::pin(async move {
            let sync = self
                .core
                .account(account_id)
                .ok_or_else(|| "the account has stopped syncing".to_string())?;
            self.core
                .call(async move { sync.raw_message(&message_id).await })
                .await
                .map_err(|err| err.to_string())
        })
    }

    fn ask(
        &self,
        opening: Engine,
        raw: Vec<u8>,
        body: MessageBody,
    ) -> Answer<'_, Result<Read, String>> {
        Box::pin(async move {
            match opening {
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
            }
            .map_err(|err| err.to_string())
        })
    }

    fn answered(&self, message_id: String, read: Read) {
        if !self.view.engine_answered(message_id, read) {
            return;
        }
        // The event card and the translation card were read off the
        // ciphertext; the opened body may carry an invitation or be in
        // another language.
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let view = Rc::clone(&self.view);
        window.refresh_translation(&view);
        glib::spawn_future_local(async move { window.refresh_invitation(&view).await });
    }
}
