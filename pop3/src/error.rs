/// Why a POP3 call failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Pop3Error {
    #[error("network error: {0}")]
    Network(String),
    /// The TLS handshake with `host` failed.
    #[error("TLS with {host} failed: {detail}")]
    Tls { host: String, detail: String },
    /// The server refused the user name or password, in its own words.
    #[error("the server refused the sign-in: {text}")]
    Auth { text: String },
    /// Another session holds the mailbox (`-ERR [IN-USE]`, RFC 2449).
    #[error("another session holds the mailbox: {0}")]
    InUse(String),
    /// Any other `-ERR`, in the server's words.
    #[error("the server refused: {0}")]
    Refused(String),
    #[error("the server answered something unexpected: {0}")]
    Protocol(String),
    /// The server lacks something Penguin Mail needs, such as UIDL or STLS.
    #[error("the server does not offer {0}")]
    Unsupported(&'static str),
    /// An answer longer than [`crate::MOST_MESSAGE_BYTES`].
    #[error("the answer is larger than Penguin Mail reads")]
    TooLarge,
}

impl Pop3Error {
    /// Failures worth trying again after a wait.
    pub fn is_transient(&self) -> bool {
        matches!(self, Pop3Error::Network(_) | Pop3Error::InUse(_))
    }
}
