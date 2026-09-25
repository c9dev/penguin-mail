use std::time::Duration;

use mailrs_domain::AccountId;
use mailrs_domain::translate::{fill, gettext};
use mailrs_gmail::{GmailError, OneClickError};
use mailrs_store::StoreError;

/// What went wrong on the server's side, as a kind every provider shares.
/// The retry rules and the account states read the kind. An adapter maps
/// its own errors to the kinds that fit and keeps the rest in its own
/// variant, as `Gmail` does. While Gmail is the only provider, a kind that
/// stands for a Gmail error reads in Gmail's own words, so no message a
/// person sees changed when these kinds arrived.
#[derive(Debug, Clone, thiserror::Error)]
pub enum BackendError {
    /// The server no longer accepts the account's sign-in.
    #[error("authorization expired or was revoked; add the account again")]
    NeedsReauth,
    #[error("network error: {0}")]
    Offline(String),
    /// Too many calls; the server may say how long to wait.
    #[error("Gmail rate limit hit")]
    RateLimited(Option<Duration>),
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
    /// The provider has no such service or cannot do this.
    #[error("the server cannot do that")]
    Unsupported,
    /// The provider has the service switched off for the app, as a Google
    /// Cloud project can for an API. Only turning it on at `enable_url`
    /// helps, so no permission prompt is worth showing.
    #[error("{service} is switched off in the Google Cloud project; turn it on at {enable_url}")]
    ApiDisabled { service: String, enable_url: String },
    #[error(transparent)]
    Gmail(GmailError),
}

impl BackendError {
    /// Failures worth retrying after a delay.
    pub fn is_transient(&self) -> bool {
        match self {
            BackendError::Offline(_) | BackendError::RateLimited(_) => true,
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
    #[error("could not write the message: {0}")]
    Mime(String),
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
            GmailError::RateLimited { retry_after: None },
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
}
