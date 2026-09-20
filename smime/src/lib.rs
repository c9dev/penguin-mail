//! S/MIME mail through the person's own GnuPG: checking a signature,
//! opening an enveloped part, and building the bodies RFC 8551 describes.
//!
//! Every call runs the `gpgsm` binary, so the certificates, the agent, the
//! pinentry and the list of roots to trust belong to the person rather than
//! to this crate. `README.md` says in which order a mail client calls all
//! this.

pub mod certificates;
mod error;
mod gpgsm;
pub mod mime;
mod read;
pub mod status;
mod write;

pub use certificates::{Certificate, Recipient};
pub use error::SmimeError;
pub use gpgsm::Smime;
pub use read::Opened;
pub use status::{Chain, Signature, Verdict};
