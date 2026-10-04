//! What went wrong at Microsoft, as kinds the adapter maps onto the
//! backend errors every provider shares. The words are for the log;
//! whatever a person reads is written in `mailrs-sync` or the app, through
//! the translation catalogue.

use std::time::Duration;

#[derive(Debug, Clone, thiserror::Error)]
pub enum GraphError {
    /// Microsoft no longer takes the refresh token, or a fresh access
    /// token was refused too.
    #[error("Microsoft refused the sign-in; the account must sign in again")]
    NeedsReauth,
    #[error("network error: {0}")]
    Network(String),
    #[error("Microsoft asked to slow down")]
    Throttled { retry_after: Option<Duration> },
    #[error("not found")]
    NotFound,
    /// A delta link Graph no longer keeps (410, `syncStateNotFound`).
    #[error("Graph lost the place of a delta link")]
    SyncStateLost,
    /// Status 403. Either the token lacks the scope or the organization blocks
    /// the feature; only the caller, which knows the token's scopes, can
    /// tell the two apart.
    #[error("access denied ({code})")]
    AccessDenied { code: String },
    /// The mailbox lives on the organization's own Exchange server, which
    /// Graph cannot reach.
    #[error("the mailbox is on an on-premises Exchange server")]
    MailboxOnPremises,
    /// The organization lets no user consent to Penguin Mail on their own.
    #[error("the organization's administrator must approve the app")]
    AdminApproval,
    /// The person said no on Microsoft's consent page.
    #[error("the person declined Microsoft's consent")]
    Declined,
    /// The consent came back without mail, which Penguin Mail cannot work
    /// without.
    #[error("the sign-in did not grant access to mail")]
    MailNotGranted,
    /// Status 412: the `If-Match` etag is old.
    #[error("it changed elsewhere first")]
    PreconditionFailed,
    #[error("it conflicts with what Graph holds")]
    Conflict,
    #[error("the answer is larger than {limit} bytes")]
    TooLarge { limit: usize },
    /// A link Graph handed back names another host, and the token must
    /// not go there.
    #[error("refused to follow a link to {0}")]
    OffHost(String),
    #[error("HTTP {status} {code}: {message}")]
    Http {
        status: u16,
        code: String,
        message: String,
    },
    #[error("could not read Graph's answer: {0}")]
    Decode(String),
    #[error("sign-in failed: {0}")]
    OAuth(String),
}

impl GraphError {
    pub fn is_transient(&self) -> bool {
        matches!(self, GraphError::Network(_) | GraphError::Throttled { .. })
    }
}

/// Graph's error kind from the status and the `error.code` of its body.
/// The code wins where it names something the status does not.
pub fn classify(status: u16, code: &str, message: &str) -> GraphError {
    let lower = code.to_ascii_lowercase();
    if lower == "mailboxnotenabledforrestapi" || lower == "mailboxnotsupportedforrestapi" {
        return GraphError::MailboxOnPremises;
    }
    if status == 410 || lower.contains("syncstate") || lower == "resyncrequired" {
        return GraphError::SyncStateLost;
    }
    match status {
        401 => GraphError::NeedsReauth,
        403 => GraphError::AccessDenied {
            code: code.to_string(),
        },
        404 => GraphError::NotFound,
        409 => GraphError::Conflict,
        412 => GraphError::PreconditionFailed,
        429 | 503 => GraphError::Throttled { retry_after: None },
        _ => GraphError::Http {
            status,
            code: code.to_string(),
            message: message.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graph_error_codes_map_to_kinds() {
        assert!(matches!(
            classify(401, "InvalidAuthenticationToken", ""),
            GraphError::NeedsReauth
        ));
        assert!(matches!(
            classify(404, "ErrorItemNotFound", ""),
            GraphError::NotFound
        ));
        assert!(matches!(
            classify(410, "SyncStateNotFound", ""),
            GraphError::SyncStateLost
        ));
        assert!(matches!(
            classify(400, "syncStateInvalid", ""),
            GraphError::SyncStateLost
        ));
        assert!(matches!(
            classify(412, "ErrorIrresolvableConflict", ""),
            GraphError::PreconditionFailed
        ));
        assert!(matches!(
            classify(409, "ErrorItemAlreadyExists", ""),
            GraphError::Conflict
        ));
        assert!(matches!(
            classify(403, "ErrorAccessDenied", "Access is denied."),
            GraphError::AccessDenied { code } if code == "ErrorAccessDenied"
        ));
        // An on-premises mailbox answers 404 on some tenants and 401 on
        // others; the code decides, whatever the status.
        assert!(matches!(
            classify(404, "MailboxNotEnabledForRESTAPI", ""),
            GraphError::MailboxOnPremises
        ));
        assert!(matches!(
            classify(401, "MailboxNotEnabledForRESTAPI", ""),
            GraphError::MailboxOnPremises
        ));
    }

    #[test]
    fn only_the_network_and_throttling_are_worth_a_retry() {
        assert!(GraphError::Network("reset".into()).is_transient());
        assert!(GraphError::Throttled { retry_after: None }.is_transient());
        assert!(!GraphError::NotFound.is_transient());
        assert!(!GraphError::SyncStateLost.is_transient());
    }
}
