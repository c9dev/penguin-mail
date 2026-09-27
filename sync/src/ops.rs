//! Mail operations: what an action does to messages, in words every server
//! shares. `ops_for` is the engine's one place that turns a triage action
//! into operations. The engine applies them to the store, which reports
//! what each message gained and lost, and hands them to the account's
//! backend; undo builds the operations that reverse that report.

use std::collections::{BTreeMap, HashMap, HashSet};

use mailrs_domain::mailbox::keyword::{FLAGGED, MUTED, SEEN};
use mailrs_domain::{AccountId, Applied, MailSet, Membership, Memberships, Role};
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

/// Where a person moved mail from: the place each account's mail was
/// listed in when they acted on it, such as a folder, or the Inbox of
/// every account for the unified Inbox. A whole conversation moved on a
/// folder account carries only its messages there; see
/// [`drop_protected`]. `MovedFrom::nowhere()` names no place, as a
/// search, a smart mailbox or a flag colour lists mail from anywhere.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MovedFrom {
    each: BTreeMap<AccountId, MailSet>,
    every: Option<MailSet>,
}

impl MovedFrom {
    /// No one place: the roles decide what a move carries.
    pub fn nowhere() -> MovedFrom {
        MovedFrom::default()
    }

    /// `set` in one account, such as a folder.
    pub fn one(account_id: AccountId, set: MailSet) -> MovedFrom {
        MovedFrom {
            each: BTreeMap::from([(account_id, set)]),
            every: None,
        }
    }

    /// `set` in every account, which only a role such as the Inbox can
    /// be, since each account names its own folders.
    pub fn every(set: MailSet) -> MovedFrom {
        MovedFrom {
            each: BTreeMap::new(),
            every: Some(set),
        }
    }

    /// These places and `other`'s, a place named for one account winning
    /// over one named for every account.
    pub fn and(mut self, other: MovedFrom) -> MovedFrom {
        self.each.extend(other.each);
        self.every = self.every.or(other.every);
        self
    }

    /// The place mail in `account_id` was moved from, if one was named.
    pub fn of(&self, account_id: AccountId) -> Option<&MailSet> {
        self.each.get(&account_id).or(self.every.as_ref())
    }
}

/// The server's id for each role's mailbox, as the backend reports it.
pub type Roles = BTreeMap<Role, String>;

/// The operations `action` stands for on an account whose mail service can
/// do `caps`. A label account files and unfiles; a folder account moves,
/// since a message there sits in one folder: the one place an action adds
/// is where the mail goes, and mail that leaves a place with nowhere named
/// goes to the Archive. `set_of` reads a mailbox id the action names as
/// the mail set it stands for, as the account's mail service reads it.
/// `Unsupported` when a label account lacks the mailbox the action files
/// mail in, or when an action puts mail in two folders at once.
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
        TriageAction::AddLabel(id) if moves => moved(&[set_of(id)], &[], roles)?,
        TriageAction::AddLabel(id) => vec![set_op(&set_of(id), true, roles)?],
        TriageAction::RemoveLabel(id) if moves => moved(&[], &[set_of(id)], roles)?,
        TriageAction::RemoveLabel(id) => vec![set_op(&set_of(id), false, roles)?],
        TriageAction::Relabel { add, remove } if moves => moved(add, remove, roles)?,
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

/// The operations that change mail sets on a folder account. Marks, such
/// as a keyword, unread or a category, go on and come off as on a label
/// account. The one place `add` names is where the mail moves, which
/// takes it out of every other; with none named, mail that leaves a place
/// goes to the Archive, since a folder server has nowhere else to keep
/// it. Two places at once is `Unsupported`.
fn moved(add: &[MailSet], remove: &[MailSet], roles: &Roles) -> Result<Vec<MailOp>, BackendError> {
    let mut ops = Vec::new();
    for (sets, on) in [(add, true), (remove, false)] {
        for set in sets.iter().filter(|set| move_op(set).is_none()) {
            ops.push(set_op(set, on, roles)?);
        }
    }
    let mut into = add.iter().filter_map(move_op);
    let destination = match (into.next(), into.next()) {
        (Some(_), Some(_)) => return Err(BackendError::Unsupported),
        (Some(op), None) => Some(op),
        (None, _) if remove.iter().any(|set| move_op(set).is_some()) => {
            Some(MailOp::MoveToRole(Role::Archive))
        }
        (None, _) => None,
    };
    ops.extend(destination);
    Ok(ops)
}

