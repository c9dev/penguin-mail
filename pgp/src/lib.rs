//! OpenPGP mail through the person's own GnuPG.

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
pub use keys::{Key, Recipient};
pub use read::Decrypted;
pub use status::{Signature, Trust, Verdict};
