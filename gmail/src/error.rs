use std::time::Duration;

#[derive(Debug, Clone, thiserror::Error)]
pub enum GmailError {
    /// Google rejected the refresh token. Only a new consent fixes this.
    #[error("authorization expired or was revoked; add the account again")]
    NeedsReauth,
    #[error("Gmail rate limit hit")]
    RateLimited { retry_after: Option<Duration> },
    /// The account never granted a scope this call needs.
    #[error("Penguin Mail needs more access to this account; grant it and try again")]
    MissingScope,
    /// The Google Cloud project behind the OAuth client has this API
    /// switched off. Google refuses before any question of permission, so
    /// only turning it on at `enable_url` helps.
    #[error("{service} is switched off in the Google Cloud project; turn it on at {enable_url}")]
    ApiDisabled { service: String, enable_url: String },
    /// The sync token has aged out. Google answers this rather than send
    /// changes it no longer holds; the caller reads everything again.
    #[error("the sync token expired; read it all again")]
    ExpiredSyncToken,
    #[error("not found")]
    NotFound,
    #[error("Gmail returned HTTP {status}: {body}")]
    Http { status: u16, body: String },
    #[error("network error: {0}")]
    Network(String),
    #[error("could not decode Gmail response: {0}")]
    Decode(String),
    #[error("OAuth flow failed: {0}")]
    OAuth(String),
    #[error("keyring error: {0}")]
    Keyring(String),
}

impl GmailError {
    /// Failures worth retrying after a delay.
    pub fn is_transient(&self) -> bool {
        match self {
            GmailError::RateLimited { .. } | GmailError::Network(_) => true,
            GmailError::Http { status, .. } => *status >= 500,
            _ => false,
        }
    }
}

impl From<reqwest::Error> for GmailError {
    fn from(err: reqwest::Error) -> Self {
        if err.is_decode() {
            GmailError::Decode(err.to_string())
        } else {
            GmailError::Network(err.to_string())
        }
    }
}
