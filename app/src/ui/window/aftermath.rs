//! What a mail action leaves stale in the window, and moving on from mail
//! that has left the list.
//!
//! The window's buttons, Undo, Delete Forever, the assistant and the
//! notification buttons all change mail, and each used to pick its own
//! refresh afterwards. [`Aftermath::of`] answers that once, with no widget
//! in sight, and [`MainWindow::after_mail`] carries it out. [`leaves`] says
//! whether an action takes mail out of a mailbox, and
//! [`MainWindow::move_on`] is the one way the window steps past it.

use std::rc::Rc;

use mailrs_domain::{FlagColor, Folder, Target, ThreadSummary, system_label};
use mailrs_sync::{History, MailAction, Outcome, TriageAction};

use super::MainWindow;
use crate::ui::Mailbox;
use crate::ui::conversation::ConversationView;

/// What changed the mail.
#[derive(Debug, Clone, Copy)]
pub(super) enum Cause<'a> {
    /// A button, the assistant, a notification or the thread run ran the
    /// action. `History::Skip` marks one taken on the reader's behalf,
    /// such as marking a thread read or cancelling a reminder.
    Did(&'a MailAction, History),
    /// Undo reversed an action. What it puts back reaches past the
    /// action's own targets' labels: earlier flag colours and reminders.
    Undid,
    /// Delete Forever erased the mail.
    Erased,
}

/// The flag colour a conversation on screen shows after the action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Flag {
    /// The action left flags alone.
    Keep,
    /// Show this colour, or no flag, on the conversations the action changed.
    Paint(Option<FlagColor>),
    /// Read every conversation's colour from the store again. Undo puts
    /// back colours from before, which the window never knew.
    Reread,
}

/// The parts of the window a mail action leaves stale. The store's change
/// events redraw the rows of a mailbox the store lists; this names the
/// rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Aftermath {
    /// Step past the mail now. Erased mail moves on only once Gmail has
    /// taken it, since a missing permission leaves every row in place.
    /// The rest move on before Gmail answers; see [`leaves`].
    pub move_on: bool,
    /// Take the changed rows out of the list at once. No change event
    /// names erased mail or a dismissed follow-up.
    pub drop_rows: bool,
    /// Ask which changed rows have left the Gmail folder on screen.
    pub prune: bool,
    /// Forget what Gmail answered and list the folder or smart mailbox on
    /// screen again. Mail put back can add rows only a fresh search shows.
    pub relist_remote: bool,
    /// Read the counts, the list and the open conversations again.
    pub refresh: bool,
    /// Read the sidebar counts again.
    pub counts: bool,
    pub flag: Flag,
    /// The Remind Me list and its count.
    pub reminders: bool,
    /// The Follow Up list and its count.
    pub follow_ups: bool,
}

impl Aftermath {
    const NOTHING: Aftermath = Aftermath {
        move_on: false,
        drop_rows: false,
        prune: false,
        relist_remote: false,
        refresh: false,
        counts: false,
        flag: Flag::Keep,
        reminders: false,
        follow_ups: false,
    };

    /// What `cause` leaves stale while `mailbox` is on screen, given what
    /// came of it.
    pub(super) fn of(cause: Cause, mailbox: &Mailbox, outcome: &Outcome) -> Aftermath {
        if outcome.done.is_empty() {
            return Aftermath::NOTHING;
        }
        match cause {
            Cause::Did(action, history) => Aftermath {
                prune: history == History::Record,
                relist_remote: history == History::Skip,
                refresh: matches!(action, MailAction::Flag(_)),
                flag: match action {
                    MailAction::Flag(color) => Flag::Paint(*color),
                    _ => Flag::Keep,
                },
                reminders: matches!(
                    action,
                    MailAction::Remind { .. } | MailAction::CancelReminder
                ),
                follow_ups: *action == MailAction::DismissFollowUp,
                drop_rows: *action == MailAction::DismissFollowUp,
                ..Aftermath::NOTHING
            },
            // The store's change events cover every mailbox the store
            // lists, so one reload is enough there; a Gmail folder needs a
            // fresh search instead.
            Cause::Undid => Aftermath {
                relist_remote: mailbox.is_remote(),
                counts: mailbox.is_remote(),
                refresh: !mailbox.is_remote(),
                flag: Flag::Reread,
                reminders: true,
                ..Aftermath::NOTHING
            },
            Cause::Erased => Aftermath {
                move_on: true,
                drop_rows: true,
                refresh: true,
                ..Aftermath::NOTHING
            },
        }
    }
}

