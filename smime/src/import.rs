//! Putting the certificates in a file the person picked into their keybox.

use mailrs_pgp::gnupg::{Change, Import, Pinentry, Run, import_counts, imports};

use crate::certificates::certificates;
use crate::error::SmimeError;
use crate::gpgsm::{Smime, failure};

/// GnuPG's error code for a passphrase that did not unlock anything.
const BAD_PASSPHRASE: u32 = 11;
/// GnuPG's error code for a request the person canceled.
const CANCELED: u32 = 99;

impl Smime {
    /// Imports every certificate in `file`, and the secret key with them
    /// when it is a PKCS#12 file, and says what each one brought, with the
    /// name and address on it.
    ///
    /// The person picked the file, so the run may ask them something: a
    /// PKCS#12 file opens with a passphrase, which gpg-agent asks for
    /// through their pinentry, and then asks for one to keep the key under.
    pub fn import(&self, file: &[u8]) -> Result<Vec<Import>, SmimeError> {
        let run = self.run(file, Pinentry::MayAsk, |command| {
            command.arg("--import");
        })?;
        let mut found = imports(&run.status);
        if found.is_empty() {
            return Err(refused(&run, file));
        }
        let counts = import_counts(&run.status);
        // gpgsm marks no certificate as the one the secret key belongs
        // to, so a file that held a key is followed by a listing of the
        // secret keys among what came in.
        if counts.is_some_and(|counts| counts.secret_read > 0) {
            let held = self.listed(&found, true);
            let fresh = counts.is_some_and(|counts| counts.secret_imported > 0);
            for import in &mut found {
                if held
                    .iter()
                    .any(|(fingerprint, _, _)| *fingerprint == import.fingerprint)
                {
                    import.secret = true;
                    if fresh {
                        import.change = Change::New;
                    }
                }
            }
        }
        for (fingerprint, name, address) in self.listed(&found, false) {
            if let Some(import) = found
                .iter_mut()
                .find(|import| import.fingerprint == fingerprint)
            {
                (import.name, import.address) = (name, address);
            }
        }
        Ok(found)
    }

    /// The fingerprint, common name and first address of each of `found`
    /// that gpgsm lists, or of the ones it holds a secret key for when
    /// `secret` is set. A listing that fails lists nothing.
    fn listed(
        &self,
        found: &[Import],
        secret: bool,
    ) -> Vec<(String, Option<String>, Option<String>)> {
        let Ok(run) = self.run(&[], Pinentry::Never, |command| {
            command.arg("--with-colons");
            command.arg(match secret {
                true => "--list-secret-keys",
                false => "--list-keys",
            });
            command.arg("--");
            command.args(found.iter().map(|import| import.fingerprint.as_str()));
        }) else {
            return Vec::new();
        };
        certificates(&String::from_utf8_lossy(&run.out))
            .into_iter()
            .map(|listed| {
                (
                    listed.fingerprint,
                    common_name(&listed.subject),
                    listed.emails.into_iter().next(),
                )
            })
            .collect()
    }
}

/// Why an import that brought nothing brought nothing.
///
/// gpgsm reports a wrong passphrase as an `ERROR` line. A canceled
/// pinentry, or one that could not start, leaves no status line at all,
/// only a count of nothing, so a PKCS#12 file that came to nothing is
/// taken to be one whose passphrase never came.
fn refused(run: &Run, file: &[u8]) -> SmimeError {
    let codes: Vec<u32> = run
        .status
        .iter()
        .filter_map(|line| line.strip_prefix("ERROR "))
        .filter_map(|rest| rest.split_whitespace().nth(1)?.parse::<u32>().ok())
        // The code may carry the part of GnuPG that raised it in its top
        // bits; the error itself is the low sixteen.
        .map(|code| code & 0xFFFF)
        .collect();
    if codes.contains(&BAD_PASSPHRASE) {
        return SmimeError::WrongPassphrase;
    }
    if codes.contains(&CANCELED) || is_pkcs12(file) {
        return SmimeError::NoPassphrase;
    }
    match import_counts(&run.status).map_or(0, |counts| counts.considered) {
        0 => SmimeError::NotACertificate,
        _ => failure(run),
    }
}

/// Whether `file` starts the way a binary PKCS#12 file does (RFC 7292):
/// a DER sequence holding version 3, then a content info whose type is a
/// PKCS#7 one.
pub fn is_pkcs12(file: &[u8]) -> bool {
    let Some(rest) = sequence(file) else {
        return false;
    };
    let Some(rest) = rest.strip_prefix(&[0x02, 0x01, 0x03]) else {
        return false;
    };
    // `1.2.840.113549.1.7`, the PKCS#7 arc, in DER.
    const PKCS7: [u8; 10] = [0x06, 0x09, 0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x07];
    sequence(rest).is_some_and(|inner| inner.starts_with(&PKCS7))
}

/// What follows the tag and length of a DER sequence at the start of
/// `bytes`.
fn sequence(bytes: &[u8]) -> Option<&[u8]> {
    let (&tag, rest) = bytes.split_first()?;
    if tag != 0x30 {
        return None;
    }
    let (&length, rest) = rest.split_first()?;
    match length {
        0..=0x7F => Some(rest),
        // The long form gives the number of length bytes that follow.
        0x81..=0x84 => rest.get(usize::from(length - 0x80)..),
        _ => None,
    }
}

/// The common name in a distinguished name as gpgsm lists it,
/// `CN=Ada Lovelace,O=Example`. A comma inside a value comes escaped.
pub fn common_name(subject: &str) -> Option<String> {
    let mut parts = Vec::new();
    let mut part = String::new();
    let mut chars = subject.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(next) = chars.next() {
                    part.push(next);
                }
            }
            ',' => parts.push(std::mem::take(&mut part)),
            c => part.push(c),
        }
    }
    parts.push(part);
    parts
        .iter()
        .find_map(|part| part.trim().strip_prefix("CN="))
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
}
