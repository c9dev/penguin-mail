//! What the window and the assistant offer for each account, read from
//! the account's services, and the words for what an account lacks.

use mailrs_domain::translate::{fill, gettext};
use mailrs_domain::{Account, AccountId, Provider};
use mailrs_sync::{AccountServices, Mailbox, Missing, Offers};

/// What an account offers. An account that is not running yet has no
/// services to ask, and the window assumes it offers everything until it
/// starts, so nothing flickers away.
pub fn offers_for(services: Option<&AccountServices>) -> Offers {
    services.map_or(Offers::EVERYTHING, AccountServices::offers)
}

/// Whether the category bar shows over `mailbox`: the person has
/// categories on, the mailbox is an inbox, and an account it lists sorts
/// its inbox into categories. In the unified inbox one such account is
/// enough; mail from the others counts as Primary.
pub fn shows_categories(
    on: bool,
    mailbox: &Mailbox,
    accounts: &[AccountId],
    offers: impl Fn(AccountId) -> Offers,
) -> bool {
    on && mailbox.takes_categories()
        && match mailbox.account() {
            Some(id) => offers(id).categories,
            None => accounts.iter().any(|id| offers(*id).categories),
        }
}

/// How the accounts on screen file mail: with labels, several at once, or
/// in folders, one at a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filing {
    Labels,
    Folders,
}

impl Filing {
    /// Folders when every account in question files in folders, labels
    /// otherwise, including when there is no account in question.
    pub fn of(offers: impl IntoIterator<Item = Offers>) -> Filing {
        let mut any = false;
        for offer in offers {
            if offer.labels {
                return Filing::Labels;
            }
            any = true;
        }
        if any { Filing::Folders } else { Filing::Labels }
    }

    pub fn menu_item(self) -> String {
        match self {
            Filing::Labels => gettext("Labels…"),
            Filing::Folders => gettext("Move to Folder…"),
        }
    }

    /// The header button's tooltip, with its key.
    pub fn tooltip(self) -> String {
        match self {
            Filing::Labels => gettext("Labels (L)"),
            Filing::Folders => gettext("Move to Folder (L)"),
        }
    }

    pub fn new_item(self) -> String {
        match self {
            Filing::Labels => gettext("New Label…"),
            Filing::Folders => gettext("New Folder…"),
        }
    }

    /// The heading of the dialog that asks for a new one's name.
    pub fn new_heading(self) -> String {
        match self {
            Filing::Labels => gettext("New Label"),
            Filing::Folders => gettext("New Folder"),
        }
    }

    /// What that dialog says when the server refuses, with `{reason}`
    /// still to fill.
    pub fn create_failed(self) -> String {
        match self {
            Filing::Labels => gettext("Could not create the label: {reason}"),
            Filing::Folders => gettext("Could not create the folder: {reason}"),
        }
    }

    /// What the picker says when the account has nothing to file in yet.
    pub fn none_yet(self) -> String {
        match self {
            Filing::Labels => gettext("This account has no labels yet."),
            Filing::Folders => gettext("This account has no folders yet."),
        }
    }
}

/// One line saying why an account on `provider` lacks `missing`.
pub fn reason(provider: Provider, missing: Missing) -> String {
    let template = match missing {
        Missing::Calendar => gettext("{provider} has no calendar that other apps can reach."),
        Missing::Contacts => gettext("{provider} keeps no contacts that other apps can reach."),
        Missing::Rules => gettext("{provider} has no rules that other apps can change."),
        Missing::AutoReply => {
            gettext("{provider} has no automatic reply that other apps can change.")
        }
        Missing::DeleteForever => {
            gettext("{provider} cannot delete mail for good. Delete moves it to the Trash.")
        }
        Missing::Categories => gettext("{provider} does not sort the inbox into categories."),
    };
    fill(&template, &[("provider", provider.name())])
}

