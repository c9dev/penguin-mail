use std::time::Duration;

use mailrs_domain::AccountId;
use mailrs_domain::translate::{fill, gettext};
use mailrs_gmail::{GmailError, OneClickError};
use mailrs_store::StoreError;

/// What went wrong on the server's side, as a kind every provider shares.
/// The retry rules and the account states read the kind. An adapter maps
/// its own errors to the kinds that fit and keeps the rest in its own
/// variant, as `Gmail` does. A shared kind says what happened without
/// naming a provider. The window names the account when it has one.
#[derive(Debug, Clone, thiserror::Error)]
pub enum BackendError {
    /// A structure read left out required text. Background prefetch retries
    /// with backoff rather than keeping or abandoning the incomplete body.
    #[error("{}", gettext("Message text could not be fetched completely"))]
    IncompleteBody,

    /// The server no longer accepts the account's sign-in.
    #[error("authorization expired or was revoked; add the account again")]
    NeedsReauth,
    #[error("network error: {0}")]
    Offline(String),
    /// Too many calls; the server may say how long to wait. The words do
    /// not name a provider, because Microsoft, IMAP, JMAP, POP3 and CalDAV
    /// report the same kind. `SyncError::for_person` names the account
    /// when the window knows it.
    #[error("{}", rate_limited(.0))]
    RateLimited(Option<Duration>),
    /// Gmail's IMAP will not hand out more mail today; the account waits
    /// an hour before it asks again.
    #[error("{}", download_limit())]
    DownloadLimit,
    #[error("not found")]
    NotFound,
    /// The server no longer knows the sync state it was handed, and the
    /// caller reads everything again. Only the call that reads changes can
    /// say this, since elsewhere a 404 means a message or a label is gone.
    #[error("the server lost its place")]
    StateLost,
    /// The account has not granted a permission this call needs: Gmail's
    /// missing scope, and later a Microsoft consent error.
    #[error("Penguin Mail needs more access to this account; grant it and try again")]
    NeedsPermission,
    #[error("refused: {0}")]
    Refused(String),
    /// The server holds another version of what this write changed.
    #[error("it changed elsewhere first")]
    Changed,
    /// The provider has no such service or cannot do this.
    #[error("the server cannot do that")]
    Unsupported,
    /// The provider has the service switched off for the app, as a Google
    /// Cloud project can for an API. Only turning it on at `enable_url`
    /// helps, so no permission prompt is worth showing.
    #[error("{service} is switched off in the Google Cloud project; turn it on at {enable_url}")]
    ApiDisabled { service: String, enable_url: String },
    /// A file to upload is no longer where it was picked. The path names
    /// where it was.
    #[error("the file is no longer at {0}")]
    FileMissing(String),
    /// The server runs a script of the person's own, cannot include it in
    /// Penguin Mail's, and the person has not said to replace it.
    #[error("the server runs your rules in “{script}” and cannot run two scripts")]
    WouldReplace { script: String },
    #[error(transparent)]
    Gmail(GmailError),
}

impl BackendError {
    /// Failures worth retrying after a delay.
    pub fn is_transient(&self) -> bool {
        match self {
            BackendError::Offline(_)
            | BackendError::RateLimited(_)
            | BackendError::DownloadLimit
            | BackendError::IncompleteBody => true,
            BackendError::Gmail(err) => err.is_transient(),
            _ => false,
        }
    }
}

