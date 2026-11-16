//! Consent to trash matching Sent messages belongs to one smart mailbox.
use adw::prelude::*;
use mailrs_domain::Target;
use mailrs_domain::translate::{fill, gettext};
use mailrs_sync::MovedFrom;

use super::Pressing;
use crate::settings::Change;
use crate::ui::Mailbox;
use crate::wanted::{Screen, Wanted};

impl Screen<Vec<Target>> for Pressing {
    fn is_showing(&self, targets: &Vec<Target>) -> bool {
        let parent = self
            .view
            .window()
            .unwrap_or_else(|| self.window.window.clone().upcast());
        parent.is_visible()
            && self.window.mailbox_of(&self.view) == self.mailbox
            && self.window.reach(&self.view).targets == *targets
    }
}

impl Pressing {
    pub(super) async fn prepare_smart_trash(
        &self,
        targets: Vec<Target>,
        from: MovedFrom,
    ) -> Option<MovedFrom> {
        let Mailbox::Smart(smart) = &self.mailbox else {
            return Some(from);
        };
        if targets.iter().all(|t| t.message_id.is_some()) {
            return Some(from);
        }
        let wanted = Wanted::new(self, self, self.window.reach(&self.view).targets);
        if !wanted.is_wanted() {
            return None;
        }
        let actions = self.window.core.actions();
        let query = smart.clone();
        let found = wanted
            .wait(|this| {
                Box::pin(
                    this.window
                        .core
                        .call(async move { actions.sent_in_smart(&targets, &query).await }),
                )
            })
            .await?;
        let sent = match found {
            Ok(sent) => sent,
            Err(err) => {
                wanted.on_screen(|this| {
                    this.window
                        .failed(&gettext("Could not check sent messages: {reason}"), &err)
                });
                return None;
            }
        };
        if sent.is_empty() {
            return Some(from);
        }
        let saved = self
            .window
            .settings_with(|s| s.smart_mailboxes.iter().find(|m| m.id == smart.id).cloned())?;
        if !saved.trash_sent_without_asking {
            let remember =
                gtk::CheckButton::with_label(&gettext("Don't Ask Again for This Smart Mailbox"));
            let dialog = adw::AlertDialog::new(
                Some(&gettext("Also Move Sent Messages to Trash?")),
                Some(&fill(
                    &gettext(
                        "Sent messages keep these conversations in “{mailbox}”. Move the matching sent messages to Trash too? You can undo this action.",
                    ),
                    &[("mailbox", &smart.name)],
                )),
            );
            dialog.set_extra_child(Some(&remember));
            dialog.add_responses(&[
                ("cancel", &gettext("Cancel")),
                ("trash", &gettext("Move to Trash")),
            ]);
            dialog.set_response_appearance("trash", adw::ResponseAppearance::Destructive);
            dialog.set_default_response(Some("cancel"));
            dialog.set_close_response("cancel");
            let answer = wanted
                .wait(|this| {
                    Box::pin(async move {
                        let parent = this
                            .view
                            .window()
                            .unwrap_or_else(|| this.window.window.clone().upcast());
                        dialog.choose_future(Some(&parent)).await
                    })
                })
                .await?;
            if answer != "trash" {
                return None;
            }
            if remember.is_active() {
                wanted.on_screen(|this| {
                    if let Some(app) = this.window.app.upgrade()
                        && let Some(mut mailbox) = app.settings_with(|s| {
                            s.smart_mailboxes.iter().find(|m| m.id == smart.id).cloned()
                        })
                    {
                        mailbox.trash_sent_without_asking = true;
                        app.change_settings(Change::SaveSmartMailbox(Box::new(mailbox)));
                    }
                });
            }
        }
        Some(from.including_sent(sent))
    }
}
