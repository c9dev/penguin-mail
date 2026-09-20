//! Who may load remote images.
//!
//! A remote image is a beacon: fetching it tells the sender the mail was
//! opened, from this address, at this minute. So nothing loads until the
//! reader says so, and this module holds the rule that decides whether
//! they already have.
//!
//! An entry names one address, or a whole domain. A domain entry covers
//! that domain and anything under it, which is what a mailing list needs,
//! and nothing else. Getting that comparison wrong hands the beacon to
//! whoever registers a lookalike, so it is written out here in one place
//! and tested from both sides.

use mailrs_store::image_senders::ImageSender;

/// Whether `from` may load remote images, given the whole list.
pub fn allowed(list: &[ImageSender], from: Option<&str>) -> bool {
    let Some(address) = from.map(str::trim).filter(|a| !a.is_empty()) else {
        return false;
    };
    let address = address.to_lowercase();
    let domain = domain_of(&address);
    list.iter().any(|entry| {
        if entry.whole_domain {
            domain.is_some_and(|domain| covers(&entry.sender, domain))
        } else {
            entry.sender == address
        }
    })
}

/// The domain half of an address, lower case. None when the address has no
/// `@`, or nothing after it.
pub fn domain_of(address: &str) -> Option<&str> {
    let (_, domain) = address.rsplit_once('@')?;
    let domain = domain.trim_end_matches('.');
    (!domain.is_empty()).then_some(domain)
}

/// Whether an entry for `rule` covers mail from `domain`: the same domain,
/// or one under it. `example.com` covers `mail.example.com` and refuses
/// both `notexample.com` and `example.com.evil.net`.
fn covers(rule: &str, domain: &str) -> bool {
    if rule == domain {
        return true;
    }
    domain
        .strip_suffix(rule)
        .is_some_and(|prefix| prefix.ends_with('.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn address(sender: &str) -> ImageSender {
        ImageSender {
            sender: sender.into(),
            whole_domain: false,
            allowed_at: 0,
        }
    }

    fn domain(sender: &str) -> ImageSender {
        ImageSender {
            sender: sender.into(),
            whole_domain: true,
            allowed_at: 0,
        }
    }

    #[test]
    fn an_allowed_address_loads_and_another_does_not() {
        let list = [address("ann@example.com")];
        assert!(allowed(&list, Some("ann@example.com")));
        assert!(!allowed(&list, Some("bob@example.com")));
        assert!(!allowed(&list, Some("ann@other.com")));
    }

    #[test]
    fn capitals_do_not_change_the_answer() {
        let list = [address("ann@example.com")];
        assert!(allowed(&list, Some("Ann@Example.COM")));
        assert!(allowed(&[domain("example.com")], Some("ANN@EXAMPLE.COM")));
    }

    #[test]
    fn a_message_with_no_sender_loads_nothing() {
        let list = [address("ann@example.com"), domain("example.com")];
        assert!(!allowed(&list, None));
        assert!(!allowed(&list, Some("")));
        assert!(!allowed(&list, Some("   ")));
    }

    #[test]
    fn a_domain_covers_what_is_under_it_and_nothing_beside_it() {
        let list = [domain("example.com")];
        assert!(allowed(&list, Some("news@example.com")));
        assert!(allowed(&list, Some("news@mail.example.com")));
        assert!(allowed(&list, Some("news@a.b.example.com")));
        // The cases that would hand the beacon to a stranger.
        assert!(!allowed(&list, Some("news@notexample.com")));
        assert!(!allowed(&list, Some("news@example.com.evil.net")));
        assert!(!allowed(&list, Some("news@evil.net")));
        assert!(!allowed(&list, Some("news@xexample.com")));
        // A domain entry is not an address entry.
        assert!(!allowed(&list, Some("example.com")));
    }

    #[test]
    fn an_address_entry_covers_only_that_address() {
        let list = [address("news@example.com")];
        assert!(!allowed(&list, Some("news@mail.example.com")));
        assert!(!allowed(&list, Some("other@example.com")));
    }

    #[test]
    fn a_trailing_dot_is_the_same_domain() {
        // A fully qualified name ends in a dot. It is the same host.
        assert!(allowed(&[domain("example.com")], Some("news@example.com.")));
        assert_eq!(domain_of("news@example.com."), Some("example.com"));
        assert_eq!(domain_of("nobody"), None);
        assert_eq!(domain_of("nobody@"), None);
    }
}
