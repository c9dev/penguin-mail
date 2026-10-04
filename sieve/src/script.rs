//! The one Sieve script Penguin Mail keeps on a server.

use std::collections::BTreeSet;

/// The Sieve extensions a server offers, from ManageSieve's `SIEVE`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Extensions(BTreeSet<String>);

impl Extensions {
    pub fn parse(listed: &str) -> Extensions {
        Extensions(listed.split_whitespace().map(str::to_ascii_lowercase).collect())
    }

    pub fn has(&self, name: &str) -> bool {
        self.0.contains(name)
    }

    /// What Penguin Mail needs before it keeps rules on the server.
    pub fn usable(&self) -> bool {
        self.has("fileinto") && self.has("vacation")
    }
}

#[cfg(test)]
mod extension_tests {
    use super::*;

    #[test]
    fn a_server_needs_fileinto_and_vacation() {
        assert!(Extensions::parse("fileinto reject vacation").usable());
        assert!(!Extensions::parse("fileinto reject").usable());
        assert!(Extensions::parse("FileInto Vacation").has("vacation"));
    }
}
