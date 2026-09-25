//! Where an IMAP account's password lives: the desktop keyring, under one
//! service name for every IMAP account, with the account's id as the
//! user. The store keeps the servers and never the password.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use mailrs_domain::AccountId;
use mailrs_domain::translate::{fill, gettext};

/// The keyring would not read, keep or delete a password.
#[derive(Debug, Clone, thiserror::Error)]
pub enum PasswordError {
    #[error("{}", refused(.0))]
    Keyring(String),
}

fn refused(reason: &str) -> String {
    fill(&gettext("The keyring refused: {reason}"), &[("reason", reason)])
}

/// Passwords by account. Implementations may block, so async callers run
/// them in `spawn_blocking`.
pub trait PasswordStore: Send + Sync {
    fn load(&self, account_id: AccountId) -> Result<Option<String>, PasswordError>;
    fn save(&self, account_id: AccountId, password: &str) -> Result<(), PasswordError>;
    /// Succeeds when there is nothing to delete.
    fn delete(&self, account_id: AccountId) -> Result<(), PasswordError>;
}

/// Passwords in the desktop keyring through Secret Service.
pub struct KeyringPasswords {
    service: String,
}

impl KeyringPasswords {
    /// One service for every IMAP account, apart from Google's refresh
    /// tokens under `mailrs`, so removing one kind never touches the other.
    pub const SERVICE: &'static str = "penguin-mail-imap";

    pub fn new() -> Self {
        KeyringPasswords {
            service: Self::SERVICE.to_string(),
        }
    }

    fn entry(&self, account_id: AccountId) -> Result<keyring::Entry, PasswordError> {
        keyring::Entry::new(&self.service, &user(account_id)).map_err(keyring_error)
    }
}

impl Default for KeyringPasswords {
    fn default() -> Self {
        Self::new()
    }
}

/// The keyring's user name for an account: its id. Each account row owns
/// one entry, and the entry stays with the row if its address changes.
fn user(account_id: AccountId) -> String {
    account_id.to_string()
}

impl PasswordStore for KeyringPasswords {
    fn load(&self, account_id: AccountId) -> Result<Option<String>, PasswordError> {
        match self.entry(account_id)?.get_password() {
            Ok(password) => Ok(Some(password)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(err) => Err(keyring_error(err)),
        }
    }

    fn save(&self, account_id: AccountId, password: &str) -> Result<(), PasswordError> {
        self.entry(account_id)?
            .set_password(password)
            .map_err(keyring_error)
    }

    fn delete(&self, account_id: AccountId) -> Result<(), PasswordError> {
        match self.entry(account_id)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(err) => Err(keyring_error(err)),
        }
    }
}

fn keyring_error(err: keyring::Error) -> PasswordError {
    PasswordError::Keyring(err.to_string())
}

/// Passwords in memory, for tests and the demo, which must never reach
/// the person's keyring.
#[derive(Default)]
pub struct MemoryPasswords {
    passwords: Mutex<HashMap<AccountId, String>>,
}

impl PasswordStore for MemoryPasswords {
    fn load(&self, account_id: AccountId) -> Result<Option<String>, PasswordError> {
        let passwords = self.passwords.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(passwords.get(&account_id).cloned())
    }

    fn save(&self, account_id: AccountId, password: &str) -> Result<(), PasswordError> {
        let mut passwords = self.passwords.lock().unwrap_or_else(PoisonError::into_inner);
        passwords.insert(account_id, password.to_string());
        Ok(())
    }

    fn delete(&self, account_id: AccountId) -> Result<(), PasswordError> {
        let mut passwords = self.passwords.lock().unwrap_or_else(PoisonError::into_inner);
        passwords.remove(&account_id);
        Ok(())
    }
}

/// The app's password store: the keyring, or memory in the demo. One
/// type, so the core holds one field whichever it runs with.
pub enum Passwords {
    Keyring(KeyringPasswords),
    Memory(MemoryPasswords),
}

impl PasswordStore for Passwords {
    fn load(&self, account_id: AccountId) -> Result<Option<String>, PasswordError> {
        match self {
            Passwords::Keyring(store) => store.load(account_id),
            Passwords::Memory(store) => store.load(account_id),
        }
    }

    fn save(&self, account_id: AccountId, password: &str) -> Result<(), PasswordError> {
        match self {
            Passwords::Keyring(store) => store.save(account_id, password),
            Passwords::Memory(store) => store.save(account_id, password),
        }
    }

    fn delete(&self, account_id: AccountId) -> Result<(), PasswordError> {
        match self {
            Passwords::Keyring(store) => store.delete(account_id),
            Passwords::Memory(store) => store.delete(account_id),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{KeyringPasswords, MemoryPasswords, PasswordStore, user};

    #[test]
    fn every_imap_password_sits_under_one_service_by_account_id() {
        assert_eq!(KeyringPasswords::SERVICE, "penguin-mail-imap");
        assert_eq!(user(7), "7");
    }

    #[test]
    fn a_password_comes_back_until_it_is_deleted() {
        let store = MemoryPasswords::default();
        assert_eq!(store.load(1).unwrap(), None);
        store.save(1, "secret").unwrap();
        assert_eq!(store.load(1).unwrap().as_deref(), Some("secret"));
        store.delete(1).unwrap();
        store.delete(1).unwrap();
        assert_eq!(store.load(1).unwrap(), None);
    }
}
