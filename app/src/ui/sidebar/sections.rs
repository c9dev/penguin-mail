//! The order of the unified mailboxes and the headings between them.
//! Favorites come first, the other mailboxes after, then the smart
//! mailboxes and each account's own section. Every row keeps its place
//! whether or not it holds mail, so a row never moves under the pointer.
//! Only the flag colours come and go, one row per colour in use, since
//! they stand for colours rather than for mailboxes.

use mailrs_domain::Folder;
use mailrs_domain::translate::gettext;

use crate::ui::{Mailbox, Standard};

/// A heading in the sidebar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Favorites,
    Mailboxes,
    Smart,
    Accounts,
}

impl Section {
    pub fn title(self) -> String {
        match self {
            Section::Favorites => gettext("Favorites"),
            Section::Mailboxes => gettext("Mailboxes"),
            Section::Smart => gettext("Smart Mailboxes"),
            Section::Accounts => gettext("Accounts"),
        }
    }
}

/// One entry under a heading. `Vips` and `FlagColors` stand for a run of
/// rows the sidebar builds from what the person has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    Unified(Standard),
    /// The VIPs row and one row per VIP, while there are any.
    Vips,
    /// One row per flag colour in use, under Flagged.
    FlagColors,
    Outbox,
    Scheduled,
    Reminders,
    FollowUp,
    Folder(Folder),
}

/// The unified mailboxes under their two headings, in order.
pub const LAYOUT: [(Section, &[Place]); 2] = [
    (
        Section::Favorites,
        &[
            Place::Unified(Standard::Inbox),
            Place::Vips,
            Place::Unified(Standard::Flagged),
            Place::FlagColors,
            Place::FollowUp,
        ],
    ),
    (
        Section::Mailboxes,
        &[
            Place::Unified(Standard::Sent),
            Place::Unified(Standard::Drafts),
            Place::Unified(Standard::Muted),
            Place::Outbox,
            Place::Scheduled,
            Place::Reminders,
            Place::Folder(Folder::Archive),
            Place::Folder(Folder::Junk),
            Place::Folder(Folder::Trash),
            Place::Folder(Folder::AllMail),
        ],
    ),
];

/// The Outbox row's icon: its own tray, or a warning while `stuck`
/// messages wait there because sending them failed. The warning used to
/// show all the time, so the sidebar wore an alert sign with nothing
/// wrong.
pub fn outbox_icon(stuck: i64) -> &'static str {
    match stuck {
        ..=0 => "penguin-mail-outgoing-symbolic",
        _ => "dialog-warning-symbolic",
    }
}

/// Rows that show only while they hold something: the flag colours.
pub fn hidden_until_used(mailbox: &Mailbox) -> bool {
    matches!(mailbox, Mailbox::Flag(_))
}

#[cfg(test)]
mod tests {
    use mailrs_domain::FlagColor;

    use super::*;

    #[test]
    fn an_outbox_with_nothing_stuck_shows_its_tray() {
        assert_eq!(outbox_icon(0), "penguin-mail-outgoing-symbolic");
    }

    #[test]
    fn an_outbox_holding_a_failed_send_shows_a_warning() {
        assert_eq!(outbox_icon(2), "dialog-warning-symbolic");
    }

    #[test]
    fn favorites_lead_with_what_the_person_marked() {
        let (section, places) = LAYOUT[0];
        assert_eq!(section, Section::Favorites);
        assert_eq!(
            places,
            &[
                Place::Unified(Standard::Inbox),
                Place::Vips,
                Place::Unified(Standard::Flagged),
                Place::FlagColors,
                Place::FollowUp,
            ]
        );
    }

    #[test]
    fn the_mailboxes_run_from_sent_to_all_mail() {
        let (section, places) = LAYOUT[1];
        assert_eq!(section, Section::Mailboxes);
        assert_eq!(
            places,
            &[
                Place::Unified(Standard::Sent),
                Place::Unified(Standard::Drafts),
                Place::Unified(Standard::Muted),
                Place::Outbox,
                Place::Scheduled,
                Place::Reminders,
                Place::Folder(Folder::Archive),
                Place::Folder(Folder::Junk),
                Place::Folder(Folder::Trash),
                Place::Folder(Folder::AllMail),
            ]
        );
    }

    #[test]
    fn every_unified_mailbox_has_one_place() {
        let places: Vec<Place> = LAYOUT.iter().flat_map(|(_, p)| p.iter().copied()).collect();
        for which in Standard::ALL {
            assert_eq!(
                places
                    .iter()
                    .filter(|p| **p == Place::Unified(which))
                    .count(),
                1,
                "{which:?}"
            );
        }
        for folder in Folder::ALL {
            assert_eq!(
                places
                    .iter()
                    .filter(|p| **p == Place::Folder(folder))
                    .count(),
                1
            );
        }
    }

    #[test]
    fn an_empty_mailbox_keeps_its_row() {
        for mailbox in [
            Mailbox::Scheduled,
            Mailbox::Reminders,
            Mailbox::FollowUp,
            Mailbox::Outbox,
        ] {
            assert!(!hidden_until_used(&mailbox), "{mailbox:?}");
        }
        assert!(hidden_until_used(&Mailbox::Flag(FlagColor::Blue)));
    }

    #[test]
    fn the_headings_say_what_is_under_them() {
        assert_eq!(Section::Favorites.title(), "Favorites");
        assert_eq!(Section::Mailboxes.title(), "Mailboxes");
        assert_eq!(Section::Accounts.title(), "Accounts");
    }
}
