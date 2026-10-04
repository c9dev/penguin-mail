//! Rules and the automatic reply on a server that runs Sieve: a writer
//! and reader for the one script Penguin Mail keeps there, and a
//! ManageSieve client (RFC 5804) behind `client::ManageSieveApi`.

pub mod client;
mod protocol;
pub mod script;
mod tls;

#[cfg(any(test, feature = "fake"))]
pub mod fake;

/// The name of the script Penguin Mail keeps on the server.
pub const SCRIPT_NAME: &str = "penguin-mail";

/// ManageSieve's port (RFC 5804 section 1.8).
pub const PORT: u16 = 4190;

/// The most bytes a script read from the server may hold.
pub const MOST_SCRIPT_BYTES: usize = 1 << 20;
