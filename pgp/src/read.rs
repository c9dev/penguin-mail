//! Reading mail that arrived signed or encrypted.

use std::io::Write;

use crate::error::PgpError;
use crate::gnupg::Pinentry;
use crate::gpg::{Pgp, failure, reading};
use crate::status::{self, Signature};

/// What was inside a `multipart/encrypted` message.
#[derive(Debug, Clone)]
pub struct Decrypted {
    /// The MIME entity the encryption held, headers and all. It is a message
    /// part in its own right, so the caller parses it as it would any other.
    pub part: Vec<u8>,
    /// The signatures the sender put inside the encryption, when there
    /// were any. A signature that travels inside is the only kind worth
    /// trusting on an encrypted message, since anyone can wrap a new outer
    /// one.
    pub signatures: Vec<Signature>,
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
    /// which it was. One detached signature can carry several, and every
    /// one comes back, in order.
    pub fn verify(&self, signed_part: &[u8], signature: &[u8]) -> Result<Vec<Signature>, PgpError> {
        // gpg reads a detached signature from a file, so it needs one.
        let mut file = tempfile::NamedTempFile::new().map_err(temp)?;
        file.write_all(signature).map_err(temp)?;
        file.flush().map_err(temp)?;
        let run = self.run(signed_part, Pinentry::Never, |command| {
            reading(command);
            command.arg("--verify").arg(file.path()).arg("-");
        })?;
        let found = status::signatures(&run.status);
        if found.is_empty() {
            return Err(failure(&run));
        }
        Ok(found.into_iter().map(|found| self.named(found)).collect())
    }

    /// Opens the ciphertext of an RFC 3156 `multipart/encrypted` message:
    /// the body of its `application/octet-stream` part, armored or not.
    ///
    /// The other part, the `application/pgp-encrypted` one, carries nothing
    /// but `Version: 1`, so there is nothing here to pass it to.
    pub fn decrypt(&self, encrypted_part: &[u8]) -> Result<Decrypted, PgpError> {
        // The one read here that needs the person's own secret key, and
        // gpg-agent asks for the passphrase that unlocks it.
        let run = self.run(encrypted_part, Pinentry::MayAsk, |command| {
            reading(command);
            command.arg("--decrypt");
        })?;
        if !run.ok && !run.says("DECRYPTION_OKAY") {
            return Err(failure(&run));
        }
        Ok(Decrypted {
            part: run.out,
            signatures: status::signatures(&run.status)
                .into_iter()
                .map(|found| self.named(found))
                .collect(),
        })
    }
}

fn temp(err: std::io::Error) -> PgpError {
    PgpError::Temp(err.to_string())
}
