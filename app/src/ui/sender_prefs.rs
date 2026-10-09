//! Addresses and suffixes sent through an account's existing SMTP server.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk::glib;
use mailrs_domain::translate::gettext;
use mailrs_domain::{Account, Provider};

use crate::app::App;
use crate::compose::{SendAsAddress, Suffixes};
use crate::settings::Change;

pub fn groups(app: &Rc<App>, accounts: &[Account], page: &adw::PreferencesPage) {
    for account in accounts
        .iter()
        .filter(|a| matches!(a.provider, Provider::Imap | Provider::Pop3))
    {
        let group = adw::PreferencesGroup::builder()
            .title(gettext("Sender Addresses"))
            .description(glib::markup_escape_text(&account.email))
            .build();
        let add = gtk::Button::with_label(&gettext("Add Address"));
        group.set_header_suffix(Some(&add));
        let list = Rc::new(List {
            app: Rc::downgrade(app),
            group: group.downgrade(),
            account: account.email.clone(),
            rows: RefCell::new(Vec::new()),
        });
        list.fill();
        add.connect_clicked(move |_| list.edit(None));
        page.add(&group);
    }
}

struct List {
    app: Weak<App>,
    group: glib::WeakRef<adw::PreferencesGroup>,
    account: String,
    rows: RefCell<Vec<adw::ActionRow>>,
}

impl List {
    fn fill(self: &Rc<Self>) {
        let (Some(app), Some(group)) = (self.app.upgrade(), self.group.upgrade()) else {
            return;
        };
        for row in self.rows.take() {
            group.remove(&row);
        }
        for sender in app.settings().senders(&self.account) {
            let row = adw::ActionRow::builder()
                .title(&sender.email)
                .subtitle(if sender.default {
                    gettext("Default sender")
                } else {
                    sender.name.clone().unwrap_or_default()
                })
                .use_markup(false)
                .activatable(true)
                .build();
            let weak = Rc::downgrade(self);
            row.connect_activated(move |_| {
                if let Some(list) = weak.upgrade() {
                    list.edit(Some(sender.clone()));
                }
            });
            row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
            group.add(&row);
            self.rows.borrow_mut().push(row);
        }
    }

    fn edit(self: &Rc<Self>, saved: Option<SendAsAddress>) {
        let Some(parent) = self.group.upgrade() else {
            return;
        };
        let dialog = adw::AlertDialog::builder()
            .heading(gettext("Sender Address"))
            .body(gettext("Uses this account's outgoing server. Replies use the address the message was sent to."))
            .build();
        let fields = adw::PreferencesGroup::new();
        let sender = saved.clone().unwrap_or_default();
        let own = sender.email.eq_ignore_ascii_case(&self.account);
        let email = adw::EntryRow::builder()
            .title(gettext("Email Address"))
            .text(&sender.email)
            .editable(!own)
            .build();
        let name = adw::EntryRow::builder()
            .title(gettext("Sender Name"))
            .text(sender.name.as_deref().unwrap_or(""))
            .build();
        let default = adw::SwitchRow::builder()
            .title(gettext("Default sender"))
            .active(sender.default)
            .build();
        let plus = adw::SwitchRow::builder()
            .title(gettext("Allow +suffix"))
            .subtitle(gettext("For example, user+shop@example.com"))
            .active(sender.suffixes.plus)
            .build();
        let dot = adw::SwitchRow::builder()
            .title(gettext("Allow .suffix"))
            .subtitle(gettext("For example, user.shop@example.com"))
            .active(sender.suffixes.dot)
            .build();
        for row in [
            email.upcast_ref::<gtk::Widget>(),
            name.upcast_ref(),
            default.upcast_ref(),
            plus.upcast_ref(),
            dot.upcast_ref(),
        ] {
            fields.add(row);
        }
        let error = gtk::Label::builder()
            .wrap(true)
            .visible(false)
            .label(gettext("Enter a valid address that is not already listed."))
            .css_classes(["error"])
            .build();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content.append(&fields);
        content.append(&error);
        dialog.set_extra_child(Some(&content));
        dialog.add_responses(&[("cancel", &gettext("Cancel")), ("save", &gettext("Save"))]);
        dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("save"));
        dialog.set_close_response("cancel");
        if saved.is_some() && !own {
            dialog.add_response("remove", &gettext("Remove"));
            dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
        }
        let read: Rc<dyn Fn() -> SendAsAddress> = {
            let (email, name, plus, dot, default) = (
                email.downgrade(),
                name.downgrade(),
                plus.downgrade(),
                dot.downgrade(),
                default.downgrade(),
            );
            Rc::new(move || {
                let (Some(email), Some(name), Some(plus), Some(dot), Some(default)) = (
                    email.upgrade(),
                    name.upgrade(),
                    plus.upgrade(),
                    dot.upgrade(),
                    default.upgrade(),
                ) else {
                    return SendAsAddress::default();
                };
                SendAsAddress {
                    email: email.text().trim().to_string(),
                    name: Some(name.text().to_string()),
                    suffixes: Suffixes {
                        plus: plus.is_active(),
                        dot: dot.is_active(),
                    },
                    default: default.is_active(),
                    signature: sender.signature.clone(),
                }
            })
        };
        let validate: Rc<dyn Fn()> = {
            let (weak, dialog, read, saved) = (
                Rc::downgrade(self),
                dialog.downgrade(),
                Rc::clone(&read),
                saved.clone(),
            );
            Rc::new(move || {
                let (Some(list), Some(dialog)) = (weak.upgrade(), dialog.upgrade()) else {
                    return;
                };
                let Some(app) = list.app.upgrade() else {
                    return;
                };
                let valid = app.settings().save_sender(
                    &list.account,
                    saved.as_ref().map(|s| s.email.as_str()),
                    read(),
                );
                dialog.set_response_enabled("save", valid);
                error.set_visible(!valid);
            })
        };
        validate();
        for entry in [&email, &name] {
            let validate = Rc::clone(&validate);
            entry.connect_changed(move |_| validate());
        }
        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |_, response| {
            let Some(list) = weak.upgrade() else { return };
            let Some(app) = list.app.upgrade() else {
                return;
            };
            let was = saved.as_ref().map(|s| s.email.clone());
            match response {
                "save" => {
                    app.change_settings(Change::SaveSender {
                        account: list.account.clone(),
                        was,
                        sender: read(),
                    });
                }
                "remove" => {
                    if let Some(email) = was {
                        app.change_settings(Change::RemoveSender {
                            account: list.account.clone(),
                            email,
                        });
                    }
                }
                _ => return,
            }
            list.fill();
        });
        dialog.present(Some(&parent));
    }
}
