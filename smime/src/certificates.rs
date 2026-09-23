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
    /// none for the address, or only ones that are expired, revoked,
    /// untrusted, or unable to do the job asked of them.
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

/// What a certificate is wanted for, which decides what counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Job {
    /// Encrypting a message to somebody. Only a certificate whose chain
    /// reaches a root in the person's trust list counts, because gpgsm
    /// puts every certificate a signature carries into the keybox when it
    /// checks that signature. Anyone can make a certificate that names
    /// `bob@company.test`, sign a message with it, and wait: a keybox that
    /// answered "what do you hold for Bob" with it would hand them the next
    /// message meant for Bob. A certificate merely seen in mail reaches no
    /// root, so it never counts here.
    Encrypt,
    /// Signing as one of the person's own addresses, which needs the
    /// secret key and nothing from the chain.
    Sign,
}

impl Smime {
    /// What gpgsm would encrypt to for each of `addresses`, in the order
    /// they were given. A caller offers encryption when every recipient has
    /// a certificate, and this says which one is missing when they do not.
    ///
    /// Only a certificate whose chain this computer trusts counts; see
    /// [`Job::Encrypt`] for why. The answer comes out of the local keybox
    /// in one listing, and gpgsm stays off the network while it checks each
    /// chain, so it is quick enough to ask again each time a recipient
    /// changes. Revocation waits for the moment the message is encrypted,
    /// when gpgsm checks the chain again in full.
    pub fn certificates_for(&self, addresses: &[String]) -> Result<Vec<Recipient>, SmimeError> {
        self.held(addresses, Job::Encrypt)
    }

    /// Which of `addresses` this computer can sign as: the ones gpgsm holds
    /// a secret key for.
    pub fn signing_certificates(&self, addresses: &[String]) -> Result<Vec<Recipient>, SmimeError> {
        self.held(addresses, Job::Sign)
    }

    fn held(&self, addresses: &[String], job: Job) -> Result<Vec<Recipient>, SmimeError> {
        if addresses.is_empty() {
            return Ok(Vec::new());
        }
        let wanted: Vec<String> = addresses.iter().map(|address| user_id(address)).collect();
        let listing = self.listing(&wanted, job)?;
        Ok(addresses
            .iter()
            .map(|address| Recipient {
                address: address.clone(),
                certificate: usable(&listing, address, job),
            })
            .collect())
    }

    /// The address on the certificate with this fingerprint, for a
    /// signature that names one.
    pub(crate) fn address_of(&self, fingerprint: &str) -> Option<String> {
        let run = self
            .read_only(&[], |command| {
                command.args(["--with-colons", "--list-keys", "--", fingerprint]);
            })
            .ok()?;
        first_email(&String::from_utf8_lossy(&run.out))
    }

    /// One listing for everything in `wanted`. For encryption gpgsm checks
    /// each chain as it lists, with dirmngr left out so nothing goes to the
    /// network, and marks the ones that reach a trusted root.
    fn listing(&self, wanted: &[String], job: Job) -> Result<String, SmimeError> {
        let run = self.read_only(&[], |command| {
            command.arg("--with-colons");
            match job {
                Job::Encrypt => {
                    command.args(["--with-validation", "--disable-dirmngr", "--list-keys"]);
                }
                Job::Sign => {
                    command.arg("--list-secret-keys");
                }
            }
            command.arg("--").args(wanted);
        })?;
        Ok(String::from_utf8_lossy(&run.out).into_owned())
    }
}

/// One certificate out of a `--with-colons` listing.
#[derive(Default)]
struct Listed {
    validity: String,
    capabilities: String,
    fingerprint: String,
    subject: String,
    emails: Vec<String>,
}

impl Listed {
    /// Whether this certificate can do `job` at all.
    fn does(&self, job: Job) -> bool {
        let letter = match job {
            Job::Encrypt => 'e',
            Job::Sign => 's',
        };
        let capable = self
            .capabilities
            .contains([letter, letter.to_ascii_uppercase()]);
        capable && !self.fingerprint.is_empty() && usable_for(job, &self.validity)
    }
}

/// The certificates in a `--with-colons` listing, in its order.
///
/// The fields are the ones GnuPG documents in `doc/DETAILS`: a record's kind
/// first, then its validity, and at the twelfth field what the certificate
/// can do. A lower case `e` or `s` there means encryption or signing.
fn certificates(listing: &str) -> Vec<Listed> {
    let mut found: Vec<Listed> = Vec::new();
    for record in listing.lines() {
        let fields: Vec<&str> = record.split(':').collect();
        let field = |index: usize| fields.get(index).copied().unwrap_or_default();
        match fields.first() {
            Some(&"crt") | Some(&"crs") => found.push(Listed {
                validity: field(1).to_string(),
                capabilities: field(11).to_string(),
                subject: field(9).to_string(),
                ..Listed::default()
            }),
            Some(&"fpr") => {
                if let Some(certificate) = found.last_mut()
                    && certificate.fingerprint.is_empty()
                {
                    certificate.fingerprint = field(9).to_string();
                }
            }
            // A certificate carries its subject and its addresses as user
            // ids alike, and only the addresses are in brackets.
            Some(&"uid") => {
                if let Some(certificate) = found.last_mut()
                    && let Some(email) = bracketed(field(9))
                {
                    certificate.emails.push(email.to_string());
                }
            }
            _ => {}
        }
    }
    found
}

/// The first certificate in a `--with-colons` listing that names `address`
/// and can do `job`, if any. A listing may hold several certificates for
/// one address, such as one that arrived in mail beside the one the person
/// trusts, and only one that passes counts, wherever it sits.
pub fn usable(listing: &str, address: &str, job: Job) -> Option<Certificate> {
    let address = address.trim().trim_matches(['<', '>']);
    certificates(listing)
        .into_iter()
        .filter(|certificate| certificate.does(job))
        .find_map(|certificate| {
            let email = certificate
                .emails
                .iter()
                .find(|email| email.eq_ignore_ascii_case(address))?
                .clone();
            Some(Certificate {
                fingerprint: certificate.fingerprint,
                subject: certificate.subject,
                email,
            })
        })
}

/// The first address in a listing, for a certificate looked up by its
/// fingerprint rather than by an address.
fn first_email(listing: &str) -> Option<String> {
    certificates(listing)
        .into_iter()
        .find_map(|certificate| certificate.emails.into_iter().next())
}

/// The address inside angle brackets, for a user id that is one.
fn bracketed(uid: &str) -> Option<&str> {
    uid.trim()
        .strip_prefix('<')
        .and_then(|rest| rest.strip_suffix('>'))
        .filter(|address| address.contains('@'))
}

/// Whether a certificate with this validity may do `job`.
///
/// Signing asks only that the certificate is not expired, revoked,
/// disabled or invalid. Encryption asks for `u`, a root in the person's
/// trust list, or `f`, a chain gpgsm validated up to one. A root nobody
/// trusts lists as `n`, and a certificate whose chain was never checked
/// lists with a blank; neither names anyone.
fn usable_for(job: Job, validity: &str) -> bool {
    match job {
        Job::Encrypt => matches!(validity, "u" | "f"),
        Job::Sign => !matches!(validity, "e" | "r" | "d" | "i"),
    }
}
