//! The window behind the engine run. `crate::protection::run` decides what
//! happens to a protected message; this file is the adapter that gives it
//! the thread on screen and makes the calls it asks for, on the GTK thread.
//! The [`Ports`] are the thread run's, from `thread.rs`.

use std::rc::Rc;

use gtk::glib;
use mailrs_domain::{AccountId, MessageBody, Target};

use super::MainWindow;
use super::thread::Ports;
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
        let ports = self.ports(view);
        Engines::new(Rc::clone(&ports) as Rc<dyn Desk>, ports as Rc<dyn Effects>)
    }
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

    /// Hands what the engine said to the thread run, which decides what
    /// else an opened body leaves stale.
    fn answered(&self, target: Target, message_id: String, read: Read) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let run = window.thread_run(&self.view);
        glib::spawn_future_local(
            async move { run.engine_answered(target, message_id, read).await },
        );
    }
}
