//! What the app tells the window. The app sends news here and waits for no
//! answer; each notice turns into its redraw or toast in one match, so the
//! window's interface to the app is this enum plus a few questions
//! (`is_active`) and commands (`reveal`, `ask_permission`).

use std::rc::Rc;

use mailrs_domain::ChangeEvent;
use mailrs_domain::translate::gettext;
use mailrs_sync::{MailAction, Outcome};

use super::MainWindow;
use crate::settings::Effects;
use crate::update::State;

pub enum Notice<'a> {
    /// The engine changed mail, labels, or an account.
    Engine(&'a ChangeEvent),
    /// A notification's button changed mail.
    MailChanged {
        action: &'a MailAction,
        outcome: &'a Outcome,
    },
    /// A saved settings change left these to bring back in line.
    SettingsChanged(&'a Effects),
    /// Where an update stands, for the menu, the banner and About.
    Update(&'a State),
    /// The answer to a check for updates the person asked for, when there
    /// is nothing to install.
    UpdateChecked(String),
    /// The recipient suggestions and contact photos loaded again.
    ContactsLoaded,
    /// Preferences changed which senders' images load.
    ImageSendersChanged,
    /// Google refused because its Cloud project has `service` switched
    /// off; `enable_url` is the page that turns it on.
    ApiOff {
        service: &'a str,
        enable_url: &'a str,
    },
    /// The filter that blocks remote content compiled.
    FilterReady(webkit::UserContentFilter),
    /// A message waits out its Undo Send delay of `seconds`; `undo` takes
    /// it back.
    UndoSend {
        seconds: u32,
        undo: Box<dyn Fn()>,
    },
    /// A message the person wrote went out.
    Sent,
    /// The Send Later, Outbox or Reminders lists changed.
    OutboxChanged,
    Toast(String),
}

impl MainWindow {
    pub fn notice(self: &Rc<Self>, notice: Notice<'_>) {
        match notice {
            Notice::Engine(event) => self.handle(event),
            Notice::MailChanged { action, outcome } => self.mail_changed(action, outcome),
            Notice::SettingsChanged(effects) => {
                for effect in effects.iter() {
                    self.apply_effect(effect);
                }
            }
            Notice::Update(state) => self.show_update(state),
            // The About window shows the answer under its button, so a
            // toast would only repeat it.
            Notice::UpdateChecked(text) => {
                if self.about.borrow().is_none() {
                    self.toast(&text);
                }
            }
            Notice::ContactsLoaded => self.contacts_loaded(),
            Notice::ImageSendersChanged => self.reload_image_senders(),
            Notice::ApiOff {
                service,
                enable_url,
            } => self.explain_api_off(service, enable_url),
            Notice::FilterReady(filter) => self.conversation.set_filter(filter),
            Notice::UndoSend { seconds, undo } => self.offer_undo_send(seconds, undo),
            Notice::Sent => self.toast(&gettext("Message sent")),
            Notice::OutboxChanged => self.scheduled_changed(),
            Notice::Toast(text) => self.toast(&text),
        }
    }
}
