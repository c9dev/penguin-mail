//! Gmail's labels as server mailboxes, keywords and categories. Gmail
//! files everything as a label, so this one table says which labels are
//! mailboxes and with which role, which stand for a keyword, and which
//! are categories. The Google adapter reads it both ways. So does the
//! store, for as long as its interface still names mail by label id, and
//! so does migration 26, whose SQL spells out the same table.

use crate::mailbox::{MailboxKind, Membership, Memberships, Role, keyword};
use crate::system_label::{
    DRAFT, IMPORTANT, INBOX, MUTE, SENT, SPAM, STARRED, TRASH, UNREAD, is_category,
};

/// The labels that are mailboxes with a role.
pub const ROLES: [(&str, Role); 6] = [
    (INBOX, Role::Inbox),
    (SENT, Role::Sent),
    (DRAFT, Role::Drafts),
    (TRASH, Role::Trash),
    (SPAM, Role::Junk),
    (IMPORTANT, Role::Important),
];

/// The labels that stand for a keyword the message carries. `UNREAD` is
/// not among them: it stands for the absence of `$seen`.
pub const KEYWORDS: [(&str, &str); 2] = [(STARRED, keyword::FLAGGED), (MUTE, keyword::MUTED)];

/// The role of the mailbox `label` names, if it has one.
pub fn role_of(label: &str) -> Option<Role> {
    ROLES.iter().find(|(l, _)| *l == label).map(|(_, r)| *r)
}

/// The label of the mailbox with `role`. Gmail has none for Archive or
/// All Mail.
pub fn label_of_role(role: Role) -> Option<&'static str> {
    ROLES.iter().find(|(_, r)| *r == role).map(|(l, _)| *l)
}

/// Who made the mailbox `label` names. Gmail numbers a person's labels
/// `Label_1`, `Label_2` and so on; its own have fixed names.
pub fn kind_of(label: &str) -> MailboxKind {
    match label.starts_with("Label_") {
        true => MailboxKind::Label,
        false => MailboxKind::System,
    }
}

/// What `label` stands for, and whether carrying the label means holding
/// that membership (`true`) or lacking it (`false`, which only `UNREAD`
/// answers).
pub fn membership_of(label: &str) -> (Membership, bool) {
    if label == UNREAD {
        return (Membership::Keyword(keyword::SEEN.into()), false);
    }
    if let Some((_, k)) = KEYWORDS.iter().find(|(l, _)| *l == label) {
        return (Membership::Keyword((*k).into()), true);
    }
    if is_category(label) {
        return (Membership::Category(label.into()), true);
    }
    (Membership::Mailbox(label.into()), true)
}

/// The label that says `membership` is held (`on`) or lacking, and
/// whether the message then carries that label (`true`) or lacks it.
/// `None` for a keyword Gmail has no label for.
pub fn label_of(membership: &Membership, on: bool) -> Option<(String, bool)> {
    match membership {
        Membership::Keyword(k) if k == keyword::SEEN => Some((UNREAD.into(), !on)),
        Membership::Keyword(k) => KEYWORDS
            .iter()
            .find(|(_, kw)| kw == k)
            .map(|(l, _)| (l.to_string(), on)),
        Membership::Mailbox(id) | Membership::Category(id) => Some((id.clone(), on)),
    }
}

/// A message's labels as memberships. A message without `UNREAD` is seen.
pub fn memberships(labels: &[String]) -> Memberships {
    let mut held = Memberships::default();
    let mut seen = true;
    for label in labels {
        // Only UNREAD is carried to say a membership is lacking.
        let (list, value) = match membership_of(label) {
            (_, false) => {
                seen = false;
                continue;
            }
            (Membership::Mailbox(id), true) => (&mut held.mailboxes, id),
            (Membership::Keyword(k), true) => (&mut held.keywords, k),
            (Membership::Category(c), true) => (&mut held.categories, c),
        };
        if !list.contains(&value) {
            list.push(value);
        }
    }
    if seen {
        held.keywords.push(keyword::SEEN.into());
    }
    held
}

