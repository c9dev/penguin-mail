//! The permissions an account grants after sign-in, and the words
//! the window uses to ask for one. The window and the assistant both name
//! a permission with [`Permission`]; `crate::ui::permission` puts the
//! question on screen, and `MainWindow::ask_permission` is the one place a
//! `Permitted::NeedsPermission` answer leads to.
//!
//! Nothing here touches GTK, so the table and the once-a-run rule are
//! tested without a display.

use std::collections::HashSet;

use mailrs_domain::{AccountId, Provider};
use mailrs_domain::translate::{fill, gettext};
use mailrs_gmail::SIGN_IN_SCOPES;
use mailrs_sync::{Offers, Withheld};

/// A permission sign-in leaves out, or one a caller can find missing.
/// CONTEXT.md describes each. A Microsoft account never meets `Drive`:
/// its consent has no such scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Permission {
    /// Reading and changing the account's Gmail settings.
    Settings,
    /// Erasing mail for good.
    Delete,
    /// Reading the account's Google contacts.
    Contacts,
    /// Adding and changing the account's Google contacts.
    ChangeContacts,
    /// Reading and changing the events on the account's calendar.
    Calendar,
    /// Making, renaming and deleting calendars, and changing the
    /// account's calendar list: colours and hiding on every device, and
    /// subscribing.
    ManageCalendars,
    /// Putting files in the account's Google Drive (`drive.file`), which
    /// attaching a file from this computer to an event needs.
    Drive,
}

/// Why the window asks for a permission, which decides how often it asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Occasion {
    /// Something the person or the assistant asked for stopped for want
    /// of the permission. Nothing happens without it, so the window asks
    /// each time.
    Needed,
    /// Something finished without the permission, and the permission
    /// would have done more: an invitation answer that reached the
    /// organizer by mail but not the person's own calendar. The window
    /// offers it once per account a run, so saying no ends it.
    Offer,
}

/// The heading and body of the question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wording {
    pub heading: String,
    pub body: String,
}

/// Who asks for the consent, as the person knows the company.
fn company(provider: Provider) -> &'static str {
    match provider {
        Provider::Microsoft => "Microsoft",
        Provider::Gmail | Provider::Imap => "Google",
    }
}

/// Whose settings the settings permission changes.
fn settings_of(provider: Provider) -> &'static str {
    match provider {
        Provider::Microsoft => "Outlook",
        Provider::Gmail | Provider::Imap => "Gmail",
    }
}

/// The toast a settings dialog shows when a save finds the permission
/// gone.
pub fn settings_needed(provider: Provider) -> String {
    fill(
        &gettext("Penguin Mail needs permission to change {service} settings"),
        &[("service", settings_of(provider))],
    )
}

impl Permission {
    #[cfg(test)]
    pub const ALL: [Permission; 7] = [
        Permission::Settings,
        Permission::Delete,
        Permission::Contacts,
        Permission::ChangeContacts,
        Permission::Calendar,
        Permission::ManageCalendars,
        Permission::Drive,
    ];

