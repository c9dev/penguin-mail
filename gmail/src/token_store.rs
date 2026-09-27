use std::collections::HashMap;
use std::sync::Mutex;

use crate::GmailError;

/// Where refresh tokens live, keyed by account email. Implementations may
/// block, so async callers run them in `spawn_blocking`.
pub trait TokenStore: Send + Sync {
    fn load(&self, email: &str) -> Result<Option<String>, GmailError>;
    fn save(&self, email: &str, refresh_token: &str) -> Result<(), GmailError>;
    /// Succeeds when there is nothing to delete.
    fn delete(&self, email: &str) -> Result<(), GmailError>;
}

/// Refresh tokens under one service name: the desktop keyring through
/// Secret Service outside a Flatpak, and, in a Flatpak build, the Secret
/// portal's own encrypted store instead, which no other app can read.
pub struct KeyringTokenStore {
    service: String,
}

impl KeyringTokenStore {
    /// The app's name before it became Penguin Mail. Keeping it keeps
    /// existing accounts signed in.
    pub const SERVICE: &'static str = "mailrs";

    pub fn new() -> Self {
        Self::with_service(Self::SERVICE)
    }

    pub fn with_service(service: impl Into<String>) -> Self {
        KeyringTokenStore {
            service: service.into(),
        }
    }

    #[cfg(not(feature = "packaging-flatpak"))]
    fn entry(&self, email: &str) -> Result<keyring::Entry, GmailError> {
        keyring::Entry::new(&self.service, email).map_err(keyring_error)
    }
}

impl Default for KeyringTokenStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(not(feature = "packaging-flatpak"))]
impl TokenStore for KeyringTokenStore {
    fn load(&self, email: &str) -> Result<Option<String>, GmailError> {
        match self.entry(email)?.get_password() {
            Ok(token) => Ok(Some(token)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(err) => Err(keyring_error(err)),
        }
    }

    fn save(&self, email: &str, refresh_token: &str) -> Result<(), GmailError> {
        self.entry(email)?
            .set_password(refresh_token)
            .map_err(keyring_error)
    }

    fn delete(&self, email: &str) -> Result<(), GmailError> {
        match self.entry(email)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(err) => Err(keyring_error(err)),
        }
    }
}

#[cfg(not(feature = "packaging-flatpak"))]
fn keyring_error(err: keyring::Error) -> GmailError {
    GmailError::Keyring(err.to_string())
}

#[cfg(feature = "packaging-flatpak")]
impl TokenStore for KeyringTokenStore {
    fn load(&self, email: &str) -> Result<Option<String>, GmailError> {
        crate::secret_portal::load(&self.service, email).map_err(GmailError::Keyring)
    }

    fn save(&self, email: &str, refresh_token: &str) -> Result<(), GmailError> {
        crate::secret_portal::save(&self.service, email, refresh_token).map_err(GmailError::Keyring)
    }

    fn delete(&self, email: &str) -> Result<(), GmailError> {
        crate::secret_portal::delete(&self.service, email).map_err(GmailError::Keyring)
    }
}

/// Refresh tokens in memory, for tests.
#[derive(Default)]
pub struct MemoryTokenStore {
    tokens: Mutex<HashMap<String, String>>,
}

impl TokenStore for MemoryTokenStore {
    fn load(&self, email: &str) -> Result<Option<String>, GmailError> {
        Ok(self
            .tokens
            .lock()
            .expect("token map poisoned")
            .get(email)
            .cloned())
    }

    fn save(&self, email: &str, refresh_token: &str) -> Result<(), GmailError> {
        self.tokens
            .lock()
            .expect("token map poisoned")
            .insert(email.into(), refresh_token.into());
        Ok(())
    }

    fn delete(&self, email: &str) -> Result<(), GmailError> {
        self.tokens
            .lock()
            .expect("token map poisoned")
            .remove(email);
        Ok(())
    }
}
