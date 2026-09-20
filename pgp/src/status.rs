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

/// The signature the status lines describe, or `None` when they describe none.
pub fn signature<S: AsRef<str>>(status: &[S]) -> Option<Signature> {
    let mut found: Option<Signature> = None;
    for line in status {
        let (keyword, rest) = split(line.as_ref());
        let verdict = match keyword {
            "GOODSIG" => Verdict::Good,
            "BADSIG" => Verdict::Bad,
            "EXPKEYSIG" => Verdict::ExpiredKey,
            "REVKEYSIG" => Verdict::RevokedKey,
            "EXPSIG" => Verdict::Expired,
            // `ERRSIG <keyid> <algo> <hash> <class> <time> <rc>`, where 9
            // means gpg has no key for the signer.
            "ERRSIG" => match rest.split_whitespace().nth(5) {
                Some("9") => Verdict::NoKey,
                _ => Verdict::Unchecked,
            },
            // gpg says this beside `ERRSIG 9`, and on its own when it read a
            // signature it could not even look a key up for.
            "NO_PUBKEY" => Verdict::NoKey,
            "VALIDSIG" => {
                if let Some(found) = &mut found {
                    found.fingerprint = rest.split_whitespace().next().map(str::to_string);
                }
                continue;
            }
            "TRUST_UNDEFINED" | "TRUST_NEVER" | "TRUST_MARGINAL" | "TRUST_FULLY"
            | "TRUST_ULTIMATE" => {
                if let Some(found) = &mut found {
                    found.trust = trust(keyword);
                }
                continue;
            }
            _ => continue,
        };
        // `GOODSIG <long key id> <user id>`; ERRSIG carries no user id.
        let (key_id, signer) = match rest.split_once(' ') {
            Some((key_id, signer)) => (key_id, Some(signer.trim().to_string())),
            None => (rest, None),
        };
        found = Some(Signature {
            verdict,
            signer: signer.filter(|_| verdict != Verdict::NoKey && verdict != Verdict::Unchecked),
            fingerprint: None,
            key_id: (!key_id.is_empty()).then(|| key_id.to_string()),
            trust: Trust::Unknown,
        });
    }
    found
}

fn trust(keyword: &str) -> Trust {
    match keyword {
        "TRUST_NEVER" => Trust::Never,
        "TRUST_MARGINAL" => Trust::Marginal,
        "TRUST_FULLY" => Trust::Full,
        "TRUST_ULTIMATE" => Trust::Ultimate,
        _ => Trust::Unknown,
    }
}

fn split(line: &str) -> (&str, &str) {
    match line.split_once(' ') {
        Some((keyword, rest)) => (keyword, rest.trim_start()),
        None => (line, ""),
    }
}
