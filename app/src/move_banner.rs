//! The model behind the banner that asks Gmail accounts to choose a new
//! way to sign in, and the rule for when the sheet shows. Nothing here
//! touches GTK.

#![cfg_attr(
    not(test),
    expect(dead_code, reason = "the banner arrives in a later task")
)]

use std::collections::HashMap;

use mailrs_domain::translate::{fill, fill_plural, gettext};
use mailrs_domain::{Account, AccountId, AccountState};
use mailrs_store::accounts::GoogleSignIn;

/// A Gmail account that has to choose a route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Waiting {
    pub account: Account,
    /// Its mail stopped arriving, rather than being about to.
    pub paused: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveBanner {
    pub title: String,
    pub button: String,
    /// The accounts the button leads to, in the order given.
    pub accounts: Vec<AccountId>,
    /// The button signs the one account in through GNOME Online Accounts.
    pub goa_now: bool,
}

/// The accounts that have to move, in the order given. None while no route
/// row is ready: a banner with nothing to offer would only worry a person.
pub fn waiting_for_move(
    accounts: &[Account],
    sign_ins: &HashMap<AccountId, GoogleSignIn>,
    any_route: bool,
) -> Vec<Waiting> {
    if !any_route {
        return Vec::new();
    }
    accounts
        .iter()
        .filter(|account| {
            sign_ins
                .get(&account.id)
                .is_some_and(|sign_in| sign_in.needs_move(account.provider))
        })
        .map(|account| Waiting {
            account: account.clone(),
            paused: account.state == AccountState::MailPaused,
        })
        .collect()
}

/// The banner for `waiting`, or none when nothing waits. `holds` says
/// whether GNOME Online Accounts is already signed in as an address.
pub fn banner(waiting: &[Waiting], holds: impl Fn(&str) -> bool) -> Option<MoveBanner> {
    let accounts: Vec<AccountId> = waiting.iter().map(|w| w.account.id).collect();
    let paused = waiting.iter().any(|w| w.paused);
    let (title, goa_now) = match waiting {
        [] => return None,
        [one] => {
            let address = one.account.email.as_str();
            let title = if one.paused {
                // Translators: {address} is an email address.
                fill(
                    &gettext(
                        "Penguin Mail can't get new mail for {address}. Choose how it signs in to get your mail back. Nothing on this computer was lost.",
                    ),
                    &[("address", address)],
                )
            } else {
                // Translators: {address} is an email address.
                fill(
                    &gettext("Choose how {address} signs in to keep your mail arriving."),
                    &[("address", address)],
                )
            };
            (title, holds(address))
        }
        _ => {
            let count = waiting.len().to_string();
            let title = if paused {
                // Translators: Gmail is a name; keep it as it is.
                fill_plural(
                    "{count} Gmail account can't get new mail. Choose how each signs in to get your mail back. Nothing on this computer was lost.",
                    "{count} Gmail accounts can't get new mail. Choose how each signs in to get your mail back. Nothing on this computer was lost.",
                    waiting.len(),
                    &[("count", &count)],
                )
            } else {
                // Translators: Gmail is a name; keep it as it is.
                fill_plural(
                    "{count} Gmail account needs a new way to sign in",
                    "{count} Gmail accounts need a new way to sign in",
                    waiting.len(),
                    &[("count", &count)],
                )
            };
            (title, false)
        }
    };
    let button = if goa_now {
        // Translators: GNOME Online Accounts is a name; keep it as it is.
        gettext("Use GNOME Online Accounts")
    } else {
        gettext("Choose…")
    };
    Some(MoveBanner {
        title,
        button,
        accounts,
        goa_now,
    })
}

