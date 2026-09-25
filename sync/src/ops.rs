//! Mail operations: what an action does to messages, in words every server
//! shares. `ops_for` is the engine's one place that turns a triage action
//! into operations. The engine applies them to the store, which reports
//! what each message gained and lost, and hands them to the account's
//! backend; undo builds the operations that reverse that report.

use std::collections::BTreeMap;

use mailrs_domain::mailbox::keyword::{FLAGGED, MUTED, SEEN};
use mailrs_domain::{Applied, MailSet, Membership, Memberships, Role};
use mailrs_store::messages::Change;

use crate::{BackendError, MailCapabilities, TriageAction};

/// One change to the messages an action names.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MailOp {
    /// Files the messages in a server mailbox, by the server's id.
    AddToMailbox(String),
    RemoveFromMailbox(String),
    /// Takes the messages out of every mailbox and into the one with this
    /// role, as a folder server files mail. A label server never gets it.
    MoveToRole(Role),
    /// Takes the messages out of every mailbox and into this one, by the
    /// server's id. A label server never gets it.
    MoveToMailbox(String),
    SetKeyword {
        keyword: String,
        on: bool,
    },
    SetCategory {
        category: String,
        on: bool,
    },
    /// Erases the messages for good. It comes alone.
    Destroy,
}

/// The server's id for each role's mailbox, as the backend reports it.
pub type Roles = BTreeMap<Role, String>;

/// The operations `action` stands for on an account whose mail service can
/// do `caps`. A label account files and unfiles; a folder account moves.
/// `set_of` reads a mailbox id the action names as the mail set it stands
/// for, as the account's mail service reads it. `Unsupported` when a label
/// account lacks the mailbox the action files mail in.
pub fn ops_for(
    action: &TriageAction,
    caps: &MailCapabilities,
    roles: &Roles,
    set_of: impl Fn(&str) -> MailSet,
) -> Result<Vec<MailOp>, BackendError> {
    let id = |role: Role| roles.get(&role).cloned().ok_or(BackendError::Unsupported);
    let keyword = |k: &str, on: bool| MailOp::SetKeyword {
        keyword: k.into(),
        on,
    };
    let add = |role| id(role).map(MailOp::AddToMailbox);
    let remove = |role| id(role).map(MailOp::RemoveFromMailbox);
    let moves = !caps.labels;
    Ok(match action {
        TriageAction::MarkRead => vec![keyword(SEEN, true)],
        TriageAction::MarkUnread => vec![keyword(SEEN, false)],
        TriageAction::Star => vec![keyword(FLAGGED, true)],
        TriageAction::Unstar => vec![keyword(FLAGGED, false)],
        TriageAction::Archive if moves => vec![MailOp::MoveToRole(Role::Archive)],
        TriageAction::Archive => vec![remove(Role::Inbox)?],
        TriageAction::Trash if moves => vec![MailOp::MoveToRole(Role::Trash)],
        TriageAction::Trash => vec![add(Role::Trash)?, remove(Role::Inbox)?],
        TriageAction::Untrash if moves => vec![MailOp::MoveToRole(Role::Inbox)],
        TriageAction::Untrash => vec![add(Role::Inbox)?, remove(Role::Trash)?],
        TriageAction::Junk if moves => vec![MailOp::MoveToRole(Role::Junk)],
        TriageAction::Junk => vec![add(Role::Junk)?, remove(Role::Inbox)?],
        TriageAction::NotJunk if moves => vec![MailOp::MoveToRole(Role::Inbox)],
        TriageAction::NotJunk => vec![add(Role::Inbox)?, remove(Role::Junk)?],
        TriageAction::Mute if moves => {
            vec![keyword(MUTED, true), MailOp::MoveToRole(Role::Archive)]
        }
        TriageAction::Mute => vec![keyword(MUTED, true), remove(Role::Inbox)?],
        TriageAction::Unmute if moves => {
            vec![MailOp::MoveToRole(Role::Inbox), keyword(MUTED, false)]
        }
        TriageAction::Unmute => vec![add(Role::Inbox)?, keyword(MUTED, false)],
        TriageAction::MoveTo(id) if moves => vec![MailOp::MoveToMailbox(id.clone())],
        // On a label account a move files the mail and takes it out of
        // the inbox, as Gmail's own Move to does.
        TriageAction::MoveTo(id) => vec![MailOp::AddToMailbox(id.clone()), remove(Role::Inbox)?],
        TriageAction::AddLabel(id) => vec![set_op(&set_of(id), true, roles)?],
        TriageAction::RemoveLabel(id) => vec![set_op(&set_of(id), false, roles)?],
        TriageAction::Relabel { add, remove } => add
            .iter()
            .map(|set| set_op(set, true, roles))
            .chain(remove.iter().map(|set| set_op(set, false, roles)))
            .collect::<Result<_, _>>()?,
    })
}

