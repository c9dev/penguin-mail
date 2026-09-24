//! IMAP and SMTP for one account: the IMAP client and its connections, the
//! SMTP submission client, and the values both hand back. Sync reaches
//! them through its `ImapApi` and `Submit` traits.

#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "ImapClient runs its commands on these connections"
    )
)]
mod connection;
mod error;
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "the connections read through these limits")
)]
mod guard;
mod logging;
mod login;
#[cfg(test)]
mod measure;
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "the connections read their answers through these")
)]
mod parse;
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "the connections classify refusals through these")
)]
mod refusal;
mod structure;
#[cfg(test)]
mod testing;
mod types;
mod uid_set;
pub mod utf7;

pub use error::ImapError;
pub use logging::quiet;
pub use login::Login;
pub use structure::BodyStructure;
pub use types::{
    AppendUid, Capabilities, CopyUid, Fetched, FlagsOf, HEADER_FIELDS, Listed, Selected, Since,
    SpecialUse, Woke,
};
pub use uid_set::UidSet;
