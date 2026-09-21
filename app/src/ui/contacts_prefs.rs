//! The Contacts & Calendar page of Preferences: each account's Google
//! contacts on its own switch, and whether GNOME shows its calendar.

use std::rc::Rc;

use adw::prelude::*;
use mailrs_domain::Account;
use mailrs_domain::translate::gettext;

use crate::app::App;
use crate::settings::Settings;

pub fn page(app: &Rc<App>, settings: &Settings, accounts: &[Account]) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title(gettext("Contacts & Calendar"))
        .icon_name("x-office-address-book-symbolic")
        .name("contacts")
        .build();
    page.add(&contacts(app, settings, accounts));
    page.add(&calendar(accounts));
    page
}

/// One switch per account. Each asks Google for that account's permission
/// the first time it is turned on, so an account the person never switched
/// on is never asked about.
fn contacts(app: &Rc<App>, settings: &Settings, accounts: &[Account]) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title(gettext("Google Contacts"))
        .description(gettext(
            "Penguin Mail can read an account's contacts: names, email addresses, photos, \
             organizations, and phone numbers. It uses them to suggest recipients, to show \
             faces beside mail, and to fill the card behind a sender's name. What it reads \
             stays on this computer, and turning an account off deletes its contacts.",
        ))
        .build();
    if accounts.is_empty() {
        group.add(
            &adw::ActionRow::builder()
                .title(gettext("No accounts yet"))
                .build(),
        );
    }
    for account in accounts {
        let row = adw::SwitchRow::builder()
            .title(&account.email)
            .subtitle(gettext("Google asks your permission the first time"))
            .active(settings.reads_contacts(&account.email))
            .build();
        let weak = Rc::downgrade(app);
        let email = account.email.clone();
        row.connect_active_notify(move |row| {
            if let Some(app) = weak.upgrade() {
                app.set_account_contacts(&email, row.is_active());
            }
        });
        group.add(&row);
    }
    group
}

/// Which accounts GNOME knows, since that is what puts their meetings in
/// GNOME Calendar and the clock. Answering an invitation needs nothing here:
/// Google asks for the calendar permission the first time.
fn calendar(accounts: &[Account]) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title(gettext("Calendar"))
        .description(gettext(
            "Answering a meeting invitation records your answer in that account's Google \
             Calendar, and Google asks your permission the first time. To see the meetings \
             in GNOME Calendar and the clock, add the account to GNOME Online Accounts.",
        ))
        .build();
    for account in accounts {
        let Some(known) = crate::goa::known(&account.email) else {
            // No Online Accounts on this desktop: there is nothing to add
            // the account to, so the rows would only say so.
            break;
        };
        let row = adw::ActionRow::builder().title(&account.email).build();
        if known {
            row.set_subtitle(&gettext("In GNOME Online Accounts"));
        } else {
            row.set_subtitle(&gettext("Not in GNOME Online Accounts"));
            let add = gtk::Button::builder()
                .label(gettext("Add…"))
                .valign(gtk::Align::Center)
                .build();
            crate::ui::name(
                &add,
                &mailrs_domain::translate::fill(
                    &gettext("Add {account} to GNOME Online Accounts"),
                    &[("account", &account.email)],
                ),
            );
            add.connect_clicked(|_| {
                if let Err(err) = crate::goa::open_online_accounts() {
                    tracing::warn!(error = %err, "could not open Online Accounts");
                }
            });
            row.add_suffix(&add);
        }
        group.add(&row);
    }
    group
}