/// The labels Gmail shows for `held`, sorted as the store lists them.
/// Keywords Gmail has no label for leave no trace.
pub fn labels(held: &Memberships) -> Vec<String> {
    let mut labels: Vec<String> = held
        .mailboxes
        .iter()
        .chain(&held.categories)
        .cloned()
        .collect();
    for k in &held.keywords {
        if let Some((label, true)) = label_of(&Membership::Keyword(k.clone()), true) {
            labels.push(label);
        }
    }
    if !held.keywords.iter().any(|k| k == keyword::SEEN) {
        labels.push(UNREAD.into());
    }
    labels.sort();
    labels.dedup();
    labels
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mailbox::{MailboxKind, Membership, Role, keyword};

    fn owned(labels: &[&str]) -> Vec<String> {
        labels.iter().map(|l| l.to_string()).collect()
    }

    #[test]
    fn every_role_label_maps_to_its_role_and_back() {
        for (label, role) in ROLES {
            assert_eq!(role_of(label), Some(role));
            assert_eq!(label_of_role(role), Some(label));
        }
        assert_eq!(role_of("Label_1"), None);
        assert_eq!(label_of_role(Role::Archive), None);
        assert_eq!(label_of_role(Role::All), None);
    }

    #[test]
    fn keyword_and_category_labels_are_not_mailboxes() {
        assert_eq!(
            membership_of("STARRED"),
            (Membership::Keyword(keyword::FLAGGED.into()), true)
        );
        assert_eq!(
            membership_of("MUTE"),
            (Membership::Keyword(keyword::MUTED.into()), true)
        );
        assert_eq!(
            membership_of("UNREAD"),
            (Membership::Keyword(keyword::SEEN.into()), false)
        );
        assert_eq!(
            membership_of("CATEGORY_SOCIAL"),
            (Membership::Category("CATEGORY_SOCIAL".into()), true)
        );
        assert_eq!(
            membership_of("INBOX"),
            (Membership::Mailbox("INBOX".into()), true)
        );
        assert_eq!(
            membership_of("Label_7"),
            (Membership::Mailbox("Label_7".into()), true)
        );
    }

    #[test]
    fn unread_is_the_absence_of_seen() {
        let read = memberships(&owned(&["INBOX"]));
        assert!(read.keywords.contains(&keyword::SEEN.to_string()));
        let unread = memberships(&owned(&["INBOX", "UNREAD"]));
        assert!(!unread.keywords.contains(&keyword::SEEN.to_string()));
        assert_eq!(
            label_of(&Membership::Keyword(keyword::SEEN.into()), true),
            Some(("UNREAD".into(), false))
        );
        assert_eq!(
            label_of(&Membership::Keyword(keyword::SEEN.into()), false),
            Some(("UNREAD".into(), true))
        );
    }

    #[test]
    fn a_label_set_survives_the_trip_through_memberships() {
        let sets: [&[&str]; 5] = [
            &[],
            &["INBOX", "UNREAD"],
            &[
                "CATEGORY_SOCIAL",
                "CHAT",
                "IMPORTANT",
                "Label_3",
                "MUTE",
                "STARRED",
            ],
            &["DRAFT", "SENT", "SPAM", "TRASH", "UNREAD"],
            &["CATEGORY_PERSONAL", "CATEGORY_UPDATES", "INBOX", "INBOX"],
        ];
        for set in sets {
            let mut expected = owned(set);
            expected.sort();
            expected.dedup();
            assert_eq!(labels(&memberships(&owned(set))), expected, "{set:?}");
        }
    }

    #[test]
    fn a_keyword_gmail_has_no_label_for_has_none() {
        assert_eq!(
            label_of(&Membership::Keyword(keyword::ANSWERED.into()), true),
            None
        );
        let with_answered = Memberships {
            mailboxes: vec!["INBOX".into()],
            keywords: vec![keyword::SEEN.into(), keyword::ANSWERED.into()],
            categories: vec![],
        };
        assert_eq!(labels(&with_answered), ["INBOX"]);
    }

    #[test]
    fn a_persons_label_is_the_only_kind_they_made() {
        assert_eq!(kind_of("Label_12"), MailboxKind::Label);
        assert_eq!(kind_of("INBOX"), MailboxKind::System);
        assert_eq!(kind_of("CHAT"), MailboxKind::System);
    }
}
