//! What an action taken from one conversation view reaches.
//!
//! The main window's buttons and keys act on the selected rows, or on the
//! open conversation when one row or none is selected. A conversation in a
//! window of its own has no list behind it and acts on itself, in the
//! mailbox it was opened from. [`Reach::new`] holds that rule with no
//! widget in sight; [`MainWindow::reach`] feeds it what a view shows.

use mailrs_domain::{AccountId, Target, ThreadSummary};

use super::MainWindow;
use super::triage::Marks;
use crate::offered::Filing;
use crate::open_thread::OpenThread;
use crate::ui::Mailbox;
use crate::ui::conversation::ConversationView;

/// The mail an action applies to and where it was picked from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Reach {
    /// The threads or messages the action changes. Empty when nothing is
    /// selected or open.
    pub targets: Vec<Target>,
    /// What the targets carry: any of them unread, all of them flagged.
    pub marks: Marks,
    /// Whether every target is muted. False when there are none.
    pub muted: bool,
    /// The mailbox the targets were picked from, which decides what
    /// Delete and Junk come to.
    pub mailbox: Mailbox,
}

/// The conversation a view has open, as far as [`Reach`] cares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Open {
    pub target: Target,
    pub marks: Marks,
    pub muted: bool,
}

impl Open {
    fn of(open: &OpenThread) -> Open {
        Open {
            target: open.target(),
            marks: Marks::from_open(open),
            muted: open.muted(),
        }
    }
}

impl Reach {
    /// Several selected rows win over the open conversation, which shows
    /// only one of them. With one row or none selected, the open
    /// conversation stands for the selection, and a lone row counts when
    /// nothing is open yet.
    pub(super) fn new(selected: &[ThreadSummary], open: Option<Open>, mailbox: Mailbox) -> Reach {
        if selected.len() > 1 {
            return Reach {
                targets: selected.iter().map(Target::from_row).collect(),
                marks: Marks {
                    unread: selected.iter().any(|r| r.unread),
                    flagged: selected.iter().all(|r| r.starred),
                },
                muted: selected.iter().all(|r| r.muted),
                mailbox,
            };
        }
        match open {
            Some(open) => Reach {
                targets: vec![open.target],
                marks: open.marks,
                muted: open.muted,
                mailbox,
            },
            None => Reach {
                targets: selected.iter().map(Target::from_row).collect(),
                marks: selected.first().map(Marks::from_row).unwrap_or_default(),
                muted: selected.first().is_some_and(|r| r.muted),
                mailbox,
            },
        }
    }
}

impl MainWindow {
    /// What an action on `view` reaches. A conversation in a window of its
    /// own ignores the main window's selection.
    pub(super) fn reach(&self, view: &ConversationView) -> Reach {
        let selected = match view.detached() {
            true => Vec::new(),
            false => self.list.selected_rows(),
        };
        Reach::new(&selected, view.read(Open::of), self.mailbox_of(view))
    }

    /// Words the Labels button of `view` the way the picker it opens words
    /// itself: for `accounts`, the ones an action on it reaches, or with
    /// none, for the accounts of the mailbox `view` was opened from.
    pub(super) fn word_filing(
        &self,
        view: &ConversationView,
        accounts: impl IntoIterator<Item = AccountId>,
    ) {
        let reached: Vec<AccountId> = accounts.into_iter().collect();
        let shown = self.accounts_of(&self.mailbox_of(view));
        view.set_filing(Filing::picker(&reached, &shown, |id| self.offers(id)));
    }

    /// Sets again what each conversation on screen offers, from what its
    /// accounts offer now. An account offers everything until it starts,
    /// so a gate set while it was starting may be out of date once it
    /// runs.
    pub(super) fn follow_gates(&self) {
        if let Some(account_id) = self.conversation.read(|o| o.account_id) {
            self.follow_sender_actions(account_id);
        }
        let shown = self.shown();
        let accounts: Vec<AccountId> = self
            .reach(&self.conversation)
            .targets
            .iter()
            .map(|t| t.account_id)
            .collect();
        let reached = match accounts.is_empty() {
            true => self.accounts_of(&shown),
            false => accounts.clone(),
        };
        self.word_buttons(&self.conversation, &shown, reached);
        self.word_filing(&self.conversation, accounts);
        // Collected first, so word_filing (which borrows `detached`
        // through mailbox_of) does not run while this loop holds it.
        let windows: Vec<_> = self
            .detached
            .borrow()
            .iter()
            .filter_map(|held| {
                let view = held.view.upgrade()?;
                Some((view, held.mailbox.clone(), held.account_id, held.actions.clone()))
            })
            .collect();
        for (view, mailbox, account_id, actions) in windows {
            self.word_buttons(&view, &mailbox, [account_id]);
            self.word_filing(&view, [account_id]);
            self.gate_window(&actions, &view, &mailbox, account_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::Standard;

    fn inbox() -> Mailbox {
        Mailbox::Unified(Standard::Inbox)
    }

    fn row(id: &str, unread: bool, starred: bool, muted: bool) -> ThreadSummary {
        ThreadSummary {
            account_id: 1,
            id: id.into(),
            unread,
            starred,
            muted,
            ..ThreadSummary::default()
        }
    }

    fn open(id: &str) -> Open {
        Open {
            target: Target::thread(1, id),
            marks: Marks {
                unread: true,
                flagged: true,
            },
            muted: true,
        }
    }

    #[test]
    fn several_selected_rows_win_over_the_open_conversation() {
        let rows = [row("a", false, true, true), row("b", true, false, true)];
        let reach = Reach::new(&rows, Some(open("a")), inbox());
        assert_eq!(
            reach.targets,
            vec![Target::thread(1, "a"), Target::thread(1, "b")]
        );
        assert_eq!(
            reach.marks,
            Marks {
                unread: true,
                flagged: false
            }
        );
        assert!(reach.muted);
    }

    #[test]
    fn the_open_conversation_stands_for_one_selected_row() {
        let rows = [row("a", false, false, false)];
        let reach = Reach::new(&rows, Some(open("a")), inbox());
        assert_eq!(reach.targets, vec![Target::thread(1, "a")]);
        assert_eq!(reach.marks, open("a").marks);
        assert!(reach.muted);
    }

    #[test]
    fn a_lone_row_counts_before_its_conversation_opens() {
        let rows = [row("a", true, true, true)];
        let reach = Reach::new(&rows, None, inbox());
        assert_eq!(reach.targets, vec![Target::thread(1, "a")]);
        assert_eq!(
            reach.marks,
            Marks {
                unread: true,
                flagged: true
            }
        );
        assert!(reach.muted);
    }

    #[test]
    fn nothing_selected_or_open_reaches_nothing() {
        let reach = Reach::new(&[], None, Mailbox::Scheduled);
        assert!(reach.targets.is_empty());
        assert_eq!(reach.marks, Marks::default());
        assert!(!reach.muted);
        assert_eq!(reach.mailbox, Mailbox::Scheduled);
    }
}
