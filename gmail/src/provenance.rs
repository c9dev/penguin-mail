//! Where a message actually came from, read off its headers.
//!
//! The From line is whatever the sender typed. These three are not: the
//! domain that handed the message to the recipient's server, the domain
//! whose key signed it, and whether the last hop was encrypted. Gmail
//! shows the same three when you open the details of a message, and they
//! are what tells a real invoice from one that only looks like it came
//! from the same company.

use mailrs_domain::Provenance;

use crate::convert::find_header;
use crate::model::MessagePart;

/// Reads the three lines the details panel shows. Every one of them is
/// absent for plenty of real mail, and absent is an answer.
pub fn provenance(payload: &MessagePart) -> Provenance {
    let authentication = find_header(payload, "Authentication-Results").unwrap_or_default();
    Provenance {
        mailed_by: mailed_by(find_header(payload, "Return-Path"), authentication),
        signed_by: signed_by(find_header(payload, "DKIM-Signature"), authentication),
        encrypted: encrypted(find_header(payload, "Received")),
    }
}

/// The domain that handed the message over. The envelope sender says it
/// outright; failing that, the domain SPF checked.
fn mailed_by(return_path: Option<&str>, authentication: &str) -> Option<String> {
    let envelope = return_path
        .map(|path| path.trim().trim_start_matches('<').trim_end_matches('>'))
        .filter(|path| !path.is_empty());
    if let Some(domain) = envelope.and_then(domain_of) {
        return Some(domain);
    }
    // `spf=pass ... smtp.mailfrom=bounce@example.com`
    value_after(authentication, "smtp.mailfrom=").and_then(|from| domain_of(&from))
}

/// The domain whose key signed the message: the `d=` of its DKIM
/// signature, or the one the recipient's server says passed.
fn signed_by(dkim: Option<&str>, authentication: &str) -> Option<String> {
    // `dkim=pass header.d=example.com` is the checked answer, so it wins
    // over a signature header that may not have verified.
    if let Some(domain) = value_after(authentication, "header.d=") {
        return Some(domain.to_ascii_lowercase());
    }
    let tags = dkim?;
    tags.split(';')
        .map(str::trim)
        .find_map(|tag| tag.strip_prefix("d="))
        .map(|domain| domain.trim().to_ascii_lowercase())
}

/// Whether the last hop used TLS. `Received` records how the server took
/// the message: ESMTPS and ESMTPSA are the TLS ones, and a parenthesized
/// `using TLSv1.3` says so outright.
fn encrypted(received: Option<&str>) -> Option<bool> {
    let received = received?;
    let lower = received.to_ascii_lowercase();
    if lower.contains("using tls") || lower.contains("version=tls") {
        return Some(true);
    }
    let with = lower
        .split_whitespace()
        .skip_while(|word| *word != "with")
        .nth(1)?;
    // `esmtps`, `esmtpsa`, and Microsoft's `smtps`. Plain `esmtp` is not.
    Some(with.starts_with("esmtps") || with.starts_with("smtps"))
}

/// The part of an address after the `@`, lower case.
fn domain_of(address: &str) -> Option<String> {
    let (_, domain) = address.rsplit_once('@')?;
    let domain = domain.trim().trim_end_matches('.').to_ascii_lowercase();
    (!domain.is_empty() && domain.contains('.')).then_some(domain)
}

/// The word after `key` in a header, up to the next space or semicolon.
fn value_after(header: &str, key: &str) -> Option<String> {
    let at = header.find(key)? + key.len();
    let rest = &header[at..];
    let end = rest
        .find(|c: char| c.is_whitespace() || c == ';')
        .unwrap_or(rest.len());
    let found = rest[..end].trim();
    (!found.is_empty()).then(|| found.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_envelope_sender_names_who_mailed_it() {
        assert_eq!(
            mailed_by(Some("<cf-bounce@notify.cloudflare.com>"), ""),
            Some("notify.cloudflare.com".into())
        );
        // Angle brackets are optional, and the case does not matter.
        assert_eq!(
            mailed_by(Some("Bounce@Example.COM"), ""),
            Some("example.com".into())
        );
    }

    #[test]
    fn spf_names_who_mailed_it_when_there_is_no_envelope_sender() {
        let authentication = "mx.google.com; spf=pass (google.com: domain of \
                              bounce@mail.example.org designates 1.2.3.4) \
                              smtp.mailfrom=bounce@mail.example.org";
        assert_eq!(
            mailed_by(None, authentication),
            Some("mail.example.org".into())
        );
        // An empty envelope sender is a bounce, and says nothing.
        assert_eq!(mailed_by(Some("<>"), ""), None);
    }

    #[test]
    fn the_checked_dkim_domain_beats_the_signature_header() {
        let header = "v=1; a=rsa-sha256; c=relaxed/relaxed; d=forged.example; s=k1";
        // The signature claims one domain; the server says another passed.
        assert_eq!(
            signed_by(
                Some(header),
                "mx.google.com; dkim=pass header.d=real.example"
            ),
            Some("real.example".into())
        );
        // With nothing checked, the signature is all there is.
        assert_eq!(signed_by(Some(header), ""), Some("forged.example".into()));
        assert_eq!(signed_by(None, ""), None);
    }

    #[test]
    fn received_says_whether_the_last_hop_was_encrypted() {
        assert_eq!(
            encrypted(Some(
                "from mail.example.com by mx.google.com with ESMTPS id 4"
            )),
            Some(true)
        );
        assert_eq!(
            encrypted(Some("from a by b with ESMTPSA id 7 (version=TLS1_3)")),
            Some(true)
        );
        assert_eq!(
            encrypted(Some("from a by b with ESMTP id 9")),
            Some(false),
            "plain ESMTP is not TLS"
        );
        assert_eq!(
            encrypted(Some("by mx.example (Postfix) id 3 (using TLSv1.3)")),
            Some(true)
        );
        assert_eq!(encrypted(None), None);
    }

    #[test]
    fn a_domain_needs_a_dot_and_something_before_it() {
        assert_eq!(domain_of("someone@example.com"), Some("example.com".into()));
        assert_eq!(domain_of("someone@localhost"), None);
        assert_eq!(domain_of("someone@"), None);
        assert_eq!(domain_of("nobody"), None);
        assert_eq!(domain_of("a@example.com."), Some("example.com".into()));
    }
}
