//! IMAP and SMTP for one account: the IMAP client and its connections, the
//! SMTP submission client, and the values both hand back. Sync reaches
//! them through its `ImapApi` and `Submit` traits.

mod check;
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
mod smtp;
mod structure;
#[cfg(test)]
mod testing;
mod tls;
mod types;
mod uid_set;
pub mod utf7;

pub use check::{CheckError, Checked, check, check_smtp, user_names};
pub use client::{Dial, IDLE_LIMIT, ImapClient, TlsDial};
pub use error::ImapError;
pub use logging::quiet;
pub use login::{Login, LoginSource};
pub use smtp::{SmtpClient, SmtpTls};
pub use structure::BodyStructure;
pub use types::{
    AppendUid, Capabilities, CopyUid, Fetched, FlagsOf, GmailMeta, HEADER_FIELDS, Listed, QuotaUse,
    Selected, Since, SpecialUse, Woke, gm_labels, gm_msgid, gm_raw, gm_thrid,
};
pub use uid_set::UidSet;
