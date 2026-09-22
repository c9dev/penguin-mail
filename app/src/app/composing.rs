//! Starting a message: which account it comes from, the identity it
//! carries, and whether it gets a signature. The window, the assistant,
//! the tray and the command line all open composers through here, so the
//! rules live in one place.

use std::rc::Rc;

use gtk::prelude::*;

use mailrs_domain::{Account, AccountId};

use super::App;
use crate::compose::Draft;
use crate::settings::Change;
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
/// else `hint`, the account on screen, else the first.
fn choose_account(
    accounts: &[Account],
    preferred: Option<&str>,
    hint: Option<AccountId>,
) -> Option<AccountId> {
    preferred
        .and_then(|email| {
            accounts
                .iter()
                .find(|a| a.email.eq_ignore_ascii_case(email))
                .map(|a| a.id)
        })
        .or(hint)
        .or_else(|| accounts.first().map(|a| a.id))
}

impl App {
    /// The account a new message comes from. `hint` is the account the
    /// window shows, when it shows one.
    pub fn default_account(&self, hint: Option<AccountId>) -> Option<AccountId> {
        let preferred = self.settings.borrow().default_account.clone();
        choose_account(&self.accounts.borrow(), preferred.as_deref(), hint)
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
            move |draft, when| {
                // The composer opened on a signed draft, and what the
                // person kept of that signature is theirs to keep.
                if let Some(app) = this.upgrade() {
                    app.send(draft, when, Signature::AsWritten);
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

    /// Sends a request the person never wrote, such as the mail that
    /// leaves a mailing list: no signature, and no Undo delay, since
    /// nothing in it is theirs to take back.
    pub fn send_request(
        self: &Rc<Self>,
        account_id: AccountId,
        to: &str,
        subject: String,
        body: String,
    ) {
        let mut draft = self.blank_draft(account_id);
        draft.to = crate::compose::parse_recipients(to);
        draft.subject = subject;
        draft.markdown = body;
        self.send_immediately(draft);
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
            settings.signature_for(&account, &draft.from.email),
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
    fn no_account_writes_nothing() {
        assert_eq!(choose_account(&[], None, None), None);
    }
}
