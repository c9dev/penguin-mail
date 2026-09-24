//! IMAP's flags and the app's keywords. The four system flags carry the
//! keywords the app spells `$seen`, `$flagged`, `$answered` and `$draft`;
//! any other keyword goes as it is. IMAP compares keywords without case.

use mailrs_domain::mailbox::keyword;

use super::SYSTEM_KEYWORDS;

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

#[cfg(test)]
mod tests {
    use super::{keywords_of, stored_keywords};

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
}