    /// What the permission lets Penguin Mail do, to finish "needs
    /// permission to" in what the assistant tells the model. The model
    /// reads English, so this is not translated.
    pub fn purpose(self) -> &'static str {
        match self {
            Permission::Settings => "change the account's mail settings",
            Permission::Delete => "delete mail for good",
            Permission::Contacts => "read contacts",
            Permission::ChangeContacts => "add and change contacts",
            Permission::Calendar => "use the calendar",
            Permission::ManageCalendars => "manage calendars",
            Permission::Drive => "add files to Google Drive",
        }
    }

    /// The question the window asks `account` on this occasion. Only the
    /// calendar has an offer of its own; the others ask the same way on
    /// either occasion.
    pub fn wording(self, occasion: Occasion, account: &str, provider: Provider) -> Wording {
        let (heading, body) = match (self, occasion) {
            (Permission::Settings, _) => (
                fill(
                    &gettext("Allow Changes to {service} Settings"),
                    &[("service", settings_of(provider))],
                ),
                gettext(
                    "Penguin Mail needs permission to change {service} settings for {account}. \
                     {company} asks you to confirm in your browser.",
                ),
            ),
            (Permission::Delete, _) => (
                gettext("Allow Penguin Mail to Delete Mail"),
                gettext(
                    "Deleting mail for good needs one more permission for {account}. \
                     {company} asks you to confirm in your browser.",
                ),
            ),
            (Permission::Contacts, _) => (
                gettext("Allow Penguin Mail to Read Your Contacts"),
                gettext(
                    "Reading the contacts of {account} needs one more permission. {company} \
                     asks you to confirm in your browser. Names and photos stay on this \
                     computer.",
                ),
            ),
            (Permission::ChangeContacts, _) => (
                gettext("Allow Penguin Mail to Change Your Contacts"),
                gettext(
                    "The assistant needs permission to add and change the contacts of \
                     {account}. {company} asks you to confirm in your browser.",
                ),
            ),
            (Permission::Calendar, Occasion::Needed) => (
                gettext("Allow Penguin Mail to Use Your Calendar"),
                gettext(
                    "The assistant needs permission to read and change events on the \
                     calendar for {account}. {company} asks you to confirm in your browser.",
                ),
            ),
            (Permission::Calendar, Occasion::Offer) => (
                gettext("Allow Penguin Mail to Use Your Calendar"),
                gettext(
                    "Your reply went to the organizer as mail. With permission to change \
                     events on the calendar for {account}, the meeting is marked on your \
                     own calendar too. {company} asks you to confirm in your browser.",
                ),
            ),
            (Permission::ManageCalendars, _) => (
                gettext("Allow Penguin Mail to Manage Your Calendars"),
                gettext(
                    "Making, renaming and deleting calendars, and subscribing to one, needs \
                     one more permission for {account}. {company} asks you to confirm in your \
                     browser.",
                ),
            ),
            (Permission::Drive, _) => (
                gettext("Allow Penguin Mail to Add Files to Google Drive"),
                gettext(
                    "Attaching a file from this computer to an event puts it in Google Drive for \
                     {account}. Penguin Mail can reach only the files it adds there. Google asks \
                     you to confirm in your browser.",
                ),
            ),
        };
        Wording {
            heading,
            body: fill(
                &body,
                &[
                    ("account", account),
                    ("company", company(provider)),
                    ("service", settings_of(provider)),
                ],
            ),
        }
    }
}

/// The permissions this run has already offered. It lives as long as the
/// process, so closing the window to the tray does not bring an offer
/// back.
#[derive(Debug, Default)]
pub struct Asked {
    offered: HashSet<(AccountId, Permission)>,
}

impl Asked {
    /// Whether to put the question on screen now. An offer counts as made
    /// the moment this says yes, whatever the person answers.
    pub fn should_ask(
        &mut self,
        account_id: AccountId,
        permission: Permission,
        occasion: Occasion,
    ) -> bool {
        match occasion {
            Occasion::Needed => true,
            Occasion::Offer => self.offered.insert((account_id, permission)),
        }
    }
}

/// The permissions `withheld` says the person did not grant, in the
/// order Preferences lists them. Calendar and the calendar list share
/// one permission, so either withheld field names it once; so do the
/// calendars and changing the list, which calendar management needs.
pub fn withheld_permissions(withheld: Withheld) -> Vec<Permission> {
    [
        (withheld.settings, Permission::Settings),
        (withheld.delete, Permission::Delete),
        (withheld.contacts, Permission::Contacts),
        (withheld.change_contacts, Permission::ChangeContacts),
        (withheld.calendar || withheld.calendar_list, Permission::Calendar),
        (withheld.calendars || withheld.change_calendar_list, Permission::ManageCalendars),
        (withheld.drive, Permission::Drive),
    ]
    .into_iter()
    .filter_map(|(missing, permission)| missing.then_some(permission))
    .collect()
}

impl Permission {
    /// What the permission lets Penguin Mail do, in words that finish
    /// "has not allowed Penguin Mail to".
    fn allows(self, provider: Provider) -> String {
        match self {
            Permission::Settings => fill(
                &gettext("change {service} settings"),
                &[("service", settings_of(provider))],
            ),
            Permission::Delete => gettext("delete mail for good"),
            Permission::Contacts => gettext("read contacts"),
            Permission::ChangeContacts => gettext("add and change contacts"),
            Permission::Calendar => gettext("use the calendar"),
            Permission::ManageCalendars => gettext("manage calendars"),
            Permission::Drive => gettext("add files to Google Drive"),
        }
    }
}

