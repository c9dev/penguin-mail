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
    /// Any other refusal. The body stays whole for the log, which prints
    /// the error with `{:?}`; what a person reads is Google's own message.
    #[error("{}", describe_http(*status, body))]
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

/// Google's own words for a refusal, when the body is its JSON error.
/// Anything else, such as a proxy's HTML page, is left out: a person
/// cannot read it and the log keeps it.
fn describe_http(status: u16, body: &str) -> String {
    let message = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|answer| {
            answer
                .pointer("/error/message")
                .and_then(|m| m.as_str())
                .map(str::to_string)
        })
        .filter(|m| !m.trim().is_empty());
    match message {
        Some(message) => format!("Gmail refused it: {message}"),
        None => format!("Gmail returned HTTP {status}"),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_in_googles_json_says_googles_message() {
        let body = r#"{"error":{"code":400,"message":"Invalid label name","status":"INVALID_ARGUMENT"}}"#;
        let err = GmailError::Http {
            status: 400,
            body: body.into(),
        };
        assert_eq!(err.to_string(), "Gmail refused it: Invalid label name");
    }

    #[test]
    fn a_body_that_is_not_googles_json_is_left_out() {
        let err = GmailError::Http {
            status: 502,
            body: "<html><body>Bad gateway</body></html>".into(),
        };
        assert_eq!(err.to_string(), "Gmail returned HTTP 502");
    }
}
