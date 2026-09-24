//! What an action taken from one conversation view reaches.
//!
//! The main window's buttons and keys act on the selected rows, or on the
//! open conversation when one row or none is selected. A conversation in a
//! window of its own has no list behind it and acts on itself, in the
//! mailbox it was opened from. [`Reach::new`] holds that rule with no
//! widget in sight; [`MainWindow::reach`] feeds it what a view shows.

use mailrs_domain::{Target, ThreadSummary};

use super::MainWindow;
use super::triage::Marks;
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
