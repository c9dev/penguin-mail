//! Whether a message matches a rule's criteria, the way Gmail's filters
//! read them: every criterion given must hold, text matches ignore case,
//! "to" covers To and Cc, and the words are looked for in the subject
//! and the body, as plain words with any search syntax taken out.

use mailrs_domain::{FilterCriteria, MessageMeta};

fn has(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(&needle.trim().to_lowercase())
}

/// The words a query term stands for once the search syntax is gone, or
/// `None` when nothing is left, which would otherwise match every message.
fn words(term: &str) -> Option<String> {
    Some(mailrs_domain::query::plain(term)).filter(|w| !w.is_empty())
}

/// Whether the criteria read the message's text.
pub fn needs_body(criteria: &FilterCriteria) -> bool {
    criteria.query.is_some() || criteria.negated_query.is_some()
}

/// Whether `meta` satisfies every criterion given. Without a body, the
/// words are looked for in the subject and the snippet. A chat is a Gmail
/// idea no IMAP message has, so `exclude_chats` never keeps a message out.
pub fn matches(criteria: &FilterCriteria, meta: &MessageMeta, body: Option<&str>) -> bool {
    let sender = meta
        .from
        .as_ref()
        .map(|a| format!("{} {}", a.name.as_deref().unwrap_or_default(), a.email))
        .unwrap_or_default();
    let recipients: String = meta
        .to
        .iter()
        .chain(&meta.cc)
        .map(|a| format!("{} {} ", a.name.as_deref().unwrap_or_default(), a.email))
        .collect();
    let text = format!("{}\n{}", meta.subject, body.unwrap_or(&meta.snippet));
    // Each given criterion answers whether it holds; a rule with none
    // given matches nothing, so a blank rule cannot sweep the Inbox.
    let mut given = Vec::new();
    if let Some(f) = criteria.from.as_deref().filter(|f| !f.trim().is_empty()) {
        given.push(has(&sender, f));
    }
    if let Some(t) = criteria.to.as_deref().filter(|t| !t.trim().is_empty()) {
        given.push(has(&recipients, t));
    }
    if let Some(s) = criteria.subject.as_deref().filter(|s| !s.trim().is_empty()) {
        given.push(has(&meta.subject, s));
    }
    if let Some(q) = criteria.query.as_deref().and_then(words) {
        given.push(has(&text, &q));
    }
    if let Some(q) = criteria.negated_query.as_deref().and_then(words) {
        given.push(!has(&text, &q));
    }
    if criteria.has_attachment {
        given.push(meta.has_attachments);
    }
    if let Some(size) = criteria.size {
        let size = i64::try_from(size).unwrap_or(i64::MAX);
        given.push(match criteria.size_comparison.as_deref() {
            Some("larger") => meta.size > size,
            Some("smaller") => meta.size < size,
            _ => false,
        });
    }
    !given.is_empty() && given.into_iter().all(|held| held)
}

#[cfg(test)]
mod tests {
    use mailrs_domain::{Address, FilterCriteria, MessageMeta};

    use super::*;

    fn from_ann() -> MessageMeta {
        MessageMeta {
            from: Some(Address { name: Some("Ann Lee".into()), email: "Ann@Example.com".into() }),
            to: vec![Address { name: None, email: "me@example.com".into() }],
            subject: "Invoice for May".into(),
            has_attachments: true,
            size: 5_000,
            ..crate::tests::meta_for_rules()
        }
    }

    #[test]
    fn every_criterion_must_hold_and_case_does_not_matter() {
        let c = FilterCriteria {
            from: Some("ann@example".into()),
            subject: Some("INVOICE".into()),
            ..FilterCriteria::default()
        };
        assert!(matches(&c, &from_ann(), None));
        let c = FilterCriteria {
            from: Some("bob@".into()),
            subject: Some("invoice".into()),
            ..FilterCriteria::default()
        };
        assert!(!matches(&c, &from_ann(), None));
    }

    #[test]
    fn words_are_looked_for_in_the_body_and_their_absence_too() {
        let has = FilterCriteria { query: Some("receipt".into()), ..FilterCriteria::default() };
        assert!(needs_body(&has));
        assert!(matches(&has, &from_ann(), Some("Your receipt is attached.")));
        assert!(!matches(&has, &from_ann(), Some("Hello.")));
        let hasnt = FilterCriteria { negated_query: Some("receipt".into()), ..FilterCriteria::default() };
        assert!(!matches(&hasnt, &from_ann(), Some("Your RECEIPT.")));
    }

    #[test]
    fn to_matches_the_to_and_cc_addresses() {
        let c = FilterCriteria { to: Some("me@example.com".into()), ..FilterCriteria::default() };
        assert!(matches(&c, &from_ann(), None));
    }

    #[test]
    fn a_rule_with_nothing_to_match_matches_nothing() {
        assert!(!matches(&FilterCriteria::default(), &from_ann(), None));
        let only_chats = FilterCriteria { exclude_chats: true, ..FilterCriteria::default() };
        assert!(!matches(&only_chats, &from_ann(), None));
        let empty_words = FilterCriteria { query: Some("()".into()), ..FilterCriteria::default() };
        assert!(!matches(&empty_words, &from_ann(), None), "words of nothing are no criterion");
    }

    #[test]
    fn size_compares_the_message_in_the_direction_the_rule_names() {
        let size = |n, how: &str| FilterCriteria {
            size: Some(n),
            size_comparison: Some(how.into()),
            ..FilterCriteria::default()
        };
        assert!(matches(&size(1_000, "larger"), &from_ann(), None));
        assert!(!matches(&size(10_000, "larger"), &from_ann(), None));
        assert!(matches(&size(10_000, "smaller"), &from_ann(), None));
        assert!(!matches(&size(1_000, "smaller"), &from_ann(), None));
        assert!(!matches(&size(1_000, "sideways"), &from_ann(), None), "an unknown direction holds nothing");
    }
}
