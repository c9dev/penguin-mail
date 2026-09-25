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
use mailrs_sync::Withheld;

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
    pub const ALL: [Permission; 5] = [
        Permission::Settings,
        Permission::Delete,
        Permission::Contacts,
        Permission::ChangeContacts,
        Permission::Calendar,
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
/// one permission, so either withheld field names it once.
pub fn withheld_permissions(withheld: Withheld) -> Vec<Permission> {
    [
        (withheld.settings, Permission::Settings),
        (withheld.delete, Permission::Delete),
        (withheld.contacts, Permission::Contacts),
        (withheld.change_contacts, Permission::ChangeContacts),
        (withheld.calendar || withheld.calendar_list, Permission::Calendar),
    ]
    .into_iter()
    .filter_map(|(missing, permission)| missing.then_some(permission))
    .collect()
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

#[cfg(test)]
mod tests {
    use super::*;

    const OCCASIONS: [Occasion; 2] = [Occasion::Needed, Occasion::Offer];

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
}
