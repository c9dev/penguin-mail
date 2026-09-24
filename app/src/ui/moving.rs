//! What dragging mail onto a mailbox does. Moves follow Apple Mail: the
//! mail gains the mailbox it lands on and leaves the one it came from.

use mailrs_sync::TriageAction;

use mailrs_domain::translate::gettext;
use mailrs_domain::{Folder, MailSet, Role};

use super::{Mailbox, Standard};

/// The mail set a move takes mail out of, when leaving `mailbox` means
/// losing one: the inbox, Muted or a person's label. Flagged, Sent and
/// Drafts are marks or records, not places mail leaves.
fn source(mailbox: &Mailbox) -> Option<MailSet> {
    match mailbox {
        Mailbox::Unified(which) | Mailbox::Standard { which, .. } => match which {
            Standard::Inbox | Standard::Muted => Some(which.set()),
            Standard::Flagged | Standard::Sent | Standard::Drafts => None,
        },
        Mailbox::Label { label_id, .. } => Some(MailSet::Mailbox(label_id.clone())),
        _ => None,
    }
}

/// The mail set a destination adds, if it is one.
fn destination(mailbox: &Mailbox) -> Option<MailSet> {
    match mailbox {
        Mailbox::Unified(which) | Mailbox::Standard { which, .. } => Some(which.set()),
        Mailbox::Label { label_id, .. } => Some(MailSet::Mailbox(label_id.clone())),
        _ => None,
    }
}

fn relabel(add: Vec<MailSet>, remove: Vec<MailSet>) -> TriageAction {
    TriageAction::Relabel { add, remove }
}

