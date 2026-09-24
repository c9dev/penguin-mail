//! A user name and password for one server.

use std::fmt;

/// What the account signs in with. `Debug` prints the user and never the
/// password, so a login can sit in a struct that the log prints.
#[derive(Clone, PartialEq, Eq)]
pub struct Login {
    pub user: String,
    pub password: String,
}

impl Login {
    pub fn new(user: impl Into<String>, password: impl Into<String>) -> Self {
        Login {
            user: user.into(),
            password: password.into(),
        }
    }
}

impl fmt::Debug for Login {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Login")
            .field("user", &self.user)
            .field("password", &"<hidden>")
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::Login;

    #[test]
    fn debug_shows_the_user_and_hides_the_password() {
        let login = Login::new("ann@example.com", "hunter2-secret");
        let printed = format!("{login:?} {login:#?}");
        assert!(printed.contains("ann@example.com"));
        assert!(!printed.contains("hunter2-secret"));
    }
}