impl Permission {
    /// The row Add Account shows for this permission when the sign-in
    /// left it out: what stays off, and what that covers.
    pub fn feature(self, provider: Provider) -> (String, String) {
        let (title, covers) = match self {
            Permission::Settings if provider == Provider::Microsoft => (
                fill(
                    &gettext("Change {service} settings"),
                    &[("service", settings_of(provider))],
                ),
                gettext("Rules and automatic reply"),
            ),
            Permission::Settings => (
                gettext("Change Gmail settings"),
                gettext("Rules, automatic reply, send-as addresses"),
            ),
            Permission::Delete => (
                gettext("Delete mail for good"),
                gettext("Emptying the trash and spam"),
            ),
            Permission::Contacts => (
                gettext("Read contacts"),
                gettext("Names and photos as you write"),
            ),
            Permission::ChangeContacts => (
                gettext("Add and change contacts"),
                gettext("Saving a sender as a contact"),
            ),
            Permission::Calendar => (
                gettext("Use the calendar"),
                gettext("Events, invitations, reminders"),
            ),
            Permission::ManageCalendars => (
                gettext("Manage calendars"),
                gettext("New calendars, colors, subscriptions"),
            ),
            Permission::Drive => (
                gettext("Add files to Google Drive"),
                gettext("Files attached to events"),
            ),
        };
        (title, covers)
    }

    /// The icon beside that row.
    pub fn feature_icon(self) -> &'static str {
        match self {
            Permission::Settings => "emblem-system-symbolic",
            Permission::Delete => "user-trash-symbolic",
            Permission::Contacts | Permission::ChangeContacts => "penguin-mail-people-symbolic",
            Permission::Calendar | Permission::ManageCalendars => {
                "penguin-mail-calendar-symbolic"
            }
            Permission::Drive => "folder-documents-symbolic",
        }
    }
}

/// What an account's Grant Access bar says: the account and each
/// feature its consent left out.
pub fn grant_bar_title(account: &str, missing: &[Permission], provider: Provider) -> String {
    let allows: Vec<String> = missing.iter().map(|p| p.allows(provider)).collect();
    let allows: Vec<&str> = allows.iter().map(String::as_str).collect();
    fill(
        &gettext("{account} has not allowed Penguin Mail to {missing}"),
        &[("account", account), ("missing", &crate::protection::joined(&allows))],
    )
}

/// Whether the account's Grant Access banner shows: something is
/// withheld, and `asked` (the account's `asked_scopes` row) does not
/// already cover every scope of either provider's sign-in. An account asked for
/// everything and unticked a box gets no banner; the feature it lacks
/// says so where it lives instead.
pub fn wants_banner(withheld: Withheld, asked: Option<&str>) -> bool {
    if withheld.is_empty() {
        return false;
    }
    let Some(asked) = asked else {
        return true;
    };
    let asked: HashSet<String> = asked.split_whitespace().map(str::to_ascii_lowercase).collect();
    // Microsoft names its scopes in mixed case and may answer in another.
    let covers = |scopes: &[&str]| scopes.iter().all(|s| asked.contains(&s.to_ascii_lowercase()));
    !(covers(&SIGN_IN_SCOPES) || covers(&mailrs_graph::SCOPES))
}

/// Whether an invitation's card offers Grant Access: the account has a
/// calendar and its consent left the calendar out, so the event cannot
/// show in the Calendar space until the person grants it. With the
/// permission granted, Show in Calendar covers the event; an account with
/// no calendar (IMAP) hands the `.ics` to the desktop instead.
pub fn card_offers_calendar_access(offers: Offers, withheld: Withheld) -> bool {
    offers.calendar && withheld_permissions(withheld).contains(&Permission::Calendar)
}

#[cfg(test)]
mod tests {
    use super::*;

    const OCCASIONS: [Occasion; 2] = [Occasion::Needed, Occasion::Offer];

