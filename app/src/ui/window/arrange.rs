//! Smart mailboxes, and arranging accounts: order, colour, and name.

use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use mailrs_domain::{Account, AccountId, Label, system_label};

use super::MainWindow;
use crate::settings::{Change, Settings};
use crate::ui::Mailbox;
use crate::ui::sidebar::Extras;
use mailrs_domain::translate::{fill, gettext};

impl MainWindow {
    /// Sorts accounts into the chosen order and gathers what the sidebar
    /// shows besides them. Also applies chosen account colours.
    pub(super) fn arrange(
        &self,
        data: Vec<(Account, Vec<Label>)>,
        settings: &Settings,
    ) -> (Vec<(Account, Vec<Label>)>, Extras) {
        let emails: Vec<&str> = data.iter().map(|(a, _)| a.email.as_str()).collect();
        let order: Vec<String> = settings
            .ordered(&emails)
            .iter()
            .map(|e| e.to_string())
            .collect();
        let mut sorted = data;
        sorted.sort_by_key(|(a, _)| order.iter().position(|e| *e == a.email));
        let find = |email: &str| {
            sorted
                .iter()
                .find(|(a, _)| a.email.eq_ignore_ascii_case(email))
                .map(|(a, _)| a.id)
        };
        let colors: HashMap<AccountId, usize> = settings
            .account_colors
            .iter()
            .filter_map(|(email, index)| Some((find(email)?, *index)))
            .collect();
        crate::format::set_account_colors(colors);
        let extras = Extras {
            vips: settings
                .vips
                .iter()
                .map(|(e, n)| (e.clone(), n.clone()))
                .collect(),
            smart: settings.smart_mailboxes.clone(),
            names: settings
                .account_names
                .iter()
                .filter_map(|(email, name)| Some((find(email)?, name.clone())))
                .collect(),
        };
        (sorted, extras)
    }

    pub(super) fn edit_smart(self: &Rc<Self>, id: Option<String>) {
        let existing = id.and_then(|id| {
            self.settings()
                .smart_mailboxes
                .into_iter()
                .find(|m| m.id == id)
        });
        let accounts: Vec<String> = self
            .accounts
            .borrow()
            .iter()
            .map(|a| a.email.clone())
            .collect();
        let weak = Rc::downgrade(self);
        super::super::smart_editor::present(&self.window, existing, accounts, move |mailbox| {
            let (Some(win), Some(app)) =
                (weak.upgrade(), weak.upgrade().and_then(|w| w.app.upgrade()))
            else {
                return;
            };
            let shown = Mailbox::Smart(mailbox.clone());
            app.change_settings(Change::SaveSmartMailbox(Box::new(mailbox)));
            win.sidebar.select(&shown);
            win.show_mailbox(shown);
        });
    }

