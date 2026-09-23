//! What gpgsm's `--status-fd` lines say about a signature.
//!
//! gpgsm prints two things: a sentence for the person reading the terminal,
//! which is translated and free to change, and these lines, which are the
//! documented interface. Only these are read here. The keywords are the
//! ones gpg writes as well, since both binaries report through the same
//! library, but what they carry differs: a certificate is named by its
//! fingerprint and its subject, and the trust line is about the chain
//! rather than about a web of signatures.

/// What gpgsm made of a signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signature {
    pub verdict: Verdict,
    /// The subject of the signing certificate, as gpgsm writes a
    /// distinguished name: `/CN=Ada Lovelace/O=Example`. Absent when gpgsm
    /// has no certificate to name.
    pub subject: Option<String>,
    /// The addresses on that certificate, in its own order. They sit in
    /// the certificate rather than in the status lines, so
    /// [`crate::Smime::verify`] looks them up and fills them in.
    pub emails: Vec<String>,
    /// The signing certificate's fingerprint, which gpgsm reports for a
    /// signature it could check.
    pub fingerprint: Option<String>,
    /// How far the chain behind the certificate got, which is a separate
    /// question from whether the signature matches.
    pub chain: Chain,
}

impl Signature {
    /// Whether the text arrived as the signer wrote it and the certificate
    /// behind it is in good standing. An expired or revoked certificate
    /// still gives a match, so the caller who wants to show that says so
    /// from [`Signature::verdict`], and a chain that reached no trusted
    /// root says so from [`Signature::chain`].
    pub fn is_good(&self) -> bool {
        self.verdict == Verdict::Good
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The text is the text that was signed, by a certificate in good
    /// standing.
    Good,
    /// The text and the signature disagree. Something changed on the way.
    Bad,
    /// The signature matches, but the certificate has run out.
    ExpiredCertificate,
    /// The signature matches, but the certificate was revoked.
    RevokedCertificate,
    /// The signature matches, but it carried an expiry that has passed.
    Expired,
    /// This computer holds no certificate for the signer, and the message
    /// carried none, so there is nothing to check the signature against.
    NoCertificate,
    /// gpgsm could not check it and gave another reason.
    Unchecked,
}

/// How far the chain from the signing certificate towards a root got. A
/// certificate whose chain reaches no root this computer trusts still signs
/// perfectly well; it just says nothing about who the signer is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chain {
    /// It reached a root in the person's own trust list.
    Trusted,
    /// It did not: an issuer is missing, the root is one nobody here has
    /// vouched for, or a certificate along the way was revoked.
    Untrusted,
    /// gpgsm said nothing about the chain.
    Unknown,
}

/// The signature the status lines describe, or `None` when they describe
/// none.
pub fn signature<S: AsRef<str>>(status: &[S]) -> Option<Signature> {
    let mut found: Option<Signature> = None;
    for line in status {
        let (keyword, rest) = split(line.as_ref());
        let verdict = match keyword {
            "GOODSIG" => Verdict::Good,
            "BADSIG" => Verdict::Bad,
            "EXPKEYSIG" => Verdict::ExpiredCertificate,
            "REVKEYSIG" => Verdict::RevokedCertificate,
            "EXPSIG" => Verdict::Expired,
            "ERRSIG" => Verdict::Unchecked,
            // gpgsm reports a certificate it could not find against the
            // step that went looking, rather than as an `ERRSIG` with a
            // reason code the way gpg does.
            "ERROR" if rest.starts_with("verify.findkey") => Verdict::NoCertificate,
            "NO_PUBKEY" => Verdict::NoCertificate,
            "VALIDSIG" => {
                if let Some(found) = &mut found {
                    found.fingerprint = rest.split_whitespace().next().map(str::to_string);
                }
                continue;
            }
            "TRUST_UNDEFINED" | "TRUST_NEVER" | "TRUST_MARGINAL" | "TRUST_FULLY"
            | "TRUST_ULTIMATE" => {
                if let Some(found) = &mut found {
                    found.chain = chain(keyword);
                }
                continue;
            }
            _ => continue,
        };
        // `GOODSIG <fingerprint> <subject>`, and the same shape for the
        // verdicts beside it. The line that names no certificate is an
        // error report rather than one of these, so the fingerprint is
        // what says whether there is anything here to read.
        let (fingerprint, subject) = match rest.split_once(' ') {
            Some((fingerprint, subject)) if hexadecimal(fingerprint) => (
                Some(fingerprint.to_string()),
                Some(subject.trim().to_string()),
            ),
            _ => (None, None),
        };
        found = Some(Signature {
            verdict,
            subject,
            emails: Vec::new(),
            fingerprint,
            chain: Chain::Unknown,
        });
    }
    found
}

/// Whether the trust line says the chain reached a root. gpgsm answers with
/// the same keywords gpg uses for its web of trust, but only a full or
/// ultimate answer means the chain validated; the rest are the ways it
/// failed to.
fn hexadecimal(word: &str) -> bool {
    !word.is_empty() && word.chars().all(|c| c.is_ascii_hexdigit())
}

fn chain(keyword: &str) -> Chain {
    match keyword {
        "TRUST_FULLY" | "TRUST_ULTIMATE" => Chain::Trusted,
        _ => Chain::Untrusted,
    }
}

fn split(line: &str) -> (&str, &str) {
    match line.split_once(' ') {
        Some((keyword, rest)) => (keyword, rest.trim_start()),
        None => (line, ""),
    }
}
