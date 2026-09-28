//! The Google permissions an account grants after sign-in, and the words
//! the window uses to ask for one. The window and the assistant both name
//! a permission with [`Permission`]; `crate::ui::permission` puts the
//! question on screen, and `MainWindow::ask_permission` is the one place a
//! `Permitted::NeedsPermission` answer leads to.
//!
//! Nothing here touches GTK, so the table and the once-a-run rule are
//! tested without a display.

use std::collections::HashSet;

use mailrs_domain::AccountId;
use mailrs_domain::translate::{fill, gettext};
use mailrs_gmail::SIGN_IN_SCOPES;
use mailrs_sync::{Offers, Withheld};

/// A Google permission sign-in leaves out, or one a caller can find
/// missing. CONTEXT.md describes each.
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

impl Permission {
    #[cfg(test)]
    pub const ALL: [Permission; 6] = [
        Permission::Settings,
        Permission::Delete,
        Permission::Contacts,
        Permission::ChangeContacts,
        Permission::Calendar,
        Permission::ManageCalendars,
    ];

    /// What the permission lets Penguin Mail do, to finish "needs
    /// permission to" in what the assistant tells the model. The model
    /// reads English, so this is not translated.
    pub fn purpose(self) -> &'static str {
        match self {
            Permission::Settings => "change Gmail settings",
            Permission::Delete => "delete mail for good",
            Permission::Contacts => "read contacts",
            Permission::ChangeContacts => "add and change contacts",
            Permission::Calendar => "use the calendar",
            Permission::ManageCalendars => "manage calendars",
        }
    }

    /// The question the window asks `account` on this occasion. Only the
    /// calendar has an offer of its own; the others ask the same way on
    /// either occasion.
    pub fn wording(self, occasion: Occasion, account: &str) -> Wording {
        let (heading, body) = match (self, occasion) {
            (Permission::Settings, _) => (
                gettext("Allow Changes to Gmail Settings"),
                gettext(
                    "Penguin Mail needs permission to change Gmail settings for {account}. \
                     Google asks you to confirm in your browser.",
                ),
            ),
            (Permission::Delete, _) => (
                gettext("Allow Penguin Mail to Delete Mail"),
                gettext(
                    "Deleting mail for good needs one more permission for {account}. \
                     Google asks you to confirm in your browser.",
                ),
            ),
            (Permission::Contacts, _) => (
                gettext("Allow Penguin Mail to Read Your Contacts"),
                gettext(
                    "Reading the contacts of {account} needs one more permission. Google \
                     asks you to confirm in your browser. Names and photos stay on this \
                     computer.",
                ),
            ),
            (Permission::ChangeContacts, _) => (
                gettext("Allow Penguin Mail to Change Your Contacts"),
                gettext(
                    "The assistant needs permission to add and change the contacts of \
                     {account}. Google asks you to confirm in your browser.",
                ),
            ),
            (Permission::Calendar, Occasion::Needed) => (
                gettext("Allow Penguin Mail to Use Your Calendar"),
                gettext(
                    "The assistant needs permission to read and change events on the \
                     calendar for {account}. Google asks you to confirm in your browser.",
                ),
            ),
            (Permission::Calendar, Occasion::Offer) => (
                gettext("Allow Penguin Mail to Use Your Calendar"),
                gettext(
                    "Your reply went to the organizer as mail. With permission to change \
                     events on the calendar for {account}, the meeting is marked on your \
                     own calendar too. Google asks you to confirm in your browser.",
                ),
            ),
            (Permission::ManageCalendars, _) => (
                gettext("Allow Penguin Mail to Manage Your Calendars"),
                gettext(
                    "Making, renaming and deleting calendars, and subscribing to one, needs \
                     one more permission for {account}. Google asks you to confirm in your \
                     browser.",
                ),
            ),
        };
        Wording {
            heading,
            body: fill(&body, &[("account", account)]),
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
    ]
    .into_iter()
    .filter_map(|(missing, permission)| missing.then_some(permission))
    .collect()
}

impl Permission {
    /// What the permission lets Penguin Mail do, in words that finish
    /// "has not allowed Penguin Mail to".
    fn allows(self) -> String {
        match self {
            Permission::Settings => gettext("change Gmail settings"),
            Permission::Delete => gettext("delete mail for good"),
            Permission::Contacts => gettext("read contacts"),
            Permission::ChangeContacts => gettext("add and change contacts"),
            Permission::Calendar => gettext("use the calendar"),
            Permission::ManageCalendars => gettext("manage calendars"),
        }
    }
}

/// What an account's Grant Access bar says: the account and each
/// feature its consent left out.
pub fn grant_bar_title(account: &str, missing: &[Permission]) -> String {
    let allows: Vec<String> = missing.iter().map(|p| p.allows()).collect();
    let allows: Vec<&str> = allows.iter().map(String::as_str).collect();
    fill(
        &gettext("{account} has not allowed Penguin Mail to {missing}"),
        &[("account", account), ("missing", &crate::protection::joined(&allows))],
    )
}

/// Whether the account's Grant Access banner shows: something is
/// withheld, and `asked` (the account's `asked_scopes` row) does not
/// already cover every [`SIGN_IN_SCOPES`] entry. An account asked for
/// everything and unticked a box gets no banner; the feature it lacks
/// says so where it lives instead.
pub fn wants_banner(withheld: Withheld, asked: Option<&str>) -> bool {
    if withheld.is_empty() {
        return false;
    }
    let Some(asked) = asked else {
        return true;
    };
    let asked: HashSet<&str> = asked.split_whitespace().collect();
    !SIGN_IN_SCOPES.iter().all(|scope| asked.contains(scope))
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
    fn the_grant_bar_names_each_missing_feature() {
        assert_eq!(
            grant_bar_title(
                "d.reyes@uni.example",
                &[Permission::Settings, Permission::Delete, Permission::Calendar]
            ),
            "d.reyes@uni.example has not allowed Penguin Mail to change Gmail settings, \
             delete mail for good and use the calendar"
        );
    }

    #[test]
    fn the_grant_bar_names_one_missing_feature_alone() {
        assert_eq!(
            grant_bar_title("a@example.com", &[Permission::Contacts]),
            "a@example.com has not allowed Penguin Mail to read contacts"
        );
    }

    #[test]
    fn every_permission_names_the_account() {
        for permission in Permission::ALL {
            assert!(!permission.purpose().is_empty(), "{permission:?}");
            for occasion in OCCASIONS {
                let words = permission.wording(occasion, "ana@example.com");
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
        let offer = Permission::Calendar.wording(Occasion::Offer, "a@example.com");
        let needed = Permission::Calendar.wording(Occasion::Needed, "a@example.com");
        assert_eq!(offer.heading, needed.heading);
        assert!(offer.body.starts_with("Your reply went to the organizer"));
        assert!(needed.body.starts_with("The assistant needs permission"));
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
        assert_eq!(withheld_permissions(withheld), [Permission::ManageCalendars]);
        assert_eq!(
            grant_bar_title("ana@example.com", &withheld_permissions(withheld)),
            "ana@example.com has not allowed Penguin Mail to manage calendars"
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
