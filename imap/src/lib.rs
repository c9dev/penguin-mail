//! IMAP and SMTP for one account: the IMAP client and its connections, the
//! SMTP submission client, and the values both hand back. Sync reaches
//! them through its `ImapApi` and `Submit` traits.

mod client;
mod connection;
mod error;
mod guard;
mod logging;
mod login;
#[cfg(test)]
mod measure;
mod parse;
mod refusal;
mod structure;
#[cfg(test)]
mod testing;
mod tls;
mod types;
mod uid_set;
pub mod utf7;

pub use client::{Dial, IDLE_LIMIT, ImapClient, TlsDial};
pub use error::ImapError;
pub use logging::quiet;
pub use login::Login;
pub use structure::BodyStructure;
pub use types::{
    AppendUid, Capabilities, CopyUid, Fetched, FlagsOf, HEADER_FIELDS, Listed, Selected, Since,
    SpecialUse, Woke,
};
pub use uid_set::UidSet;
