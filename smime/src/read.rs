//! Reading mail that arrived signed or enveloped.
//!
//! Every blob handed to these calls is the body of a part as it arrived,
//! base64 and all: CMS is bytes rather than armor, so mail carries it that
//! way and gpgsm is told to expect it.

use std::io::Write;

use crate::error::SmimeError;
use crate::gpgsm::Smime;
use crate::status::{self, Signature};

/// What an `application/pkcs7-mime` part held once gpgsm opened it.
#[derive(Debug, Clone)]
pub struct Opened {
    /// The MIME entity that was inside, headers and all. It is a message
    /// part in its own right, so the caller parses it as it would any
    /// other.
    pub part: Vec<u8>,
    /// What gpgsm made of the signature the blob carried.
    pub signature: Signature,
}

impl Smime {
    /// Checks the signature on a `multipart/signed` part whose protocol is
    /// `application/pkcs7-signature`.
    ///
    /// `signed_part` is the first body part as it arrived, its own headers
    /// included, up to but not including the CRLF before the boundary that
    /// closes it. Every byte counts, line endings included, so whoever
    /// takes the part out of the message passes the raw bytes rather than a
    /// parsed and rebuilt copy of them. `signature` is the body of the
    /// `application/pkcs7-signature` part beside it, base64 as it arrived.
    ///
    /// A signature that does not match, or one from a certificate this
    /// computer does not hold, is an answer rather than an error: the
    /// [`Signature`] says which it was.
    pub fn verify(&self, signed_part: &[u8], signature: &[u8]) -> Result<Signature, SmimeError> {
        // gpgsm reads a detached signature from a file, so it needs one.
        let mut file = tempfile::NamedTempFile::new().map_err(temp)?;
        file.write_all(signature).map_err(temp)?;
        file.flush().map_err(temp)?;
        let run = self.read_only(signed_part, |command| {
            command
                .arg("--assume-base64")
                .arg("--verify")
                .arg(file.path())
                .arg("-");
        })?;
        let found = status::signature(&run.status).ok_or_else(|| run.failure())?;
        Ok(self.named(found))
    }

    /// Opens an `application/pkcs7-mime` part with `smime-type=signed-data`:
    /// the shape where the message sits inside the signature rather than
    /// beside it, which is what Outlook sends when nobody told it
    /// otherwise.
    ///
    /// `blob` is the body of that part, base64 as it arrived. What comes
    /// back is the entity that was inside, with what gpgsm made of the
    /// signature over it.
    pub fn open_signed(&self, blob: &[u8]) -> Result<Opened, SmimeError> {
        let run = self.read_only(blob, |command| {
            command.args(["--assume-base64", "--output", "-", "--verify"]);
        })?;
        let Some(found) = status::signature(&run.status) else {
            return Err(run.failure());
        };
        Ok(Opened {
            part: run.out,
            signature: self.named(found),
        })
    }

    /// Opens an `application/pkcs7-mime` part with
    /// `smime-type=enveloped-data`: the body of that part, base64 as it
    /// arrived.
    ///
    /// What comes back is a MIME entity in its own right. gpgsm opens one
    /// wrapper at a time, so a message that was signed before it was
    /// enveloped gives back a signed entity here, and checking that
    /// signature is a second call over what is inside it.
    pub fn decrypt(&self, enveloped: &[u8]) -> Result<Vec<u8>, SmimeError> {
        // The one read that may ask the person something, because opening
        // the envelope needs their own secret key and gpg-agent asks for
        // the passphrase that unlocks it. That window they expect; the one
        // about trusting a stranger's root they do not.
        let run = self.run(enveloped, |command| {
            command.args(["--assume-base64", "--output", "-", "--decrypt"]);
        })?;
        if !run.ok && !run.says("DECRYPTION_OKAY") {
            return Err(run.failure());
        }
        Ok(run.out)
    }

    /// The signature with the signer's address filled in. gpgsm names a
    /// signer by the subject of their certificate, and the address sits
    /// inside the certificate, so this reads it out of the one the
    /// fingerprint names.
    fn named(&self, signature: Signature) -> Signature {
        let Some(fingerprint) = signature.fingerprint.as_deref() else {
            return signature;
        };
        Signature {
            email: self.address_of(fingerprint),
            ..signature
        }
    }
}

fn temp(err: std::io::Error) -> SmimeError {
    SmimeError::Temp(err.to_string())
}
