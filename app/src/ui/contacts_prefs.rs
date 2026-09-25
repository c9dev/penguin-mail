//! The Contacts & Calendar page of Preferences: each account's Google
//! contacts on its own switch, and whether GNOME shows its calendar.

use std::rc::Rc;

use adw::prelude::*;
use mailrs_domain::Account;
use mailrs_domain::translate::gettext;
use mailrs_sync::{Missing, Offers};

use crate::app::App;
use crate::offered::reason;
use crate::settings::{Change, Settings};

/// The page for `accounts`, each with what its server offers.
pub fn page(
    app: &Rc<App>,
    settings: &Settings,
    accounts: &[(Account, Offers)],
) -> adw::PreferencesPage {
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
/// on is never asked about. An account whose server keeps no contacts has
/// its switch off and says why.
fn contacts(
    app: &Rc<App>,
    settings: &Settings,
    accounts: &[(Account, Offers)],
) -> adw::PreferencesGroup {
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
    for (account, offers) in accounts {
        let (subtitle, offered) = contacts_row(account, *offers);
        let row = adw::SwitchRow::builder()
            .title(&account.email)
            .subtitle(subtitle)
            .active(offered && settings.reads_contacts(&account.email))
            .sensitive(offered)
            .build();
        let weak = Rc::downgrade(app);
        let email = account.email.clone();
        row.connect_active_notify(move |row| {
            if let Some(app) = weak.upgrade() {
                app.change_settings(Change::AccountContacts {
                    email: email.clone(),
                    on: row.is_active(),
                });
            }
        });
        group.add(&row);
    }
    group
}

/// Which accounts GNOME knows, since that is what puts their meetings in
/// GNOME Calendar and the clock. Answering an invitation needs nothing here:
/// Google asks for the calendar permission the first time. An account
/// whose server has no calendar gets a row that says why instead.
fn calendar(accounts: &[(Account, Offers)]) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title(gettext("Calendar"))
        .description(gettext(
            "Answering a meeting invitation records your answer in that account's Google \
             Calendar, and Google asks your permission the first time. To see the meetings \
             in GNOME Calendar and the clock, add the account to GNOME Online Accounts.",
        ))
        .build();
    let mut online_accounts = true;
    for (account, offers) in accounts {
        if let Some(lack) = calendar_lack(account, *offers) {
            group.add(
                &adw::ActionRow::builder()
                    .title(&account.email)
                    .subtitle(lack)
                    .build(),
            );
            continue;
        }
        if !online_accounts {
            continue;
        }
        let Some(known) = crate::goa::known(&account.email) else {
            // No Online Accounts on this desktop: there is nothing to add
            // the account to, so the rows would only say so.
            online_accounts = false;
            continue;
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

/// The subtitle of `account`'s contacts switch, and whether the switch
/// works: it does not on a server that keeps no contacts, and then the
/// subtitle says why.
fn contacts_row(account: &Account, offers: Offers) -> (String, bool) {
    match offers.contacts {
        true => (gettext("Google asks your permission the first time"), true),
        false => (reason(account, Missing::Contacts), false),
    }
}

/// Why `account` has no calendar, when its server has none.
fn calendar_lack(account: &Account, offers: Offers) -> Option<String> {
    (!offers.calendar).then(|| reason(account, Missing::Calendar))
}

#[cfg(test)]
mod tests {
    use mailrs_domain::{Account, AccountState, Provider};
    use mailrs_sync::{Missing, Offers};

    use super::{calendar_lack, contacts_row};
    use crate::offered::reason;

    fn account() -> Account {
        Account {
            id: 1,
            email: "me@gmail.com".into(),
            state: AccountState::Ok,
            provider: Provider::Gmail,
            provider_name: None,
        }
    }

    #[test]
    fn a_gmail_account_switches_its_contacts_as_before() {
        assert_eq!(
            contacts_row(&account(), Offers::EVERYTHING),
            ("Google asks your permission the first time".to_string(), true)
        );
        assert_eq!(calendar_lack(&account(), Offers::EVERYTHING), None);
    }

    #[test]
    fn an_account_without_contacts_or_a_calendar_says_why() {
        let bare = Offers {
            contacts: false,
            calendar: false,
            ..Offers::EVERYTHING
        };
        assert_eq!(
            contacts_row(&account(), bare),
            (reason(&account(), Missing::Contacts), false)
        );
        assert_eq!(
            calendar_lack(&account(), bare),
            Some(reason(&account(), Missing::Calendar))
        );
    }
}
