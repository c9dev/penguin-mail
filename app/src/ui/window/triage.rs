//! What the archive, trash, junk, flag and read buttons come to.
//!
//! The mailbox on screen has as much say as the button: Delete erases
//! mail that is already in the Trash and calls off what the Scheduled,
//! Outbox, Remind Me and Follow Up lists hold, and Junk sends mail that
//! is already in Junk back out. [`decide`] answers all of that on its
//! own, with no widget in sight, so the main window and a conversation in
//! a window of its own read one table rather than two.

use std::rc::Rc;

use mailrs_domain::{Folder, ThreadSummary};
use mailrs_sync::{History, MailAction, TriageAction};

use super::MainWindow;
use crate::ui::Mailbox;
use crate::ui::conversation::{Action, ConversationView, OpenThread};

/// What a mail button does, once the mailbox and the targets' own marks
/// are known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Decision {
    /// Labels, which Undo puts back.
    Triage(TriageAction),
    /// Flag in the colour last chosen, or take the flag off.
    Flag(bool),
    /// Erase the mail. The Trash has nowhere further to move it to.
    DeleteForever,
    /// The mailbox lists something other than mail, so Delete calls that
    /// off instead of moving anything.
    Cancel(Cancel),
}

/// What Delete calls off in a mailbox that lists no mail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Cancel {
    Scheduled,
    Queued,
    Reminder,
    FollowUp,
}

/// What the targets carry, as far as these buttons care.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Marks {
    /// Any of them is unread.
    pub unread: bool,
    /// All of them are flagged.
    pub flagged: bool,
}

impl Marks {
    pub(super) fn from_row(row: &ThreadSummary) -> Marks {
        Marks {
            unread: row.unread,
            flagged: row.starred,
        }
    }

    pub(super) fn from_open(open: &OpenThread) -> Marks {
        Marks {
            unread: open.unread(),
            flagged: open.starred(),
        }
    }
}

/// What `action` does to mail carrying `marks` in `mailbox`. `None` for
/// an action that changes no mail.
pub(super) fn decide(action: &Action, mailbox: &Mailbox, marks: Marks) -> Option<Decision> {
    match action {
        Action::Archive => Some(Decision::Triage(TriageAction::Archive)),
        Action::Trash => Some(match mailbox {
            Mailbox::Scheduled => Decision::Cancel(Cancel::Scheduled),
            Mailbox::Outbox => Decision::Cancel(Cancel::Queued),
            Mailbox::Reminders => Decision::Cancel(Cancel::Reminder),
            Mailbox::FollowUp => Decision::Cancel(Cancel::FollowUp),
            _ if mailbox.folder() == Some(Folder::Trash) => Decision::DeleteForever,
            _ => Decision::Triage(TriageAction::Trash),
        }),
        Action::Junk => Some(Decision::Triage(match mailbox.folder() {
            Some(Folder::Junk) => TriageAction::NotJunk,
            _ => TriageAction::Junk,
        })),
        Action::ToggleStar => Some(Decision::Flag(!marks.flagged)),
        Action::ToggleRead => Some(Decision::Triage(if marks.unread {
            TriageAction::MarkRead
        } else {
            TriageAction::MarkUnread
        })),
        _ => None,
    }
}

impl MainWindow {
    /// Runs what a mail button comes to on what `view` covers.
    pub(super) fn organize_from(self: &Rc<Self>, view: &Rc<ConversationView>, action: &Action) {
        let mailbox = self.mailbox_of(view);
        let Some(decision) = decide(action, &mailbox, self.target_marks_from(view)) else {
            return;
        };
        let targets = self.targets_from(view);
        if targets.is_empty() {
            return;
        }
        match decision {
            Decision::Triage(action) => {
                self.follow_out_from(view, &action);
                self.perform(targets, MailAction::Triage(action), History::Record, None);
            }
            Decision::Flag(on) => {
                self.flag_targets(targets, on.then(|| self.settings().flag_color))
            }
            Decision::DeleteForever => self.confirm_delete_forever(view, targets),
            Decision::Cancel(Cancel::Scheduled) => self.cancel_scheduled(targets),
            Decision::Cancel(Cancel::Queued) => self.drop_queued(),
            Decision::Cancel(Cancel::Reminder) => self.cancel_reminders(targets),
            Decision::Cancel(Cancel::FollowUp) => self.dismiss_follow_ups(targets),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailrs_domain::system_label;

    fn folder(folder: Folder) -> Mailbox {
        Mailbox::Folder {
            account_id: None,
            folder,
        }
    }

    fn inbox() -> Mailbox {
        Mailbox::Unified(system_label::INBOX)
    }

    fn decide_in(action: Action, mailbox: Mailbox) -> Option<Decision> {
        decide(&action, &mailbox, Marks::default())
    }

    #[test]
    fn delete_moves_mail_to_the_trash_and_erases_it_once_it_is_there() {
        assert_eq!(
            decide_in(Action::Trash, inbox()),
            Some(Decision::Triage(TriageAction::Trash))
        );
        assert_eq!(
            decide_in(Action::Trash, folder(Folder::AllMail)),
            Some(Decision::Triage(TriageAction::Trash))
        );
        assert_eq!(
            decide_in(Action::Trash, folder(Folder::Trash)),
            Some(Decision::DeleteForever)
        );
    }

    #[test]
    fn delete_calls_off_what_a_mailbox_of_something_else_holds() {
        let cancels = [
            (Mailbox::Scheduled, Cancel::Scheduled),
            (Mailbox::Outbox, Cancel::Queued),
            (Mailbox::Reminders, Cancel::Reminder),
            (Mailbox::FollowUp, Cancel::FollowUp),
        ];
        for (mailbox, cancel) in cancels {
            assert_eq!(
                decide_in(Action::Trash, mailbox),
                Some(Decision::Cancel(cancel))
            );
        }
    }

    #[test]
    fn junk_sends_mail_back_out_of_the_junk_folder() {
        assert_eq!(
            decide_in(Action::Junk, inbox()),
            Some(Decision::Triage(TriageAction::Junk))
        );
        assert_eq!(
            decide_in(Action::Junk, folder(Folder::Junk)),
            Some(Decision::Triage(TriageAction::NotJunk))
        );
        assert_eq!(
            decide_in(Action::Junk, folder(Folder::Trash)),
            Some(Decision::Triage(TriageAction::Junk))
        );
    }

    #[test]
    fn archive_means_the_same_in_every_mailbox() {
        for mailbox in [inbox(), folder(Folder::Trash), Mailbox::Reminders] {
            assert_eq!(
                decide_in(Action::Archive, mailbox),
                Some(Decision::Triage(TriageAction::Archive))
            );
        }
    }

    #[test]
    fn the_star_and_read_buttons_read_the_marks() {
        let marks = |unread, flagged| Marks { unread, flagged };
        let star = |marks| decide(&Action::ToggleStar, &inbox(), marks);
        assert_eq!(star(marks(false, false)), Some(Decision::Flag(true)));
        assert_eq!(star(marks(false, true)), Some(Decision::Flag(false)));
        let read = |marks| decide(&Action::ToggleRead, &inbox(), marks);
        assert_eq!(
            read(marks(true, false)),
            Some(Decision::Triage(TriageAction::MarkRead))
        );
        assert_eq!(
            read(marks(false, false)),
            Some(Decision::Triage(TriageAction::MarkUnread))
        );
    }

    #[test]
    fn an_action_that_changes_no_mail_decides_nothing() {
        assert_eq!(decide_in(Action::EditDraft, inbox()), None);
        assert_eq!(decide_in(Action::Translate, folder(Folder::Trash)), None);
    }
}
