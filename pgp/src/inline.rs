//! PGP written into a `text/plain` body rather than into MIME parts.
//!
//! RFC 3156 came after this habit and never replaced it. Mail still arrives
//! with the armor sitting in the text, sometimes with a mail client's own
//! lines above and below it, so a reader that only knows `multipart/signed`
//! shows people a screen of base64.

use crate::error::PgpError;
use crate::gpg::Pgp;
use crate::status::{self, Signature};

const MESSAGE: &str = "-----BEGIN PGP MESSAGE-----";
const MESSAGE_END: &str = "-----END PGP MESSAGE-----";
const SIGNED: &str = "-----BEGIN PGP SIGNED MESSAGE-----";
const SIGNATURE_END: &str = "-----END PGP SIGNATURE-----";

/// What a body holds inline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Armor {
    /// An encrypted message, and whatever signature is inside it.
    Message,
    /// Text left readable with the signature written under it.
    Clearsigned,
}

/// What an inline body held once gpg opened it.
#[derive(Debug, Clone)]
pub struct Opened {
    /// What was inside the armor. The sender chose its character set, so the
    /// caller decodes these bytes the way it decodes any other body.
    pub text: Vec<u8>,
    /// The signature the block carried, for either kind of armor.
    pub signature: Option<Signature>,
}

/// Which kind of armor `body` carries, if any. A caller asks this before it
/// draws the body, and calls [`Pgp::open_inline`] when the answer is
/// something.
pub fn armor(body: &str) -> Option<Armor> {
    block(body).map(|(kind, _)| kind)
}

/// The armored block itself, without the lines a mail client put around it.
pub(crate) fn block(body: &str) -> Option<(Armor, &str)> {
    // Clearsigned text holds a signature block of its own further down, so
    // it is the one to look for first.
    for (kind, start, end) in [
        (Armor::Clearsigned, SIGNED, SIGNATURE_END),
        (Armor::Message, MESSAGE, MESSAGE_END),
    ] {
        let Some(at) = body.find(start) else { continue };
        let rest = &body[at..];
        if let Some(stop) = rest.find(end) {
            return Some((kind, &rest[..stop + end.len()]));
        }
    }
    None
}

impl Pgp {
    /// Opens the armor in a `text/plain` body: an encrypted message, or text
    /// left readable with a signature under it. gpg tells the two apart on
    /// its own, so a caller passes whatever [`armor`] found something in.
    ///
    /// A signature that does not match is an answer rather than an error, so
    /// the text still comes back with the verdict beside it.
    pub fn open_inline(&self, body: &str) -> Result<Opened, PgpError> {
        let (_, block) = block(body).ok_or(PgpError::NotPgp)?;
        let run = self.run(block.as_bytes(), |command| {
            command.arg("--decrypt");
        })?;
        let signature = status::signature(&run.status);
        if !run.ok && signature.is_none() && !run.says("DECRYPTION_OKAY") {
            return Err(run.failure());
        }
        Ok(Opened {
            text: run.out,
            signature,
        })
    }
}