/// The change that moves mail shown in `from` into `to`, or why it cannot.
pub fn move_action(from: &Mailbox, to: &Mailbox) -> Result<TriageAction, String> {
    let already = || gettext("The mail is already there");
    let from_folder = from.folder();
    let inbox = MailSet::Role(Role::Inbox);
    let junk = MailSet::Role(Role::Junk);
    let dest = destination(to);
    if dest == Some(MailSet::flagged()) {
        return Ok(TriageAction::Star);
    }
    match (from_folder, to.folder()) {
        (Some(Folder::Trash), Some(Folder::Trash)) | (Some(Folder::Junk), Some(Folder::Junk)) => {
            return Err(already());
        }
        (_, Some(Folder::Trash)) => return Ok(TriageAction::Trash),
        (Some(Folder::Trash), _) if dest.as_ref() == Some(&inbox) => {
            return Ok(TriageAction::Untrash);
        }
        (Some(Folder::Trash), _) => {
            return Err(gettext("Move it from the Trash to the Inbox first"));
        }
        (Some(Folder::Junk), _) if dest.as_ref() == Some(&inbox) => {
            return Ok(TriageAction::NotJunk);
        }
        (Some(Folder::Junk), Some(Folder::AllMail | Folder::Archive)) => {
            return Ok(relabel(vec![], vec![junk]));
        }
        (Some(Folder::Junk), _) => {
            let dest = dest.ok_or_else(already)?;
            return Ok(relabel(vec![dest], vec![junk]));
        }
        _ => {}
    }
    let source = source(from);
    match to.folder() {
        Some(Folder::Junk) => {
            return Ok(match source.filter(|s| *s != inbox) {
                Some(set) => relabel(vec![junk], vec![inbox, set]),
                None => TriageAction::Junk,
            });
        }
        // Archiving leaves the inbox; from a label, the mail leaves that
        // label too, as it does on any other move.
        Some(Folder::Archive) => {
            return match source {
                _ if from_folder == Some(Folder::Archive) => Err(already()),
                Some(set) if set != inbox => Ok(relabel(vec![], vec![set, inbox])),
                _ => Ok(TriageAction::Archive),
            };
        }
        Some(Folder::AllMail) => {
            return match source {
                Some(set) if set == inbox => Ok(TriageAction::Archive),
                Some(MailSet::Mailbox(id)) => Ok(TriageAction::RemoveLabel(id)),
                Some(set) => Ok(relabel(vec![], vec![set])),
                None => Err(gettext("The mail is already in All Mail")),
            };
        }
        _ => {}
    }
    let dest = dest.ok_or_else(|| gettext("Mail cannot go there"))?;
    match source {
        Some(from) if from == dest => Err(already()),
        Some(from) => Ok(relabel(vec![dest], vec![from])),
        None => Ok(relabel(vec![dest], vec![])),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn label(id: &str) -> Mailbox {
        Mailbox::Label {
            account_id: 1,
            label_id: id.into(),
            name: id.into(),
        }
    }

    fn account_inbox() -> Mailbox {
        Mailbox::Standard {
            account_id: 1,
            which: Standard::Inbox,
        }
    }

    fn folder(folder: Folder) -> Mailbox {
        Mailbox::Folder {
            account_id: None,
            folder,
        }
    }

    fn named(id: &str) -> MailSet {
        MailSet::Mailbox(id.into())
    }

    fn inbox_set() -> MailSet {
        MailSet::Role(Role::Inbox)
    }

    fn junk_set() -> MailSet {
        MailSet::Role(Role::Junk)
    }

    #[test]
    fn moving_from_the_inbox_to_a_label_files_it_there() {
        let inbox = Mailbox::Unified(Standard::Inbox);
        assert_eq!(
            move_action(&inbox, &label("Travel")),
            Ok(relabel(vec![named("Travel")], vec![inbox_set()]))
        );
        assert_eq!(
            move_action(&inbox, &folder(Folder::AllMail)),
            Ok(TriageAction::Archive)
        );
        assert_eq!(
            move_action(&inbox, &folder(Folder::Archive)),
            Ok(TriageAction::Archive)
        );
        assert_eq!(
            move_action(&inbox, &folder(Folder::Trash)),
            Ok(TriageAction::Trash)
        );
        assert_eq!(
            move_action(&inbox, &folder(Folder::Junk)),
            Ok(TriageAction::Junk)
        );
        assert_eq!(
            move_action(&inbox, &Mailbox::Unified(Standard::Flagged)),
            Ok(TriageAction::Star)
        );
        assert!(move_action(&inbox, &account_inbox()).is_err());
    }

    #[test]
    fn labels_trade_places_and_searches_only_add() {
        assert_eq!(
            move_action(&label("Work"), &label("Travel")),
            Ok(relabel(vec![named("Travel")], vec![named("Work")]))
        );
        assert_eq!(
            move_action(&label("Work"), &folder(Folder::AllMail)),
            Ok(TriageAction::RemoveLabel("Work".into()))
        );
        let search = Mailbox::Search {
            query: "x".into(),
            account_id: None,
        };
        assert_eq!(
            move_action(&search, &label("Travel")),
            Ok(relabel(vec![named("Travel")], vec![]))
        );
        assert_eq!(
            move_action(&Mailbox::Unified(Standard::Sent), &account_inbox()),
            Ok(relabel(vec![inbox_set()], vec![]))
        );
    }

    #[test]
    fn the_archive_takes_mail_out_of_the_inbox_and_gives_it_back() {
        let archive = folder(Folder::Archive);
        assert_eq!(
            move_action(&label("Work"), &archive),
            Ok(relabel(vec![], vec![named("Work"), inbox_set()]))
        );
        assert_eq!(
            move_action(&archive, &Mailbox::Unified(Standard::Inbox)),
            Ok(relabel(vec![inbox_set()], vec![]))
        );
        assert!(move_action(&archive, &archive).is_err());
        assert_eq!(
            move_action(&folder(Folder::Junk), &archive),
            Ok(relabel(vec![], vec![junk_set()]))
        );
    }

    #[test]
    fn junk_and_trash_hand_mail_back_properly() {
        let trash = folder(Folder::Trash);
        let junk = folder(Folder::Junk);
        assert_eq!(
            move_action(&trash, &Mailbox::Unified(Standard::Inbox)),
            Ok(TriageAction::Untrash)
        );
        assert!(move_action(&trash, &label("Travel")).is_err());
        assert!(move_action(&trash, &trash).is_err());
        assert_eq!(
            move_action(&junk, &account_inbox()),
            Ok(TriageAction::NotJunk)
        );
        assert_eq!(
            move_action(&junk, &label("Travel")),
            Ok(relabel(vec![named("Travel")], vec![junk_set()]))
        );
        assert_eq!(
            move_action(&label("Work"), &junk),
            Ok(relabel(vec![junk_set()], vec![inbox_set(), named("Work")]))
        );
    }

    #[test]
    fn dragging_onto_muted_mutes_and_dragging_off_it_unmutes() {
        let inbox = Mailbox::Unified(Standard::Inbox);
        let muted = Mailbox::Unified(Standard::Muted);
        assert_eq!(
            move_action(&inbox, &muted),
            Ok(relabel(vec![MailSet::muted()], vec![inbox_set()]))
        );
        assert_eq!(
            move_action(&muted, &inbox),
            Ok(relabel(vec![inbox_set()], vec![MailSet::muted()]))
        );
    }
}
