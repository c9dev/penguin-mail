//! What gpg's `--status-fd` lines say about a signature.
//!
//! gpg prints two things: a sentence for the person reading the terminal,
//! which is translated and free to change, and these lines, which are the
//! documented interface. Only these are read here.

/// What gpg made of a signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signature {
    pub verdict: Verdict,
    /// The user id on the signing key, as `Ada Lovelace <ada@example.com>`.
    /// Absent when gpg has no key to name.
    pub signer: Option<String>,
    /// The signing key's fingerprint, which gpg reports for a signature it
    /// could check.
    pub fingerprint: Option<String>,
    pub key_id: Option<String>,
    /// How far the owner of the signing key is trusted, which is a separate
    /// question from whether the signature matches.
    pub trust: Trust,
    /// Every name on the signing key, each with its own validity. The
    /// status lines give only one, so [`crate::Pgp::verify`] and the other
    /// reads fill these from the keyring once gpg names the key.
    pub user_ids: Vec<crate::UserId>,
}

impl Signature {
    /// Whether the text arrived as the signer wrote it and the key behind it
    /// is in good standing. An expired or revoked key still gives a match,
    /// so the caller who wants to show that says so from [`Signature::verdict`].
    pub fn is_good(&self) -> bool {
        self.verdict == Verdict::Good
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The text is the text that was signed, by a key in good standing.
    Good,
    /// The text and the signature disagree. Something changed on the way.
    Bad,
    /// The signature matches, but the key expired.
    ExpiredKey,
    /// The signature matches, but the owner revoked the key.
    RevokedKey,
    /// The signature matches, but it carried an expiry that has passed.
    Expired,
    /// This computer holds no key for the signer, so there is nothing to
    /// check the signature against.
    NoKey,
    /// gpg could not check it and gave another reason.
    Unchecked,
}

/// How far the key's owner is trusted, out of the person's own trust
/// database. gpg answers this for every signature it checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trust {
    /// Nobody has said anything about this key's owner.
    Unknown,
    /// The person marked this key as one not to trust.
    Never,
    Marginal,
    Full,
    /// One of the person's own keys.
    Ultimate,
}

/// The first signature the status lines describe, or `None` when they
/// describe none.
pub fn signature<S: AsRef<str>>(status: &[S]) -> Option<Signature> {
    signatures(status).into_iter().next()
}

/// Every signature the status lines describe, in order. A detached
/// signature can carry several, and a reader who saw only one of them
/// could miss the one that does not match.
pub fn signatures<S: AsRef<str>>(status: &[S]) -> Vec<Signature> {
    crate::gnupg::seen(status)
        .into_iter()
        .map(|seen| Signature {
            verdict: seen.verdict,
            signer: seen.name,
            fingerprint: seen.fingerprint,
            key_id: seen.id,
            trust: seen.trust.unwrap_or(Trust::Unknown),
            user_ids: Vec::new(),
        })
        .collect()
}
