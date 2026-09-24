//! What the window and the assistant offer for each account, read from
//! the account's services, and the words for what an account lacks.

use mailrs_domain::Provider;
use mailrs_domain::translate::{fill, gettext};
use mailrs_sync::{AccountServices, Missing, Offers};

/// What an account offers. An account that is not running yet has no
/// services to ask, and the window assumes it offers everything until it
/// starts, so nothing flickers away.
pub fn offers_for(services: Option<&AccountServices>) -> Offers {
    services.map_or(Offers::EVERYTHING, AccountServices::offers)
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
}
