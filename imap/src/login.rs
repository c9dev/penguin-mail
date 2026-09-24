//! A user name and password for one server.

use std::fmt;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;

use crate::refusal::MAX_ERROR_TEXT;

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

/// What stands where the password was.
const HIDDEN: &str = "<hidden>";

impl Login {
    /// `text` with the password taken out, in every form a server could
    /// repeat it: as typed, as LOGIN quotes it, as Debug escapes either of
    /// those, and in the base64 that AUTH PLAIN and AUTH LOGIN send. Server text goes
    /// into errors, and errors go to the log.
    pub(crate) fn hide(&self, text: &str) -> String {
        let secrets = self.secrets();
        // Read before anything is replaced: a text at the limit may have
        // been clipped partway through the password.
        let clipped = text.chars().count() >= MAX_ERROR_TEXT;
        let mut text = text.to_string();
        for secret in &secrets {
            if text.contains(secret.as_str()) {
                text = text.replace(secret.as_str(), HIDDEN);
            }
        }
        if clipped {
            cut_partial(&mut text, &secrets);
        }
        text
    }

    /// The password's forms, longest first, so that a form containing a
    /// shorter one goes whole.
    fn secrets(&self) -> Vec<String> {
        if self.password.is_empty() {
            return Vec::new();
        }
        let password = &self.password;
        let quoted = password.replace('\\', "\\\\").replace('"', "\\\"");
        let mut secrets = vec![
            BASE64.encode(format!("\0{}\0{password}", self.user)),
            BASE64.encode(password),
            quoted.escape_debug().to_string(),
            quoted,
            password.escape_debug().to_string(),
            password.clone(),
        ];
        secrets.sort_by_key(|secret| std::cmp::Reverse(secret.len()));
        secrets.dedup();
        secrets
    }
}

/// Takes off the end of a clipped `text` when it is the start of one of
/// `secrets`, two characters or more.
fn cut_partial(text: &mut String, secrets: &[String]) {
    for secret in secrets {
        let cut = secret
            .char_indices()
            .map(|(at, _)| at)
            .skip(2)
            .filter(|&at| text.ends_with(&secret[..at]))
            .max();
        if let Some(at) = cut {
            text.truncate(text.len() - at);
            text.push_str(HIDDEN);
            return;
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
    use crate::refusal::MAX_ERROR_TEXT;
    use crate::testing::forms;

    #[test]
    fn hide_takes_out_every_form_of_the_password() {
        let login = Login::new("ann@example.com", "pä\"ss\\word 1");
        for form in forms(&login.user, &login.password) {
            let hidden = login.hide(&format!("535 no: {form} (sent)"));
            assert_eq!(hidden, "535 no: <hidden> (sent)", "{form}");
        }
    }

    #[test]
    fn a_clipped_text_that_ends_inside_the_password_loses_that_part() {
        let login = Login::new("ann", "hunter2 secret");
        let head = "x".repeat(MAX_ERROR_TEXT - 7);
        let hidden = login.hide(&format!("{head}hunter2"));
        assert!(!hidden.contains("hunter"), "{hidden}");
        assert!(hidden.ends_with("<hidden>"), "{hidden}");
    }

    #[test]
    fn a_text_under_the_limit_keeps_its_ending() {
        let login = Login::new("ann", "denied-secret");
        assert_eq!(login.hide("access denied"), "access denied");
    }

    #[test]
    fn debug_shows_the_user_and_hides_the_password() {
        let login = Login::new("ann@example.com", "hunter2-secret");
        let printed = format!("{login:?} {login:#?}");
        assert!(printed.contains("ann@example.com"));
        assert!(!printed.contains("hunter2-secret"));
    }
}
