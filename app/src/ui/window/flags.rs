//! Flags: Gmail's star, with Apple Mail's colours kept on this computer.

use std::rc::Rc;

use gtk::glib;
use mailrs_domain::FlagColor;
use mailrs_store::flags;
use mailrs_sync::TriageAction;

use super::{MainWindow, Target};

impl MainWindow {
    /// Flags the targets with `color`, or takes the flag off with `None`.
    pub(super) fn flag(self: &Rc<Self>, color: Option<FlagColor>) {
        let targets = self.targets();
        self.flag_targets(targets, color);
    }

    pub(super) fn flag_targets(self: &Rc<Self>, targets: Vec<Target>, color: Option<FlagColor>) {
        if targets.is_empty() {
            return;
        }
        if let (Some(color), Some(app)) = (color, self.app.upgrade()) {
            app.update_settings(|s| s.flag_color = color);
        }
        let action = match color {
            Some(_) => TriageAction::Star,
            None => TriageAction::Unstar,
        };
        let message = color.map(|c| format!("Flagged {}", c.name().to_lowercase()));
        self.apply_then(
            targets,
            action,
            true,
            message,
            Some(Box::new(move |win: &Rc<MainWindow>, targets: &[Target]| {
                let targets = targets.to_vec();
                let open_flagged = win.conversation.with_open(|o| {
                    targets
                        .iter()
                        .any(|t| t.account_id == o.account_id && t.thread_id == o.thread_id)
                });
                if open_flagged == Some(true) {
                    win.conversation.with_open(|o| o.flag_color = color);
                    win.conversation.render_buttons();
                }
                win.core.spawn_write(move |c| {
                    for target in &targets {
                        flags::set_color(
                            c,
                            target.account_id,
                            &target.thread_id,
                            target.message_id.as_deref(),
                            color,
                        )?;
                    }
                    Ok(())
                });
                let win = Rc::clone(win);
                glib::timeout_add_local_once(std::time::Duration::from_millis(100), move || {
                    win.queue_refresh();
                });
            })),
        );
    }

    /// The flag button and Ctrl+Shift+L: flag in the last colour used, or
    /// take the flag off when everything is flagged already.
    pub(super) fn toggle_flag(self: &Rc<Self>) {
        let (_, all_flagged) = self.target_marks();
        if all_flagged {
            self.flag(None);
        } else {
            self.flag(Some(self.settings().flag_color));
        }
    }
}
