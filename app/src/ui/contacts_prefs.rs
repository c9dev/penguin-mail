//! The Contacts & Calendar page of Preferences: each account's Google
//! contacts on its own switch, and whether GNOME shows its calendar.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use mailrs_domain::translate::{fill, gettext, pgettext};
use gtk::glib;
use mailrs_domain::{Account, AccountId, Provider};
use mailrs_store::services::{Miss, ServiceKind};
use mailrs_sync::{Missing, Offers, Withheld};

use crate::app::App;
use crate::offered::reason;
use crate::settings::{Change, Settings};

/// The page for `accounts`, each with what its server offers and what its
/// own consent withheld. `grant` runs the consent flow again for one
/// account's Grant Access button. `calendar_rows`, the calendar's own
/// settings, open the Calendar section.
pub fn page(
    app: &Rc<App>,
    settings: &Settings,
    accounts: &[(Account, Offers)],
    withheld: impl Fn(AccountId) -> Withheld,
    grant: impl Fn(AccountId) + Clone + 'static,
    calendar_rows: &[gtk::Widget],
) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title(gettext("Contacts & Calendar"))
        .icon_name("x-office-address-book-symbolic")
        .name("contacts")
        .build();
    page.add(&contacts(app, settings, accounts, &withheld, grant.clone()));
    let missed = |id, missing| app.core.missed(id, missing);
    let (calendar, online) = calendar(accounts, &withheld, &missed, grant, calendar_rows);
    page.add(&calendar);
    if let Some(online) = online {
        page.add(&online);
    }
    if let Some(group) = servers(app, accounts) {
        page.add(&group);
    }
    page
}

/// Where each IMAP or POP3 account's calendar, contacts and rules are, with Find
/// Again and Edit. A server found outside the address's domain waits here
/// for a yes before the password goes to it.
fn servers(app: &Rc<App>, accounts: &[(Account, Offers)]) -> Option<adw::PreferencesGroup> {
    let found_for: Vec<&Account> = accounts
        .iter()
        .map(|(a, _)| a)
        .filter(|a| matches!(a.provider, Provider::Imap | Provider::Pop3))
        .collect();
    if found_for.is_empty() {
        return None;
    }
    let group = adw::PreferencesGroup::builder()
        .title(gettext("Calendar, Contacts and Rules Servers"))
        .description(gettext("Penguin Mail looks for these when you add an account. The login is the account's own."))
        .build();
    for account in found_for {
        let row = adw::ExpanderRow::builder().use_markup(false).title(&account.email).build();
        group.add(&row);
        let shown: Shown = Rc::default();
        fill_servers(app, &row, account, &shown);
    }
    Some(group)
}

type Shown = Rc<RefCell<Vec<adw::ActionRow>>>;

