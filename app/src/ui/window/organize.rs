//! Moving mail by drag and drop, and creating, renaming, and deleting labels.

use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use mailrs_domain::{AccountId, LabelKind, system_label};
use mailrs_sync::{History, MailAction, TriageAction};

use super::{MainWindow, Target};
use crate::ui::Mailbox;
use crate::ui::moving::move_action;
use mailrs_domain::translate::{fill, gettext};

/// What to do with a label once it exists, given its id.
pub(super) type AfterCreate = Box<dyn Fn(&Rc<MainWindow>, String)>;

impl MainWindow {
    /// Moves the dragged rows into `mailbox`. True when the drop was taken.
    pub(super) fn drop_on(self: &Rc<Self>, mailbox: Mailbox) -> bool {
        let rows = self.list.dragged();
        if rows.is_empty() {
            return false;
        }
        if let Mailbox::Label { account_id, .. } = &mailbox
            && rows.iter().any(|r| r.account_id != *account_id)
        {
            self.toast(&gettext("Drop mail on its own account's mailboxes"));
            return false;
        }
        if let Mailbox::Flag(color) = mailbox {
            self.flag_targets(rows.iter().map(Target::from_row).collect(), Some(color));
            return true;
        }
        let from = self.mailbox.borrow().clone();
        let action = match move_action(&from, &mailbox) {
            Ok(action) => action,
            Err(reason) => {
                self.toast(&reason);
                return false;
            }
        };
        let targets: Vec<Target> = rows.iter().map(Target::from_row).collect();
        let open_moved = self.conversation.read(|o| o.among(&targets)) == Some(true);
        if open_moved && !matches!(action, TriageAction::Star) {
            self.move_on(&self.conversation);
        }
        let message = match action {
            TriageAction::AddLabel(_)
            | TriageAction::RemoveLabel(_)
            | TriageAction::Relabel { .. } => Some(fill(
                &gettext("Moved to {mailbox}"),
                &[("mailbox", &mailbox.title())],
            )),
            _ => None,
        };
        self.perform(
            targets,
            MailAction::Triage(action),
            History::Record,
            message,
        );
        true
    }

