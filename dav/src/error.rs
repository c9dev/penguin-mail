use std::time::Duration;

/// What went wrong talking to a DAV server, by what the caller does next.
#[derive(Debug, Clone, thiserror::Error)]
pub enum DavError {
    /// 401: the server refused the user name or the password.
    #[error("the server refused the user name or password")]
    Unauthorized,
    /// 403, with what the server said.
    #[error("the server refused: {0}")]
    Forbidden(String),
    #[error("not found")]
    NotFound,
    /// 412 on If-Match or If-None-Match, or 409 on a create: the resource
    /// changed on the server, or already exists.
    #[error("it changed on the server first")]
    Changed,
    /// The server no longer knows the sync token (`valid-sync-token`).
    #[error("the server no longer knows that sync token")]
    InvalidSyncToken,
    /// The collection answers no `sync-collection` report.
    #[error("the server cannot list what changed in a collection")]
    NoSyncCollection,
    /// An answer longer than the limit, in bytes.
    #[error("the server sent more than {0} bytes")]
    TooLarge(usize),
    #[error("network error: {0}")]
    Network(String),
    /// 429 or 503, with the server's Retry-After.
    #[error("the server is busy")]
    Busy(Option<Duration>),
    #[error("HTTP {status}: {detail}")]
    Http { status: u16, detail: String },
    #[error("could not read the server's answer: {0}")]
    Parse(String),
}

impl DavError {
    /// Worth trying again after a wait.
    pub fn is_transient(&self) -> bool {
        matches!(self, DavError::Network(_) | DavError::Busy(_))
            || matches!(self, DavError::Http { status, .. } if *status >= 500)
    }
}