/// The operation that puts messages in `set` (`on`) or takes them out.
/// Being in `MailSet::Unseen` is lacking `$seen`, so adding it clears the
/// keyword.
fn set_op(set: &MailSet, on: bool, roles: &Roles) -> Result<MailOp, BackendError> {
    Ok(match set {
        MailSet::Role(role) => {
            let id = roles.get(role).cloned().ok_or(BackendError::Unsupported)?;
            op(Membership::Mailbox(id), on)
        }
        MailSet::Mailbox(id) => op(Membership::Mailbox(id.clone()), on),
        MailSet::Keyword(k) => MailOp::SetKeyword { keyword: k.clone(), on },
        MailSet::Unseen => MailOp::SetKeyword { keyword: SEEN.into(), on: !on },
        MailSet::Category(c) => MailOp::SetCategory { category: c.clone(), on },
    })
}

/// The operation that gives `membership` or takes it away.
fn op(membership: Membership, on: bool) -> MailOp {
    match membership {
        Membership::Mailbox(id) if on => MailOp::AddToMailbox(id),
        Membership::Mailbox(id) => MailOp::RemoveFromMailbox(id),
        Membership::Keyword(keyword) => MailOp::SetKeyword { keyword, on },
        Membership::Category(category) => MailOp::SetCategory { category, on },
    }
}

/// The store changes `ops` make to message `id`, which now holds `held`.
pub fn local_changes(id: &str, held: &Memberships, ops: &[MailOp], roles: &Roles) -> Vec<Change> {
    let mut changes = Vec::new();
    for op in ops {
        match op {
            MailOp::AddToMailbox(mailbox) => {
                changes.push(Change::of(id, Membership::Mailbox(mailbox.clone()), true));
            }
            MailOp::RemoveFromMailbox(mailbox) => {
                changes.push(Change::of(id, Membership::Mailbox(mailbox.clone()), false));
            }
            MailOp::SetKeyword { keyword, on } => {
                changes.push(Change::of(id, Membership::Keyword(keyword.clone()), *on));
            }
            MailOp::SetCategory { category, on } => {
                changes.push(Change::of(id, Membership::Category(category.clone()), *on));
            }
            MailOp::Destroy => changes.push(Change::Delete {
                message_id: id.into(),
            }),
            MailOp::MoveToRole(role) => {
                let Some(target) = roles.get(role) else {
                    continue;
                };
                move_to(&mut changes, id, held, target);
            }
            MailOp::MoveToMailbox(target) => move_to(&mut changes, id, held, target),
        }
    }
    changes
}

/// The store changes that take message `id` out of every mailbox it
/// holds and into `target`.
fn move_to(changes: &mut Vec<Change>, id: &str, held: &Memberships, target: &str) {
    for mailbox in held.mailboxes.iter().filter(|m| *m != target) {
        changes.push(Change::of(id, Membership::Mailbox(mailbox.clone()), false));
    }
    changes.push(Change::of(id, Membership::Mailbox(target.into()), true));
}

