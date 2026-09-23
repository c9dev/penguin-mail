//! Reading mail that arrived signed or enveloped.
//!
//! Every blob handed to these calls is the body of a part as it arrived,
//! base64 and all: CMS is bytes rather than armor, so mail carries it that
//! way and gpgsm is told to expect it.

use std::io::{ErrorKind, Write};
use std::process::Command;

use crate::error::SmimeError;
use mailrs_pgp::gnupg::{Pinentry, Run};

use crate::gpgsm::{Smime, failure};
use crate::status::{self, Chain, Signature, Verdict};

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
        // gpgsm fetches no certificate over the network unless the
        // person's gpgsm.conf says `auto-issuer-key-retrieve`, and it has no
        // option to say otherwise here. gpg's equivalent is switched off on
        // every read in `mailrs_pgp`.
        let (_, found) = self.checked(signed_part, |command| {
            command
                .arg("--assume-base64")
                .arg("--verify")
                .arg(file.path())
                .arg("-");
        })?;
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
        let (run, found) = self.checked(blob, |command| {
            command.args(["--assume-base64", "--output", "-", "--verify"]);
        })?;
        Ok(Opened {
            part: run.out,
            signature: self.named(found),
        })
    }

    /// Checks a signature with `args`, revocation included, in a bounded
    /// time.
    ///
    /// gpgsm asks dirmngr for the CRL of every certificate below the root,
    /// and the answer is only as quick as the certificate authority's
    /// server. The first run gets [`crate::gpgsm::REVOCATION_WAIT`]. When
    /// it runs out, or when the chain comes back failed for a reason other
    /// than a revocation, a second run with `--disable-crl-checks` says
    /// whether the chain holds apart from revocation. If it does, what
    /// failed was the revocation check alone: the server refused, had no
    /// CRL, dirmngr could not start, or nothing answered in time. gpgsm
    /// reports each of those with its own error code on a
    /// `TRUST_UNDEFINED` line (see `tests/revocation.rs`), and asking again
    /// covers them all without a list of codes to keep up to date.
    fn checked(
        &self,
        input: &[u8],
        args: impl Fn(&mut Command),
    ) -> Result<(Run, Signature), SmimeError> {
        let first = match self.run_limited(input, &args) {
            Ok(run) => {
                let found = status::signature(&run.status).ok_or_else(|| failure(&run))?;
                // A trusted chain, a revoked certificate, and a signature
                // with no chain to speak of are gpgsm's final word.
                if found.chain != Chain::Untrusted || found.verdict == Verdict::RevokedCertificate
                {
                    return Ok((run, found));
                }
                Some((run, found))
            }
            Err(err) if err.kind() == ErrorKind::TimedOut => None,
            Err(err) => return Err(self.cannot_run(&err)),
        };
        // The option goes before the command and its files, where gpgsm
        // still reads options.
        let again = self
            .run_limited(input, |command| {
                command.arg("--disable-crl-checks");
                args(command);
            })
            .map_err(|err| self.cannot_run(&err))?;
        let unrevoked = status::signature(&again.status).ok_or_else(|| failure(&again))?;
        let holds = unrevoked.chain == Chain::Trusted;
        let (run, found) = first.unwrap_or((again, unrevoked));
        let found = match holds {
            true => Signature {
                chain: Chain::RevocationUnknown,
                ..found
            },
            false => found,
        };
        Ok((run, found))
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
        let run = self.run(enveloped, Pinentry::MayAsk, |command| {
            command.args(["--assume-base64", "--output", "-", "--decrypt"]);
        })?;
        if !run.ok && !run.says("DECRYPTION_OKAY") {
            return Err(failure(&run));
        }
        Ok(run.out)
    }

    /// The signature with the signer's addresses filled in. gpgsm names a
    /// signer by the subject of their certificate, and the addresses sit
    /// inside the certificate, so this reads them out of the one the
    /// fingerprint names. The status lines carry none, so this is a second
    /// run of gpgsm.
    fn named(&self, signature: Signature) -> Signature {
        let Some(fingerprint) = signature.fingerprint.as_deref() else {
            return signature;
        };
        Signature {
            emails: self.addresses_of(fingerprint),
            ..signature
        }
    }
}

fn temp(err: std::io::Error) -> SmimeError {
    SmimeError::Temp(err.to_string())
}