    #[test]
    fn add_account_names_each_feature_left_off_in_the_mockups_words() {
        assert_eq!(
            Permission::Settings.feature(Provider::Gmail),
            (
                "Change Gmail settings".to_string(),
                "Rules, automatic reply, send-as addresses".to_string()
            )
        );
        assert_eq!(
            Permission::ManageCalendars.feature(Provider::Gmail),
            (
                "Manage calendars".to_string(),
                "New calendars, colors, subscriptions".to_string()
            )
        );
        for permission in Permission::ALL {
            assert!(!permission.feature(Provider::Gmail).1.is_empty(), "{permission:?}");
            assert!(permission.feature_icon().ends_with("-symbolic"));
        }
    }

    #[test]
    fn the_grant_bar_names_each_missing_feature() {
        assert_eq!(
            grant_bar_title(
                "d.reyes@uni.example",
                &[Permission::Settings, Permission::Delete, Permission::Calendar],
                Provider::Gmail
            ),
            "d.reyes@uni.example has not allowed Penguin Mail to change Gmail settings, \
             delete mail for good and use the calendar"
        );
    }

    #[test]
    fn the_grant_bar_names_one_missing_feature_alone() {
        assert_eq!(
            grant_bar_title("a@example.com", &[Permission::Contacts], Provider::Gmail),
            "a@example.com has not allowed Penguin Mail to read contacts"
        );
    }

    #[test]
    fn every_permission_names_the_account() {
        for permission in Permission::ALL {
            assert!(!permission.purpose().is_empty(), "{permission:?}");
            for occasion in OCCASIONS {
                let words = permission.wording(occasion, "ana@example.com", Provider::Gmail);
                assert!(!words.heading.is_empty(), "{permission:?}");
                assert!(
                    words.body.contains("ana@example.com"),
                    "{permission:?} {occasion:?}: {}",
                    words.body
                );
                assert!(!words.body.contains('{'), "{}", words.body);
            }
        }
    }

    #[test]
    fn the_calendar_offer_explains_the_reply_already_went() {
        let offer = Permission::Calendar.wording(Occasion::Offer, "a@example.com", Provider::Gmail);
        let needed = Permission::Calendar.wording(Occasion::Needed, "a@example.com", Provider::Gmail);
        assert_eq!(offer.heading, needed.heading);
        assert!(offer.body.starts_with("Your reply went to the organizer"));
        assert!(needed.body.starts_with("The assistant needs permission"));
    }

    #[test]
    fn a_microsoft_account_hears_of_microsoft_and_outlook() {
        let words = Permission::Settings.wording(Occasion::Needed, "d@outlook.com", Provider::Microsoft);
        assert!(words.body.contains("Microsoft asks you to confirm"), "{}", words.body);
        assert!(words.body.contains("Outlook settings"));
        assert!(!words.body.contains("Google") && !words.body.contains("Gmail"));
        let google = Permission::Settings.wording(Occasion::Needed, "d@gmail.com", Provider::Gmail);
        assert!(google.body.contains("Google asks you to confirm") && google.body.contains("Gmail settings"));
        assert_eq!(
            grant_bar_title(
                "d@outlook.com",
                &[Permission::Settings, Permission::Calendar],
                Provider::Microsoft
            ),
            "d@outlook.com has not allowed Penguin Mail to change Outlook settings and use the calendar"
        );
    }

    #[test]
    fn the_settings_toast_names_the_providers_settings() {
        assert_eq!(settings_needed(Provider::Gmail), "Penguin Mail needs permission to change Gmail settings");
        assert_eq!(settings_needed(Provider::Microsoft), "Penguin Mail needs permission to change Outlook settings");
    }

    #[test]
    fn no_word_a_microsoft_account_reads_names_google_or_gmail() {
        // Drive is Google's alone: Microsoft never asks for it.
        for permission in Permission::ALL.into_iter().filter(|p| *p != Permission::Drive) {
            let (title, covers) = permission.feature(Provider::Microsoft);
            let mut said = vec![title, covers];
            for occasion in OCCASIONS {
                let words = permission.wording(occasion, "d@outlook.com", Provider::Microsoft);
                said.extend([words.heading, words.body]);
            }
            said.push(grant_bar_title("d@outlook.com", &[permission], Provider::Microsoft));
            for text in said {
                assert!(!text.contains("Google") && !text.contains("Gmail"), "{permission:?}: {text}");
            }
        }
        assert!(!Permission::Settings.purpose().contains("Gmail"));
    }