/// `ops` without the keyword changes the server cannot store, and those
/// keywords. A server stores the keywords its capabilities list; any
/// other stays on this computer, marked local, and never syncs.
pub fn split_keywords(ops: &[MailOp], stored: &[&str]) -> (Vec<MailOp>, Vec<String>) {
    let mut to_server = Vec::new();
    let mut kept_here = Vec::new();
    for op in ops {
        match op {
            MailOp::SetKeyword { keyword, .. } if !stored.contains(&keyword.as_str()) => {
                kept_here.push(keyword.clone());
            }
            other => to_server.push(other.clone()),
        }
    }
    (to_server, kept_here)
}

/// The operations that reverse `applied`: what the message lost goes
/// back, and what it gained comes off.
pub fn undo_ops(applied: &Applied) -> Vec<MailOp> {
    applied
        .lost
        .iter()
        .map(|m| op(m.clone(), true))
        .chain(applied.gained.iter().map(|m| op(m.clone(), false)))
        .collect()
}

/// The store changes that reverse `applied`, for a write the server
/// refused.
pub fn reverse_changes(applied: &Applied) -> Vec<Change> {
    let id = applied.message_id.as_str();
    applied
        .lost
        .iter()
        .map(|m| Change::of(id, m.clone(), true))
        .chain(
            applied
                .gained
                .iter()
                .map(|m| Change::of(id, m.clone(), false)),
        )
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use mailrs_domain::mailbox::keyword::{FLAGGED, MUTED, SEEN};
    use mailrs_domain::{Applied, MailSet, Membership, Memberships, Role};
    use mailrs_gmail::labels as gmail;
    use mailrs_store::messages::Change;

    use super::*;
    use crate::fake::FakeGmail;
    use crate::{Google, MailBackend, MailCapabilities, TriageAction};

    fn gmail_roles() -> Roles {
        gmail::ROLES
            .iter()
            .map(|(label, role)| (*role, label.to_string()))
            .collect()
    }

    /// Reads a mailbox id as the Google adapter reads it.
    fn named(id: &str) -> MailSet {
        Google::new(Arc::new(FakeGmail::new())).set_of(id)
    }

    fn label_account() -> MailCapabilities {
        Google::new(Arc::new(FakeGmail::new())).capabilities()
    }

    fn folder_account() -> MailCapabilities {
        MailCapabilities {
            labels: false,
            ..label_account()
        }
    }

    fn keyword(k: &str, on: bool) -> MailOp {
        MailOp::SetKeyword {
            keyword: k.into(),
            on,
        }
    }

    #[test]
    fn a_label_account_adds_and_removes_mailboxes() {
        let roles = gmail_roles();
        let ops = |action: TriageAction| {
            ops_for(&action, &label_account(), &roles, named).unwrap()
        };
        assert_eq!(
            ops(TriageAction::Archive),
            [MailOp::RemoveFromMailbox("INBOX".into())]
        );
        assert_eq!(
            ops(TriageAction::Trash),
            [
                MailOp::AddToMailbox("TRASH".into()),
                MailOp::RemoveFromMailbox("INBOX".into())
            ]
        );
        assert_eq!(
            ops(TriageAction::NotJunk),
            [
                MailOp::AddToMailbox("INBOX".into()),
                MailOp::RemoveFromMailbox("SPAM".into())
            ]
        );
        assert_eq!(ops(TriageAction::MarkRead), [keyword(SEEN, true)]);
        assert_eq!(ops(TriageAction::MarkUnread), [keyword(SEEN, false)]);
        assert_eq!(
            ops(TriageAction::Mute),
            [
                keyword(MUTED, true),
                MailOp::RemoveFromMailbox("INBOX".into())
            ]
        );
        assert_eq!(
            ops(TriageAction::AddLabel("Label_1".into())),
            [MailOp::AddToMailbox("Label_1".into())]
        );
        assert_eq!(
            ops(TriageAction::Relabel {
                add: vec![
                    MailSet::Category("CATEGORY_SOCIAL".into()),
                    MailSet::Unseen
                ],
                remove: vec![MailSet::Category("CATEGORY_UPDATES".into())],
            }),
            [
                MailOp::SetCategory {
                    category: "CATEGORY_SOCIAL".into(),
                    on: true
                },
                keyword(SEEN, false),
                MailOp::SetCategory {
                    category: "CATEGORY_UPDATES".into(),
                    on: false
                },
            ]
        );
    }

    #[test]
    fn a_reminder_coming_due_files_in_the_inbox_and_marks_unread() {
        let roles = gmail_roles();
        let back = TriageAction::Relabel {
            add: vec![MailSet::Role(Role::Inbox), MailSet::Unseen],
            remove: vec![],
        };
        assert_eq!(
            ops_for(&back, &label_account(), &roles, named).unwrap(),
            vec![
                MailOp::AddToMailbox("INBOX".into()),
                MailOp::SetKeyword { keyword: SEEN.into(), on: false },
            ]
        );
    }

    #[test]
    fn a_relabel_by_category_and_keyword_needs_no_label_ids() {
        let roles = gmail_roles();
        let sort = TriageAction::Relabel {
            add: vec![MailSet::Category("CATEGORY_SOCIAL".into()), MailSet::flagged()],
            remove: vec![MailSet::Category("CATEGORY_UPDATES".into())],
        };
        assert_eq!(
            ops_for(&sort, &label_account(), &roles, named).unwrap(),
            vec![
                MailOp::SetCategory { category: "CATEGORY_SOCIAL".into(), on: true },
                MailOp::SetKeyword { keyword: FLAGGED.into(), on: true },
                MailOp::SetCategory { category: "CATEGORY_UPDATES".into(), on: false },
            ]
        );
    }

    #[test]
    fn a_role_the_account_lacks_is_unsupported() {
        let relabel = TriageAction::Relabel {
            add: vec![MailSet::Role(Role::Archive)],
            remove: vec![],
        };
        assert!(matches!(
            ops_for(&relabel, &label_account(), &gmail_roles(), named),
            Err(BackendError::Unsupported)
        ));
    }

    #[test]
    fn a_label_or_unlabel_of_a_gmail_keyword_label_becomes_a_keyword_op() {
        let roles = gmail_roles();
        assert_eq!(
            ops_for(
                &TriageAction::RemoveLabel("UNREAD".into()),
                &label_account(),
                &roles,
                named
            )
            .unwrap(),
            [keyword(SEEN, true)]
        );
        assert_eq!(
            ops_for(
                &TriageAction::AddLabel("STARRED".into()),
                &label_account(),
                &roles,
                named
            )
            .unwrap(),
            [MailOp::SetKeyword { keyword: FLAGGED.into(), on: true }]
        );
    }

    #[test]
    fn a_folder_account_moves_to_a_role() {
        let roles = gmail_roles();
        let ops = |action: TriageAction| {
            ops_for(&action, &folder_account(), &roles, named).unwrap()
        };
        assert_eq!(
            ops(TriageAction::Archive),
            [MailOp::MoveToRole(Role::Archive)]
        );
        assert_eq!(ops(TriageAction::Trash), [MailOp::MoveToRole(Role::Trash)]);
        assert_eq!(
            ops(TriageAction::Untrash),
            [MailOp::MoveToRole(Role::Inbox)]
        );
        assert_eq!(ops(TriageAction::Junk), [MailOp::MoveToRole(Role::Junk)]);
        assert_eq!(
            ops(TriageAction::Mute),
            [keyword(MUTED, true), MailOp::MoveToRole(Role::Archive)]
        );
        assert_eq!(ops(TriageAction::MarkRead), [keyword(SEEN, true)]);
    }

    #[test]
    fn a_label_account_without_the_mailbox_cannot_file_there() {
        assert!(matches!(
            ops_for(&TriageAction::Trash, &label_account(), &Roles::new(), named),
            Err(crate::BackendError::Unsupported)
        ));
    }

    #[test]
    fn a_move_leaves_every_other_mailbox_and_lands_in_the_role() {
        let held = Memberships {
            mailboxes: vec!["INBOX".into(), "Work".into()],
            ..Memberships::default()
        };
        let roles = Roles::from([(Role::Archive, "Archive".to_string())]);
        assert_eq!(
            local_changes("m1", &held, &[MailOp::MoveToRole(Role::Archive)], &roles),
            [
                Change::RemoveFromMailbox {
                    message_id: "m1".into(),
                    mailbox: "INBOX".into()
                },
                Change::RemoveFromMailbox {
                    message_id: "m1".into(),
                    mailbox: "Work".into()
                },
                Change::AddToMailbox {
                    message_id: "m1".into(),
                    mailbox: "Archive".into()
                },
            ]
        );
    }

    #[test]
    fn moving_to_a_folder_moves_on_a_folder_account_and_files_on_a_label_account() {
        let roles = gmail_roles();
        let action = TriageAction::MoveTo("Label_5".into());
        assert_eq!(
            ops_for(&action, &folder_account(), &roles, named).unwrap(),
            vec![MailOp::MoveToMailbox("Label_5".into())]
        );
        assert_eq!(
            ops_for(&action, &label_account(), &roles, named).unwrap(),
            vec![
                MailOp::AddToMailbox("Label_5".into()),
                MailOp::RemoveFromMailbox("INBOX".into()),
            ]
        );
    }

    #[test]
    fn a_move_takes_the_message_out_of_every_other_mailbox_in_the_store() {
        let held = Memberships {
            mailboxes: vec!["INBOX".into(), "Label_1".into()],
            ..Memberships::default()
        };
        let changes = local_changes(
            "m1",
            &held,
            &[MailOp::MoveToMailbox("Label_5".into())],
            &gmail_roles(),
        );
        assert_eq!(
            changes,
            vec![
                Change::of("m1", Membership::Mailbox("INBOX".into()), false),
                Change::of("m1", Membership::Mailbox("Label_1".into()), false),
                Change::of("m1", Membership::Mailbox("Label_5".into()), true),
            ]
        );
    }

    #[test]
    fn a_keyword_the_server_cannot_store_stays_here() {
        let mute = [keyword(MUTED, true), MailOp::MoveToRole(Role::Archive)];
        assert_eq!(
            split_keywords(&mute, &["$seen", "$flagged"]),
            (vec![MailOp::MoveToRole(Role::Archive)], vec![MUTED.to_string()])
        );
        assert_eq!(
            split_keywords(&mute, &["$seen", "$flagged", "$muted"]),
            (mute.to_vec(), vec![])
        );
    }

    #[test]
    fn undo_puts_back_what_went_and_takes_off_what_came() {
        let trashed = Applied {
            thread_id: "t1".into(),
            message_id: "m1".into(),
            gained: vec![Membership::Mailbox("TRASH".into())],
            lost: vec![Membership::Mailbox("INBOX".into())],
        };
        assert_eq!(
            undo_ops(&trashed),
            [
                MailOp::AddToMailbox("INBOX".into()),
                MailOp::RemoveFromMailbox("TRASH".into())
            ]
        );
        let read = Applied {
            gained: vec![Membership::Keyword(SEEN.into())],
            lost: vec![],
            ..trashed
        };
        assert_eq!(undo_ops(&read), [keyword(SEEN, false)]);
        assert_eq!(
            reverse_changes(&read),
            [Change::SetKeyword {
                message_id: "m1".into(),
                keyword: SEEN.into(),
                on: false
            }]
        );
    }
}