/// The lines Preferences shows for what accounts lack that no other row
/// covers: rules, the automatic reply and deleting for good, each as the
/// account's address and the reason. The contacts and calendar rows carry
/// their own reason, and the category bar needs none: it is not there.
pub fn missing_lines(accounts: &[(Account, Offers)]) -> Vec<(String, String)> {
    accounts
        .iter()
        .flat_map(|(account, offers)| {
            offers
                .missing()
                .into_iter()
                .filter(|m| {
                    matches!(m, Missing::Rules | Missing::AutoReply | Missing::DeleteForever)
                })
                .map(|m| (account.email.clone(), reason(account.provider, m)))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use mailrs_domain::Provider;
    use mailrs_sync::{Missing, Offers};

    use super::{offers_for, reason};

    #[test]
    fn each_missing_service_says_why_and_names_the_provider() {
        for missing in [
            Missing::Calendar,
            Missing::Contacts,
            Missing::Rules,
            Missing::AutoReply,
            Missing::DeleteForever,
            Missing::Categories,
        ] {
            let said = reason(Provider::Gmail, missing);
            assert!(said.starts_with("Gmail "), "{said}");
            assert!(said.ends_with('.'), "{said}");
        }
    }

    #[test]
    fn an_account_that_is_not_running_yet_hides_nothing() {
        assert_eq!(offers_for(None), Offers::EVERYTHING);
    }

    use mailrs_sync::Mailbox;
    use mailrs_sync::mailbox::Standard;

    use super::shows_categories;

    fn without_categories() -> Offers {
        Offers {
            categories: false,
            ..Offers::EVERYTHING
        }
    }

    #[test]
    fn a_gmail_inbox_shows_its_categories() {
        let inbox = Mailbox::Standard { account_id: 1, which: Standard::Inbox };
        assert!(shows_categories(true, &inbox, &[1], |_| Offers::EVERYTHING));
        assert!(!shows_categories(false, &inbox, &[1], |_| Offers::EVERYTHING));
    }

    #[test]
    fn an_inbox_whose_server_has_no_categories_hides_the_bar() {
        let inbox = Mailbox::Standard { account_id: 2, which: Standard::Inbox };
        assert!(!shows_categories(true, &inbox, &[2], |_| without_categories()));
    }

    #[test]
    fn the_unified_inbox_shows_the_bar_when_one_account_sorts() {
        let all = Mailbox::Unified(Standard::Inbox);
        let offers = |id| if id == 1 { Offers::EVERYTHING } else { without_categories() };
        assert!(shows_categories(true, &all, &[1, 2], offers));
        assert!(!shows_categories(true, &all, &[2], offers));
    }

    use super::Filing;

    fn folders() -> Offers {
        Offers {
            labels: false,
            ..Offers::EVERYTHING
        }
    }

    #[test]
    fn filing_reads_folders_only_when_every_account_files_in_folders() {
        assert_eq!(Filing::of([Offers::EVERYTHING]), Filing::Labels);
        assert_eq!(Filing::of([folders()]), Filing::Folders);
        assert_eq!(Filing::of([folders(), Offers::EVERYTHING]), Filing::Labels);
        assert_eq!(Filing::of([]), Filing::Labels);
        assert_eq!(Filing::Labels.menu_item(), "Labels…");
        assert_eq!(Filing::Folders.menu_item(), "Move to Folder…");
    }

    #[test]
    fn the_new_folder_dialog_says_folder_where_the_new_label_one_says_label() {
        assert_eq!(Filing::Labels.new_heading(), "New Label");
        assert_eq!(Filing::Folders.new_heading(), "New Folder");
        assert_eq!(
            Filing::Labels.create_failed(),
            "Could not create the label: {reason}"
        );
        assert_eq!(
            Filing::Folders.create_failed(),
            "Could not create the folder: {reason}"
        );
    }

    use mailrs_domain::{Account, AccountState};

    use super::missing_lines;

    #[test]
    fn preferences_names_what_each_account_lacks_and_nothing_for_gmail() {
        let gmail = Account {
            id: 1,
            email: "me@gmail.com".into(),
            state: AccountState::Ok,
            provider: Provider::Gmail,
        };
        let bare = Account {
            id: 2,
            email: "me@example.com".into(),
            ..gmail.clone()
        };
        let lacking = Offers {
            rules: false,
            auto_reply: false,
            ..Offers::EVERYTHING
        };
        let lines = missing_lines(&[(gmail, Offers::EVERYTHING), (bare, lacking)]);
        assert_eq!(
            lines,
            [
                ("me@example.com".to_string(), reason(Provider::Gmail, Missing::Rules)),
                ("me@example.com".to_string(), reason(Provider::Gmail, Missing::AutoReply)),
            ]
        );
    }

    #[test]
    fn a_mailbox_that_is_not_an_inbox_never_shows_the_bar() {
        let sent = Mailbox::Unified(Standard::Sent);
        assert!(!shows_categories(true, &sent, &[1], |_| Offers::EVERYTHING));
    }
}
