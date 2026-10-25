//! Starting a message: which account it comes from, the identity it
//! carries, and whether it gets a signature. The window, the assistant,
//! the tray and the command line all open composers through here, so the
//! rules live in one place.

use std::rc::Rc;

use gtk::prelude::*;

use mailrs_domain::{Account, AccountId};

use super::App;
use crate::compose::Draft;
use crate::settings::{AccountUse, Change};
use crate::ui::composer::{Composer, Remembered, Writing};

/// Whether a message gets the signature of the address it comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signature {
    /// A message Penguin Mail starts: a new one, a reply, a forward.
    Add,
    /// A message a composer opened before, which added the signature
    /// then: a saved draft, an undone send, one back from the outbox. And
    /// one the person never wrote, such as an unsubscribe request.
    AsWritten,
}

/// The account a new message comes from: the one set in Preferences,
/// else `hint`, the account on screen, else the first. Only an account
/// with mail counts, so a calendar account never writes.
fn choose_account(
    accounts: &[Account],
    preferred: Option<&str>,
    hint: Option<AccountId>,
) -> Option<AccountId> {
    let writers = mailrs_domain::mail_accounts(accounts);
    preferred
        .and_then(|email| writers.iter().find(|a| a.email.eq_ignore_ascii_case(email)))
        .or_else(|| hint.and_then(|id| writers.iter().find(|a| a.id == id)))
        .or(writers.first())
        .map(|a| a.id)
}

impl App {
    /// The account a new message comes from. `hint` is the account the
    /// window shows, when it shows one.
    pub fn default_account(&self, hint: Option<AccountId>) -> Option<AccountId> {
        self.account_for(AccountUse::NewMessages, hint)
    }

    /// Applies a task default while keeping a live contextual account as fallback.
    pub fn account_for(&self, task: AccountUse, hint: Option<AccountId>) -> Option<AccountId> {
        let settings = self.settings.borrow();
        let accounts = self.accounts();
        let emails: Vec<_> = accounts
            .iter()
            .map(|account| account.email.as_str())
            .collect();
        let preferred = settings.preferred_live_account(task, &emails);
        choose_account(&accounts, preferred, hint)
    }

    /// A blank message from the account, carrying its identity.
    pub fn blank_draft(&self, account_id: AccountId) -> Draft {
        Draft::new(account_id, self.identity(account_id))
    }

    /// Opens a composer on a new, signed message to `to`, which may be
    /// empty, from the default account. False when there is no account.
    pub fn new_message(self: &Rc<Self>, hint: Option<AccountId>, to: &str) -> bool {
        let Some(account_id) = self.default_account(hint) else {
            return false;
        };
        let mut draft = self.blank_draft(account_id);
        draft.to = crate::compose::parse_recipients(to);
        self.open_composer(draft, Signature::Add);
        true
    }

    /// Opens a composer on `draft`.
    pub fn open_composer(
        self: &Rc<Self>,
        draft: Draft,
        signature: Signature,
    ) -> Option<Rc<Composer>> {
        let draft = self.signed_when(draft, signature);
        crate::ensure_gtk();
        self.apply_style();
        let identities = self.identities();
        if identities.is_empty() {
            return None;
        }
        self.reload_contacts();
        let writing = {
            let settings = self.settings.borrow();
            Writing {
                identities,
                signature: crate::compose::expand_signature(
                    settings
                        .signature_for(&self.account_email(draft.account_id), &draft.from.email),
                    &draft.from.email,
                ),
                last_used: settings
                    .last_sender
                    .iter()
                    .map(|(account, email)| (account.clone(), email.clone()))
                    .collect(),
                dictionaries: self.dictionaries(draft.account_id),
                remember: {
                    let app = Rc::downgrade(self);
                    Rc::new(move |learned| {
                        let Some(app) = app.upgrade() else { return };
                        app.change_settings(match learned {
                            Remembered::SentFrom { account, email } => {
                                Change::LastSender { account, email }
                            }
                            Remembered::Word(word) => Change::KeepWord(word),
                        });
                    })
                },
                check_attachments: settings.check_attachments,
                sign_by_default: settings.sign_by_default,
                encrypt_when_possible: settings.encrypt_when_possible,
            }
        };
        let this = Rc::downgrade(self);
        let format = self.settings.borrow().compose_format;
        let composer = Composer::open(
            Rc::clone(&self.core),
            writing,
            Rc::clone(&self.contacts),
            draft,
            format,
            move |draft, built, when| {
                // The composer opened on a signed draft, and what the
                // person kept of that signature is theirs to keep, so the
                // bytes it built are the ones that go.
                if let Some(app) = this.upgrade() {
                    app.send_built(draft, built, when);
                }
            },
        );
        self.window_opened();
        let keep = Rc::clone(&composer);
        let app = Rc::downgrade(self);
        composer.window().connect_destroy(move |_| {
            let _ = &keep;
            if let Some(app) = app.upgrade() {
                app.window_closed();
            }
        });
        Some(composer)
    }

