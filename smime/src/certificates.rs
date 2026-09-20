//! Which addresses this computer holds a certificate for.
//!
//! gpgsm answers in its colon-separated listing, which is documented and
//! does not move between releases, rather than in the table it draws for a
//! person.

use crate::error::SmimeError;
use crate::gpgsm::{Smime, user_id};

/// One address a message is going to, and what gpgsm holds for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recipient {
    pub address: String,
    /// The certificate gpgsm would use. `None` when this computer holds
    /// none for the address, or only ones that are expired, revoked, or
    /// unable to do the job asked of them.
    pub certificate: Option<Certificate>,
}

/// A certificate that can take a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Certificate {
    pub fingerprint: String,
    /// Who the certificate says its owner is, as gpgsm writes a
    /// distinguished name: `CN=Ada Lovelace,O=Example`.
    pub subject: String,
    /// The address on it that was asked about.
    pub email: String,
}

impl Smime {
    /// What gpgsm would encrypt to for each of `addresses`, in the order
    /// they were given. A caller offers encryption when every recipient has
    /// a certificate, and this says which one is missing when they do not.
    ///
    /// The answer comes out of the local keybox alone. Nothing here asks a
    /// directory, so it is quick enough to ask again each time a recipient
    /// changes, and how far the chain behind a certificate reaches is left
    /// to the moment somebody reads a signature.
    pub fn certificates_for(&self, addresses: &[String]) -> Result<Vec<Recipient>, SmimeError> {
        self.held(addresses, false, 'e')
    }

    /// Which of `addresses` this computer can sign as: the ones gpgsm holds
    /// a secret key for.
    pub fn own_certificates(&self, addresses: &[String]) -> Result<Vec<Recipient>, SmimeError> {
        self.held(addresses, true, 's')
    }

    fn held(
        &self,
        addresses: &[String],
        secret: bool,
        capability: char,
    ) -> Result<Vec<Recipient>, SmimeError> {
        addresses
            .iter()
            .map(|address| {
                let listing = self.listing(&user_id(address), secret)?;
                Ok(Recipient {
                    address: address.clone(),
                    certificate: usable(&listing, address, capability),
                })
            })
            .collect()
    }

    /// The address on the certificate with this fingerprint, for a
    /// signature that names one.
    pub(crate) fn address_of(&self, fingerprint: &str) -> Option<String> {
        let listing = self.listing(fingerprint, false).ok()?;
        first_email(&listing)
    }

    fn listing(&self, wanted: &str, secret: bool) -> Result<String, SmimeError> {
        let run = self.run(&[], |command| {
            command.args([
                "--with-colons",
                if secret {
                    "--list-secret-keys"
                } else {
                    "--list-keys"
                },
                "--",
            ]);
            command.arg(wanted);
        })?;
        Ok(String::from_utf8_lossy(&run.out).into_owned())
    }
}

/// The certificate in a `--with-colons` listing that gpgsm would use, if
/// any.
///
/// The fields are the ones GnuPG documents in `doc/DETAILS`: a record's kind
/// first, then how far its owner is trusted, and at the twelfth field what
/// the certificate can do. A lower case `e` or `s` there means encryption
/// or signing.
pub fn usable(listing: &str, address: &str, capability: char) -> Option<Certificate> {
    let mut found: Option<Certificate> = None;
    for record in listing.lines() {
        let fields: Vec<&str> = record.split(':').collect();
        match fields.first() {
            Some(&"crt") | Some(&"crs") => {
                if found.is_some() {
                    break;
                }
                let validity = fields.get(1).copied().unwrap_or_default();
                let capable = fields.get(11).is_some_and(|capabilities| {
                    capabilities.contains(capability)
                        || capabilities.contains(capability.to_ascii_uppercase())
                });
                found = (capable && trusted(validity)).then(|| Certificate {
                    fingerprint: String::new(),
                    subject: fields.get(9).copied().unwrap_or_default().to_string(),
                    email: String::new(),
                });
            }
            Some(&"fpr") => {
                if let Some(certificate) = &mut found
                    && certificate.fingerprint.is_empty()
                {
                    certificate.fingerprint =
                        fields.get(9).copied().unwrap_or_default().to_string();
                }
            }
            Some(&"uid") => {
                let Some(certificate) = &mut found else {
                    continue;
                };
                let uid = fields.get(9).copied().unwrap_or_default();
                // A certificate carries its subject and its addresses as
                // user ids alike, and only the addresses are in brackets.
                if let Some(found) = bracketed(uid)
                    && (certificate.email.is_empty() || found.eq_ignore_ascii_case(address.trim()))
                {
                    certificate.email = found.to_string();
                }
            }
            _ => {}
        }
    }
    found.filter(|certificate| !certificate.fingerprint.is_empty())
}

/// The first address in a listing, for a certificate looked up by its
/// fingerprint rather than by an address.
fn first_email(listing: &str) -> Option<String> {
    listing.lines().find_map(|record| {
        let fields: Vec<&str> = record.split(':').collect();
        (fields.first() == Some(&"uid"))
            .then(|| bracketed(fields.get(9).copied().unwrap_or_default()))
            .flatten()
            .map(str::to_string)
    })
}

/// The address inside angle brackets, for a user id that is one.
fn bracketed(uid: &str) -> Option<&str> {
    uid.trim()
        .strip_prefix('<')
        .and_then(|rest| rest.strip_suffix('>'))
        .filter(|address| address.contains('@'))
}

/// Whether a certificate in this state can be used at all. Expired,
/// revoked, disabled and invalid ones cannot, whoever they belong to.
fn trusted(validity: &str) -> bool {
    !matches!(validity, "e" | "r" | "d" | "i")
}
