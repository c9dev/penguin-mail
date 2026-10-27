//! What the window says when Online Accounts changes under a linked Gmail
//! account: which notice shows, and its words. Nothing here touches GTK.

use mailrs_domain::translate::{fill, gettext};
use mailrs_domain::{Account, AccountState};
use mailrs_sync::goa::{self, GoaAccount, GoaError};

/// How many failures in a row make "can't reach" worth saying.
const FAILURES_BEFORE_SAYING: u32 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notice {
    /// Online Accounts wants the person to sign in again.
    SignInAgain {
        address: String,
    },
    /// The account is gone from Online Accounts.
    Removed {
        address: String,
    },
    /// Mail is turned off for the account in Online Accounts.
    MailOff {
        address: String,
    },
    /// Google refused the sign-in Online Accounts handed over.
    ImapRefused {
        address: String,
    },
    Unreachable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    OpenOnlineAccounts,
}

/// A notice as the banner shows it: its words and its buttons.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoticeContent {
    pub title: String,
    pub buttons: Vec<(Action, String)>,
}

/// The notices to show, first one first. `linked` holds each account that
/// reads through Online Accounts with the Online Accounts id it saved.
pub fn notices(
    linked: &[(Account, Option<String>)],
    list: &Result<Vec<GoaAccount>, GoaError>,
    failures: u32,
) -> Vec<Notice> {
    let list = match list {
        Ok(list) => list,
        Err(_) if failures >= FAILURES_BEFORE_SAYING => return vec![Notice::Unreachable],
        Err(_) => return Vec::new(),
    };
    linked
        .iter()
        .filter_map(|(account, saved)| {
            let address = account.email.clone();
            let Some(found) = goa::find(list, saved.as_deref(), &account.email) else {
                return Some(Notice::Removed { address });
            };
            if found.attention_needed {
                Some(Notice::SignInAgain { address })
            } else if found.mail_disabled {
                Some(Notice::MailOff { address })
            } else if account.state == AccountState::NeedsReauth {
                Some(Notice::ImapRefused { address })
            } else {
                None
            }
        })
        .collect()
}

/// The words and buttons of `notice`.
pub fn shown(notice: &Notice) -> NoticeContent {
    let mut buttons = Vec::new();
    let title = match notice {
        Notice::SignInAgain { address } => {
            buttons.push((Action::OpenOnlineAccounts, open()));
            fill(
                // Translators: "Online Accounts" is the name GNOME and Linux Mint show for this window; use the name your desktop shows in your language. {address} is an email address.
                &gettext("Sign in to {address} again in Online Accounts."),
                &[("address", address)],
            )
        }
        Notice::Removed { address } => {
            buttons.push((Action::OpenOnlineAccounts, gettext("Add It Again")));
            fill(
                // Translators: "Online Accounts" is the name GNOME and Linux Mint show for this window; use the name your desktop shows in your language. {address} is an email address.
                &gettext("{address} was removed from Online Accounts. Your mail here is kept."),
                &[("address", address)],
            )
        }
        Notice::MailOff { address } => {
            buttons.push((Action::OpenOnlineAccounts, open()));
            fill(
                // Translators: "Online Accounts" is the name GNOME and Linux Mint show for this window; use the name your desktop shows in your language. {address} is an email address.
                &gettext("Mail is turned off for {address} in Online Accounts."),
                &[("address", address)],
            )
        }
        // Translators: Google and IMAP are names; keep them as they are.
        Notice::ImapRefused { .. } => gettext(
            "Google refused this computer's sign-in for mail. Is IMAP turned on for this account?",
        ),
        // Translators: "Online Accounts" is the name GNOME and Linux Mint show for this window; use the name your desktop shows in your language.
        Notice::Unreachable => gettext("Can't reach Online Accounts on this computer."),
    };
    NoticeContent { title, buttons }
}