/// The move into `set`, when `set` is a place mail sits in rather than a
/// mark it carries.
fn move_op(set: &MailSet) -> Option<MailOp> {
    match set {
        MailSet::Role(role) => Some(MailOp::MoveToRole(*role)),
        MailSet::Mailbox(id) => Some(MailOp::MoveToMailbox(id.clone())),
        MailSet::Keyword(_) | MailSet::Unseen | MailSet::Category(_) => None,
    }
}

/// `ids` without those a whole-thread move should leave alone. A folder
/// server keeps one copy of each message, so a move carries only what
/// sits in `from`, the server mailbox the person moved the conversation
/// out of: a conversation opened from Work and archived leaves its
/// message in Travel where it is.
///
/// Without a `from`, as from a search or a smart mailbox, or for a thread
/// that holds nothing in it any more, the roles decide: a message that
/// sits only in Sent, Drafts, Trash or Junk stays there unless `ops`
/// moves mail into or out of that same place, so a change aimed
/// elsewhere cannot pull a sent reply out of Sent or an old message out
/// of Trash. `whole_thread` maps each id a thread target picked up on its
/// own to its thread; an id a person named by hand always moves.
pub fn drop_protected(
    ids: Vec<String>,
    whole_thread: &HashMap<String, String>,
    held: &HashMap<String, Memberships>,
    ops: &[MailOp],
    roles: &Roles,
    from: Option<&str>,
) -> Vec<String> {
    if !ops.iter().any(is_move_op) {
        return ids;
    }
    let none = Memberships::default();
    let mailboxes = |id: &String| &held.get(id).unwrap_or(&none).mailboxes;
    let sits_in_source = |id: &String| from.is_some_and(|from| mailboxes(id).iter().any(|m| m == from));
    let with_source: HashSet<&String> = whole_thread
        .iter()
        .filter(|(id, _)| sits_in_source(id))
        .map(|(_, thread)| thread)
        .collect();
    ids.into_iter()
        .filter(|id| {
            let Some(thread) = whole_thread.get(id) else {
                return true;
            };
            if with_source.contains(thread) {
                return sits_in_source(id);
            }
            match only_role(mailboxes(id), roles) {
                Some(role) => !move_leaves_alone(role, ops, roles),
                None => true,
            }
        })
        .collect()
}

/// The server mailbox `from` names in an account whose role mailboxes are
/// `roles`. `None` for a mark, such as flagged or unread, which is no
/// place mail moves out of, and for a role the account has no mailbox for.
pub fn source_mailbox(from: &MailSet, roles: &Roles) -> Option<String> {
    match from {
        MailSet::Role(role) => roles.get(role).cloned(),
        MailSet::Mailbox(id) => Some(id.clone()),
        MailSet::Keyword(_) | MailSet::Unseen | MailSet::Category(_) => None,
    }
}

/// The place `action` itself takes mail out of, when it names one: the
/// mailbox a `RemoveLabel` or a `Relabel` removes. That is the source of
/// the move on a folder account, whatever list the mail was picked from.
/// `set_of` reads a mailbox id as the account's mail service reads it.
pub fn moved_out_of(action: &TriageAction, set_of: impl Fn(&str) -> MailSet) -> Option<MailSet> {
    let place = |set: MailSet| move_op(&set).is_some().then_some(set);
    match action {
        TriageAction::RemoveLabel(id) => place(set_of(id)),
        TriageAction::Relabel { remove, .. } => remove.iter().cloned().find_map(place),
        _ => None,
    }
}

fn is_move_op(op: &MailOp) -> bool {
    matches!(op, MailOp::MoveToRole(_) | MailOp::MoveToMailbox(_))
}

/// The role `mailboxes` names, when it is exactly one mailbox and that
/// mailbox carries a role.
fn only_role(mailboxes: &[String], roles: &Roles) -> Option<Role> {
    let [only] = mailboxes else { return None };
    roles.iter().find(|(_, id)| *id == only).map(|(role, _)| *role)
}

