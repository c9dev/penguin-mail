//! Changes a person makes to an account's list of calendars, rather than
//! to the events on one: a new calendar, a new name, a colour, hiding one
//! from the list, deleting it, and subscribing to one someone else
//! publishes. Each is a [`ListEdit`], queued and sent like an event
//! change, so it survives going offline and shows in the local copy at
//! once. [`allows`] says which of them a calendar takes.

use serde::{Deserialize, Serialize};

use super::{Access, Calendar};

/// The start of the id a calendar made on this computer carries until
/// the provider names it. Google picks a new calendar's id itself, so
/// the copy files it under one of these and swaps it for Google's once
/// the queue sends it.
pub const LOCAL_ID_PREFIX: &str = "new:";

/// One change to an account's calendar list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ListEdit {
    /// A new calendar the account owns, in `color` (`#rrggbb`), whose
    /// events default to `zone`. An empty zone leaves it to the provider.
    Create { name: String, color: String, zone: String },
    /// A new name for a calendar the account owns.
    Rename { name: String },
    /// Deletes a calendar the account owns, with every event on it.
    Delete,
    /// Takes a calendar the account does not own off its list, on every
    /// device: a subscription, a holiday calendar or one shared with it.
    Unsubscribe,
    /// A new colour, `#rrggbb`, on every device the account uses.
    Recolor { color: String },
    /// Hides the calendar from the account's list, or shows it again, on
    /// every device.
    Hide { hidden: bool },
    /// Subscribes to a calendar published at an `http` or `https`
    /// address, which the provider then fetches.
    Subscribe { url: String },
    /// Adds a calendar the provider already publishes, such as a public
    /// holiday calendar, by its id.
    Add,
}

impl ListEdit {
    /// Whether this edit makes a calendar that was not on the list.
    pub fn adds(&self) -> bool {
        matches!(self, ListEdit::Create { .. } | ListEdit::Subscribe { .. } | ListEdit::Add)
    }
}

/// What a person may change about one calendar, from whose it is. The
/// permissions the account granted are a separate question, which the
/// sync crate answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Allows {
    /// Rename it: only a calendar the account owns.
    pub rename: bool,
    /// Delete it: a calendar the account owns, other than its primary.
    pub delete: bool,
    /// Take it off the list: a calendar the account does not own.
    pub unsubscribe: bool,
}

/// What a person may change about `calendar`. A subscribed calendar, a
/// holiday calendar and one shared by someone else are read or written
/// but not owned, so they take neither, and are unsubscribed from instead.
pub fn allows(calendar: &Calendar) -> Allows {
    let owned = calendar.access == Access::Owner;
    Allows { rename: owned, delete: owned && !calendar.primary, unsubscribe: !owned && !calendar.primary }
}

impl ListEdit {
    /// Whether this edit takes the calendar off the list.
    pub fn removes(&self) -> bool {
        matches!(self, ListEdit::Delete | ListEdit::Unsubscribe)
    }
}

/// Whether `id` names a calendar made on this computer that the provider
/// has not taken yet.
pub fn is_local(id: &str) -> bool {
    id.starts_with(LOCAL_ID_PREFIX)
}

/// The address to subscribe to, from what a person typed: an `http`,
/// `https` or `webcal` address with a host. `webcal` is the same feed
/// over the web, and the provider fetches it over `https`. `None` for
/// anything else.
pub fn subscription_address(typed: &str) -> Option<String> {
    let typed = typed.trim();
    let (scheme, rest) = typed.split_once("://")?;
    let scheme = match scheme.to_ascii_lowercase().as_str() {
        "http" => "http",
        "https" | "webcal" | "webcals" => "https",
        _ => return None,
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if host.is_empty() || rest.chars().any(char::is_whitespace) {
        return None;
    }
    Some(format!("{scheme}://{rest}"))
}

/// A name for a subscribed calendar until the provider reads its own:
/// the host the feed comes from.
pub fn subscription_name(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    rest.split(['/', '?', '#']).next().unwrap_or(rest).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn calendar(access: Access, primary: bool) -> Calendar {
        Calendar { id: "c".into(), name: "C".into(), access, primary, ..Calendar::default() }
    }

    #[test]
    fn an_owned_calendar_can_be_renamed_and_deleted() {
        assert_eq!(allows(&calendar(Access::Owner, false)), Allows { rename: true, delete: true, unsubscribe: false });
    }

    #[test]
    fn the_primary_calendar_can_be_renamed_but_not_deleted() {
        assert_eq!(allows(&calendar(Access::Owner, true)), Allows { rename: true, delete: false, unsubscribe: false });
    }

    #[test]
    fn a_subscribed_calendar_takes_neither() {
        // Google lists a calendar subscribed by address, and a holiday
        // calendar, as `reader`.
        assert_eq!(allows(&calendar(Access::Reader, false)), Allows { rename: false, delete: false, unsubscribe: true });
    }

    #[test]
    fn a_calendar_shared_for_writing_is_still_someone_elses() {
        assert_eq!(allows(&calendar(Access::Writer, false)), Allows { rename: false, delete: false, unsubscribe: true });
        assert_eq!(allows(&calendar(Access::FreeBusy, false)), Allows { rename: false, delete: false, unsubscribe: true });
    }

    #[test]
    fn a_calendar_the_account_does_not_own_can_be_unsubscribed() {
        for access in [Access::Reader, Access::Writer, Access::FreeBusy] {
            assert!(allows(&calendar(access, false)).unsubscribe, "{access:?}");
        }
        assert!(!allows(&calendar(Access::Owner, false)).unsubscribe, "an owned calendar is deleted instead");
        assert!(!allows(&calendar(Access::Owner, true)).unsubscribe);
    }

    #[test]
    fn a_webcal_address_is_fetched_over_https() {
        assert_eq!(
            subscription_address("  webcal://example.com/team.ics "),
            Some("https://example.com/team.ics".into())
        );
        assert_eq!(subscription_address("WEBCAL://example.com/a.ics"), Some("https://example.com/a.ics".into()));
        assert_eq!(subscription_address("http://example.com/a.ics"), Some("http://example.com/a.ics".into()));
        assert_eq!(subscription_address("https://example.com/a.ics"), Some("https://example.com/a.ics".into()));
    }

    #[test]
    fn anything_but_a_web_address_with_a_host_is_refused() {
        for typed in ["", "example.com/a.ics", "ftp://example.com/a.ics", "https:///a.ics", "https://exa mple.com"] {
            assert_eq!(subscription_address(typed), None, "{typed}");
        }
    }

    #[test]
    fn a_subscription_is_named_after_its_host_until_the_provider_reads_it() {
        assert_eq!(subscription_name("https://example.com/team.ics"), "example.com");
    }

    #[test]
    fn a_local_id_reads_as_local() {
        assert!(is_local("new:abc"));
        assert!(!is_local("abc@group.calendar.google.com"));
    }

    #[test]
    fn an_edit_that_makes_a_calendar_says_so() {
        assert!(ListEdit::Add.adds());
        assert!(ListEdit::Subscribe { url: "https://a".into() }.adds());
        assert!(!ListEdit::Delete.adds());
    }
}
