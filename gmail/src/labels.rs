//! Gmail's labels, and what each stands for in words every provider
//! shares: a server mailbox with a role, a keyword, unread mail or a
//! category. Only the Google adapter, the Gmail fake and the fixtures
//! that seed the fake read this.
//!
//! Gmail spells its own labels the same in every account, so code can
//! name them directly. A person's labels have ids like `Label_12` that
//! only the account knows. Migration 26 spells out the same table in SQL.

use mailrs_domain::category;
use mailrs_domain::mailbox::{MailboxKind, Membership, Memberships, Role, keyword};
use mailrs_domain::{MailSet, MessageMeta};

pub const INBOX: &str = "INBOX";
pub const SENT: &str = "SENT";
pub const DRAFT: &str = "DRAFT";
pub const STARRED: &str = "STARRED";
pub const UNREAD: &str = "UNREAD";
pub const TRASH: &str = "TRASH";
pub const SPAM: &str = "SPAM";
pub const IMPORTANT: &str = "IMPORTANT";
/// Marks a thread muted. Gmail's own filters archive whatever arrives on a
/// thread that carries it, so the reply never reaches the inbox.
pub const MUTE: &str = "MUTE";

// The store keeps categories under Gmail's own names, so Gmail's category
// labels are the store's category ids.
pub const CATEGORY_PERSONAL: &str = category::PERSONAL;
pub const CATEGORY_UPDATES: &str = category::UPDATES;
pub const CATEGORY_PROMOTIONS: &str = category::PROMOTIONS;
pub const CATEGORY_SOCIAL: &str = category::SOCIAL;
pub const CATEGORY_FORUMS: &str = category::FORUMS;

/// Every category label Gmail puts on inbox mail, Personal first.
pub const CATEGORIES: [&str; 5] = category::IDS;

/// Whether `label` names one of Gmail's inbox categories, including any
/// Gmail adds later.
pub fn is_category(label: &str) -> bool {
    label.starts_with("CATEGORY_")
}

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

/// The mail set `label` names.
pub fn set_of(label: &str) -> MailSet {
    if let Some(role) = role_of(label) {
        return MailSet::Role(role);
    }
    match membership_of(label) {
        (Membership::Keyword(_), false) => MailSet::Unseen,
        (Membership::Keyword(k), true) => MailSet::Keyword(k),
        (Membership::Category(c), _) => MailSet::Category(c),
        (Membership::Mailbox(id), _) => MailSet::Mailbox(id),
    }
}

/// The label that stands for `set`. `None` for a role Gmail has no label
/// for (Archive, All) and a keyword it does not keep.
pub fn label_of_set(set: &MailSet) -> Option<String> {
    match set {
        MailSet::Role(role) => label_of_role(*role).map(str::to_string),
        MailSet::Mailbox(id) | MailSet::Category(id) => Some(id.clone()),
        MailSet::Unseen => Some(UNREAD.into()),
        MailSet::Keyword(k) => KEYWORDS
            .iter()
            .find(|(_, kw)| kw == k)
            .map(|(l, _)| l.to_string()),
    }
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

/// A message's memberships as Gmail labels, sorted.
pub fn label_ids(meta: &MessageMeta) -> Vec<String> {
    labels(&meta.held)
}

/// Gives a message the memberships and roles Gmail's `labels` stand for.
pub fn set_label_ids(meta: &mut MessageMeta, labels: &[String]) {
    meta.held = memberships(labels);
    meta.held.sort();
    meta.roles = labels.iter().filter_map(|l| role_of(l)).collect();
    meta.roles.sort();
    meta.roles.dedup();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owned(labels: &[&str]) -> Vec<String> {
        labels.iter().map(|l| l.to_string()).collect()
    }

    #[test]
    fn every_category_label_counts_as_a_category() {
        assert!(CATEGORIES.into_iter().all(is_category));
        assert!(!is_category(INBOX));
        assert!(!is_category("Label_12"));
    }

    #[test]
    fn a_message_keeps_its_labels_through_memberships_and_roles() {
        let mut meta = crate::tests::blank_meta();
        set_label_ids(
            &mut meta,
            &owned(&["INBOX", "UNREAD", "CATEGORY_SOCIAL", "Label_3"]),
        );
        assert_eq!(meta.roles, [Role::Inbox]);
        assert!(meta.is_unread());
        assert_eq!(
            label_ids(&meta),
            owned(&["CATEGORY_SOCIAL", "INBOX", "Label_3", "UNREAD"])
        );
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

    #[test]
    fn every_label_names_one_mail_set_and_back() {
        let sets = [
            ("INBOX", MailSet::Role(Role::Inbox)),
            ("SENT", MailSet::Role(Role::Sent)),
            ("DRAFT", MailSet::Role(Role::Drafts)),
            ("TRASH", MailSet::Role(Role::Trash)),
            ("SPAM", MailSet::Role(Role::Junk)),
            ("IMPORTANT", MailSet::Role(Role::Important)),
            ("STARRED", MailSet::flagged()),
            ("MUTE", MailSet::muted()),
            ("UNREAD", MailSet::Unseen),
            (
                "CATEGORY_SOCIAL",
                MailSet::Category("CATEGORY_SOCIAL".into()),
            ),
            ("Label_4", MailSet::Mailbox("Label_4".into())),
            ("CHAT", MailSet::Mailbox("CHAT".into())),
        ];
        for (label, set) in sets {
            assert_eq!(set_of(label), set, "{label}");
            assert_eq!(label_of_set(&set).as_deref(), Some(label), "{label}");
        }
        assert_eq!(label_of_set(&MailSet::Role(Role::Archive)), None);
        assert_eq!(
            label_of_set(&MailSet::Keyword(keyword::ANSWERED.into())),
            None
        );
    }
}
