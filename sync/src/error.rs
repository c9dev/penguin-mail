use mailrs_domain::AccountId;
use mailrs_gmail::GmailError;
use mailrs_store::StoreError;

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error(transparent)]
    Gmail(#[from] GmailError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("no sync is running for account {0}")]
    UnknownAccount(AccountId),
    #[error("there is no label called {0}")]
    NoLabel(String),
}

impl SyncError {
    /// Whether a message that would not go out is worth trying again. No
    /// network, a 5xx and a rate limit are; a refused recipient, a message
    /// over Gmail's size limit and a revoked token are not, because the
    /// same bytes fail the same way however long the outbox waits, and the
    /// person is the only one who can fix any of them.
    pub fn worth_retrying(&self) -> bool {
        match self {
            // Gmail answers a request that took too long with 408 and
            // nothing else; every other 4xx is about the message.
            SyncError::Gmail(GmailError::Http { status: 408, .. }) => true,
            SyncError::Gmail(err) => err.is_transient(),
            // This computer's own trouble, not the message's, and it
            // usually clears on its own.
            SyncError::Store(_) => true,
            // The account is still connecting.
            SyncError::UnknownAccount(_) => true,
            SyncError::NoLabel(_) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn http(status: u16) -> SyncError {
        SyncError::Gmail(GmailError::Http {
            status,
            body: String::new(),
        })
    }

    #[test]
    fn a_message_waits_out_no_network_a_rate_limit_and_gmail_falling_over() {
        for err in [
            SyncError::Gmail(GmailError::Network("connection refused".into())),
            SyncError::Gmail(GmailError::RateLimited {
                retry_after: Some(Duration::from_secs(3)),
            }),
            http(500),
            http(503),
            http(408),
            SyncError::UnknownAccount(1),
        ] {
            assert!(err.worth_retrying(), "{err} is worth another try");
        }
    }

    #[test]
    fn a_refused_recipient_an_oversized_message_and_a_revoked_token_go_to_the_person() {
        for err in [
            http(400),
            http(413),
            http(403),
            SyncError::Gmail(GmailError::NeedsReauth),
            SyncError::Gmail(GmailError::MissingScope),
            SyncError::Gmail(GmailError::NotFound),
        ] {
            assert!(!err.worth_retrying(), "{err} needs the person");
        }
    }
}