/// Whether `action` takes its targets out of `mailbox`'s list.
pub(super) fn leaves(action: &MailAction, mailbox: &Mailbox) -> bool {
    match action {
        MailAction::Triage(triage) => leaves_list(mailbox, triage),
        MailAction::Mute { muted: true } => leaves_list(mailbox, &TriageAction::Mute),
        MailAction::Mute { muted: false } => leaves_list(mailbox, &TriageAction::Unmute),
        // A reminder archives the mail and lists it under Remind Me.
        MailAction::Remind { .. } => {
            *mailbox != Mailbox::Reminders && leaves_list(mailbox, &TriageAction::Archive)
        }
        MailAction::CancelReminder => *mailbox == Mailbox::Reminders,
        MailAction::DismissFollowUp => *mailbox == Mailbox::FollowUp,
        MailAction::Flag(_) | MailAction::Label { .. } => false,
    }
}

/// Whether `action` takes the targets out of `mailbox`'s list.
fn leaves_list(mailbox: &Mailbox, action: &TriageAction) -> bool {
    let folder = mailbox.folder();
    let listed = listed_label(mailbox);
    // A label change leaves the list that shows the label it takes away,
    // and the Archive once it puts the mail back in the inbox. Sent,
    // Starred and a search keep listing mail that only gained a label.
    let adds = |label: &str| {
        (folder == Some(Folder::Archive) && label == system_label::INBOX)
            || (label == system_label::TRASH && folder != Some(Folder::Trash))
            || (label == system_label::SPAM && folder != Some(Folder::Junk))
    };
    let removes = |label: &str| listed == Some(label);
    match action {
        TriageAction::AddLabel(label) => adds(label),
        TriageAction::RemoveLabel(label) => removes(label),
        TriageAction::Relabel { add, remove } => {
            add.iter().any(|l| adds(l))
                || remove.iter().any(|l| removes(l) && !add.contains(l))
        }
        TriageAction::Unstar => removes(system_label::STARRED),
        TriageAction::Archive => !matches!(folder, Some(Folder::AllMail | Folder::Archive)),
        TriageAction::Trash => folder != Some(Folder::Trash),
        TriageAction::Junk => folder != Some(Folder::Junk),
        TriageAction::Untrash => folder == Some(Folder::Trash),
        TriageAction::NotJunk => folder == Some(Folder::Junk),
        TriageAction::Mute => {
            !matches!(folder, Some(Folder::AllMail | Folder::Archive)) && !lists_muted(mailbox)
        }
        TriageAction::Unmute => lists_muted(mailbox),
        _ => false,
    }
}

/// The label a mailbox lists, when it lists one: the label itself, or
/// what the Trash and Junk folders stand for.
fn listed_label(mailbox: &Mailbox) -> Option<&str> {
    match mailbox {
        Mailbox::Unified(label) => Some(label),
        Mailbox::Label { label_id, .. } => Some(label_id),
        Mailbox::Folder {
            folder: Folder::Trash,
            ..
        } => Some(system_label::TRASH),
        Mailbox::Folder {
            folder: Folder::Junk,
            ..
        } => Some(system_label::SPAM),
        _ => None,
    }
}

/// Whether `mailbox` is the Muted list, unified or for one account.
fn lists_muted(mailbox: &Mailbox) -> bool {
    match mailbox {
        Mailbox::Unified(label) => *label == system_label::MUTE,
        Mailbox::Label { label_id, .. } => label_id == system_label::MUTE,
        _ => false,
    }
}

/// Whether `row` is mail `target` names: the thread, or its one message
/// when the target names one.
fn covers(target: &Target, row: &ThreadSummary) -> bool {
    target.account_id == row.account_id
        && target.thread_id == row.id
        && (target.message_id.is_none() || target.message_id == row.message_id)
}

impl MainWindow {
    /// Steps past the mail `view` shows. The main window opens the row
    /// after the selection, or the one before when nothing follows, and
    /// hides the conversation pane when the list has no other row. A
    /// conversation in a window of its own has nowhere to go, so the
    /// window closes.
    pub(super) fn move_on(self: &Rc<Self>, view: &ConversationView) {
        if view.detached() {
            return view.close_detached();
        }
        let next = self.list.neighbour_of_selected();
        self.conversation.leave();
        self.list.unselect();
        match next {
            Some(next) => self
                .list
                .select(next.account_id, &next.id, next.message_id.as_deref()),
            None => self.nav.set_show_content(false),
        }
    }

    /// Redraws what a mail action from the assistant or a notification
    /// changed.
    pub(super) fn mail_changed(self: &Rc<Self>, action: &MailAction, outcome: &Outcome) {
        self.after_mail(Cause::Did(action, History::Record), outcome, None);
    }

