//! OpenPGP mail through the person's own GnuPG.

mod error;
mod gpg;
mod read;
pub mod status;

pub use error::PgpError;
pub use gpg::Pgp;
pub use read::Decrypted;
pub use status::{Signature, Trust, Verdict};
