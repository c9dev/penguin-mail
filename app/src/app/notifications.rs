//! Desktop delivery. Both platforms use the same app actions and their
//! validation; only macOS uses UserNotifications instead of GIO.

use gtk::gio;
use serde::{Deserialize, Serialize};

use crate::event_reminders::notice as reminder;
use crate::notify;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos::Desktop;
#[cfg(not(target_os = "macos"))]
mod linux;
#[cfg(not(target_os = "macos"))]
use linux::Desktop;

pub(super) struct Notifications(Desktop);

impl Notifications {
    pub fn new(app: &gio::Application, demo: bool) -> Self {
        Self(Desktop::new(app, demo))
    }

    pub fn mail(&self, id: &str, notice: &notify::Notice) {
        self.0.post(
            id,
            &notice.title,
            &notice.body,
            Actions {
                open: notice.target.as_ref().map(|_| Action::OpenMail),
                target: id.to_string(),
                buttons: notify::Button::ALL
                    .into_iter()
                    .filter(|button| notice.buttons.contains(button))
                    .map(Action::Mail)
                    .collect(),
            },
        );
    }

    pub fn reminder(&self, notice: &reminder::Notice) {
        self.0.post(
            &notice.id,
            &notice.title,
            &notice.body,
            Actions {
                open: Some(Action::OpenEvent),
                target: notice.target.clone(),
                buttons: notice
                    .buttons
                    .iter()
                    .map(|button| match button {
                        reminder::Button::Join => Action::Join,
                        reminder::Button::Snooze => Action::Snooze,
                    })
                    .collect(),
            },
        );
    }

    pub fn withdraw(&self, id: &str) {
        self.0.withdraw(id);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Action {
    OpenMail,
    Mail(notify::Button),
    OpenEvent,
    Join,
    Snooze,
}

impl Action {
    fn label(self) -> String {
        match self {
            Self::Mail(button) => button.label(),
            Self::Join => reminder::Button::Join.label(),
            Self::Snooze => reminder::Button::Snooze.label(),
            Self::OpenMail | Self::OpenEvent => unreachable!("opening is not a button"),
        }
    }

    fn destination(self, target: &str) -> (&'static str, String) {
        match self {
            Self::OpenMail => (notify::CHOSEN, notify::parameter(target, notify::OPEN_KEY)),
            Self::Mail(button) => (notify::CHOSEN, notify::parameter(target, button.key())),
            Self::OpenEvent => (reminder::SHOW_EVENT, target.to_string()),
            Self::Join => (reminder::Button::Join.action(), target.to_string()),
            Self::Snooze => (reminder::Button::Snooze.action(), target.to_string()),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Actions {
    open: Option<Action>,
    target: String,
    buttons: Vec<Action>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mail_buttons_keep_the_issued_token_and_reminders_keep_their_key() {
        for button in notify::Button::ALL {
            assert_eq!(
                Action::Mail(button).destination("token"),
                (notify::CHOSEN, notify::parameter("token", button.key()))
            );
        }
        assert_eq!(
            Action::OpenMail.destination("token"),
            (notify::CHOSEN, "token/default".into())
        );
        assert_eq!(
            Action::Snooze.destination("event"),
            (reminder::SNOOZE_REMINDER, "event".into())
        );
    }
}
