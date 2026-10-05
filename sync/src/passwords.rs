//! Where an IMAP account's password and a Microsoft account's refresh
//! token live: the desktop keyring, under one service name for each kind,
//! with the account's id as the user. The store keeps the servers and
//! never the secret.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use mailrs_domain::{Account, AccountId, Provider};
use mailrs_gmail::{KeyringTokenStore, MemoryTokenStore, TokenStore};
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

/// Passwords under one service name: the desktop keyring through Secret
/// Service outside a Flatpak, and, in a Flatpak build, the Secret
/// portal's own encrypted store instead, which no other app can read.
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

    /// Microsoft's refresh tokens, apart from IMAP passwords and Google's
    /// tokens, keyed by account id like the passwords. Both the desktop
    /// keyring and the Flatpak's Secret portal file key by service name,
    /// so this works on either.
    pub const MICROSOFT_SERVICE: &'static str = "penguin-mail-microsoft";

    pub fn microsoft() -> Self {
        KeyringPasswords {
            service: Self::MICROSOFT_SERVICE.to_string(),
        }
    }

    #[cfg(not(feature = "packaging-flatpak"))]
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

#[cfg(not(feature = "packaging-flatpak"))]
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

#[cfg(not(feature = "packaging-flatpak"))]
fn keyring_error(err: keyring::Error) -> PasswordError {
    PasswordError::Keyring(err.to_string())
}

#[cfg(feature = "packaging-flatpak")]
impl PasswordStore for KeyringPasswords {
    fn load(&self, account_id: AccountId) -> Result<Option<String>, PasswordError> {
        mailrs_gmail::secret_portal::load(&self.service, &user(account_id))
            .map_err(PasswordError::Keyring)
    }

    fn save(&self, account_id: AccountId, password: &str) -> Result<(), PasswordError> {
        mailrs_gmail::secret_portal::save(&self.service, &user(account_id), password)
            .map_err(PasswordError::Keyring)
    }

    fn delete(&self, account_id: AccountId) -> Result<(), PasswordError> {
        mailrs_gmail::secret_portal::delete(&self.service, &user(account_id))
            .map_err(PasswordError::Keyring)
    }
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

/// Where each kind of account keeps what signs it in: a Google account's
/// refresh token under its address, an IMAP or POP3 account's password
/// and a Microsoft account's refresh token under its id. Clones share the
/// stores.
#[derive(Clone)]
pub struct Secrets {
    pub google: Arc<dyn TokenStore>,
    pub passwords: Arc<Passwords>,
    pub microsoft: Arc<Passwords>,
}

impl Secrets {
    /// The desktop keyring, under the service names every earlier version
    /// used, so existing accounts find their secrets.
    pub fn keyring() -> Self {
        Secrets {
            google: Arc::new(KeyringTokenStore::new()),
            passwords: Arc::new(Passwords::Keyring(KeyringPasswords::new())),
            microsoft: Arc::new(Passwords::Keyring(KeyringPasswords::microsoft())),
        }
    }

    /// Memory, for the demo and tests, which must never reach the
    /// person's keyring.
    pub fn memory() -> Self {
        Secrets {
            google: Arc::new(MemoryTokenStore::default()),
            passwords: Arc::new(Passwords::Memory(MemoryPasswords::default())),
            microsoft: Arc::new(Passwords::Memory(MemoryPasswords::default())),
        }
    }

    /// Deletes what signs `account` in from the store its provider keeps
    /// it in, off the runtime. Succeeds when nothing was kept.
    pub async fn forget(&self, account: &Account) -> Result<(), PasswordError> {
        let id = account.id;
        let delete: Box<dyn FnOnce() -> Result<(), PasswordError> + Send> = match account.provider {
            Provider::Gmail => {
                let (tokens, email) = (Arc::clone(&self.google), account.email.clone());
                Box::new(move || {
                    tokens.delete(&email).map_err(|err| match err {
                        mailrs_gmail::GmailError::Keyring(reason) => PasswordError::Keyring(reason),
                        err => PasswordError::Keyring(err.to_string()),
                    })
                })
            }
            Provider::Imap | Provider::Pop3 => {
                let passwords = Arc::clone(&self.passwords);
                Box::new(move || passwords.delete(id))
            }
            Provider::Microsoft => {
                let tokens = Arc::clone(&self.microsoft);
                Box::new(move || tokens.delete(id))
            }
        };
        tokio::task::spawn_blocking(delete)
            .await
            .map_err(|err| PasswordError::Keyring(err.to_string()))?
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