    /// Brings the window back in line after `cause`. `view` is the
    /// conversation the action was taken from, when there was one.
    pub(super) fn after_mail(
        self: &Rc<Self>,
        cause: Cause,
        outcome: &Outcome,
        view: Option<&ConversationView>,
    ) {
        let after = Aftermath::of(cause, &self.shown(), outcome);
        let done = &outcome.done;
        if after.move_on
            && let Some(view) = view
            && (view.detached() || self.selection_among(done))
        {
            self.move_on(view);
        }
        if after.drop_rows {
            self.list
                .retain(|row| !done.iter().any(|target| covers(target, row)));
        }
        if after.prune {
            self.prune_folder(done);
        }
        if after.relist_remote {
            self.core.forget_remote();
            self.reload_folder();
        }
        if after.refresh {
            self.queue_refresh();
        }
        if after.counts {
            self.refresh_counts();
        }
        match after.flag {
            Flag::Keep => {}
            Flag::Paint(color) => {
                for view in self.views() {
                    if view.read(|o| o.among(done)) == Some(true) {
                        view.set_flag_color(color);
                    }
                }
            }
            Flag::Reread => self.refresh_flag_color(),
        }
        if after.reminders {
            self.reminders_changed();
        }
        if after.follow_ups {
            self.follow_ups_changed();
        }
    }