/// Gmail's errors as kinds. Written by hand rather than derived, since a
/// derived conversion would wrap every Gmail error as `Gmail` and the
/// retry rules would never see a kind.
impl From<GmailError> for BackendError {
    fn from(err: GmailError) -> Self {
        match err {
            GmailError::NeedsReauth => BackendError::NeedsReauth,
            GmailError::Network(detail) => BackendError::Offline(detail),
            GmailError::RateLimited { retry_after } => BackendError::RateLimited(retry_after),
            GmailError::NotFound => BackendError::NotFound,
            GmailError::MissingScope => BackendError::NeedsPermission,
            GmailError::FileMissing(path) => BackendError::FileMissing(path),
            GmailError::ApiDisabled {
                service,
                enable_url,
            } => BackendError::ApiDisabled {
                service,
                enable_url,
            },
            other => BackendError::Gmail(other),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error(transparent)]
    Backend(#[from] BackendError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("no sync is running for account {0}")]
    UnknownAccount(AccountId),
    #[error("there is no label called {0}")]
    NoLabel(String),
    /// A new event named a calendar the account cannot write to, or one
    /// the copy does not hold.
    #[error("there is no calendar {0} to put the event on")]
    NoCalendar(String),
    /// A change to a birthday or a working location, which Google's own
    /// apps make and change, named by its title.
    #[error("{}", made_in_google(.0))]
    MadeInGoogle(String),
    #[error("could not write the message: {0}")]
    Mime(String),
    /// A file or stream an export writes to refused the bytes.
    #[error("could not write the export: {0}")]
    Write(String),
    #[error("{0} is not an email address")]
    NotAnAddress(String),
    /// A label name Gmail keeps for one of its own labels. Gmail would
    /// answer "Invalid label name", which does not say why.
    #[error("{}", reserved_label(.0))]
    ReservedLabel(String),
    /// A mailing list's own server turned down a one-click request, or
    /// never answered it. Gmail takes no part in that request.
    #[error("{}", one_click(.0))]
    OneClick(OneClickError),
    /// The keyring would not read or keep an IMAP account's password.
    #[error(transparent)]
    Password(#[from] crate::passwords::PasswordError),
    /// One step of starting an account, named here, waited longer than it
    /// may. The keyring waits for as long as an unlock prompt stays on
    /// screen, and a tray-only process has no window to show one over.
    #[error("{0} did not answer in time")]
    NoAnswer(&'static str),
}

impl SyncError {
    /// An export's write failed.
    pub(crate) fn io(err: std::io::Error) -> Self {
        Self::Write(err.to_string())
    }

    /// The words a person reads. A rate limit names `provider` when the
    /// caller knows which account asked the app to wait. An empty name
    /// keeps the provider-neutral sentence. Every other error keeps its
    /// own wording.
    pub fn for_person(&self, provider: &str) -> String {
        match self {
            SyncError::Backend(BackendError::RateLimited(_)) => Self::asked_to_wait(provider),
            other => other.to_string(),
        }
    }

    /// The rate-limit sentence. An empty `provider` stays neutral, because
    /// "The server" is not a name to fill into `{provider}`.
    pub fn asked_to_wait(provider: &str) -> String {
        if provider.is_empty() {
            rate_limited(&None)
        } else {
            fill(
                &gettext("{provider} asked Penguin Mail to wait"),
                &[("provider", provider)],
            )
        }
    }
}

/// A rate limit, without a provider. The window fills in the account
/// through `SyncError::for_person` when it has one.
fn rate_limited(_: &Option<Duration>) -> String {
    gettext("The server asked Penguin Mail to wait")
}

/// How long an account waits after Gmail's daily download limit.
pub const DOWNLOAD_LIMIT_WAIT: Duration = Duration::from_secs(3600);

pub(crate) fn download_limit() -> String {
    // Translators: shown beside a Gmail account that reads its mail over IMAP when Gmail stopped handing out mail for the day.
    gettext(
        "Gmail limits how much mail an app can download each day. Penguin Mail tries again later.",
    )
}

/// Lets `?` take a Gmail error where a sync error is due.
impl From<GmailError> for SyncError {
    fn from(err: GmailError) -> Self {
        SyncError::Backend(err.into())
    }
}

fn reserved_label(name: &str) -> String {
    fill(
        &gettext("Gmail keeps “{name}” for its own label. Choose another name."),
        &[("name", name)],
    )
}

fn made_in_google(title: &str) -> String {
    fill(
        &gettext("Google Calendar makes “{title}” itself, so only Google Calendar can change it."),
        &[("title", title)],
    )
}

fn one_click(err: &OneClickError) -> String {
    match err {
        OneClickError::Refused { host, status } => fill(
            &gettext("{host} refused the request to unsubscribe (HTTP {status})"),
            &[("host", host), ("status", &status.to_string())],
        ),
        OneClickError::Unreachable { host, .. } => {
            fill(&gettext("could not reach {host}"), &[("host", host)])
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn gmail_errors_become_the_kinds_they_mean() {
        assert!(matches!(
            BackendError::from(GmailError::NeedsReauth),
            BackendError::NeedsReauth
        ));
        assert!(matches!(
            BackendError::from(GmailError::Network("reset".into())),
            BackendError::Offline(detail) if detail == "reset"
        ));
        let wait = Some(Duration::from_secs(3));
        assert!(matches!(
            BackendError::from(GmailError::RateLimited { retry_after: wait }),
            BackendError::RateLimited(w) if w == wait
        ));
        assert!(matches!(
            BackendError::from(GmailError::NotFound),
            BackendError::NotFound
        ));
        assert!(matches!(
            BackendError::from(GmailError::MissingScope),
            BackendError::NeedsPermission
        ));
    }

    #[test]
    fn what_only_gmail_can_say_stays_gmails() {
        // People's expired sync token is a contacts matter, and nothing
        // outside a history call can tell a lost place from a missing
        // message, so neither becomes `StateLost` here.
        assert!(matches!(
            BackendError::from(GmailError::ExpiredSyncToken),
            BackendError::Gmail(GmailError::ExpiredSyncToken)
        ));
        assert!(matches!(
            BackendError::from(GmailError::Http {
                status: 400,
                body: String::new()
            }),
            BackendError::Gmail(GmailError::Http { status: 400, .. })
        ));
    }

    /// Google turning an API off in the Cloud project is a kind of its own,
    /// so the window and the assistant can explain it without reaching
    /// into Gmail's error type.
    #[test]
    fn a_switched_off_api_is_a_kind_every_provider_can_give() {
        let err = BackendError::from(GmailError::ApiDisabled {
            service: "Google Calendar API".into(),
            enable_url: "https://console.example/calendar".into(),
        });
        assert!(matches!(
            err,
            BackendError::ApiDisabled { ref service, ref enable_url }
                if service == "Google Calendar API" && enable_url == "https://console.example/calendar"
        ));
    }

    #[test]
    fn every_gmail_error_reads_as_it_did() {
        for err in [
            GmailError::NeedsReauth,
            GmailError::MissingScope,
            GmailError::ApiDisabled {
                service: "Google Calendar API".into(),
                enable_url: "https://console.example/calendar".into(),
            },
            GmailError::ExpiredSyncToken,
            GmailError::NotFound,
            GmailError::Http {
                status: 400,
                body: r#"{"error":{"message":"Invalid label name"}}"#.into(),
            },
            GmailError::Http {
                status: 502,
                body: String::new(),
            },
            GmailError::Network("reset".into()),
            GmailError::Decode("bad json".into()),
            GmailError::OAuth("denied".into()),
            GmailError::Keyring("locked".into()),
        ] {
            assert_eq!(SyncError::from(err.clone()).to_string(), err.to_string());
        }
    }

    /// The shared kind used to say "Gmail rate limit hit", which a
    /// Microsoft or IMAP account then showed. Gmail's own error keeps
    /// that sentence. The shared one does not.
    #[test]
    fn a_rate_limit_names_the_provider_when_one_is_known() {
        let gmail = GmailError::RateLimited { retry_after: None };
        assert_eq!(gmail.to_string(), "Gmail rate limit hit");
        let err = SyncError::from(gmail);
        assert_eq!(err.to_string(), "The server asked Penguin Mail to wait");
        assert_eq!(
            err.for_person("Outlook"),
            "Outlook asked Penguin Mail to wait"
        );
        assert_eq!(err.for_person(""), "The server asked Penguin Mail to wait");
        let missing = SyncError::from(GmailError::NotFound);
        assert_eq!(missing.for_person("Outlook"), missing.to_string());
    }
}
