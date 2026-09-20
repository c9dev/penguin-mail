//! Making the bodies of mail that goes out signed or enveloped.

use crate::error::SmimeError;
use crate::gpgsm::{Run, Smime, user_id};
use crate::mime;

impl Smime {
    /// Signs `part` and gives back the whole `multipart/signed` entity: the
    /// `Content-Type` header that names the boundary, a blank line, then
    /// the part and the signature over it. The caller puts those headers on
    /// the message it sends and drops in the rest as the body.
    ///
    /// `part` is a MIME entity, headers and all, as the composer built it.
    /// What gets signed is [`mailrs_pgp::mime::canonical`] of it, which is
    /// what goes into the body as well, so the two cannot drift apart.
    ///
    /// The signature is detached, so a reader whose mail client knows
    /// nothing about S/MIME still sees the message.
    pub fn sign(&self, part: &[u8], from: &str) -> Result<Vec<u8>, SmimeError> {
        let signed = signable(part);
        let run = self.run(&signed, |command| {
            command
                .args(["--detach-sign", "--local-user"])
                .arg(user_id(from));
        })?;
        if !run.ok {
            return Err(run.failure());
        }
        Ok(mime::multipart_signed(&signed, &run.out, micalg(&run)))
    }

    /// Encrypts `part` to every address in `to`, and gives back the whole
    /// `application/pkcs7-mime` entity.
    ///
    /// `from` signs the message first, so the signature travels inside the
    /// envelope, which is the only place one on encrypted mail means
    /// anything, and puts the sender among the recipients so their own copy
    /// stays readable. Passing `None` encrypts without signing, for a
    /// sender who holds no certificate of their own.
    ///
    /// Every address in `to` needs a certificate gpgsm can use.
    /// [`Smime::certificates_for`] answers that before the message is
    /// written, which is a kinder moment to find out than this one.
    pub fn encrypt(
        &self,
        part: &[u8],
        to: &[String],
        from: Option<&str>,
    ) -> Result<Vec<u8>, SmimeError> {
        // gpgsm signs or encrypts in one run, never both, which is also
        // what RFC 8551 describes: the signed entity is the thing that gets
        // enveloped.
        let inside = match from {
            Some(from) => self.sign(part, from)?,
            None => mailrs_pgp::mime::canonical(part),
        };
        let run = self.run(&inside, |command| {
            command.arg("--encrypt");
            // gpgsm otherwise refuses to encrypt to a certificate whose
            // chain reaches no root this computer trusts, and a batch run
            // cannot ask. How far a chain reaches belongs in front of the
            // person, not in a refusal to send.
            command.arg("--always-trust");
            for address in to {
                command.arg("--recipient").arg(user_id(address));
            }
            if let Some(from) = from {
                command.arg("--recipient").arg(user_id(from));
            }
        })?;
        if !run.ok {
            return Err(run.failure());
        }
        Ok(mime::enveloped(&run.out))
    }
}

/// The bytes to sign: the part made canonical, with the blank lines at its
/// end taken off. The CRLF before the boundary that closes a part belongs
/// to the boundary rather than the part, so trailing blank lines leave two
/// readings of where the signed bytes stop, and a reader who picks the
/// other one calls a good signature bad.
fn signable(part: &[u8]) -> Vec<u8> {
    let mut signed = mailrs_pgp::mime::canonical(part);
    while signed.ends_with(b"\r\n") {
        signed.truncate(signed.len() - 2);
    }
    signed
}

/// The digest gpgsm signed with, under the name RFC 8551 gives it, ready
/// for the `micalg` parameter. It comes from
/// `SIG_CREATED <kind> <key algorithm> <digest> ...`, numbered as in RFC
/// 4880. A digest with no name here leaves the parameter out rather than
/// putting the wrong one in.
fn micalg(run: &Run) -> Option<&'static str> {
    match run.field("SIG_CREATED")?.split_whitespace().nth(2)? {
        "1" => Some("md5"),
        "2" => Some("sha-1"),
        "8" => Some("sha-256"),
        "9" => Some("sha-384"),
        "10" => Some("sha-512"),
        "11" => Some("sha-224"),
        _ => None,
    }
}
