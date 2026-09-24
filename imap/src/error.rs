//! What went wrong, in classes the adapter and the account dialog act on.
//! The words here are for the log; the dialog writes what a person reads.

/// An IMAP or SMTP failure.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ImapError {
    /// The server refused the user name or password. `text` is the
    /// server's own answer, which often says more than "wrong password".
    #[error("the server refused the sign-in: {text}")]
    Auth { text: String },
    /// The server turns IMAP away for this account until the person
    /// switches it on (GMX, Web.de, Zoho, Yandex) or pays for it.
    #[error("IMAP is off for this account: {text}")]
    ImapDisabled { text: String },
    /// The TLS handshake with `host` failed: a certificate that does not
    /// validate for that name, or a server without TLS 1.2.
    #[error("TLS with {host} failed: {detail}")]
    Tls { host: String, detail: String },
    /// The server holds as many connections for this account as it allows.
    #[error("the server allows no more connections: {text}")]
    TooManyConnections { text: String },
    #[error("network error: {0}")]
    Network(String),
    /// An answer this client cannot read, or a command the server called
    /// malformed.
    #[error("the server answered something unexpected: {0}")]
    Protocol(String),
    /// The server has no selectable mailbox by this name.
    #[error("the server has no mailbox called {0}")]
    NoMailbox(String),
    /// The call needs an extension the server lacks.
    #[error("the server does not offer {0}")]
    Unsupported(&'static str),
    /// Any other refusal, in the server's words: over quota, a message too
    /// large, a name already taken.
    #[error("the server refused: {0}")]
    Refused(String),
}

impl ImapError {
    /// Failures worth trying again after a wait.
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            ImapError::Network(_) | ImapError::TooManyConnections { .. }
        )
    }

    /// The error with the password of `login` taken out of every text in
    /// it, for a failure that came while `login` went to the server.
    pub(crate) fn hidden(self, login: &crate::Login) -> ImapError {
        let hide = |text: String| login.hide(&text);
        match self {
            ImapError::Auth { text } => ImapError::Auth { text: hide(text) },
            ImapError::ImapDisabled { text } => ImapError::ImapDisabled { text: hide(text) },
            ImapError::Tls { host, detail } => ImapError::Tls {
                host,
                detail: hide(detail),
            },
            ImapError::TooManyConnections { text } => {
                ImapError::TooManyConnections { text: hide(text) }
            }
            ImapError::Network(text) => ImapError::Network(hide(text)),
            ImapError::Protocol(text) => ImapError::Protocol(hide(text)),
            ImapError::NoMailbox(name) => ImapError::NoMailbox(hide(name)),
            ImapError::Unsupported(what) => ImapError::Unsupported(what),
            ImapError::Refused(text) => ImapError::Refused(hide(text)),
        }
    }

    /// Failures after which the connection is in no state to carry the
    /// next command.
    pub(crate) fn drops_connection(&self) -> bool {
        matches!(
            self,
            ImapError::Network(_) | ImapError::Protocol(_) | ImapError::Tls { .. }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::ImapError;

    #[test]
    fn only_network_and_connection_limits_are_worth_retrying() {
        assert!(ImapError::Network("reset".into()).is_transient());
        assert!(
            ImapError::TooManyConnections {
                text: String::new()
            }
            .is_transient()
        );
        assert!(
            !ImapError::Auth {
                text: String::new()
            }
            .is_transient()
        );
        assert!(!ImapError::NoMailbox("x".into()).is_transient());
    }

    #[test]
    fn only_network_protocol_and_tls_failures_end_the_connection() {
        assert!(ImapError::Network("reset".into()).drops_connection());
        assert!(ImapError::Protocol("garbled".into()).drops_connection());
        assert!(
            ImapError::Tls {
                host: "imap.example.com".into(),
                detail: String::new()
            }
            .drops_connection()
        );
        assert!(!ImapError::NoMailbox("x".into()).drops_connection());
        assert!(!ImapError::Unsupported("MOVE").drops_connection());
    }
}
