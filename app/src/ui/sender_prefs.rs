//! Addresses and suffixes sent through an account's existing SMTP server.

mod signature;

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk::glib;
use mailrs_domain::translate::{fill, gettext};
use mailrs_domain::{Account, MailRoute, Provider};

use crate::app::App;
use crate::compose::{SendAsAddress, Suffixes};
use crate::settings::Change;

/// Whether the account's addresses can be edited here. A Gmail account
/// that reads its mail over IMAP can: Gmail sends only from the ones the
/// person added in its own settings, which the line under the row says.
pub fn is_editable(provider: Provider, route: Option<MailRoute>) -> bool {
    matches!(provider, Provider::Imap | Provider::Pop3) || is_gmail_over_imap(provider, route)
}

/// Whether `provider`'s account on `route` is a Gmail account that reads
/// its mail over IMAP.
pub fn is_gmail_over_imap(provider: Provider, route: Option<MailRoute>) -> bool {
    provider == Provider::Gmail && route == Some(MailRoute::Imap)
}

/// The line under the addresses of a Gmail account on the IMAP route.
fn send_as_line() -> String {
    // Translators: Gmail is a name; use the words Gmail's own settings show in your language for
    // Send mail as.
    gettext("Gmail sends only from addresses you added in Gmail's Send mail as setting.")
}

pub fn account_row(app: &Rc<App>, account: &Account, route: Option<MailRoute>) -> adw::ExpanderRow {
    let row = crate::ui::disclosure::row(
        adw::ExpanderRow::builder()
            .use_markup(false)
            .title(gettext("Addresses You Send From"))
            .build(),
    );
    let editable = is_editable(account.provider, route);
    let list = Rc::new(List {
        app: Rc::downgrade(app),
        group: row.downgrade(),
        account: account.email.clone(),
        editable,
        rows: RefCell::new(Vec::new()),
    });
    list.fill();
    if is_gmail_over_imap(account.provider, route) {
        let line = adw::ActionRow::builder()
            .use_markup(false)
            .title(send_as_line())
            .title_lines(0)
            .css_classes(["dim-label"])
            .build();
        let content = adw::ButtonContent::builder()
            .icon_name("adw-external-link-symbolic")
            .label(crate::offered::settings_link_label())
            .build();
        let link = gtk::LinkButton::builder()
            .uri(crate::gmail_password::SEND_AS_URL)
            .valign(gtk::Align::Center)
            .build();
        link.set_child(Some(&content));
        crate::ui::name(&link, &crate::offered::settings_link_label());
        line.add_suffix(&link);
        row.add_row(&line);
    }
    if editable {
        let add = gtk::Button::builder()
            .label(gettext("Add Address"))
            .valign(gtk::Align::Center)
            .build();
        row.add_suffix(&add);
        add.connect_clicked(move |_| list.edit(None));
    }
    row
}

struct List {
    app: Weak<App>,
    group: glib::WeakRef<adw::ExpanderRow>,
    account: String,
    editable: bool,
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
        let senders = app.settings().senders(&self.account);
        group.set_subtitle(
            &senders
                .iter()
                .map(|sender| sender.email.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        );
        for sender in senders {
            let row = adw::ActionRow::builder()
                .title(&sender.email)
                .subtitle(if sender.default {
                    gettext("Default sender")
                } else {
                    sender.name.clone().unwrap_or_default()
                })
                .use_markup(false)
                .activatable(self.editable)
                .build();
            if !sender.email.eq_ignore_ascii_case(&self.account) {
                let button = gtk::Button::builder()
                    .label(gettext("Signature…"))
                    .valign(gtk::Align::Center)
                    .build();
                crate::ui::name(
                    &button,
                    &fill(
                        &gettext("Signature for {account}"),
                        &[("account", &sender.email)],
                    ),
                );
                let (app, account, email) = (
                    Rc::downgrade(&app),
                    self.account.clone(),
                    sender.email.clone(),
                );
                button.connect_clicked(move |button| {
                    if let Some(app) = app.upgrade() {
                        signature::show(&app, button, &account, &email);
                    }
                });
                row.add_suffix(&button);
            }
            let weak = Rc::downgrade(self);
            row.connect_activated(move |_| {
                if let Some(list) = weak.upgrade().filter(|list| list.editable) {
                    list.edit(Some(sender.clone()));
                }
            });
            if self.editable {
                row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
            }
            group.add_row(&row);
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

#[cfg(test)]
mod tests {
    use mailrs_domain::{MailRoute, Provider};

    use super::{is_editable, is_gmail_over_imap};

    #[test]
    fn the_send_as_line_shows_only_on_the_imap_route() {
        assert!(is_gmail_over_imap(Provider::Gmail, Some(MailRoute::Imap)));
        assert!(!is_gmail_over_imap(
            Provider::Gmail,
            Some(MailRoute::GmailApi)
        ));
        assert!(!is_gmail_over_imap(Provider::Imap, Some(MailRoute::Imap)));
        assert!(!is_gmail_over_imap(Provider::Gmail, None));
    }

    #[test]
    fn a_gmail_account_is_editable_on_the_imap_route_alone() {
        assert!(is_editable(Provider::Gmail, Some(MailRoute::Imap)));
        assert!(!is_editable(Provider::Gmail, Some(MailRoute::GmailApi)));
        assert!(is_editable(Provider::Pop3, None));
        assert!(!is_editable(Provider::Microsoft, None));
    }
}
