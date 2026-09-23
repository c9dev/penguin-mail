//! What gpgsm's `--status-fd` lines say about a signature.
//!
//! gpgsm prints two things: a sentence for the person reading the terminal,
//! which is translated and free to change, and these lines, which are the
//! documented interface. Only these are read here. The keywords are the
//! ones gpg writes as well, since both binaries report through the same
//! library, but what they carry differs: a certificate is named by its
//! fingerprint and its subject, and the trust line is about the chain
//! rather than about a web of signatures.

use mailrs_pgp::{Trust, Verdict as PgpVerdict};

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
    /// It reached a root in the person's own trust list, and nothing could
    /// say whether a certificate along the way was revoked: dirmngr could
    /// not fetch the CRL, could not be reached, or did not answer in time.
    /// The status lines alone never say this. [`crate::Smime::verify`]
    /// finds it by asking gpgsm a second time with CRL checks off.
    RevocationUnknown,
    /// gpgsm said nothing about the chain.
    Unknown,
}

/// The first signature the status lines describe, or `None` when they
/// describe none.
pub fn signature<S: AsRef<str>>(status: &[S]) -> Option<Signature> {
    signatures(status).into_iter().next()
}

/// Every signature the status lines describe, in order, read through the
/// parser gpg's lines go through too.
pub fn signatures<S: AsRef<str>>(status: &[S]) -> Vec<Signature> {
    mailrs_pgp::gnupg::seen(status)
        .into_iter()
        .map(|seen| {
            // `GOODSIG <fingerprint> <subject>`, and the same shape for the
            // verdicts beside it. The line that names no certificate is an
            // error report rather than one of these, so the fingerprint is
            // what says whether there is anything here to read.
            let named = seen.id.as_deref().is_some_and(hexadecimal);
            // gpgsm writes no `REVKEYSIG` for a certificate its authority's
            // CRL lists. It writes `GOODSIG`, since the text is the text
            // that certificate signed, and says what is wrong on the trust
            // line: `TRUST_NEVER 94`, where 94 is `GPG_ERR_CERT_REVOKED`.
            let revoked =
                seen.trust == Some(Trust::Never) && seen.trust_code == Some(CERT_REVOKED);
            Signature {
                verdict: match seen.verdict {
                    PgpVerdict::Good if revoked => Verdict::RevokedCertificate,
                    PgpVerdict::Good => Verdict::Good,
                    PgpVerdict::Bad => Verdict::Bad,
                    PgpVerdict::ExpiredKey => Verdict::ExpiredCertificate,
                    PgpVerdict::RevokedKey => Verdict::RevokedCertificate,
                    PgpVerdict::Expired => Verdict::Expired,
                    PgpVerdict::NoKey => Verdict::NoCertificate,
                    PgpVerdict::Unchecked => Verdict::Unchecked,
                },
                subject: seen.name.filter(|_| named),
                emails: Vec::new(),
                fingerprint: seen.fingerprint.or(seen.id.filter(|_| named)),
                chain: match seen.trust {
                    // gpgsm answers with the keywords gpg uses for its web
                    // of trust, but only a full or ultimate answer means
                    // the chain validated; the rest are the ways it failed
                    // to.
                    Some(Trust::Full | Trust::Ultimate) => Chain::Trusted,
                    Some(_) => Chain::Untrusted,
                    None => Chain::Unknown,
                },
            }
        })
        .collect()
}

/// GnuPG's error code for a revoked certificate, `GPG_ERR_CERT_REVOKED`.
const CERT_REVOKED: u32 = 94;

fn hexadecimal(word: &str) -> bool {
    !word.is_empty() && word.chars().all(|c| c.is_ascii_hexdigit())
}
