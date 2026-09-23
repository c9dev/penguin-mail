//! OpenPGP mail through the person's own GnuPG: checking a signature,
//! opening an encrypted part, and building the bodies RFC 3156 describes.
//!
//! Every call runs the `gpg` binary, so the keys, the agent, the pinentry
//! and the trust database belong to the person rather than to this crate.
//! `README.md` says in which order a mail client calls all this.

mod error;
mod gpg;
pub mod inline;
pub mod keys;
pub mod mime;
mod read;
pub mod status;
mod write;

pub use error::PgpError;
pub use gpg::Pgp;
pub use inline::{Armor, Opened};
pub use keys::{Key, Recipient, UserId};
pub use read::Decrypted;
pub use status::{Signature, Trust, Verdict};
pub use write::Readers;