    #[test]
    fn a_microsoft_consent_that_asked_for_everything_shows_no_banner() {
        let withheld = Withheld { calendar: true, ..Withheld::NONE };
        assert!(!wants_banner(withheld, Some(&mailrs_graph::SCOPES.join(" "))));
        assert!(wants_banner(withheld, Some("openid Mail.ReadWrite")));
    }

    #[test]
    fn a_needed_permission_asks_every_time() {
        let mut asked = Asked::default();
        let account: AccountId = 1;
        for _ in 0..3 {
            assert!(asked.should_ask(account, Permission::Delete, Occasion::Needed));
        }
    }

    #[test]
    fn an_offer_comes_once_per_account_and_permission() {
        let mut asked = Asked::default();
        let (one, two): (AccountId, AccountId) = (1, 2);
        assert!(asked.should_ask(one, Permission::Calendar, Occasion::Offer));
        assert!(!asked.should_ask(one, Permission::Calendar, Occasion::Offer));
        assert!(asked.should_ask(two, Permission::Calendar, Occasion::Offer));
        // A tool that cannot work without the permission still asks after
        // the offer was declined.
        assert!(asked.should_ask(one, Permission::Calendar, Occasion::Needed));
    }

    #[test]
    fn withheld_scopes_name_their_permissions() {
        assert_eq!(withheld_permissions(Withheld::NONE), []);
        assert_eq!(
            withheld_permissions(Withheld { settings: true, ..Withheld::NONE }),
            [Permission::Settings]
        );
        // Calendar and the calendar list share one permission, either way.
        assert_eq!(
            withheld_permissions(Withheld { calendar: true, ..Withheld::NONE }),
            [Permission::Calendar]
        );
        assert_eq!(
            withheld_permissions(Withheld { calendar_list: true, ..Withheld::NONE }),
            [Permission::Calendar]
        );
        assert_eq!(
            withheld_permissions(Withheld {
                delete: true,
                contacts: true,
                ..Withheld::NONE
            }),
            [Permission::Delete, Permission::Contacts]
        );
    }

    #[test]
    fn a_banner_shows_for_an_account_never_asked_for_everything() {
        let some_withheld = Withheld { calendar: true, ..Withheld::NONE };
        assert!(wants_banner(some_withheld, None), "an old account asked for far less");
        let old_asked = "https://www.googleapis.com/auth/gmail.modify \
                          https://www.googleapis.com/auth/gmail.settings.basic";
        assert!(wants_banner(some_withheld, Some(old_asked)));
    }

    #[test]
    fn no_banner_once_the_account_was_asked_and_chose() {
        let asked = SIGN_IN_SCOPES.join(" ");
        let some_withheld = Withheld { calendar: true, ..Withheld::NONE };
        assert!(
            !wants_banner(some_withheld, Some(&asked)),
            "a consent that asked for everything got its answer already"
        );
        assert!(
            !wants_banner(Withheld::NONE, Some(&asked)),
            "nothing withheld needs no banner either"
        );
    }

    /// The grant every account held between the five-scope sign-in and
    /// the owner's approval of calendar management on 2026-09-28.
    fn five_scope_grant() -> String {
        use mailrs_gmail::{CALENDAR_LIST_SCOPE, CALENDAR_SCOPE, CONTACTS_WRITE_SCOPE, DELETE_SCOPE, SETTINGS_SCOPE};
        [DELETE_SCOPE, SETTINGS_SCOPE, CONTACTS_WRITE_SCOPE, CALENDAR_SCOPE, CALENDAR_LIST_SCOPE].join(" ")
    }

