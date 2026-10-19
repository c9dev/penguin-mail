//! The words and rules of the page that signs in to Gmail with an app
//! password. Nothing here touches GTK, so the failures, the note and the
//! Sign In rule are tested without a display.

use mailrs_domain::translate::gettext;
use mailrs_imap::{CheckError, ImapError};
use mailrs_sync::sign_in::app_password::{GmailCheckError, read_pasted};

/// The page's tag in the dialog's navigation view.
pub const TAG: &str = "gmail-password";

/// Where a person turns on 2-Step Verification.
pub const TWO_STEP_URL: &str = "https://myaccount.google.com/signinoptions/twosv";
/// Where a person makes an app password.
pub const APP_PASSWORDS_URL: &str = "https://myaccount.google.com/apppasswords";
/// Gmail's Labels settings, where All Mail is shown to apps.
pub const LABELS_URL: &str = "https://mail.google.com/mail/u/0/#settings/labels";
/// Gmail's Send mail as setting.
pub const SEND_AS_URL: &str = "https://mail.google.com/mail/u/0/#settings/accounts";

/// Why a sign-in with an app password did not work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GmailFailure {
    /// The person typed their Google password.
    NeedsAppPassword,
    /// Google turned the app password down.
    WrongAppPassword,
    /// An administrator turned this way of signing in off.
    Organization,
    /// The server that answered is not Gmail's.
    NotGmail,
    /// Gmail leaves All Mail out of apps until the person turns it on.
    AllMailHidden,
    /// Anything else, in the words it already has.
    Other(String),
}

/// A button the failure offers under its words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    OpenAppPasswords,
    SignInAnotherWay,
    Back,
    OpenGmailLabels,
    TryAgain,
}

impl Action {
    pub fn label(self) -> String {
        match self {
            // Translators: App passwords is the name of Google's page; use the words Google's
            // own page shows in your language.
            Action::OpenAppPasswords => gettext("Open App passwords"),
            Action::SignInAnotherWay => gettext("Sign In Another Way"),
            Action::Back => gettext("Back"),
            // Translators: Gmail and Labels are names; use the words Gmail's own settings show
            // in your language.
            Action::OpenGmailLabels => gettext("Open Gmail Labels"),
            Action::TryAgain => gettext("Try Again"),
        }
    }
}

/// Reads why `err` happened.
pub fn failure(err: &anyhow::Error) -> GmailFailure {
    match err.downcast_ref::<GmailCheckError>() {
        Some(GmailCheckError::NotGmail) => return GmailFailure::NotGmail,
        Some(GmailCheckError::AllMailHidden) => return GmailFailure::AllMailHidden,
        Some(GmailCheckError::Check(check)) => return from_check(check, err),
        None => {}
    }
    match err.downcast_ref::<CheckError>() {
        Some(check) => from_check(check, err),
        None => GmailFailure::Other(err.to_string()),
    }
}

fn from_check(check: &CheckError, err: &anyhow::Error) -> GmailFailure {
    let (CheckError::Imap(inner) | CheckError::Smtp(inner)) = check;
    match inner {
        ImapError::Auth { text } => {
            let lower = text.to_lowercase();
            let over_smtp = matches!(check, CheckError::Smtp(_));
            let wants_app = lower.contains("application-specific password required")
                || (over_smtp && lower.contains("534") && lower.contains("5.7.9"));
            match wants_app {
                true => GmailFailure::NeedsAppPassword,
                false => GmailFailure::WrongAppPassword,
            }
        }
        ImapError::ImapDisabled { .. } => GmailFailure::Organization,
        _ => GmailFailure::Other(err.to_string()),
    }
}

impl GmailFailure {
    /// The failure in a sentence.
    pub fn words(&self) -> String {
        match self {
            // Translators: Google is a name; keep it as it is.
            GmailFailure::NeedsAppPassword => {
                gettext("Google wants an app password here, not your Google password.")
            }
            // Translators: Google is a name; keep it as it is.
            GmailFailure::WrongAppPassword => {
                gettext("Google didn't accept that app password. Make a new one and paste it here.")
            }
            GmailFailure::Organization => gettext(
                "Your organization turned off this way of signing in. Ask them, or sign in another way.",
            ),
            // Translators: Gmail is a name; keep it as it is.
            GmailFailure::NotGmail => gettext("This doesn't look like Gmail's server."),
            // Translators: Gmail and All Mail are names; use the words Gmail's own settings show
            // in your language for Labels, Show in IMAP and All Mail.
            GmailFailure::AllMailHidden => gettext(
                "Gmail hides All Mail from apps. In Gmail's settings, open Labels and tick Show in IMAP next to All Mail.",
            ),
            GmailFailure::Other(words) => words.clone(),
        }
    }

    /// The buttons under the words, in order.
    pub fn actions(&self) -> Vec<Action> {
        match self {
            GmailFailure::NeedsAppPassword | GmailFailure::WrongAppPassword => {
                vec![Action::OpenAppPasswords]
            }
            GmailFailure::Organization => vec![Action::SignInAnotherWay],
            GmailFailure::NotGmail => vec![Action::Back],
            GmailFailure::AllMailHidden => vec![Action::OpenGmailLabels, Action::TryAgain],
            GmailFailure::Other(_) => Vec::new(),
        }
    }
}

