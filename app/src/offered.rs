//! What the window and the assistant offer for each account, read from
//! the account's services, and the words for what an account lacks.

use mailrs_domain::translate::{fill, gettext};
use mailrs_domain::{AccountId, Provider};
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

/// One line saying why an account on `provider` lacks `missing`.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "Preferences reads this once it shows why a service is missing")
)]
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

    #[test]
    fn a_mailbox_that_is_not_an_inbox_never_shows_the_bar() {
        let sent = Mailbox::Unified(Standard::Sent);
        assert!(!shows_categories(true, &sent, &[1], |_| Offers::EVERYTHING));
    }
}
