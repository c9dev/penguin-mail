//! Addresses chosen by the account owner, including permitted local-part suffixes.

use mailrs_domain::{Address, MessageMeta};
use serde::{Deserialize, Serialize};

use super::SendAsAddress;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Suffixes {
    pub plus: bool,
    pub dot: bool,
}

impl Suffixes {
    pub fn enabled(self) -> bool {
        self.plus || self.dot
    }

    /// Only an opted-in base on the same domain may supply a reply's From.
    pub fn suffix<'a>(self, base: &str, email: &'a str) -> Option<&'a str> {
        if !sender_address(email) {
            return None;
        }
        let (local, domain) = base.split_once('@')?;
        let (candidate, host) = email.split_once('@')?;
        if !domain.eq_ignore_ascii_case(host)
            || !candidate.get(..local.len())?.eq_ignore_ascii_case(local)
        {
            return None;
        }
        let suffix = &candidate[local.len()..];
        match suffix.as_bytes() {
            [b'+', _, ..] if self.plus => Some(suffix),
            [b'.', _, ..] if self.dot => Some(suffix),
            _ => None,
        }
    }

    pub fn address(self, base: &Address, suffix: &str) -> Option<Address> {
        if suffix.is_empty() {
            return Some(base.clone());
        }
        let (local, domain) = base.email.split_once('@')?;
        let email = format!("{local}{suffix}@{domain}");
        self.suffix(&base.email, &email)?;
        Some(Address {
            name: base.name.clone(),
            email,
        })
    }
}

/// One bare dot-atom address, with no display name or header delimiters.
pub fn sender_address(email: &str) -> bool {
    let Some((local, domain)) = email.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && local.len() <= 64
        && email.len() <= 254
        && local.split('.').all(|part| !part.is_empty())
        && local
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".!#$%&'*+-/=?^_`{|}~".contains(&b))
        && domain.contains('.')
        && domain.split('.').all(|part| {
            !part.is_empty()
                && part.len() <= 63
                && !part.starts_with('-')
                && !part.ends_with('-')
                && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

/// An exact address wins over a suffix; overlapping bases use the longest.
pub fn sender_for<'a>(senders: &'a [SendAsAddress], email: &str) -> Option<&'a SendAsAddress> {
    senders
        .iter()
        .find(|s| s.email.eq_ignore_ascii_case(email))
        .or_else(|| {
            senders
                .iter()
                .filter(|s| s.suffixes.suffix(&s.email, email).is_some())
                .max_by_key(|s| s.email.len())
        })
}

/// Include the message's permitted suffix addresses when choosing From and
/// removing our own addresses from Reply All. Names come from preferences.
pub fn reply_addresses(senders: &[SendAsAddress], original: &MessageMeta) -> Vec<Address> {
    let mut mine: Vec<Address> = senders
        .iter()
        .map(|s| Address {
            name: s.name.clone(),
            email: s.email.clone(),
        })
        .collect();
    for address in original.to.iter().chain(&original.cc).chain(&original.from) {
        if !mine
            .iter()
            .any(|a| a.email.eq_ignore_ascii_case(&address.email))
            && let Some(base) = sender_for(senders, &address.email)
        {
            mine.push(Address {
                name: base.name.clone(),
                email: address.email.clone(),
            });
        }
    }
    mine
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suffixes_need_a_separator_an_opt_in_and_the_same_domain() {
        let plus = Suffixes {
            plus: true,
            dot: false,
        };
        for email in [
            "user.shop@example.com",
            "username@example.com",
            "user+shop@other.example",
            "user+@example.com",
            "user+shop@example.com\r\nBcc: a@b.com",
        ] {
            assert_eq!(plus.suffix("user@example.com", email), None, "{email}");
        }
        assert_eq!(
            plus.suffix("user@example.com", "USER+Shop@EXAMPLE.COM"),
            Some("+Shop")
        );
        assert_eq!(
            Suffixes::default().suffix("user@example.com", "user+shop@example.com"),
            None
        );
        let dot = Suffixes {
            dot: true,
            plus: false,
        };
        assert_eq!(
            dot.suffix("user.name@example.com", "user.name.shop@example.com"),
            Some(".shop")
        );
        assert_eq!(
            dot.suffix("user.name@example.com", "user.name..shop@example.com"),
            None
        );
    }

    #[test]
    fn exact_addresses_and_longer_bases_win() {
        let senders: Vec<SendAsAddress> = [
            "user@example.com",
            "user.shop@example.com",
            "user.shop.orders@example.com",
        ]
        .into_iter()
        .map(|email| SendAsAddress {
            email: email.into(),
            suffixes: Suffixes {
                plus: true,
                dot: true,
            },
            ..Default::default()
        })
        .collect();
        assert_eq!(
            sender_for(&senders, "user.shop@example.com"),
            Some(&senders[1])
        );
        assert_eq!(
            sender_for(&senders, "user.shop.orders+receipt@example.com"),
            Some(&senders[2])
        );
    }

    #[test]
    fn sender_fields_accept_only_one_bare_address() {
        for bad in [
            "",
            "a@@example.com",
            "a@example.com,b@example.com",
            "Name <a@example.com>",
            "a b@example.com",
            "a\n@example.com",
            ".a@example.com",
            "a@-example.com",
        ] {
            assert!(!sender_address(bad), "{bad}");
        }
        for good in [
            "user@example.com",
            "user.name+shop@second.example",
            "o'neil@example.com",
        ] {
            assert!(sender_address(good), "{good}");
        }
    }
}