    /// `draft`, with the signature when `signature` asks for it.
    pub(super) fn signed_when(&self, draft: Draft, signature: Signature) -> Draft {
        match signature {
            Signature::Add => self.signed(draft),
            Signature::AsWritten => draft,
        }
    }

    /// `draft` with the signature of the address it comes from. Gmail keeps
    /// one per send-as address, so a reply from an alias is signed as that
    /// alias.
    fn signed(&self, mut draft: Draft) -> Draft {
        let account = self.account_email(draft.account_id);
        let settings = self.settings.borrow();
        draft.markdown = crate::compose::with_signature(
            &draft.markdown,
            &crate::compose::expand_signature(
                settings.signature_for(&account, &draft.from.email),
                &draft.from.email,
            ),
        );
        draft
    }

    /// Opens a composer on a new message to `to`, or the window when there
    /// is no account to write from yet.
    pub(super) fn compose_to(self: &Rc<Self>, to: &str) {
        if !self.new_message(None, to) {
            self.show_window();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailrs_domain::AccountState;

    fn accounts() -> Vec<Account> {
        ["ana@example.com", "rui@example.com"]
            .iter()
            .zip(1..)
            .map(|(email, id)| Account {
                id,
                email: email.to_string(),
                state: AccountState::Ok,
                provider: mailrs_domain::Provider::Gmail,
                provider_name: None,
            })
            .collect()
    }

    #[test]
    fn the_preferred_account_wins_over_the_one_on_screen() {
        assert_eq!(
            choose_account(&accounts(), Some("RUI@example.com"), Some(1)),
            Some(2)
        );
    }

    #[test]
    fn with_no_preference_the_account_on_screen_writes() {
        assert_eq!(choose_account(&accounts(), None, Some(2)), Some(2));
    }

    #[test]
    fn a_preference_for_a_removed_account_falls_back_to_the_hint_then_the_first() {
        let gone = Some("old@example.com");
        assert_eq!(choose_account(&accounts(), gone, Some(2)), Some(2));
        assert_eq!(choose_account(&accounts(), gone, None), Some(1));
    }

    #[test]
    fn a_calendar_account_never_writes() {
        let mut all = accounts();
        all.insert(
            0,
            Account {
                id: 9,
                email: "robin@cloud.example.org".into(),
                state: AccountState::Ok,
                provider: mailrs_domain::Provider::CalDav,
                provider_name: None,
            },
        );
        assert_eq!(
            choose_account(&all, None, None),
            Some(1),
            "the first account with mail"
        );
        assert_eq!(
            choose_account(&all, Some("robin@cloud.example.org"), None),
            Some(1)
        );
        assert_eq!(
            choose_account(&all, None, Some(9)),
            Some(1),
            "nor as the account on screen"
        );
        assert_eq!(
            choose_account(&all[..1], None, None),
            None,
            "alone, it leaves nobody to write"
        );
    }

    #[test]
    fn no_account_writes_nothing() {
        assert_eq!(choose_account(&[], None, None), None);
    }
}