/// Whether the sheet shows now: it has not shown before, an account
/// waits, and some route row can be offered.
pub fn sheet_due(shown: bool, waiting: &[Waiting], any_route: bool) -> bool {
    !shown && any_route && !waiting.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailrs_domain::{AccountState, MailRoute, Provider, SignInClient};
    use mailrs_store::accounts::GoogleSignIn;

    fn account(id: i64, email: &str, state: AccountState) -> Account {
        Account {
            id,
            email: email.into(),
            state,
            provider: Provider::Gmail,
            provider_name: None,
        }
    }

    fn waiting(id: i64, email: &str, paused: bool) -> Waiting {
        Waiting {
            account: account(id, email, AccountState::Ok),
            paused,
        }
    }

    fn nobody(_: &str) -> bool {
        false
    }

    #[test]
    fn one_waiting_account_is_named() {
        let banner = banner(&[waiting(1, "dana@gmail.com", false)], nobody).unwrap();
        assert_eq!(
            banner.title,
            "Choose how dana@gmail.com signs in to keep your mail arriving."
        );
        assert_eq!(banner.button, "Choose…");
        assert_eq!(banner.accounts, [1]);
        assert!(!banner.goa_now);
    }

    #[test]
    fn several_waiting_accounts_share_one_banner() {
        let all = [
            waiting(3, "c@gmail.com", false),
            waiting(1, "a@gmail.com", false),
            waiting(2, "b@gmail.com", false),
        ];
        let banner = banner(&all, nobody).unwrap();
        assert_eq!(banner.title, "3 Gmail accounts need a new way to sign in");
        assert_eq!(banner.accounts, [3, 1, 2]);
        assert_eq!(banner.button, "Choose…");
    }

    #[test]
    fn a_paused_account_changes_the_words() {
        let banner = banner(&[waiting(1, "dana@gmail.com", true)], nobody).unwrap();
        assert_eq!(
            banner.title,
            "Penguin Mail can't get new mail for dana@gmail.com. Choose how it signs in to get your mail back. Nothing on this computer was lost."
        );
        assert_eq!(banner.button, "Choose…");
        let several = banner_of_two(true);
        assert_eq!(
            several.title,
            "2 Gmail accounts can't get new mail. Choose how each signs in to get your mail back. Nothing on this computer was lost."
        );
    }

    fn banner_of_two(paused: bool) -> MoveBanner {
        banner(
            &[
                waiting(1, "a@gmail.com", paused),
                waiting(2, "b@gmail.com", false),
            ],
            nobody,
        )
        .unwrap()
    }

    #[test]
    fn goa_holding_the_address_offers_one_click() {
        let banner = banner(&[waiting(1, "dana@gmail.com", false)], |email| {
            email == "dana@gmail.com"
        })
        .unwrap();
        assert_eq!(banner.button, "Use GNOME Online Accounts");
        assert!(banner.goa_now);
    }

    #[test]
    fn goa_one_click_needs_a_single_account() {
        let banner = banner(
            &[
                waiting(1, "a@gmail.com", false),
                waiting(2, "b@gmail.com", false),
            ],
            |_| true,
        )
        .unwrap();
        assert_eq!(banner.button, "Choose…");
        assert!(!banner.goa_now);
    }

    #[test]
    fn no_banner_without_waiting_accounts() {
        assert_eq!(banner(&[], nobody), None);
    }

    #[test]
    fn the_sheet_shows_once_and_only_with_a_route_to_offer() {
        let one = [waiting(1, "a@gmail.com", false)];
        assert!(sheet_due(false, &one, true));
        assert!(!sheet_due(true, &one, true));
        assert!(!sheet_due(false, &one, false));
        assert!(!sheet_due(false, &[], true));
    }

    #[test]
    fn built_in_api_accounts_wait_and_a_paused_one_says_so() {
        let accounts = [
            account(1, "a@gmail.com", AccountState::Ok),
            account(2, "b@gmail.com", AccountState::MailPaused),
            account(3, "c@gmail.com", AccountState::Ok),
            account(4, "d@outlook.com", AccountState::Ok),
        ];
        let mut sign_ins = std::collections::HashMap::new();
        let built_in = GoogleSignIn {
            client: SignInClient::BuiltIn,
            route: MailRoute::GmailApi,
        };
        sign_ins.insert(1, built_in);
        sign_ins.insert(2, built_in);
        sign_ins.insert(
            3,
            GoogleSignIn {
                client: SignInClient::Own,
                route: MailRoute::GmailApi,
            },
        );
        let mut accounts = accounts.to_vec();
        accounts[3].provider = Provider::Microsoft;
        let found = waiting_for_move(&accounts, &sign_ins, true);
        assert_eq!(
            found
                .iter()
                .map(|w| (w.account.id, w.paused))
                .collect::<Vec<_>>(),
            [(1, false), (2, true)]
        );
    }

    #[test]
    fn no_account_waits_while_no_route_row_is_ready() {
        let accounts = [account(1, "a@gmail.com", AccountState::Ok)];
        let mut sign_ins = std::collections::HashMap::new();
        sign_ins.insert(
            1,
            GoogleSignIn {
                client: SignInClient::BuiltIn,
                route: MailRoute::GmailApi,
            },
        );
        assert_eq!(waiting_for_move(&accounts, &sign_ins, false), []);
    }
}
