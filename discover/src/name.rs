//! Host and domain names as discovery accepts them.

/// The domain of `address` in lower case ASCII, with an international
/// name in its punycode form, or `None` when discovery must not ask
/// anyone about it.
pub(crate) fn domain_of(address: &str) -> Option<String> {
    let (local, domain) = address.trim().rsplit_once('@')?;
    if local.is_empty() {
        return None;
    }
    let ascii = match url::Host::parse(domain.strip_suffix('.').unwrap_or(domain)).ok()? {
        url::Host::Domain(ascii) => ascii,
        url::Host::Ipv4(_) | url::Host::Ipv6(_) => return None,
    };
    let domain = host(&ascii)?;
    // A public suffix such as `com` or `co.uk` is nobody's mail domain.
    // Asking about it would send requests to whoever registered
    // `autoconfig.com` or `imap.co.uk`.
    psl::domain_str(&domain)?;
    Some(domain)
}

/// `name` as a host name discovery may connect to: lower case, without
/// the root dot, at least two labels of letters, digits and inner
/// hyphens, and not an IP address. `None` otherwise.
pub(crate) fn host(name: &str) -> Option<String> {
    let name = name.strip_suffix('.').unwrap_or(name).to_ascii_lowercase();
    if name.is_empty() || name.len() > 253 {
        return None;
    }
    let labels: Vec<&str> = name.split('.').collect();
    let well_formed = labels.iter().all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    });
    // A last label of digits alone is an IPv4 address, not a name.
    let last_is_name = labels
        .last()
        .is_some_and(|last| !last.bytes().all(|b| b.is_ascii_digit()));
    (labels.len() >= 2 && well_formed && last_is_name).then_some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_domain_is_lower_case_ascii_without_the_root_dot() {
        assert_eq!(
            domain_of("Ann@FastMail.COM").as_deref(),
            Some("fastmail.com")
        );
        assert_eq!(
            domain_of("  ann@example.org.  ").as_deref(),
            Some("example.org")
        );
        assert_eq!(
            domain_of("ann@bücher.de").as_deref(),
            Some("xn--bcher-kva.de")
        );
    }

    #[test]
    fn the_last_at_sign_splits_the_address() {
        assert_eq!(
            domain_of("\"a@b\"@example.org").as_deref(),
            Some("example.org")
        );
    }

    #[test]
    fn an_address_without_a_usable_domain_has_none() {
        for address in [
            "",
            "ann",
            "@example.org",
            "ann@",
            "ann@localhost",
            "ann@com",
            "ann@co.uk",
            "ann@gmail",
            "ann@127.0.0.1",
            "ann@[::1]",
            "ann@exa mple.org",
            "ann@-example.org",
        ] {
            assert_eq!(domain_of(address), None, "{address}");
        }
    }

    #[test]
    fn a_host_name_needs_two_labels_and_no_ip_address() {
        assert_eq!(
            host("IMAP.Example.COM.").as_deref(),
            Some("imap.example.com")
        );
        assert_eq!(host("posteo.de").as_deref(), Some("posteo.de"));
        for name in [
            "",
            ".",
            "localhost",
            "127.0.0.1",
            "a..b",
            "-a.example.com",
            "a_b.example.com",
        ] {
            assert_eq!(host(name), None, "{name}");
        }
    }
}