/// Whether a move `ops` describes should leave a message alone that sits
/// only in `role`. Sent and Drafts never move on a thread-wide change;
/// Trash and Junk stay put unless the move takes mail into the Inbox
/// (Untrash, Not Junk, a reminder coming due), and Junk mail goes to
/// Trash as any other message does.
fn move_leaves_alone(role: Role, ops: &[MailOp], roles: &Roles) -> bool {
    match role {
        Role::Sent | Role::Drafts => true,
        Role::Trash => !targets_role(ops, Role::Inbox, roles),
        Role::Junk => {
            !targets_role(ops, Role::Inbox, roles) && !targets_role(ops, Role::Trash, roles)
        }
        _ => false,
    }
}

/// Whether one of `ops` moves mail into `role`, by role or by the
/// mailbox `roles` names for it.
fn targets_role(ops: &[MailOp], role: Role, roles: &Roles) -> bool {
    let id = roles.get(&role);
    ops.iter().any(|op| match op {
        MailOp::MoveToRole(r) => *r == role,
        MailOp::MoveToMailbox(m) => id.is_some_and(|rid| rid == m),
        _ => false,
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
    fn a_folder_account_moves_where_a_label_account_adds_a_mailbox() {
        let roles = gmail_roles();
        let ops = |action: TriageAction| {
            ops_for(&action, &folder_account(), &roles, named).unwrap()
        };
        // Dropped from Flagged or a search, nothing is taken away, and the
        // mail still moves rather than gaining a copy.
        assert_eq!(
            ops(TriageAction::Relabel {
                add: vec![MailSet::Mailbox("Label_5".into())],
                remove: vec![],
            }),
            [MailOp::MoveToMailbox("Label_5".into())]
        );
        assert_eq!(
            ops(TriageAction::Relabel {
                add: vec![MailSet::Mailbox("Label_5".into())],
                remove: vec![MailSet::Role(Role::Inbox)],
            }),
            [MailOp::MoveToMailbox("Label_5".into())]
        );
        assert_eq!(
            ops(TriageAction::AddLabel("Label_5".into())),
            [MailOp::MoveToMailbox("Label_5".into())]
        );
        // A reminder coming due puts the mail back in the inbox, unread.
        assert_eq!(
            ops(TriageAction::Relabel {
                add: vec![MailSet::Role(Role::Inbox), MailSet::Unseen],
                remove: vec![],
            }),
            [keyword(SEEN, false), MailOp::MoveToRole(Role::Inbox)]
        );
    }

    #[test]
    fn a_folder_account_archives_mail_that_leaves_a_folder_for_nowhere() {
        let roles = gmail_roles();
        let ops = |action: TriageAction| {
            ops_for(&action, &folder_account(), &roles, named).unwrap()
        };
        assert_eq!(
            ops(TriageAction::Relabel {
                add: vec![],
                remove: vec![MailSet::Mailbox("Work".into()), MailSet::Role(Role::Inbox)],
            }),
            [MailOp::MoveToRole(Role::Archive)]
        );
        assert_eq!(
            ops(TriageAction::RemoveLabel("Work".into())),
            [MailOp::MoveToRole(Role::Archive)]
        );
        assert_eq!(
            ops(TriageAction::Relabel {
                add: vec![MailSet::muted()],
                remove: vec![MailSet::Role(Role::Inbox)],
            }),
            [keyword(MUTED, true), MailOp::MoveToRole(Role::Archive)]
        );
        assert_eq!(
            ops(TriageAction::RemoveLabel("UNREAD".into())),
            [keyword(SEEN, true)],
            "a mark comes off without moving anything"
        );
    }

    #[test]
    fn a_folder_account_cannot_put_mail_in_two_folders() {
        let two = TriageAction::Relabel {
            add: vec![MailSet::Mailbox("Work".into()), MailSet::Mailbox("Travel".into())],
            remove: vec![],
        };
        assert!(matches!(
            ops_for(&two, &folder_account(), &gmail_roles(), named),
            Err(BackendError::Unsupported)
        ));
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
    fn a_whole_thread_move_leaves_a_sent_copy_in_sent() {
        let roles = Roles::from([
            (Role::Archive, "Archive".to_string()),
            (Role::Sent, "Sent".to_string()),
        ]);
        let held = HashMap::from([
            (
                "work".to_string(),
                Memberships { mailboxes: vec!["Work".into()], ..Memberships::default() },
            ),
            (
                "sent".to_string(),
                Memberships { mailboxes: vec!["Sent".into()], ..Memberships::default() },
            ),
        ]);
        let whole_thread = one_thread(&["work", "sent"]);
        let ids = vec!["work".to_string(), "sent".to_string()];
        let ops = [MailOp::MoveToRole(Role::Archive)];
        assert_eq!(
            drop_protected(ids, &whole_thread, &held, &ops, &roles, None),
            ["work".to_string()]
        );
    }

    #[test]
    fn a_message_picked_by_hand_moves_even_from_sent() {
        let roles = Roles::from([
            (Role::Archive, "Archive".to_string()),
            (Role::Sent, "Sent".to_string()),
        ]);
        let held = HashMap::from([(
            "sent".to_string(),
            Memberships { mailboxes: vec!["Sent".into()], ..Memberships::default() },
        )]);
        let ids = vec!["sent".to_string()];
        let ops = [MailOp::MoveToRole(Role::Archive)];
        assert_eq!(
            drop_protected(ids, &HashMap::new(), &held, &ops, &roles, Some("Work")),
            ["sent".to_string()]
        );
    }

    #[test]
    fn untrash_brings_a_trashed_copy_back_to_the_inbox() {
        let roles = Roles::from([
            (Role::Inbox, "INBOX".to_string()),
            (Role::Trash, "Trash".to_string()),
        ]);
        let held = HashMap::from([(
            "t".to_string(),
            Memberships { mailboxes: vec!["Trash".into()], ..Memberships::default() },
        )]);
        let whole_thread = one_thread(&["t"]);
        let ids = vec!["t".to_string()];
        let ops = [MailOp::MoveToRole(Role::Inbox)];
        assert_eq!(
            drop_protected(ids, &whole_thread, &held, &ops, &roles, None),
            ["t".to_string()]
        );
    }

    #[test]
    fn trashing_takes_junk_mail_too() {
        let roles = Roles::from([
            (Role::Trash, "Trash".to_string()),
            (Role::Junk, "Junk".to_string()),
        ]);
        let held = HashMap::from([(
            "j".to_string(),
            Memberships { mailboxes: vec!["Junk".into()], ..Memberships::default() },
        )]);
        let whole_thread = one_thread(&["j"]);
        let ids = vec!["j".to_string()];
        let ops = [MailOp::MoveToRole(Role::Trash)];
        assert_eq!(
            drop_protected(ids, &whole_thread, &held, &ops, &roles, None),
            ["j".to_string()]
        );
    }

    #[test]
    fn a_mark_with_no_move_touches_every_message_of_the_thread() {
        let roles = Roles::new();
        let held = HashMap::from([(
            "sent".to_string(),
            Memberships { mailboxes: vec!["Sent".into()], ..Memberships::default() },
        )]);
        let whole_thread = one_thread(&["sent"]);
        let ids = vec!["sent".to_string()];
        let ops = [keyword(SEEN, true)];
        assert_eq!(
            drop_protected(ids, &whole_thread, &held, &ops, &roles, None),
            ["sent".to_string()]
        );
    }

    /// Each of `ids` as a message a thread target picked up, all of one
    /// thread.
    fn one_thread(ids: &[&str]) -> HashMap<String, String> {
        ids.iter().map(|id| (id.to_string(), "t".to_string())).collect()
    }

    /// Where each message sits: its id and its one mailbox.
    fn sitting(places: &[(&str, &str)]) -> HashMap<String, Memberships> {
        places
            .iter()
            .map(|(id, mailbox)| {
                let held = Memberships { mailboxes: vec![mailbox.to_string()], ..Memberships::default() };
                (id.to_string(), held)
            })
            .collect()
    }

    fn folder_roles() -> Roles {
        Roles::from([
            (Role::Inbox, "INBOX".to_string()),
            (Role::Archive, "Archive".to_string()),
            (Role::Sent, "Sent".to_string()),
            (Role::Trash, "Trash".to_string()),
        ])
    }

    /// A conversation opened from Work and archived takes the message in
    /// Work and leaves the one filed alone in Travel where it is.
    #[test]
    fn a_move_from_a_folder_takes_only_what_sits_there() {
        let held = sitting(&[("work", "Work"), ("travel", "Travel"), ("sent", "Sent")]);
        let ids = vec!["work".to_string(), "travel".to_string(), "sent".to_string()];
        let whole_thread = one_thread(&["work", "travel", "sent"]);
        let ops = [MailOp::MoveToRole(Role::Archive)];
        assert_eq!(
            drop_protected(ids, &whole_thread, &held, &ops, &folder_roles(), Some("Work")),
            ["work".to_string()]
        );
    }

    /// With no one folder to move from, as after a search, the roles
    /// decide: Sent stays and an ordinary folder goes with the rest.
    #[test]
    fn a_move_from_nowhere_keeps_the_role_rule() {
        let held = sitting(&[("work", "Work"), ("travel", "Travel"), ("sent", "Sent")]);
        let ids = vec!["work".to_string(), "travel".to_string(), "sent".to_string()];
        let whole_thread = one_thread(&["work", "travel", "sent"]);
        let ops = [MailOp::MoveToRole(Role::Archive)];
        assert_eq!(
            drop_protected(ids, &whole_thread, &held, &ops, &folder_roles(), None),
            ["work".to_string(), "travel".to_string()]
        );
    }

    /// Mail moved from Sent is the sent copy the person pointed at.
    #[test]
    fn a_move_from_sent_takes_the_sent_copy() {
        let held = sitting(&[("work", "Work"), ("sent", "Sent")]);
        let ids = vec!["work".to_string(), "sent".to_string()];
        let whole_thread = one_thread(&["work", "sent"]);
        let ops = [MailOp::MoveToRole(Role::Archive)];
        assert_eq!(
            drop_protected(ids, &whole_thread, &held, &ops, &folder_roles(), Some("Sent")),
            ["sent".to_string()]
        );
    }

    /// A thread that holds nothing in the folder it was moved from has
    /// moved since it was listed; the roles decide for it, so the move
    /// the person asked for still happens.
    #[test]
    fn a_thread_with_nothing_left_in_the_source_falls_back_to_the_roles() {
        let held = sitting(&[("travel", "Travel"), ("sent", "Sent")]);
        let ids = vec!["travel".to_string(), "sent".to_string()];
        let whole_thread = one_thread(&["travel", "sent"]);
        let ops = [MailOp::MoveToRole(Role::Archive)];
        assert_eq!(
            drop_protected(ids, &whole_thread, &held, &ops, &folder_roles(), Some("Work")),
            ["travel".to_string()]
        );
    }

    /// Two threads in one call: each falls back on its own.
    #[test]
    fn each_thread_is_judged_by_its_own_messages() {
        let held = sitting(&[("a-work", "Work"), ("a-travel", "Travel"), ("b-travel", "Travel")]);
        let ids = vec!["a-work".to_string(), "a-travel".to_string(), "b-travel".to_string()];
        let whole_thread = HashMap::from([
            ("a-work".to_string(), "a".to_string()),
            ("a-travel".to_string(), "a".to_string()),
            ("b-travel".to_string(), "b".to_string()),
        ]);
        let ops = [MailOp::MoveToRole(Role::Archive)];
        assert_eq!(
            drop_protected(ids, &whole_thread, &held, &ops, &folder_roles(), Some("Work")),
            ["a-work".to_string(), "b-travel".to_string()]
        );
    }

    #[test]
    fn a_source_resolves_by_role_or_by_mailbox() {
        let roles = folder_roles();
        assert_eq!(source_mailbox(&MailSet::Role(Role::Inbox), &roles).as_deref(), Some("INBOX"));
        assert_eq!(source_mailbox(&MailSet::Mailbox("Work".into()), &roles).as_deref(), Some("Work"));
        assert_eq!(source_mailbox(&MailSet::Role(Role::Junk), &roles), None, "no Junk here");
        assert_eq!(source_mailbox(&MailSet::flagged(), &roles), None, "a mark is no place");
    }

    /// Taking mail out of a folder names where it comes from, whatever
    /// the list on screen showed.
    #[test]
    fn a_removal_names_its_own_source() {
        let set_of = |id: &str| MailSet::Mailbox(id.into());
        assert_eq!(
            moved_out_of(&TriageAction::RemoveLabel("Travel".into()), set_of),
            Some(MailSet::Mailbox("Travel".into()))
        );
        let relabel = TriageAction::Relabel {
            add: vec![MailSet::Mailbox("Work".into())],
            remove: vec![MailSet::Unseen, MailSet::Role(Role::Inbox)],
        };
        assert_eq!(moved_out_of(&relabel, set_of), Some(MailSet::Role(Role::Inbox)));
        assert_eq!(moved_out_of(&TriageAction::Archive, set_of), None);
        assert_eq!(
            moved_out_of(&TriageAction::RemoveLabel("$flagged".into()), |_| MailSet::flagged()),
            None
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
