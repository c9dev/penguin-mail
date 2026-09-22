//! Making the bodies of mail that goes out signed or encrypted.

use crate::error::PgpError;
use crate::gpg::{Pgp, Run, user_id};
use crate::mime;

impl Pgp {
    /// Signs `part` and gives back the whole `multipart/signed` entity: the
    /// `Content-Type` header that names the boundary, a blank line, then the
    /// part and the signature over it. The caller puts those headers on the
    /// message it sends and drops in the rest as the body.
    ///
    /// `part` is a MIME entity, headers and all, as the composer built it.
    /// What gets signed is [`mime::canonical`] of it, which is what goes into
    /// the body as well, so the two cannot drift apart.
    pub fn sign(&self, part: &[u8], from: &str) -> Result<Vec<u8>, PgpError> {
        let signed = signable(part);
        let run = self.run(&signed, |command| {
            command
                .args(["--armor", "--detach-sign", "--local-user"])
                .arg(user_id(from));
        })?;
        if !run.ok {
            return Err(run.failure());
        }
        Ok(mime::multipart_signed(&signed, &run.out, micalg(&run)))
    }

    /// Encrypts `part` so that everyone in `readers` can open it, and gives
    /// back the whole `multipart/encrypted` entity.
    ///
    /// gpg writes the key id of every named reader into the message, where
    /// anyone who receives it can list them. A hidden reader goes in with a
    /// key id of zero (`--hidden-recipient`), so the others see that
    /// somebody else can open it and not who. That is what a blind copy
    /// needs.
    ///
    /// `sign_as` signs the message from inside the encryption, which is the
    /// only place a signature on encrypted mail means anything. Signing adds
    /// nobody to `readers`: a sender who wants to read their own copy in
    /// Sent names themselves there.
    ///
    /// Every address in `readers` needs a key gpg can use. [`Pgp::keys_for`]
    /// answers that before the message is written, which is a kinder moment
    /// to find out than this one.
    pub fn encrypt(
        &self,
        part: &[u8],
        readers: &Readers,
        sign_as: Option<&str>,
    ) -> Result<Vec<u8>, PgpError> {
        let inside = mime::canonical(part);
        let run = self.run(&inside, |command| {
            command.args(["--armor", "--encrypt"]);
            // gpg otherwise refuses to encrypt to a key its owner has not
            // signed, which is most keys anybody holds, and a batch run
            // cannot ask. The trust that gpg reports belongs in front of the
            // person through `keys_for`, not in a refusal to send.
            command.args(["--trust-model", "always"]);
            for address in &readers.named {
                command.arg("--recipient").arg(user_id(address));
            }
            for address in &readers.hidden {
                command.arg("--hidden-recipient").arg(user_id(address));
            }
            if let Some(from) = sign_as {
                command.args(["--sign", "--local-user"]).arg(user_id(from));
            }
        })?;
        if !run.ok {
            return Err(run.failure());
        }
        Ok(mime::multipart_encrypted(&run.out))
    }
}

/// Who can open a message [`Pgp::encrypt`] writes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Readers {
    /// Addresses whose key ids go into the message for every reader to see.
    pub named: Vec<String>,
    /// Addresses whose key ids stay out of it, for a blind copy.
    pub hidden: Vec<String>,
}

impl Readers {
    /// Readers who are all named, as on a message with no blind copy.
    pub fn named<S: Into<String>>(addresses: impl IntoIterator<Item = S>) -> Readers {
        Readers {
            named: addresses.into_iter().map(Into::into).collect(),
            hidden: Vec::new(),
        }
    }
}

/// The bytes to sign: the part made canonical, with the blank lines at its
/// end taken off. The CRLF before the boundary that closes a part belongs to
/// the boundary rather than the part, so trailing blank lines leave two
/// readings of where the signed bytes stop, and a reader who picks the other
/// one calls a good signature bad.
fn signable(part: &[u8]) -> Vec<u8> {
    let mut signed = mime::canonical(part);
    while signed.ends_with(b"\r\n") {
        signed.truncate(signed.len() - 2);
    }
    signed
}

/// The digest gpg signed with, under the name RFC 3156 gives it, ready for
/// the `micalg` parameter. It comes from
/// `SIG_CREATED <kind> <key algorithm> <digest> ...`, numbered as in RFC
/// 4880. A digest with no name here leaves the parameter out rather than
/// putting the wrong one in.
fn micalg(run: &Run) -> Option<&'static str> {
    match run.field("SIG_CREATED")?.split_whitespace().nth(2)? {
        "1" => Some("md5"),
        "2" => Some("sha1"),
        "3" => Some("ripemd160"),
        "8" => Some("sha256"),
        "9" => Some("sha384"),
        "10" => Some("sha512"),
        "11" => Some("sha224"),
        _ => None,
    }
}
