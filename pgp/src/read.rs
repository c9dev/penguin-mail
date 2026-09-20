//! Reading mail that arrived signed or encrypted.

use std::io::Write;

use crate::error::PgpError;
use crate::gpg::Pgp;
use crate::status::{self, Signature};

/// What was inside a `multipart/encrypted` message.
#[derive(Debug, Clone)]
pub struct Decrypted {
    /// The MIME entity the encryption held, headers and all. It is a message
    /// part in its own right, so the caller parses it as it would any other.
    pub part: Vec<u8>,
    /// The signature the sender put inside the encryption, when there was
    /// one. A signature that travels inside is the only kind worth trusting
    /// on an encrypted message, since anyone can wrap a new outer one.
    pub signature: Option<Signature>,
}

impl Pgp {
    /// Checks the signature on an RFC 3156 `multipart/signed` part.
    ///
    /// `signed_part` is the first body part as it arrived, its own headers
    /// included, up to but not including the CRLF before the boundary that
    /// closes it. Every byte counts, line endings included, so whoever takes
    /// the part out of the message passes the raw bytes rather than a parsed
    /// and rebuilt copy of them. `signature` is the body of the
    /// `application/pgp-signature` part beside it.
    ///
    /// A signature that does not match, or one from a key this computer does
    /// not hold, is an answer rather than an error: the [`Signature`] says
    /// which it was.
    pub fn verify(&self, signed_part: &[u8], signature: &[u8]) -> Result<Signature, PgpError> {
        // gpg reads a detached signature from a file, so it needs one.
        let mut file = tempfile::NamedTempFile::new().map_err(temp)?;
        file.write_all(signature).map_err(temp)?;
        file.flush().map_err(temp)?;
        let run = self.run(signed_part, |command| {
            command.arg("--verify").arg(file.path()).arg("-");
        })?;
        status::signature(&run.status).ok_or_else(|| run.failure())
    }

    /// Opens the ciphertext of an RFC 3156 `multipart/encrypted` message:
    /// the body of its `application/octet-stream` part, armored or not.
    ///
    /// The other part, the `application/pgp-encrypted` one, carries nothing
    /// but `Version: 1`, so there is nothing here to pass it to.
    pub fn decrypt(&self, encrypted_part: &[u8]) -> Result<Decrypted, PgpError> {
        let run = self.run(encrypted_part, |command| {
            command.arg("--decrypt");
        })?;
        if !run.ok && !run.says("DECRYPTION_OKAY") {
            return Err(run.failure());
        }
        Ok(Decrypted {
            part: run.out,
            signature: status::signature(&run.status),
        })
    }
}

fn temp(err: std::io::Error) -> PgpError {
    PgpError::Temp(err.to_string())
}
