//! Which new mail still deserves a notification once the local rules ran.

/// Whether new mail still deserves its notification once the rules ran:
/// still in the Inbox and still unread.
pub(crate) fn still_news(in_inbox: bool, unread: bool) -> bool {
    in_inbox && unread
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mail_a_rule_moved_or_read_is_not_announced() {
        assert!(still_news(true, true));
        assert!(!still_news(false, true), "a rule moved it out of the Inbox");
        assert!(!still_news(true, false), "a rule marked it read");
    }
}
