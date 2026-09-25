//! The Contacts & Calendar page of Preferences: each account's Google
//! contacts on its own switch, and whether GNOME shows its calendar.

use std::rc::Rc;

use adw::prelude::*;
use mailrs_domain::translate::{fill, gettext};
use mailrs_domain::{Account, AccountId};
use mailrs_sync::{Missing, Offers, Withheld};

use crate::app::App;
use crate::offered::reason;
use crate::settings::{Change, Settings};

/// The page for `accounts`, each with what its server offers and what its
/// own consent withheld. `grant` runs the consent flow again for one
/// account's Grant Access button.
pub fn page(
    app: &Rc<App>,
    settings: &Settings,
    accounts: &[(Account, Offers)],
    withheld: impl Fn(AccountId) -> Withheld,
    grant: impl Fn(AccountId) + Clone + 'static,
) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title(gettext("Contacts & Calendar"))
        .icon_name("x-office-address-book-symbolic")
        .name("contacts")
        .build();
    page.add(&contacts(app, settings, accounts, &withheld, grant.clone()));
    page.add(&calendar(accounts, &withheld, grant));
    page
}

/// What one account's contacts switch shows.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ContactsRow {
    /// A switch, on as the person set it.
    Switch,
    /// The person unticked the contacts scope on Google's consent
    /// screen, or it has never been asked: a Grant Access row.
    Withheld,
    /// The server keeps no contacts to read.
    NotOffered(String),
}

/// One switch per account. A withheld account gets a Grant Access row
/// instead of a switch, since it has nothing to switch on yet. An
/// account whose server keeps no contacts has its row say why.
fn contacts(
    app: &Rc<App>,
    settings: &Settings,
    accounts: &[(Account, Offers)],
    withheld: &impl Fn(AccountId) -> Withheld,
    grant: impl Fn(AccountId) + Clone + 'static,
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
        match contacts_row(account, *offers, withheld(account.id)) {
            ContactsRow::Withheld => {
                group.add(&grant_access_row(account, grant.clone()));
            }
            ContactsRow::NotOffered(reason) => {
                let row = adw::SwitchRow::builder()
                    .title(&account.email)
                    .subtitle(reason)
                    .active(false)
                    .sensitive(false)
                    .build();
                group.add(&row);
            }
            ContactsRow::Switch => {
                let row = adw::SwitchRow::builder()
                    .title(&account.email)
                    .active(settings.reads_contacts(&account.email))
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
        }
    }
    group
}

/// The subtitle of `account`'s contacts switch, and whether the switch
/// works: it does not on a server that keeps no contacts, and then the
/// subtitle says why.
fn contacts_row(account: &Account, offers: Offers, withheld: Withheld) -> ContactsRow {
    if !offers.contacts {
        return ContactsRow::NotOffered(reason(account, Missing::Contacts));
    }
    if withheld.contacts {
        return ContactsRow::Withheld;
    }
    ContactsRow::Switch
}

/// What one account's calendar row shows in Preferences.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CalendarRow {
    /// The account can be added to GNOME Online Accounts.
    Available,
    /// The person unticked the calendar scope, or it has never been
    /// asked: a Grant Access row.
    Withheld,
    /// The server has no calendar to read.
    NotOffered(String),
}

/// Which accounts GNOME knows, since that is what puts their meetings in
/// GNOME Calendar and the clock. Answering an invitation needs nothing
/// here: sign-in already asked for the calendar permission. An account
/// whose server has no calendar, or whose own consent withheld it, gets
/// a row that says so instead.
fn calendar(
    accounts: &[(Account, Offers)],
    withheld: &impl Fn(AccountId) -> Withheld,
    grant: impl Fn(AccountId) + Clone + 'static,
) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title(gettext("Calendar"))
        .description(gettext(
            "Answering a meeting invitation records your answer in that account's Google \
             Calendar. To see the meetings in GNOME Calendar and the clock, add the account \
             to GNOME Online Accounts.",
        ))
        .build();
    let mut online_accounts = true;
    for (account, offers) in accounts {
        match calendar_lack(account, *offers, withheld(account.id)) {
            CalendarRow::NotOffered(lack) => {
                group.add(
                    &adw::ActionRow::builder()
                        .title(&account.email)
                        .subtitle(lack)
                        .build(),
                );
                continue;
            }
            CalendarRow::Withheld => {
                group.add(&grant_access_row(account, grant.clone()));
                continue;
            }
            CalendarRow::Available => {}
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
                &fill(
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

/// Why `account`'s calendar row is not the Online Accounts row: the
/// server has none, or the person's own consent withheld it.
fn calendar_lack(account: &Account, offers: Offers, withheld: Withheld) -> CalendarRow {
    if !offers.calendar {
        return CalendarRow::NotOffered(reason(account, Missing::Calendar));
    }
    if withheld.calendar || withheld.calendar_list {
        return CalendarRow::Withheld;
    }
    CalendarRow::Available
}

/// A row for a withheld account: the reason below its address and a
/// Grant Access button that runs `grant`, named so a screen reader tells
/// several such rows apart.
fn grant_access_row(account: &Account, grant: impl Fn(AccountId) + 'static) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(&account.email)
        .subtitle(gettext("Not allowed when you signed in"))
        .build();
    let button = gtk::Button::builder()
        .label(gettext("Grant Access"))
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    crate::ui::name(
        &button,
        &fill(&gettext("Grant Access for {account}"), &[("account", &account.email)]),
    );
    let account_id = account.id;
    button.connect_clicked(move |_| grant(account_id));
    row.add_suffix(&button);
    row
}

#[cfg(test)]
mod tests {
    use mailrs_domain::{Account, AccountState, Provider};
    use mailrs_sync::{Missing, Offers, Withheld};

    use super::{CalendarRow, ContactsRow, calendar_lack, contacts_row};
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
    fn a_granted_account_switches_its_contacts_as_before() {
        assert_eq!(
            contacts_row(&account(), Offers::EVERYTHING, Withheld::NONE),
            ContactsRow::Switch
        );
        assert_eq!(
            calendar_lack(&account(), Offers::EVERYTHING, Withheld::NONE),
            CalendarRow::Available
        );
    }

    #[test]
    fn an_account_without_contacts_or_a_calendar_says_why() {
        let bare = Offers {
            contacts: false,
            calendar: false,
            ..Offers::EVERYTHING
        };
        assert_eq!(
            contacts_row(&account(), bare, Withheld::NONE),
            ContactsRow::NotOffered(reason(&account(), Missing::Contacts))
        );
        assert_eq!(
            calendar_lack(&account(), bare, Withheld::NONE),
            CalendarRow::NotOffered(reason(&account(), Missing::Calendar))
        );
    }

    #[test]
    fn a_withheld_account_gets_a_grant_access_row() {
        let withheld_contacts = Withheld { contacts: true, ..Withheld::NONE };
        assert_eq!(
            contacts_row(&account(), Offers::EVERYTHING, withheld_contacts),
            ContactsRow::Withheld
        );
        let withheld_calendar = Withheld { calendar: true, ..Withheld::NONE };
        assert_eq!(
            calendar_lack(&account(), Offers::EVERYTHING, withheld_calendar),
            CalendarRow::Withheld
        );
        let withheld_list = Withheld { calendar_list: true, ..Withheld::NONE };
        assert_eq!(
            calendar_lack(&account(), Offers::EVERYTHING, withheld_list),
            CalendarRow::Withheld
        );
    }
}
