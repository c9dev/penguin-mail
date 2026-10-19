//! The model behind the banner that asks Gmail accounts to choose a new
//! way to sign in, and the rule for when the sheet shows. Nothing here
//! touches GTK.

use std::collections::HashMap;

use mailrs_domain::translate::{fill, fill_plural, gettext};
use mailrs_domain::{Account, AccountId, AccountState, GmailSignIn, Provider};

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
    /// The button signs the one account in through Online Accounts.
    pub goa_now: bool,
    /// Some account's mail has stopped, which the banner's tint shows.
    pub paused: bool,
}

/// The accounts that have to move, in the order given.
pub fn waiting_for_move(
    accounts: &[Account],
    sign_ins: &HashMap<AccountId, GmailSignIn>,
) -> Vec<Waiting> {
    accounts
        .iter()
        .filter(|account| needs_move(account, sign_ins.get(&account.id)))
        .map(|account| Waiting {
            account: account.clone(),
            paused: account.state == AccountState::MailPaused,
        })
        .collect()
}

/// The banner for `waiting`, or none when nothing waits. `holds` says
/// whether Online Accounts is already signed in as an address.
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
        gettext("Use This Account")
    } else {
        gettext("Choose…")
    };
    Some(MoveBanner {
        title,
        button,
        accounts,
        goa_now,
        paused,
    })
}

/// Whether `account` needs a move, by how it signs in.
fn needs_move(account: &Account, sign_in: Option<&GmailSignIn>) -> bool {
    account.provider == Provider::Gmail && sign_in.is_some_and(GmailSignIn::waits_to_move)
}

/// Where a press that signs `account` in, or connects its calendar, leads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Path {
    /// The account needs a move: the route choice.
    Move,
    /// The account signs in the way it always has.
    Usual,
}

/// [`Path`] for `account`.
pub fn path(account: &Account, sign_in: Option<&GmailSignIn>) -> Path {
    match needs_move(account, sign_in) {
        true => Path::Move,
        false => Path::Usual,
    }
}

/// Whether `account` gets a Grant Access banner. One that needs a move
/// gets none: a consent through the built-in client would return a token without the mail scope and
/// overwrite the working one. An Online Accounts account gets none: its
/// own banners name what Online Accounts needs.
pub fn grant_banner_wanted(account: &Account, sign_in: Option<&GmailSignIn>) -> bool {
    !needs_move(account, sign_in) && !on_online_accounts(sign_in)
}

fn on_online_accounts(sign_in: Option<&GmailSignIn>) -> bool {
    sign_in.is_some_and(GmailSignIn::on_online_accounts)
}

/// Whether `account` gets the Sign In banner: it needs to sign in again and
/// does not need a move, which has its own banner and its own sign-in.
pub fn sign_in_banner_wanted(account: &Account, sign_in: Option<&GmailSignIn>) -> bool {
    account.state == AccountState::NeedsReauth
        && !needs_move(account, sign_in)
        && !on_online_accounts(sign_in)
}

/// Whether the sheet shows now: it has not shown before and an account
/// waits.
pub fn sheet_due(shown: bool, waiting: &[Waiting]) -> bool {
    !shown && !waiting.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailrs_domain::AccountState;
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

    /// Grant Access in the calendar sidebar, the Calendar and Contacts
    /// Connect rows and the permission questions all go through
    /// `App::sign_in_path`. In this build, with no client from the
    /// environment, each leads a waiting account to the route choice,
    /// whose own-project row opens the setup; none is left doing nothing.
    #[test]
    fn every_control_of_a_waiting_account_leads_to_the_route_choice() {
        use crate::route_choice::{Platform, Route, ready_now, rows};
        for platform in [Platform::Linux, Platform::MacOs] {
            let ready = ready_now(crate::route_choice::Goa::Absent);
            assert!(
                rows(platform, ready)
                    .iter()
                    .any(|row| row.route == Route::OwnProject)
            );
            let built_in_api = GmailSignIn::BuiltInApi;
            for state in [AccountState::Ok, AccountState::MailPaused] {
                assert_eq!(
                    path(&account(1, "dana@gmail.com", state), Some(&built_in_api)),
                    Path::Move
                );
            }
        }
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
        assert_eq!(banner.button, "Use This Account");
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
    fn the_sheet_shows_once_and_only_with_an_account_waiting() {
        let one = [waiting(1, "a@gmail.com", false)];
        assert!(sheet_due(false, &one));
        assert!(!sheet_due(true, &one));
        assert!(!sheet_due(false, &[]));
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
        sign_ins.insert(1, GmailSignIn::BuiltInApi);
        sign_ins.insert(2, GmailSignIn::BuiltInApi);
        sign_ins.insert(3, GmailSignIn::OwnProject);
        let mut accounts = accounts.to_vec();
        accounts[3].provider = Provider::Microsoft;
        let found = waiting_for_move(&accounts, &sign_ins);
        assert_eq!(
            found
                .iter()
                .map(|w| (w.account.id, w.paused))
                .collect::<Vec<_>>(),
            [(1, false), (2, true)]
        );
    }

    const BUILT_IN_API: &GmailSignIn = &GmailSignIn::BuiltInApi;

    const OWN_API: &GmailSignIn = &GmailSignIn::OwnProject;

    #[test]
    fn a_goa_account_gets_no_sign_in_or_grant_banner() {
        let goa = GmailSignIn::OnlineAccounts {
            goa_id: "account_1_0".into(),
        };
        let dana = account(3, "dana@gmail.com", AccountState::NeedsReauth);
        assert!(!sign_in_banner_wanted(&dana, Some(&goa)));
        assert!(!grant_banner_wanted(&dana, Some(&goa)));
        assert!(sign_in_banner_wanted(&dana, Some(OWN_API)));
    }

    #[test]
    fn an_account_that_needs_a_move_gets_no_grant_banner() {
        let dana = account(3, "dana@gmail.com", AccountState::Ok);
        assert!(!grant_banner_wanted(&dana, Some(BUILT_IN_API)));
        assert!(grant_banner_wanted(&dana, Some(OWN_API)));
        assert!(grant_banner_wanted(&dana, None));
    }

    #[test]
    fn only_a_move_account_reaches_the_route_choice() {
        let dana = account(3, "dana@gmail.com", AccountState::MailPaused);
        assert_eq!(path(&dana, Some(BUILT_IN_API)), Path::Move);
        assert_eq!(path(&dana, Some(OWN_API)), Path::Usual);
        assert_eq!(path(&dana, None), Path::Usual);
    }

    #[test]
    fn an_account_that_needs_a_move_gets_no_sign_in_banner() {
        let again = account(3, "c@gmail.com", AccountState::NeedsReauth);
        assert!(!sign_in_banner_wanted(&again, Some(BUILT_IN_API)));
        assert!(sign_in_banner_wanted(&again, Some(OWN_API)));
        assert!(sign_in_banner_wanted(&again, None));
        let fine = account(3, "c@gmail.com", AccountState::Ok);
        assert!(!sign_in_banner_wanted(&fine, None));
    }
}