    /// Whether a selected row is among `done`. The reader may have moved
    /// to other mail while Gmail answered, and then there is nothing to
    /// step past.
    fn selection_among(&self, done: &[Target]) -> bool {
        self.list
            .selected_rows()
            .iter()
            .any(|row| done.iter().any(|target| covers(target, row)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailrs_sync::Failure;

    fn inbox() -> Mailbox {
        Mailbox::Unified(system_label::INBOX)
    }

    fn folder(folder: Folder) -> Mailbox {
        Mailbox::Folder {
            account_id: None,
            folder,
        }
    }

    fn done() -> Outcome {
        Outcome {
            done: vec![Target::thread(1, "t1")],
            failed: vec![],
        }
    }

    fn after(cause: Cause, mailbox: Mailbox) -> Aftermath {
        Aftermath::of(cause, &mailbox, &done())
    }

    #[test]
    fn an_action_that_changed_nothing_leaves_nothing_stale() {
        let failed = Outcome {
            done: vec![],
            failed: vec![Failure {
                target: Target::thread(1, "t1"),
                error: "gone".into(),
            }],
        };
        let archive = MailAction::Triage(TriageAction::Archive);
        for cause in [
            Cause::Did(&archive, History::Record),
            Cause::Undid,
            Cause::Erased,
        ] {
            assert_eq!(Aftermath::of(cause, &inbox(), &failed), Aftermath::NOTHING);
        }
    }

    #[test]
    fn a_label_change_prunes_a_gmail_folder_and_leaves_the_rest_to_the_store() {
        let archive = MailAction::Triage(TriageAction::Archive);
        let after = after(Cause::Did(&archive, History::Record), inbox());
        assert_eq!(
            after,
            Aftermath {
                prune: true,
                ..Aftermath::NOTHING
            }
        );
    }

    #[test]
    fn mail_put_back_on_the_readers_behalf_lists_a_gmail_folder_again() {
        let cancel = MailAction::CancelReminder;
        let after = after(Cause::Did(&cancel, History::Skip), inbox());
        assert!(after.relist_remote);
        assert!(!after.prune);
        assert!(after.reminders);
    }

    #[test]
    fn a_flag_paints_its_colour_and_rereads_the_flag_mailboxes() {
        let red = MailAction::Flag(Some(FlagColor::Red));
        let after = after(Cause::Did(&red, History::Record), inbox());
        assert_eq!(after.flag, Flag::Paint(Some(FlagColor::Red)));
        assert!(after.refresh);
    }

    #[test]
    fn a_dismissed_follow_up_leaves_the_list_at_once() {
        let dismiss = MailAction::DismissFollowUp;
        let after = after(Cause::Did(&dismiss, History::Record), Mailbox::FollowUp);
        assert!(after.drop_rows);
        assert!(after.follow_ups);
        assert!(!after.move_on, "the caller moved on before the action");
    }

    #[test]
    fn undo_rereads_what_it_put_back_and_searches_a_gmail_folder_again() {
        let local = after(Cause::Undid, inbox());
        assert!(local.refresh && !local.relist_remote && !local.counts);
        let remote = after(Cause::Undid, folder(Folder::Trash));
        assert!(remote.relist_remote && remote.counts && !remote.refresh);
        for undone in [local, remote] {
            assert_eq!(undone.flag, Flag::Reread);
            assert!(undone.reminders);
        }
    }

    #[test]
    fn erased_mail_moves_on_and_goes_from_the_list_once_gmail_took_it() {
        let after = after(Cause::Erased, folder(Folder::Trash));
        assert!(after.move_on && after.drop_rows && after.refresh);
        assert!(!after.prune, "the rows go at once");
    }

    #[test]
    fn the_muted_list_is_the_one_named_by_the_mute_label() {
        assert!(lists_muted(&Mailbox::Unified(system_label::MUTE)));
        assert!(lists_muted(&Mailbox::Label {
            account_id: 1,
            label_id: system_label::MUTE.into(),
            name: "Muted".into(),
        }));
        assert!(!lists_muted(&inbox()));
        assert!(!lists_muted(&Mailbox::Reminders));
    }

    #[test]
    fn mail_leaves_a_list_only_when_the_action_takes_it_out_of_that_mailbox() {
        let all_mail = folder(Folder::AllMail);
        let triage = |action| MailAction::Triage(action);
        assert!(leaves(&triage(TriageAction::Archive), &inbox()));
        assert!(!leaves(&triage(TriageAction::Archive), &all_mail));
        assert!(!leaves(
            &triage(TriageAction::Archive),
            &folder(Folder::Archive)
        ));
        assert!(leaves(
            &triage(TriageAction::Trash),
            &folder(Folder::Archive)
        ));
        assert!(leaves(&triage(TriageAction::Trash), &all_mail));
        assert!(!leaves(&triage(TriageAction::MarkRead), &inbox()));
        assert!(!leaves(&MailAction::Mute { muted: true }, &all_mail));
        assert!(leaves(
            &MailAction::Mute { muted: false },
            &Mailbox::Unified(system_label::MUTE)
        ));
        assert!(!leaves(&MailAction::Flag(None), &inbox()));
    }

    #[test]
    fn a_label_change_leaves_only_the_list_of_the_label_it_takes_away() {
        let triage = |action| MailAction::Triage(action);
        let work = Mailbox::Label {
            account_id: 1,
            label_id: "Work".into(),
            name: "Work".into(),
        };
        let relabel = |add: &[&str], remove: &[&str]| {
            triage(TriageAction::Relabel {
                add: add.iter().map(|l| l.to_string()).collect(),
                remove: remove.iter().map(|l| l.to_string()).collect(),
            })
        };
        // Filed from Work into Travel: gone from Work.
        assert!(leaves(&relabel(&["Travel"], &["Work"]), &work));
        assert!(leaves(&relabel(&["Travel"], &["INBOX"]), &inbox()));
        assert!(!leaves(&relabel(&["Travel"], &["Work"]), &inbox()));
        // Out of Work into All Mail.
        assert!(leaves(&triage(TriageAction::RemoveLabel("Work".into())), &work));
        assert!(!leaves(&triage(TriageAction::RemoveLabel("Work".into())), &inbox()));
        // Sent and Starred keep mail that only gained a label.
        let sent = Mailbox::Unified(system_label::SENT);
        assert!(!leaves(&triage(TriageAction::AddLabel("Travel".into())), &sent));
        assert!(!leaves(&triage(TriageAction::AddLabel("INBOX".into())), &sent));
        // Back into the inbox from the Archive, and out of Junk.
        let archive = folder(Folder::Archive);
        assert!(leaves(&triage(TriageAction::AddLabel("INBOX".into())), &archive));
        assert!(leaves(&relabel(&[], &["SPAM"]), &folder(Folder::Junk)));
        assert!(!leaves(&triage(TriageAction::Star), &inbox()));
        assert!(leaves(
            &triage(TriageAction::Unstar),
            &Mailbox::Unified(system_label::STARRED)
        ));
    }

    #[test]
    fn a_reminder_takes_mail_out_of_every_list_but_its_own_and_all_mail() {
        let remind = MailAction::Remind { at: 1 };
        assert!(leaves(&remind, &inbox()));
        assert!(!leaves(&remind, &Mailbox::Reminders));
        assert!(!leaves(&remind, &folder(Folder::AllMail)));
        assert!(leaves(&MailAction::CancelReminder, &Mailbox::Reminders));
        assert!(leaves(&MailAction::DismissFollowUp, &Mailbox::FollowUp));
    }

    #[test]
    fn a_target_covers_its_thread_or_only_its_one_message() {
        let row = |message: Option<&str>| ThreadSummary {
            account_id: 1,
            id: "t1".into(),
            message_id: message.map(Into::into),
            ..ThreadSummary::default()
        };
        assert!(covers(&Target::thread(1, "t1"), &row(None)));
        assert!(covers(&Target::thread(1, "t1"), &row(Some("m1"))));
        let one = Target {
            message_id: Some("m1".into()),
            ..Target::thread(1, "t1")
        };
        assert!(covers(&one, &row(Some("m1"))));
        assert!(!covers(&one, &row(Some("m2"))));
        assert!(!covers(&Target::thread(2, "t1"), &row(None)));
    }
}
