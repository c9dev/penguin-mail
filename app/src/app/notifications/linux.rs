use gtk::gio;
use gtk::prelude::*;

use super::{Action, Actions};

pub(super) struct Desktop(gio::Application);

impl Desktop {
    pub fn new(app: &gio::Application, _demo: bool) -> Self {
        Self(app.clone())
    }

    pub fn post(&self, id: &str, title: &str, body: &str, actions: Actions) {
        let notification = gio::Notification::new(title);
        if !body.is_empty() {
            notification.set_body(Some(body));
        }
        notification.set_icon(&gio::ThemedIcon::new(crate::APP_ID));
        if actions.open == Some(Action::OpenEvent) {
            notification.set_priority(gio::NotificationPriority::High);
        } else {
            notification.set_category(Some("email.arrived"));
        }
        if let Some(open) = actions.open {
            let (action, target) = open.destination(&actions.target);
            notification.set_default_action_and_target_value(
                &format!("app.{action}"),
                Some(&target.to_variant()),
            );
        }
        for button in actions.buttons {
            let (action, target) = button.destination(&actions.target);
            notification.add_button_with_target_value(
                &button.label(),
                &format!("app.{action}"),
                Some(&target.to_variant()),
            );
        }
        self.0.send_notification(Some(id), &notification);
    }

    pub fn withdraw(&self, id: &str) {
        self.0.withdraw_notification(id);
    }
}
