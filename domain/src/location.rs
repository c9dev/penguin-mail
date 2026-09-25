//! Where a message sits on an IMAP server.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Where a message sits on an IMAP server now: the mailbox by the server's
/// name for it, that mailbox's UIDVALIDITY, and the message's UID in it.
/// A move gives the message a new location. The store's id for the
/// message is the location where the app first met it, and stays.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Location {
    pub mailbox: String,
    pub uidvalidity: u32,
    pub uid: u32,
}

impl Location {
    /// Reads the text `Display` writes. A mailbox name may hold slashes,
    /// so the two numbers are read from the right.
    pub fn parse(text: &str) -> Option<Location> {
        let mut from_right = text.rsplitn(3, '/');
        let uid = from_right.next()?.parse().ok()?;
        let uidvalidity = from_right.next()?.parse().ok()?;
        let mailbox = from_right.next().filter(|m| !m.is_empty())?;
        Some(Location {
            mailbox: mailbox.to_string(),
            uidvalidity,
            uid,
        })
    }
}

impl fmt::Display for Location {
    /// `<mailbox>/<uidvalidity>/<uid>`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}/{}", self.mailbox, self.uidvalidity, self.uid)
    }
}

#[cfg(test)]
mod tests {
    use super::Location;

    fn at(mailbox: &str, uidvalidity: u32, uid: u32) -> Location {
        Location {
            mailbox: mailbox.into(),
            uidvalidity,
            uid,
        }
    }

    #[test]
    fn a_location_reads_back_what_it_writes() {
        let inbox = at("INBOX", 7, 42);
        assert_eq!(inbox.to_string(), "INBOX/7/42");
        assert_eq!(Location::parse("INBOX/7/42"), Some(inbox));
    }

    #[test]
    fn a_nested_mailbox_keeps_its_slashes() {
        let nested = at("Work/Clients", 3, 10);
        assert_eq!(Location::parse(&nested.to_string()), Some(nested));
    }

    #[test]
    fn a_gmail_id_or_a_bare_pair_of_numbers_is_no_location() {
        assert_eq!(Location::parse("18c2a4f09b7e1d3a"), None);
        assert_eq!(Location::parse("7/42"), None);
        assert_eq!(Location::parse("INBOX/seven/42"), None);
    }
}
