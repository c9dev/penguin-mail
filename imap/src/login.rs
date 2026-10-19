//! A user name and password for one server.

use std::fmt;
use std::future::Future;
use std::pin::Pin;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;

use crate::refusal::MAX_ERROR_TEXT;

/// What the account signs in with. `Debug` prints the user and never the
/// password, so a login can sit in a struct that the log prints.
#[derive(Clone, PartialEq, Eq)]
pub struct Login {
    pub user: String,
    pub password: String,
    pub(crate) oauth: bool,
}

impl Login {
    pub fn new(user: impl Into<String>, password: impl Into<String>) -> Self {
        Login {
            user: user.into(),
            password: password.into(),
            oauth: false,
        }
    }
    /// An OAuth access token for an IMAP connection.
    pub fn bearer(user: impl Into<String>, token: impl Into<String>) -> Self {
        let mut login = Self::new(user, token);
        login.oauth = true;
        login
    }
}

/// Where a client gets its login, asked again for each connection. A
/// password never changes, but an access token lasts an hour and a pooled
/// client outlives it.
pub trait LoginSource: Send + Sync + fmt::Debug {
    /// The login to sign in with. `refused` is true only on the one retry
    /// after the server turned the last login down.
    fn login(
        &self,
        refused: bool,
    ) -> Pin<Box<dyn Future<Output = Result<Login, crate::ImapError>> + Send + '_>>;
}

impl LoginSource for Login {
    fn login(
        &self,
        _refused: bool,
    ) -> Pin<Box<dyn Future<Output = Result<Login, crate::ImapError>> + Send + '_>> {
        Box::pin(async move { Ok(self.clone()) })
    }
}

/// Signs in through `sign_in` with `source`'s login, and answers what it
/// opened with the login that opened it. A token the server turned down
/// may only have run out, so the source is asked for a fresh one, once; a
/// password is never tried twice.
pub(crate) async fn signed_in<T, F, Fut>(
    source: &dyn LoginSource,
    sign_in: F,
) -> Result<(T, Login), crate::ImapError>
where
    F: Fn(Login) -> Fut,
    Fut: Future<Output = Result<T, crate::ImapError>>,
{
    let login = source.login(false).await?;
    // Boxed to keep this future small: every call that may open a
    // connection carries it.
    match Box::pin(sign_in(login.clone())).await {
        Err(crate::ImapError::Auth { .. }) if login.oauth => {
            let login = source.login(true).await?;
            let opened = Box::pin(sign_in(login.clone())).await?;
            Ok((opened, login))
        }
        other => other.map(|opened| (opened, login)),
    }
}

/// What stands where the password was.
const HIDDEN: &str = "<hidden>";

impl Login {
    pub(crate) fn bearer_response(&self) -> String {
        let user = self.user.replace('=', "=3D").replace(',', "=2C");
        format!("n,a={user},\x01auth=Bearer {}\x01\x01", self.password)
    }

    /// The one message of Google's and Microsoft's XOAUTH2.
    pub(crate) fn xoauth2_response(&self) -> String {
        format!(
            "user={}\x01auth=Bearer {}\x01\x01",
            self.user, self.password
        )
    }

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
        if !cut_inside_a_character(&mut text, &secrets) && clipped {
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
        if self.oauth {
            secrets.push(BASE64.encode(self.bearer_response()));
            secrets.push(BASE64.encode(self.xoauth2_response()));
        }
        secrets.sort_by_key(|secret| std::cmp::Reverse(secret.len()));
        secrets.dedup();
        secrets
    }
}

/// Takes off the end of a clipped `text` when it is the start of one of
/// `secrets`, two characters or more. A text can end inside more than one
/// form at once, so the longest such start goes.
fn cut_partial(text: &mut String, secrets: &[String]) {
    let cut = secrets
        .iter()
        .flat_map(|secret| {
            secret
                .char_indices()
                .map(|(at, _)| at)
                .skip(2)
                .filter(|&at| text.ends_with(&secret[..at]))
        })
        .max();
    if let Some(at) = cut {
        text.truncate(text.len() - at);
        text.push_str(HIDDEN);
    }
}

/// Takes off the end of `text` when it is the start of one of `secrets`
/// cut partway through a character: one character or more of the secret,
/// then the U+FFFD that lossy decoding left for the bytes of the next,
/// which is not ASCII. imap-proto decodes 8-bit server text lossily, so a
/// server that clips its own line inside the password ends it this way at
/// any length. The longest such start goes. Answers whether it cut.
fn cut_inside_a_character(text: &mut String, secrets: &[String]) -> bool {
    let before = text.trim_end_matches(char::REPLACEMENT_CHARACTER);
    if before.len() == text.len() {
        return false;
    }
    let cut = secrets
        .iter()
        .flat_map(|secret| {
            secret
                .char_indices()
                .skip(1)
                .filter(|&(at, next)| !next.is_ascii() && before.ends_with(&secret[..at]))
                .map(|(at, _)| at)
        })
        .max();
    let Some(at) = cut else {
        return false;
    };
    text.truncate(before.len() - at);
    text.push_str(HIDDEN);
    true
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
    fn a_server_cannot_repeat_the_encoded_bearer_token_in_an_error() {
        let login = Login::bearer("ann@example.com", "sensitive-token");
        let encoded = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            login.bearer_response(),
        );
        assert_eq!(
            login.hide(&format!("NO rejected {encoded}")),
            "NO rejected <hidden>"
        );
    }

    #[test]
    fn a_server_cannot_repeat_the_encoded_xoauth2_message_in_an_error() {
        let login = Login::bearer("ann@example.com", "sensitive-token");
        let encoded = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            login.xoauth2_response(),
        );
        assert_eq!(
            login.hide(&format!("NO rejected {encoded}")),
            "NO rejected <hidden>"
        );
    }

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

    /// A text can end inside a longer part of one form than of another:
    /// here two characters of the Debug-escaped quoted form, and six of
    /// the quoted one. The longest match goes.
    #[test]
    fn a_clipped_text_loses_the_longest_partial_form_it_ends_with() {
        let login = Login::new("ann", "ab\"ab\"ab");
        let head = "x".repeat(MAX_ERROR_TEXT - 6);
        let hidden = login.hide(&format!("{head}ab\\\"ab"));
        assert_eq!(hidden, format!("{head}<hidden>"));
    }

    /// A server that cuts its own line partway through a character of the
    /// password leaves bytes that decode to U+FFFD, at any length.
    #[test]
    fn a_text_cut_inside_a_character_of_the_password_loses_that_part() {
        let login = Login::new("ann", "secretä-word");
        let hidden = login.hide("NO login failed for \"secret\u{FFFD}");
        assert_eq!(hidden, "NO login failed for \"<hidden>");
    }

    /// One character is enough to cut: "p" is a fragment of "pä…" too.
    #[test]
    fn a_text_cut_after_the_first_character_of_the_password_loses_it() {
        let login = Login::new("ann", "päss");
        assert_eq!(login.hide("NO bad \"p\u{FFFD}"), "NO bad \"<hidden>");
    }

    #[test]
    fn a_character_the_password_does_not_start_with_stays_before_a_cut() {
        let login = Login::new("ann", "päss");
        let text = "NO bad \"x\u{FFFD}";
        assert_eq!(login.hide(text), text);
    }

    #[test]
    fn a_cut_text_with_no_part_of_the_password_stays_whole() {
        let login = Login::new("ann", "päss");
        let text = "NO the server said \u{FFFD}\u{FFFD}";
        assert_eq!(login.hide(text), text);
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