    /// An account in a store that holds the old read-only calendar list
    /// gets the Grant Access bar for calendar management, and nothing
    /// else it had turns off.
    #[test]
    fn a_stored_five_scope_grant_asks_for_calendar_management() {
        let conn = mailrs_store::open_in_memory().unwrap();
        let id = mailrs_store::accounts::insert_account(&conn, "ana@example.com", 0).unwrap();
        mailrs_store::accounts::set_granted(&conn, id, &five_scope_grant()).unwrap();
        mailrs_store::accounts::set_asked(&conn, id, &five_scope_grant()).unwrap();

        let consent = mailrs_store::accounts::consent(&conn, id).unwrap();
        let granted = mailrs_gmail::Granted::parse(consent.granted.as_deref().unwrap());
        let withheld = mailrs_sync::withheld_by_grant(Some(&granted));
        assert!(!withheld.calendar && !withheld.calendar_list, "every calendar still reads");
        assert!(wants_banner(withheld, consent.asked.as_deref()));
        assert_eq!(withheld_permissions(withheld), [Permission::ManageCalendars, Permission::Drive]);
        assert_eq!(
            grant_bar_title("ana@example.com", &withheld_permissions(withheld), Provider::Gmail),
            "ana@example.com has not allowed Penguin Mail to manage calendars and add files to Google Drive"
        );
    }

    /// Once the account went through a consent that asked for all seven,
    /// a box left unticked there brings no bar back.
    #[test]
    fn a_consent_that_asked_for_all_seven_ends_the_bar() {
        let conn = mailrs_store::open_in_memory().unwrap();
        let id = mailrs_store::accounts::insert_account(&conn, "ana@example.com", 0).unwrap();
        mailrs_store::accounts::set_granted(&conn, id, &five_scope_grant()).unwrap();
        mailrs_store::accounts::set_asked(&conn, id, &SIGN_IN_SCOPES.join(" ")).unwrap();
        let consent = mailrs_store::accounts::consent(&conn, id).unwrap();
        let granted = mailrs_gmail::Granted::parse(consent.granted.as_deref().unwrap());
        let withheld = mailrs_sync::withheld_by_grant(Some(&granted));
        assert!(!wants_banner(withheld, consent.asked.as_deref()));
    }

    #[test]
    fn either_calendar_management_scope_names_one_permission() {
        for withheld in [
            Withheld { calendars: true, ..Withheld::NONE },
            Withheld { change_calendar_list: true, ..Withheld::NONE },
        ] {
            assert_eq!(withheld_permissions(withheld), [Permission::ManageCalendars]);
        }
    }

    #[test]
    fn a_withheld_drive_names_its_own_permission() {
        assert_eq!(withheld_permissions(Withheld { drive: true, ..Withheld::NONE }), [Permission::Drive]);
    }

    #[test]
    fn asking_for_drive_says_what_it_reaches() {
        let wording = Permission::Drive.wording(Occasion::Needed, "ana@example.com", Provider::Gmail);
        assert_eq!(wording.heading, "Allow Penguin Mail to Add Files to Google Drive");
        assert_eq!(
            wording.body,
            "Attaching a file from this computer to an event puts it in Google Drive for ana@example.com. \
             Penguin Mail can reach only the files it adds there. Google asks you to confirm in your browser."
        );
    }

    const IMAP: Offers = Offers { calendar: false, ..Offers::EVERYTHING };

    #[test]
    fn the_card_offers_calendar_access_when_the_calendar_permission_is_withheld() {
        let withheld = Withheld { calendar: true, ..Withheld::NONE };
        assert!(card_offers_calendar_access(Offers::EVERYTHING, withheld));
        let list_only = Withheld { calendar_list: true, ..Withheld::NONE };
        assert!(card_offers_calendar_access(Offers::EVERYTHING, list_only));
    }

    #[test]
    fn the_card_offers_nothing_when_the_calendar_permission_is_granted() {
        let other = Withheld { contacts: true, ..Withheld::NONE };
        assert!(!card_offers_calendar_access(Offers::EVERYTHING, Withheld::NONE));
        assert!(!card_offers_calendar_access(Offers::EVERYTHING, other));
    }

    #[test]
    fn the_card_offers_nothing_for_an_account_with_no_calendar() {
        let withheld = Withheld { calendar: true, ..Withheld::NONE };
        assert!(!card_offers_calendar_access(IMAP, withheld));
        assert!(!card_offers_calendar_access(IMAP, Withheld::NONE));
    }
}