fn open() -> String {
    // Translators: "Online Accounts" is the name GNOME and Linux Mint show for this window; use the name your desktop shows in your language.
    gettext("Open Online Accounts")
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailrs_domain::{AccountId, Provider};

    fn account(id: AccountId, state: AccountState) -> Account {
        Account {
            id,
            email: "dana@example.com".into(),
            state,
            provider: Provider::Gmail,
            provider_name: None,
        }
    }

    fn goa(id: &str, attention: bool, mail_disabled: bool) -> GoaAccount {
        GoaAccount {
            id: id.into(),
            identity: "dana@example.com".into(),
            presentation: "dana@example.com".into(),
            mail_disabled,
            attention_needed: attention,
        }
    }

    fn linked() -> Vec<(Account, Option<String>)> {
        vec![(account(1, AccountState::Ok), Some("account_1_0".into()))]
    }

    #[test]
    fn attention_needed_asks_for_a_sign_in_in_online_accounts() {
        let list = Ok(vec![goa("account_1_0", true, false)]);
        let found = notices(&linked(), &list, 0);
        let shown = shown(&found[0]);
        assert_eq!(
            shown.title,
            "Sign in to dana@example.com again in Online Accounts."
        );
        assert_eq!(
            shown.buttons,
            [(
                Action::OpenOnlineAccounts,
                "Open Online Accounts".to_string()
            )]
        );
    }

    #[test]
    fn a_removed_account_says_its_mail_is_kept() {
        let found = notices(&linked(), &Ok(Vec::new()), 0);
        let shown = shown(&found[0]);
        assert_eq!(
            shown.title,
            "dana@example.com was removed from Online Accounts. Your mail here is kept."
        );
        let labels: Vec<_> = shown.buttons.iter().map(|b| b.1.as_str()).collect();
        assert_eq!(labels, ["Add It Again"]);
    }

    #[test]
    fn the_same_identity_back_with_a_new_id_shows_nothing() {
        let list = Ok(vec![goa("account_9_0", false, false)]);
        assert_eq!(notices(&linked(), &list, 0), []);
    }

    #[test]
    fn mail_off_shows_its_banner() {
        let list = Ok(vec![goa("account_1_0", false, true)]);
        let found = notices(&linked(), &list, 0);
        assert_eq!(
            shown(&found[0]).title,
            "Mail is turned off for dana@example.com in Online Accounts."
        );
    }

    #[test]
    fn a_healthy_account_that_google_still_refuses_names_the_refusal() {
        let list = Ok(vec![goa("account_1_0", false, false)]);
        let refused = vec![(
            account(1, AccountState::NeedsReauth),
            Some("account_1_0".into()),
        )];
        let found = notices(&refused, &list, 0);
        assert_eq!(
            found,
            [Notice::ImapRefused {
                address: "dana@example.com".into()
            }]
        );
        assert_eq!(
            shown(&found[0]).title,
            "Google refused this computer's sign-in for mail. Is IMAP turned on for this account?"
        );
    }

    #[test]
    fn online_accounts_unreachable_waits_for_three_failures() {
        let down: Result<Vec<GoaAccount>, GoaError> = Err(GoaError::Unreachable("x".into()));
        assert_eq!(notices(&linked(), &down, 2), []);
        assert_eq!(notices(&linked(), &down, 3), [Notice::Unreachable]);
        assert!(shown(&Notice::Unreachable).buttons.is_empty());
    }

    fn every_notice() -> Vec<Notice> {
        let address = "dana@example.com".to_string();
        vec![
            Notice::SignInAgain {
                address: address.clone(),
            },
            Notice::Removed {
                address: address.clone(),
            },
            Notice::MailOff {
                address: address.clone(),
            },
            Notice::ImapRefused { address },
            Notice::Unreachable,
        ]
    }

    #[test]
    fn no_notice_names_gnome_or_a_protocol_word() {
        for notice in every_notice() {
            let shown = shown(&notice);
            let words = std::iter::once(shown.title.clone())
                .chain(shown.buttons.iter().map(|b| b.1.clone()));
            for text in words {
                for word in ["GNOME", "GOA", "D-Bus", "token", "scope"] {
                    assert!(!text.contains(word), "{text}");
                }
            }
        }
    }

    #[test]
    fn no_notice_gives_a_reason_for_the_sign_in_change() {
        for notice in every_notice() {
            let shown = shown(&notice);
            for word in ["verif", "CASA", "review", "free", "cost"] {
                assert!(!shown.title.contains(word), "{}", shown.title);
            }
        }
    }
}
