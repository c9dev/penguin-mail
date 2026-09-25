//! IMAP's flags and the app's keywords. The four system flags carry the
//! keywords the app spells `$seen`, `$flagged`, `$answered` and `$draft`;
//! any other keyword goes as it is. IMAP compares keywords without case.

use mailrs_domain::Membership;
use mailrs_domain::mailbox::keyword;

use super::SYSTEM_KEYWORDS;
use crate::services::RemoteChange;

/// The keywords a mailbox stores when its PERMANENTFLAGS take `\*` or
/// name `$muted`: the system flags and the app's own.
pub(super) const EVERY_KEYWORD: &[&str] = &[
    keyword::SEEN,
    keyword::FLAGGED,
    keyword::ANSWERED,
    keyword::DRAFT,
    keyword::MUTED,
];

/// A message's flags as keywords: the system flags in the app's spelling,
/// and any other keyword in lower case. `\Recent` and `\Deleted` say
/// nothing a person reads, and `\Deleted` mail is left out before this.
pub(super) fn keywords_of(flags: &[String]) -> Vec<String> {
    flags
        .iter()
        .filter_map(|flag| {
            let lower = flag.to_ascii_lowercase();
            match lower.as_str() {
                "\\seen" => Some(keyword::SEEN.to_string()),
                "\\flagged" => Some(keyword::FLAGGED.to_string()),
                "\\answered" => Some(keyword::ANSWERED.to_string()),
                "\\draft" => Some(keyword::DRAFT.to_string()),
                _ if lower.starts_with('\\') => None,
                _ => Some(lower),
            }
        })
        .collect()
}

/// Whether the flags mark a message deleted: another client, or a move on
/// a server without MOVE, left it for an expunge.
pub(super) fn is_deleted(flags: &[String]) -> bool {
    flags.iter().any(|f| f.eq_ignore_ascii_case("\\Deleted"))
}

/// The flag the server stores for `keyword`: IMAP's own for the four it
/// has, the keyword itself otherwise.
pub(super) fn flag_of(keyword: &str) -> String {
    match keyword {
        self::keyword::SEEN => "\\Seen".to_string(),
        self::keyword::FLAGGED => "\\Flagged".to_string(),
        self::keyword::ANSWERED => "\\Answered".to_string(),
        self::keyword::DRAFT => "\\Draft".to_string(),
        other => other.to_string(),
    }
}

/// Which keywords a mailbox with `permanent` as its PERMANENTFLAGS stores.
/// A server that sends none is taken to store the system flags alone, so
/// `$muted` stays on this computer rather than vanish.
pub(super) fn stored_keywords(permanent: &[String]) -> &'static [&'static str] {
    let takes_muted = permanent
        .iter()
        .any(|f| f == "\\*" || f.eq_ignore_ascii_case(keyword::MUTED));
    match takes_muted {
        true => EVERY_KEYWORD,
        false => SYSTEM_KEYWORDS,
    }
}

/// The changes that bring message `id` to `flags`: a gain of every
/// keyword it carries, and a loss of every keyword the server stores that
/// it lacks. A keyword the server cannot store stays as the app left it
/// on this computer.
pub(super) fn flag_changes(id: &str, flags: &[String], stored: &[&str]) -> Vec<RemoteChange> {
    let carried = keywords_of(flags);
    let lost: Vec<Membership> = stored
        .iter()
        .copied()
        .filter(|k| !carried.iter().any(|c| c.as_str() == *k))
        .map(|k| Membership::Keyword(k.to_string()))
        .collect();
    let gained: Vec<Membership> = carried.into_iter().map(Membership::Keyword).collect();
    let mut changes = Vec::new();
    if !gained.is_empty() {
        changes.push(RemoteChange::Gained {
            id: id.to_string(),
            thread_id: id.to_string(),
            memberships: gained,
        });
    }
    if !lost.is_empty() {
        changes.push(RemoteChange::Lost {
            id: id.to_string(),
            memberships: lost,
        });
    }
    changes
}

#[cfg(test)]
mod tests {
    use mailrs_domain::Membership;

    use super::{flag_changes, flag_of, keywords_of, stored_keywords};
    use crate::services::RemoteChange;

    fn owned(flags: &[&str]) -> Vec<String> {
        flags.iter().map(|f| f.to_string()).collect()
    }

    #[test]
    fn system_flags_take_the_apps_spelling_and_other_keywords_keep_theirs() {
        assert_eq!(
            keywords_of(&owned(&[
                "\\Seen",
                "\\FLAGGED",
                "\\Answered",
                "\\Draft",
                "$Forwarded",
                "\\Recent",
                "\\Deleted"
            ])),
            ["$seen", "$flagged", "$answered", "$draft", "$forwarded"]
        );
    }

    #[test]
    fn a_mailbox_stores_muted_only_when_its_permanent_flags_take_it() {
        let system = owned(&["\\Seen", "\\Flagged", "\\Answered", "\\Draft", "\\Deleted"]);
        assert!(!stored_keywords(&system).contains(&"$muted"));
        let open = owned(&["\\Seen", "\\*"]);
        assert!(stored_keywords(&open).contains(&"$muted"));
        let named = owned(&["\\Seen", "$Muted"]);
        assert!(stored_keywords(&named).contains(&"$muted"));
    }

    #[test]
    fn keywords_go_to_the_server_as_its_flags() {
        assert_eq!(flag_of("$seen"), "\\Seen");
        assert_eq!(flag_of("$draft"), "\\Draft");
        assert_eq!(flag_of("$muted"), "$muted");
    }

    #[test]
    fn flags_become_gains_and_losses_of_what_the_server_stores() {
        let stored = ["$seen", "$flagged", "$answered", "$draft"];
        assert_eq!(
            flag_changes("INBOX/1/4", &owned(&["\\Seen"]), &stored),
            [
                RemoteChange::Gained {
                    id: "INBOX/1/4".into(),
                    thread_id: "INBOX/1/4".into(),
                    memberships: vec![Membership::Keyword("$seen".into())],
                },
                RemoteChange::Lost {
                    id: "INBOX/1/4".into(),
                    memberships: vec![
                        Membership::Keyword("$flagged".into()),
                        Membership::Keyword("$answered".into()),
                        Membership::Keyword("$draft".into()),
                    ],
                },
            ]
        );
    }
}