/// The line under the entry when the paste is not sixteen letters.
pub fn note(pasted: &str) -> Option<String> {
    (!pasted.trim().is_empty() && read_pasted(pasted).looks_wrong()).then(note_words)
}

fn note_words() -> String {
    // Translators: App passwords is the name of Google's page; use the words Google's own
    // page shows in your language.
    gettext("App passwords are 16 letters. This looks like a different password.")
}

/// Whether Sign In can run. A paste that looks wrong still signs in:
/// Google has the last word.
pub fn can_sign_in(address: &str, pasted: &str, running: bool) -> bool {
    !running && !address.trim().is_empty() && !pasted.trim().is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn imap_auth(text: &str) -> anyhow::Error {
        anyhow::Error::new(GmailCheckError::Check(CheckError::Imap(ImapError::Auth {
            text: text.into(),
        })))
    }

    fn smtp_auth(text: &str) -> anyhow::Error {
        anyhow::Error::new(GmailCheckError::Check(CheckError::Smtp(ImapError::Auth {
            text: text.into(),
        })))
    }

    #[test]
    fn a_normal_password_is_told_to_use_an_app_password() {
        let imap = imap_auth(
            "[ALERT] Application-specific password required: https://support.google.com/accounts/answer/185833 (Failure)",
        );
        let smtp = smtp_auth("534-5.7.9 Application-specific password required.");
        for err in [imap, smtp] {
            assert_eq!(failure(&err), GmailFailure::NeedsAppPassword);
        }
        assert_eq!(
            GmailFailure::NeedsAppPassword.words(),
            "Google wants an app password here, not your Google password."
        );
        assert_eq!(
            GmailFailure::NeedsAppPassword.actions(),
            [Action::OpenAppPasswords]
        );
    }

    #[test]
    fn invalid_credentials_is_a_wrong_app_password() {
        let err = imap_auth("[AUTHENTICATIONFAILED] Invalid credentials (Failure)");
        assert_eq!(failure(&err), GmailFailure::WrongAppPassword);
        assert_eq!(
            GmailFailure::WrongAppPassword.words(),
            "Google didn't accept that app password. Make a new one and paste it here."
        );
    }

    #[test]
    fn imap_off_is_the_organization() {
        let err = anyhow::Error::new(GmailCheckError::Check(CheckError::Imap(
            ImapError::ImapDisabled {
                text: "IMAP access is disabled for your domain".into(),
            },
        )));
        assert_eq!(failure(&err), GmailFailure::Organization);
        assert_eq!(
            GmailFailure::Organization.actions(),
            [Action::SignInAnotherWay]
        );
    }

    #[test]
    fn all_mail_hidden_names_show_in_imap() {
        let err = anyhow::Error::new(GmailCheckError::AllMailHidden);
        let failed = failure(&err);
        assert_eq!(failed, GmailFailure::AllMailHidden);
        assert!(failed.words().contains("Show in IMAP"));
        assert_eq!(
            failed.actions(),
            [Action::OpenGmailLabels, Action::TryAgain]
        );
        let other = anyhow::Error::new(GmailCheckError::NotGmail);
        assert_eq!(failure(&other), GmailFailure::NotGmail);
    }

    #[test]
    fn other_trouble_keeps_its_own_words() {
        let err = anyhow::anyhow!("The keyring is locked");
        assert_eq!(
            failure(&err),
            GmailFailure::Other("The keyring is locked".into())
        );
    }

    #[test]
    fn the_note_shows_only_for_a_paste_that_is_not_sixteen_letters() {
        assert_eq!(note(""), None);
        assert_eq!(note("abcd efgh ijkl mnop"), None);
        assert_eq!(
            note("hunter2").as_deref(),
            Some("App passwords are 16 letters. This looks like a different password.")
        );
    }

    #[test]
    fn sign_in_needs_an_address_a_paste_and_no_run_under_way() {
        assert!(can_sign_in("me@gmail.com", "hunter2", false));
        assert!(!can_sign_in("me@gmail.com", "", false));
        assert!(!can_sign_in("", "abcdefghijklmnop", false));
        assert!(!can_sign_in("me@gmail.com", "abcdefghijklmnop", true));
    }

    #[test]
    fn no_string_on_the_page_names_a_protocol_or_a_reason() {
        let failures = [
            GmailFailure::NeedsAppPassword,
            GmailFailure::WrongAppPassword,
            GmailFailure::Organization,
            GmailFailure::NotGmail,
            GmailFailure::AllMailHidden,
        ];
        let mut said: Vec<String> = failures.iter().map(GmailFailure::words).collect();
        said.push(note_words());
        for action in [
            Action::OpenAppPasswords,
            Action::SignInAnotherWay,
            Action::Back,
            Action::OpenGmailLabels,
            Action::TryAgain,
        ] {
            said.push(action.label());
        }
        for text in said {
            // The one place Google's own page names it.
            let text = text.replace("Show in IMAP", "");
            for banned in [
                "IMAP", "SMTP", "server", "port", "OAuth", "verif", "CASA", "review",
            ] {
                // "This doesn't look like Gmail's server." is the one
                // sentence that names what answered.
                if text == "This doesn't look like Gmail's server." {
                    continue;
                }
                assert!(!text.contains(banned), "{banned} in {text}");
            }
        }
    }
}
