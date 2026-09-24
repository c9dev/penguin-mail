//! What dragging mail onto a mailbox does. Moves follow Apple Mail: the
//! mail gains the mailbox it lands on and leaves the one it came from.

use mailrs_sync::TriageAction;

use mailrs_domain::translate::gettext;
use mailrs_domain::{Folder, gmail, system_label};

use super::Mailbox;

/// The label a move takes mail out of, when leaving `mailbox` means
/// losing one: the inbox or a user label.
fn source_label(mailbox: &Mailbox) -> Option<String> {
    let label = match mailbox {
        Mailbox::Unified(label) => *label,
        Mailbox::Label { label_id, .. } => label_id.as_str(),
        _ => return None,
    };
    (!matches!(
        label,
        system_label::STARRED | system_label::SENT | system_label::DRAFT | system_label::IMPORTANT
    ))
    .then(|| label.to_string())
}

/// The label a destination adds, if it is one.
fn dest_label(mailbox: &Mailbox) -> Option<&str> {
    match mailbox {
        Mailbox::Unified(label) => Some(label),
        Mailbox::Label { label_id, .. } => Some(label_id.as_str()),
        _ => None,
    }
}

/// The change that moves mail shown in `from` into `to`, or why it cannot.
pub fn move_action(from: &Mailbox, to: &Mailbox) -> Result<TriageAction, String> {
    let already = || gettext("The mail is already there");
    let from_folder = from.folder();
    let relabel = |add: Vec<String>, remove: Vec<String>| TriageAction::Relabel {
        add: add.iter().map(|l| gmail::set_of(l)).collect(),
        remove: remove.iter().map(|l| gmail::set_of(l)).collect(),
    };
    if dest_label(to) == Some(system_label::STARRED) {
        return Ok(TriageAction::Star);
    }
    match (from_folder, to.folder()) {
        (Some(Folder::Trash), Some(Folder::Trash)) | (Some(Folder::Junk), Some(Folder::Junk)) => {
            return Err(already());
        }
        (_, Some(Folder::Trash)) => return Ok(TriageAction::Trash),
        (Some(Folder::Trash), _) if dest_label(to) == Some(system_label::INBOX) => {
            return Ok(TriageAction::Untrash);
        }
        (Some(Folder::Trash), _) => {
            return Err(gettext("Move it from the Trash to the Inbox first"));
        }
        (Some(Folder::Junk), _) if dest_label(to) == Some(system_label::INBOX) => {
            return Ok(TriageAction::NotJunk);
        }
        (Some(Folder::Junk), Some(Folder::AllMail | Folder::Archive)) => {
            return Ok(relabel(vec![], vec![system_label::SPAM.into()]));
        }
        (Some(Folder::Junk), _) => {
            let label = dest_label(to).ok_or_else(already)?;
            return Ok(relabel(vec![label.into()], vec![system_label::SPAM.into()]));
        }
        _ => {}
    }
    let source = source_label(from);
    match to.folder() {
        Some(Folder::Junk) => {
            return Ok(match source.filter(|s| s != system_label::INBOX) {
                Some(label) => relabel(
                    vec![system_label::SPAM.into()],
                    vec![system_label::INBOX.into(), label],
                ),
                None => TriageAction::Junk,
            });
        }
        // Archiving leaves the inbox; from a label, the mail leaves that
        // label too, as it does on any other move.
        Some(Folder::Archive) => {
            return match source {
                _ if from_folder == Some(Folder::Archive) => Err(already()),
                Some(label) if label != system_label::INBOX => {
                    Ok(relabel(vec![], vec![label, system_label::INBOX.into()]))
                }
                _ => Ok(TriageAction::Archive),
            };
        }
        Some(Folder::AllMail) => {
            return match source.as_deref() {
                Some(system_label::INBOX) => Ok(TriageAction::Archive),
                Some(label) => Ok(TriageAction::RemoveLabel(label.into())),
                None => Err(gettext("The mail is already in All Mail")),
            };
        }
        _ => {}
    }
    let dest = dest_label(to).ok_or_else(|| gettext("Mail cannot go there"))?;
    match source {
        Some(label) if label == dest => Err(already()),
        Some(label) => Ok(relabel(vec![dest.into()], vec![label])),
        None => Ok(TriageAction::AddLabel(dest.into())),
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

    fn folder(folder: Folder) -> Mailbox {
        Mailbox::Folder {
            account_id: None,
            folder,
        }
    }

    fn relabel(add: &[&str], remove: &[&str]) -> TriageAction {
        TriageAction::Relabel {
            add: add.iter().map(|l| gmail::set_of(l)).collect(),
            remove: remove.iter().map(|l| gmail::set_of(l)).collect(),
        }
    }

    #[test]
    fn moving_from_the_inbox_to_a_label_files_it_there() {
        let inbox = Mailbox::Unified("INBOX");
        assert_eq!(
            move_action(&inbox, &label("Travel")),
            Ok(relabel(&["Travel"], &["INBOX"]))
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
            move_action(&inbox, &Mailbox::Unified("STARRED")),
            Ok(TriageAction::Star)
        );
        assert!(move_action(&inbox, &label("INBOX")).is_err());
    }

    #[test]
    fn labels_trade_places_and_searches_only_add() {
        assert_eq!(
            move_action(&label("Work"), &label("Travel")),
            Ok(relabel(&["Travel"], &["Work"]))
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
            Ok(TriageAction::AddLabel("Travel".into()))
        );
        assert_eq!(
            move_action(&Mailbox::Unified("SENT"), &label("INBOX")),
            Ok(TriageAction::AddLabel("INBOX".into()))
        );
    }

    #[test]
    fn the_archive_takes_mail_out_of_the_inbox_and_gives_it_back() {
        let archive = folder(Folder::Archive);
        assert_eq!(
            move_action(&label("Work"), &archive),
            Ok(relabel(&[], &["Work", "INBOX"]))
        );
        assert_eq!(
            move_action(&archive, &Mailbox::Unified("INBOX")),
            Ok(TriageAction::AddLabel("INBOX".into()))
        );
        assert!(move_action(&archive, &archive).is_err());
        assert_eq!(
            move_action(&folder(Folder::Junk), &archive),
            Ok(relabel(&[], &["SPAM"]))
        );
    }

    #[test]
    fn junk_and_trash_hand_mail_back_properly() {
        let trash = folder(Folder::Trash);
        let junk = folder(Folder::Junk);
        assert_eq!(
            move_action(&trash, &Mailbox::Unified("INBOX")),
            Ok(TriageAction::Untrash)
        );
        assert!(move_action(&trash, &label("Travel")).is_err());
        assert!(move_action(&trash, &trash).is_err());
        assert_eq!(
            move_action(&junk, &label("INBOX")),
            Ok(TriageAction::NotJunk)
        );
        assert_eq!(
            move_action(&junk, &label("Travel")),
            Ok(relabel(&["Travel"], &["SPAM"]))
        );
        assert_eq!(
            move_action(&label("Work"), &junk),
            Ok(relabel(&["SPAM"], &["INBOX", "Work"]))
        );
    }

    #[test]
    fn dragging_onto_muted_mutes_and_dragging_off_it_unmutes() {
        let inbox = Mailbox::Unified(system_label::INBOX);
        let muted = Mailbox::Unified(system_label::MUTE);
        assert_eq!(
            move_action(&inbox, &muted),
            Ok(relabel(&["MUTE"], &["INBOX"]))
        );
        assert_eq!(
            move_action(&muted, &inbox),
            Ok(relabel(&["INBOX"], &["MUTE"]))
        );
    }
}
