//! Which addresses this computer can encrypt to.
//!
//! gpg answers in its colon-separated listing, which is documented and does
//! not move between releases, rather than in the table it draws for a person.

use crate::error::PgpError;
use crate::gnupg::{Pinentry, user_id};
use crate::gpg::Pgp;
use crate::status::{Signature, Trust};

/// One address a message is going to, and what gpg holds for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recipient {
    pub address: String,
    /// The key gpg would encrypt to. `None` when this computer holds no key
    /// for the address, or only keys that are expired, revoked, or unable to
    /// encrypt.
    pub key: Option<Key>,
}

/// A key that can take a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Key {
    pub fingerprint: String,
    /// The user id gpg lists for the address that was asked about.
    pub user_id: String,
    /// How far the person's own trust database vouches for the owner. It has
    /// no bearing on whether the message can be encrypted, only on how much
    /// the recipient's identity is worth.
    pub trust: Trust,
}

/// One name on a key, and how far the person's trust database vouches
/// for it. gpg weighs each user id on its own: a key somebody vouched for
/// under one name can carry another that its owner added and nobody
/// vouched for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserId {
    /// As `Ada Lovelace <ada@example.com>`.
    pub user_id: String,
    pub trust: Trust,
}

impl Pgp {
    /// The signature with every name on its key filled in, from the local
    /// keyring. A signature gpg could not tie to a key keeps none.
    pub(crate) fn named(&self, mut signature: Signature) -> Signature {
        let Some(fingerprint) = signature.fingerprint.clone() else {
            return signature;
        };
        if let Ok(run) = self.run(&[], Pinentry::Never, |command| {
            command
                .args(["--with-colons", "--list-keys", "--"])
                .arg(fingerprint);
        }) {
            signature.user_ids = user_ids(&String::from_utf8_lossy(&run.out));
        }
        signature
    }

    /// What gpg holds for each of `addresses`, in the order they were given.
    /// A caller offers encryption when every recipient has a key, and this
    /// says which one is missing when they do not.
    ///
    /// The answer comes out of the local keyring alone. Nothing here goes
    /// looking on a key server, so it is quick enough to ask again each time
    /// a recipient is added.
    pub fn keys_for(&self, addresses: &[String]) -> Result<Vec<Recipient>, PgpError> {
        if addresses.is_empty() {
            return Ok(Vec::new());
        }
        // One listing for every address: the composer asks again each time
        // a recipient changes, and a gpg per address adds up.
        let run = self.run(&[], Pinentry::Never, |command| {
            command.args(["--with-colons", "--list-keys", "--"]);
            command.args(addresses.iter().map(|address| user_id(address)));
        })?;
        // gpg leaves with an error when a name matches no key, which is an
        // answer rather than a failure.
        let listing = String::from_utf8_lossy(&run.out);
        Ok(addresses
            .iter()
            .map(|address| Recipient {
                address: address.clone(),
                key: usable(&listing, address),
            })
            .collect())
    }
}

/// The key in a `--with-colons` listing that gpg would encrypt to, if any.
///
/// The fields are the ones GnuPG documents in `doc/DETAILS`: a record's kind
/// first, then how far its owner is trusted, and at the twelfth field what
/// the key can do. An upper case `E` there means the key or one of its
/// subkeys takes encryption.
pub fn usable(listing: &str, address: &str) -> Option<Key> {
    let address = address.trim().trim_matches(['<', '>']);
    keys(listing).into_iter().find_map(|listed| {
        if !listed.capable || !trusted(&listed.validity) || listed.fingerprint.is_empty() {
            return None;
        }
        // A key can carry several addresses, vouched for one by one. The
        // one being written to is the one whose trust counts, and a user id
        // its owner revoked names nobody.
        let (uid, validity) = listed
            .uids
            .iter()
            .find(|(uid, validity)| trusted(validity) && names(uid, address))?;
        Some(Key {
            fingerprint: listed.fingerprint.clone(),
            user_id: uid.clone(),
            trust: trust(validity),
        })
    })
}

/// One key out of a `--with-colons` listing.
struct Listed {
    validity: String,
    capable: bool,
    fingerprint: String,
    /// Every user id, with its own validity.
    uids: Vec<(String, String)>,
}

/// The keys in a `--with-colons` listing, in its order.
fn keys(listing: &str) -> Vec<Listed> {
    let mut found: Vec<Listed> = Vec::new();
    for record in listing.lines() {
        let fields: Vec<&str> = record.split(':').collect();
        let field = |index: usize| fields.get(index).copied().unwrap_or_default();
        match fields.first() {
            Some(&"pub") => found.push(Listed {
                validity: field(1).to_string(),
                capable: field(11).contains('E'),
                fingerprint: String::new(),
                uids: Vec::new(),
            }),
            Some(&"fpr") => {
                if let Some(key) = found.last_mut()
                    && key.fingerprint.is_empty()
                {
                    key.fingerprint = field(9).to_string();
                }
            }
            Some(&"uid") => {
                if let Some(key) = found.last_mut() {
                    key.uids.push((unescape(field(9)), field(1).to_string()));
                }
            }
            _ => {}
        }
    }
    found
}

/// Whether a user id names `address`: in angle brackets, as gpg matched
/// it, or as the whole user id.
fn names(uid: &str, address: &str) -> bool {
    let uid = uid.trim();
    let named = match uid.rsplit_once('<') {
        Some((_, rest)) => rest.strip_suffix('>').unwrap_or(rest),
        None => uid,
    };
    named.trim().eq_ignore_ascii_case(address)
}

/// The names on the first key of a `--with-colons` listing, each with the
/// validity gpg gives it. A revoked or expired name names nobody.
pub fn user_ids(listing: &str) -> Vec<UserId> {
    let mut found = Vec::new();
    let mut keys = 0;
    for record in listing.lines() {
        let fields: Vec<&str> = record.split(':').collect();
        let field = |index: usize| fields.get(index).copied().unwrap_or_default();
        match fields.first() {
            Some(&"pub") => {
                keys += 1;
                if keys > 1 {
                    break;
                }
            }
            Some(&"uid") if trusted(field(1)) => found.push(UserId {
                user_id: unescape(field(9)),
                trust: trust(field(1)),
            }),
            _ => {}
        }
    }
    found
}

/// A user id as gpg writes it in a colon listing, where a colon and a few
/// other bytes come as `\x3a` and the like.
fn unescape(field: &str) -> String {
    let mut out = Vec::with_capacity(field.len());
    let bytes = field.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'\\'
            && bytes.get(at + 1) == Some(&b'x')
            && let Some(byte) = field
                .get(at + 2..at + 4)
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
        {
            out.push(byte);
            at += 4;
            continue;
        }
        out.push(bytes[at]);
        at += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Whether a key in this state can be used at all. Expired, revoked,
/// disabled and invalid keys cannot, whatever their owner is worth.
fn trusted(validity: &str) -> bool {
    !matches!(validity, "e" | "r" | "d" | "i")
}

fn trust(validity: &str) -> Trust {
    match validity {
        "n" => Trust::Never,
        "m" => Trust::Marginal,
        "f" => Trust::Full,
        "u" | "w" => Trust::Ultimate,
        _ => Trust::Unknown,
    }
}