/// Fills `row` with `account`'s server lines and its two buttons, after
/// taking out what an earlier fill put there.
fn fill_servers(app: &Rc<App>, row: &adw::ExpanderRow, account: &Account, shown: &Shown) {
    let (app, row, account, shown) = (Rc::clone(app), row.clone(), account.clone(), Rc::clone(shown));
    glib::spawn_future_local(async move {
        let found = app.core.services_found(account.id).await.unwrap_or_default();
        let (refused_calendar, refused_contacts) = app.core.login_refusals(account.id);
        let rules_here = app.core.rules_here(account.id);
        for old in shown.borrow_mut().drain(..) {
            row.remove(&old);
        }
        let lines = crate::servers::lines(&found, refused_calendar.as_deref(), refused_contacts.as_deref(), rules_here);
        for line in lines {
            let child = adw::ActionRow::builder().use_markup(false).title(&line.title).subtitle(&line.subtitle).build();
            if let Some(kind) = line.ask {
                let host = found.iter().find(|f| f.kind == kind).map(|f| f.url.clone()).unwrap_or_default();
                let use_it = gtk::Button::builder().label(gettext("Use It")).valign(gtk::Align::Center).css_classes(["flat"]).build();
                crate::ui::name(&use_it, &fill(&gettext("Use {host} for {account}"), &[("host", &host), ("account", &account.email)]));
                let (app, row, account, shown) = (Rc::clone(&app), row.clone(), account.clone(), Rc::clone(&shown));
                use_it.connect_clicked(move |button| {
                    button.set_sensitive(false);
                    let (app, row, account, shown) = (Rc::clone(&app), row.clone(), account.clone(), Rc::clone(&shown));
                    glib::spawn_future_local(async move {
                        if let Err(err) = app.core.confirm_server(account.clone(), kind).await {
                            tracing::info!(account = %account.email, %err, "the server did not take the login");
                        }
                        fill_servers(&app, &row, &account, &shown);
                    });
                });
                child.add_suffix(&use_it);
            }
            row.add_row(&child);
            shown.borrow_mut().push(child);
        }
        let again = gtk::Button::builder().label(gettext("Find Again")).valign(gtk::Align::Center).build();
        let edit = gtk::Button::builder().label(gettext("Edit…")).valign(gtk::Align::Center).build();
        let holder = adw::ActionRow::builder().activatable(false).build();
        holder.add_suffix(&again);
        holder.add_suffix(&edit);
        row.add_row(&holder);
        shown.borrow_mut().push(holder);
        {
            let (app, row, account, shown) = (Rc::clone(&app), row.clone(), account.clone(), Rc::clone(&shown));
            again.connect_clicked(move |button| {
                button.set_sensitive(false);
                let (app, row, account, shown) = (Rc::clone(&app), row.clone(), account.clone(), Rc::clone(&shown));
                glib::spawn_future_local(async move {
                    if let Err(err) = app.core.find_services(account.clone()).await {
                        tracing::info!(account = %account.email, %err, "the search for servers failed");
                    }
                    fill_servers(&app, &row, &account, &shown);
                });
            });
        }
        let calendar = found.iter().find(|f| f.kind == ServiceKind::CalDav).map(|f| f.url.clone()).unwrap_or_default();
        let contacts = found.iter().find(|f| f.kind == ServiceKind::CardDav).map(|f| f.url.clone()).unwrap_or_default();
        edit.connect_clicked(move |button| {
            let (again_app, again_row, again_account, again_shown) = (Rc::clone(&app), row.clone(), account.clone(), Rc::clone(&shown));
            super::dav_edit::present(&app, &account, &calendar, &contacts, button, move || {
                fill_servers(&again_app, &again_row, &again_account, &again_shown)
            });
        });
    });
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
        .title(gettext("Contacts"))
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
        match contacts_row(account, *offers, withheld(account.id), app.core.missed(account.id, Missing::Contacts)) {
            ContactsRow::Withheld => {
                group.add(&grant_access_row(account, grant.clone()));
            }
            ContactsRow::NotOffered(reason) => {
                let row = adw::SwitchRow::builder().use_markup(false)
                    .title(&account.email)
                    .subtitle(reason)
                    .active(false)
                    .sensitive(false)
                    .build();
                group.add(&row);
            }
            ContactsRow::Switch => {
                let row = adw::SwitchRow::builder().use_markup(false)
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
fn contacts_row(account: &Account, offers: Offers, withheld: Withheld, missed: Option<Miss>) -> ContactsRow {
    if !offers.contacts {
        return ContactsRow::NotOffered(reason(account, Missing::Contacts, missed));
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
/// a row that says so instead. `settings_rows`, the calendar's own
/// settings, sit above the accounts.
///
/// The Online Accounts rows go in a group of their own, the second one
/// returned, so they do not sit among Event Reminders and Working Hours
/// under no heading. It is `None` when there are none.
fn calendar(
    accounts: &[(Account, Offers)],
    withheld: &impl Fn(AccountId) -> Withheld,
    missed: &impl Fn(AccountId, Missing) -> Option<Miss>,
    grant: impl Fn(AccountId) + Clone + 'static,
    settings_rows: &[gtk::Widget],
) -> (adw::PreferencesGroup, Option<adw::PreferencesGroup>) {
    let group = adw::PreferencesGroup::builder()
        .title(gettext("Calendar"))
        .description(gettext(
            "Penguin Mail shows each account's meetings and reminds you before they start.",
        ))
        .build();
    let online = adw::PreferencesGroup::builder()
        .title(gettext("GNOME Online Accounts"))
        .description(gettext(
            "Add an account to GNOME Online Accounts to see its meetings in GNOME \
             Calendar and the clock too.",
        ))
        .build();
    let mut online_rows = 0;
    // The calendar's own settings come first, above the accounts.
    for row in settings_rows {
        group.add(row);
    }
    let mut online_accounts = true;
    for (account, offers) in accounts {
        match calendar_lack(account, *offers, withheld(account.id), missed(account.id, Missing::Calendar)) {
            CalendarRow::NotOffered(lack) => {
                group.add(
                    &adw::ActionRow::builder().use_markup(false)
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
        let row = adw::ActionRow::builder().use_markup(false).title(&account.email).build();
        if known {
            row.set_subtitle(&pgettext("an account in Online Accounts", "Added"));
        } else {
            row.set_subtitle(&pgettext("an account in Online Accounts", "Not added"));
            let add = gtk::Button::builder()
                .label(gettext("Add to Online Accounts…"))
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
        online.add(&row);
        online_rows += 1;
    }
    (group, (online_rows > 0).then_some(online))
}

/// Why `account`'s calendar row is not the Online Accounts row: the
/// server has none, or the person's own consent withheld it.
fn calendar_lack(account: &Account, offers: Offers, withheld: Withheld, missed: Option<Miss>) -> CalendarRow {
    if !offers.calendar {
        return CalendarRow::NotOffered(reason(account, Missing::Calendar, missed));
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
    let row = adw::ActionRow::builder().use_markup(false)
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
            contacts_row(&account(), Offers::EVERYTHING, Withheld::NONE, None),
            ContactsRow::Switch
        );
        assert_eq!(
            calendar_lack(&account(), Offers::EVERYTHING, Withheld::NONE, None),
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
            contacts_row(&account(), bare, Withheld::NONE, None),
            ContactsRow::NotOffered(reason(&account(), Missing::Contacts, None))
        );
        assert_eq!(
            calendar_lack(&account(), bare, Withheld::NONE, None),
            CalendarRow::NotOffered(reason(&account(), Missing::Calendar, None))
        );
    }

    #[test]
    fn a_withheld_account_gets_a_grant_access_row() {
        let withheld_contacts = Withheld { contacts: true, ..Withheld::NONE };
        assert_eq!(
            contacts_row(&account(), Offers::EVERYTHING, withheld_contacts, None),
            ContactsRow::Withheld
        );
        let withheld_calendar = Withheld { calendar: true, ..Withheld::NONE };
        assert_eq!(
            calendar_lack(&account(), Offers::EVERYTHING, withheld_calendar, None),
            CalendarRow::Withheld
        );
        let withheld_list = Withheld { calendar_list: true, ..Withheld::NONE };
        assert_eq!(
            calendar_lack(&account(), Offers::EVERYTHING, withheld_list, None),
            CalendarRow::Withheld
        );
    }
}