    pub(super) fn delete_smart(self: &Rc<Self>, id: String) {
        let Some(name) = self
            .settings()
            .smart_mailboxes
            .iter()
            .find(|m| m.id == id)
            .map(|m| m.name.clone())
        else {
            return;
        };
        let dialog = adw::AlertDialog::new(
            Some(&fill(&gettext("Delete “{name}”?"), &[("name", &name)])),
            Some(&gettext(
                "Only the smart mailbox goes. The mail it shows stays where it is.",
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
            if let Some(app) = this.app.upgrade() {
                app.change_settings(Change::DeleteSmartMailbox(id));
            }
            let inbox = Mailbox::Unified(system_label::INBOX);
            this.sidebar.select(&inbox);
            this.show_mailbox(inbox);
        });
    }

    pub(super) fn rename_account(self: &Rc<Self>, account: Account) {
        let current = self
            .settings()
            .account_names
            .get(&account.email)
            .cloned()
            .unwrap_or_default();
        let entry = gtk::Entry::builder()
            .text(&current)
            .placeholder_text(&account.email)
            .activates_default(true)
            .build();
        entry.connect_map(|entry| {
            let entry = entry.clone();
            glib::idle_add_local_once(move || {
                entry.grab_focus();
            });
        });
        let dialog = adw::AlertDialog::builder()
            .heading(gettext("Name This Account"))
            .body(gettext(
                "The sidebar shows the name instead of the address. Leave it empty for \
                 the address.",
            ))
            .extra_child(&entry)
            .build();
        dialog.add_responses(&[("cancel", &gettext("Cancel")), ("save", &gettext("Save"))]);
        dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("save"));
        dialog.set_close_response("cancel");
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            if dialog.choose_future(Some(&this.window)).await != "save" {
                return;
            }
            let name = entry.text().trim().to_string();
            if let Some(app) = this.app.upgrade() {
                app.change_settings(Change::AccountName {
                    email: account.email.clone(),
                    name,
                });
            }
        });
    }

    pub(super) fn move_account(self: &Rc<Self>, account: Account, step: isize) {
        let emails: Vec<String> = self
            .accounts
            .borrow()
            .iter()
            .map(|a| a.email.clone())
            .collect();
        if let Some(app) = self.app.upgrade() {
            app.change_settings(Change::MoveAccount {
                emails,
                email: account.email.clone(),
                step,
            });
        }
    }

    /// The actions behind the smart mailbox and account menus.
    pub(super) fn install_arrange_actions(self: &Rc<Self>) {
        let with_id = |name: &str, run: fn(&Rc<MainWindow>, String)| {
            let action = gio::SimpleAction::new(name, Some(glib::VariantTy::STRING));
            let weak = Rc::downgrade(self);
            action.connect_activate(move |_, parameter| {
                if let (Some(win), Some(id)) =
                    (weak.upgrade(), parameter.and_then(|p| p.get::<String>()))
                {
                    run(&win, id);
                }
            });
            self.actions.add_action(&action);
        };
        with_id("smart-edit", |win, id| win.edit_smart(Some(id)));
        with_id("vip-remove", |win, email| {
            if let Some(app) = win.app.upgrade() {
                app.change_settings(Change::SetVip {
                    email,
                    name: String::new(),
                    add: false,
                });
            }
        });
        with_id("smart-delete", |win, id| win.delete_smart(id));
        with_id("smart-up", |win, id| {
            if let Some(app) = win.app.upgrade() {
                app.change_settings(Change::MoveSmartMailbox { id, step: -1 });
            }
        });
        with_id("smart-down", |win, id| {
            if let Some(app) = win.app.upgrade() {
                app.change_settings(Change::MoveSmartMailbox { id, step: 1 });
            }
        });
        let new = gio::SimpleAction::new("smart-new", None);
        let weak = Rc::downgrade(self);
        new.connect_activate(move |_, _| {
            if let Some(win) = weak.upgrade() {
                win.edit_smart(None);
            }
        });
        self.actions.add_action(&new);

        let color = gio::SimpleAction::new(
            "account-color",
            Some(&glib::VariantType::new("(xi)").expect("valid type")),
        );
        let weak = Rc::downgrade(self);
        color.connect_activate(move |_, parameter| {
            let (Some(win), Some((id, index))) = (
                weak.upgrade(),
                parameter.and_then(|p| p.get::<(i64, i32)>()),
            ) else {
                return;
            };
            let (Some(account), Some(app)) = (win.account(id), win.app.upgrade()) else {
                return;
            };
            app.change_settings(Change::AccountColor {
                email: account.email.clone(),
                index: index.max(0) as usize,
            });
        });
        self.actions.add_action(&color);

        let label_color = gio::SimpleAction::new(
            "label-color",
            Some(&glib::VariantType::new("(xsi)").expect("valid type")),
        );
        let weak = Rc::downgrade(self);
        label_color.connect_activate(move |_, parameter| {
            if let (Some(win), Some((account, label, index))) = (
                weak.upgrade(),
                parameter.and_then(|p| p.get::<(i64, String, i32)>()),
            ) {
                win.color_label(account, label, index.max(0) as usize);
            }
        });
        self.actions.add_action(&label_color);
    }
}