    /// Asks for a name and creates a label in `account_id`. With `then`,
    /// runs it on the new label's id, as the label menu does to apply it.
    pub(super) fn new_label(self: &Rc<Self>, account_id: AccountId, then: Option<AfterCreate>) {
        let entry = gtk::Entry::builder()
            .placeholder_text(gettext("Name, or Parent/Name to nest it"))
            .activates_default(true)
            .build();
        focus_when_shown(&entry);
        let dialog = adw::AlertDialog::builder()
            .heading(gettext("New Label"))
            .extra_child(&entry)
            .build();
        dialog.add_responses(&[
            ("cancel", &gettext("Cancel")),
            ("create", &gettext("Create")),
        ]);
        dialog.set_response_appearance("create", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("create"));
        dialog.set_close_response("cancel");
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            if dialog.choose_future(Some(&this.window)).await != "create" {
                return;
            }
            let name = entry.text().trim().to_string();
            if name.is_empty() {
                return;
            }
            let Some(sync) = this.core.account(account_id) else {
                return this.toast(&gettext("That account is not connected"));
            };
            let wanted = name.clone();
            match this
                .core
                .call(async move { sync.create_label(&wanted).await })
                .await
            {
                Ok(label) => match then {
                    Some(then) => then(&this, label.id),
                    None => this.toast(&fill(&gettext("Created “{name}”"), &[("name", &name)])),
                },
                Err(err) => this.toast(&fill(
                    &gettext("Could not create the label: {reason}"),
                    &[("reason", &err.to_string())],
                )),
            }
        });
    }

    pub(super) fn rename_label(self: &Rc<Self>, account_id: AccountId, label_id: String) {
        let Some(current) = self.label_name(account_id, &label_id) else {
            return;
        };
        let entry = gtk::Entry::builder()
            .text(&current)
            .activates_default(true)
            .build();
        focus_when_shown(&entry);
        let dialog = adw::AlertDialog::builder()
            .heading(gettext("Rename Label"))
            .body(gettext("Labels nested under it move along."))
            .extra_child(&entry)
            .build();
        dialog.add_responses(&[
            ("cancel", &gettext("Cancel")),
            ("rename", &gettext("Rename")),
        ]);
        dialog.set_response_appearance("rename", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("rename"));
        dialog.set_close_response("cancel");
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            if dialog.choose_future(Some(&this.window)).await != "rename" {
                return;
            }
            let name = entry.text().trim().to_string();
            if name.is_empty() || name == current {
                return;
            }
            let Some(sync) = this.core.account(account_id) else {
                return this.toast(&gettext("That account is not connected"));
            };
            let wanted = name.clone();
            if let Err(err) = this
                .core
                .call(async move { sync.rename_label(&label_id, &wanted).await })
                .await
            {
                this.toast(&fill(
                    &gettext("Could not rename the label: {reason}"),
                    &[("reason", &err.to_string())],
                ));
            }
        });
    }

    pub(super) fn delete_label(self: &Rc<Self>, account_id: AccountId, label_id: String) {
        let Some(name) = self.label_name(account_id, &label_id) else {
            return;
        };
        let dialog = adw::AlertDialog::new(
            Some(&fill(&gettext("Delete “{name}”?"), &[("name", &name)])),
            Some(&gettext(
                "Its mail stays in Gmail, without the label. Nested labels stay too.",
            )),
        );
        dialog.add_responses(&[
            ("cancel", &gettext("Cancel")),
            ("delete", &gettext("Delete")),
        ]);
        dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        dialog.set_close_response("cancel");
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            if dialog.choose_future(Some(&this.window)).await != "delete" {
                return;
            }
            let Some(sync) = this.core.account(account_id) else {
                return this.toast(&gettext("That account is not connected"));
            };
            let showing = matches!(
                &*this.mailbox.borrow(),
                Mailbox::Label { account_id: a, label_id: l, .. } if *a == account_id && *l == label_id
            );
            match this
                .core
                .call(async move { sync.delete_label(&label_id).await })
                .await
            {
                Ok(()) => {
                    if showing {
                        let inbox = Mailbox::Unified(system_label::INBOX);
                        this.sidebar.select(&inbox);
                        this.show_mailbox(inbox);
                    }
                    this.toast(&fill(&gettext("Deleted “{name}”"), &[("name", &name)]));
                }
                Err(err) => this.toast(&fill(
                    &gettext("Could not delete the label: {reason}"),
                    &[("reason", &err.to_string())],
                )),
            }
        });
    }

    /// Gives a label colour `index` of Gmail's palette.
    pub(super) fn color_label(
        self: &Rc<Self>,
        account_id: AccountId,
        label_id: String,
        index: usize,
    ) {
        let Some((background, text)) = crate::ui::LABEL_COLORS.get(index) else {
            return;
        };
        let Some(sync) = self.core.account(account_id) else {
            return self.toast(&gettext("That account is not connected"));
        };
        let color = mailrs_gmail::LabelColor {
            background_color: background.to_string(),
            text_color: text.to_string(),
        };
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            if let Err(err) = this
                .core
                .call(async move { sync.set_label_color(&label_id, color).await })
                .await
            {
                this.toast(&fill(
                    &gettext("Could not change the colour: {reason}"),
                    &[("reason", &err.to_string())],
                ));
            }
        });
    }

    fn label_name(&self, account_id: AccountId, label_id: &str) -> Option<String> {
        self.labels
            .borrow()
            .get(&account_id)?
            .iter()
            .find(|l| l.id == label_id && l.kind == LabelKind::User)
            .map(|l| l.name.clone())
    }
}

/// Puts the cursor in `entry` once its dialog is on screen; dialogs
/// otherwise focus their default button.
fn focus_when_shown(entry: &gtk::Entry) {
    entry.connect_map(|entry| {
        let entry = entry.clone();
        glib::idle_add_local_once(move || {
            entry.grab_focus();
        });
    });
}
