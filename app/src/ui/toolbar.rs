//! Which buttons the conversation's header bar shows, and how they group
//! into capsules. The header packs one linked box per capsule in the
//! order of [`CAPSULES`] and hides a capsule whose buttons are all
//! hidden, so this module decides everything the header shows and the
//! widgets only follow it.
//!
//! The mockup (`mockups.py`'s `capsule()`) draws four groups: replying,
//! filing, Move with its chevron, then the flag with its chevron. It has
//! no Read button; `U`, the More menu and the message menu keep marking a
//! message read or unread. [`Slot::Read`] stays in the enum for those
//! menus to name, but [`shows`] never shows it here.

/// One button of the header bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    Reply,
    ReplyAll,
    Forward,
    Edit,
    Archive,
    Trash,
    Junk,
    Read,
    Flag,
    Labels,
    Tags,
}

impl Slot {
    pub const ALL: [Slot; 11] = [
        Slot::Reply,
        Slot::ReplyAll,
        Slot::Forward,
        Slot::Edit,
        Slot::Archive,
        Slot::Trash,
        Slot::Junk,
        Slot::Read,
        Slot::Flag,
        Slot::Labels,
        Slot::Tags,
    ];
}

/// What the conversation pane holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Holds {
    /// Nothing, or a queued message, whose card carries its own buttons.
    Nothing,
    /// Several conversations selected at once.
    Many,
    /// One conversation.
    Message,
    /// A draft, which Edit Draft opens.
    Draft,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct On {
    pub holds: Holds,
    /// A phone-width window, where the secondary buttons wait in More.
    pub compact: bool,
    /// A conversation in a window of its own. Labels stay with the main
    /// window, which has the list the label popover works on.
    pub detached: bool,
    /// The mail open comes from one account that keeps tags.
    pub tags: bool,
}

/// The capsules from the start of the header bar, in the mockup's order:
/// replying, filing, Move, then the flag. Edit Draft has one of its own,
/// which shows only where the replies do not.
pub const CAPSULES: [&[Slot]; 5] = [
    &[Slot::Reply, Slot::ReplyAll, Slot::Forward],
    &[Slot::Edit],
    &[Slot::Archive, Slot::Trash, Slot::Junk],
    &[Slot::Labels, Slot::Tags],
    &[Slot::Flag],
];

/// Whether the header shows `slot`.
pub fn shows(slot: Slot, on: On) -> bool {
    let any = matches!(on.holds, Holds::Many | Holds::Message | Holds::Draft);
    let wide = !on.compact;
    match slot {
        Slot::Reply => on.holds == Holds::Message,
        Slot::ReplyAll | Slot::Forward => on.holds == Holds::Message && wide,
        Slot::Edit => on.holds == Holds::Draft,
        Slot::Archive | Slot::Trash => any,
        Slot::Junk => any && wide,
        // The mockup gives Read no button of its own; U, the More menu
        // and the message menu still mark a message read or unread.
        Slot::Read => false,
        Slot::Flag => any && wide,
        Slot::Labels => any && wide && !on.detached,
        // Tags sit with the Labels button, in the same window, and show
        // only where the account keeps tags.
        Slot::Tags => any && wide && !on.detached && on.tags,
    }
}

/// Whether More Actions shows. Only one conversation has a menu of
/// actions; a draft has Edit, and a selection has the bulk page.
pub fn more_shows(on: On) -> bool {
    on.holds == Holds::Message
}

/// The capsules that show, each with the buttons in it that show.
pub fn groups(on: On) -> Vec<Vec<Slot>> {
    CAPSULES
        .iter()
        .map(|capsule| {
            capsule
                .iter()
                .copied()
                .filter(|&slot| shows(slot, on))
                .collect::<Vec<_>>()
        })
        .filter(|group| !group.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::Slot::*;
    use super::*;

    fn on(holds: Holds) -> On {
        On {
            holds,
            compact: false,
            detached: false,
            tags: false,
        }
    }

    #[test]
    fn a_message_groups_replies_then_filing_then_move_then_flag() {
        assert_eq!(
            groups(on(Holds::Message)),
            vec![
                vec![Reply, ReplyAll, Forward],
                vec![Archive, Trash, Junk],
                vec![Labels],
                vec![Flag],
            ]
        );
        assert!(more_shows(on(Holds::Message)));
    }

    #[test]
    fn a_draft_puts_edit_where_the_replies_were() {
        assert_eq!(
            groups(on(Holds::Draft)),
            vec![
                vec![Edit],
                vec![Archive, Trash, Junk],
                vec![Labels],
                vec![Flag],
            ]
        );
        assert!(!more_shows(on(Holds::Draft)));
    }

    #[test]
    fn a_narrow_window_keeps_reply_archive_and_trash() {
        let narrow = On {
            compact: true,
            ..on(Holds::Message)
        };
        assert_eq!(groups(narrow), vec![vec![Reply], vec![Archive, Trash]]);
        assert!(more_shows(narrow), "the rest waits in More");
    }

    #[test]
    fn several_conversations_take_no_replies_and_no_more_menu() {
        assert_eq!(
            groups(on(Holds::Many)),
            vec![vec![Archive, Trash, Junk], vec![Labels], vec![Flag]]
        );
        assert!(!more_shows(on(Holds::Many)));
    }

    #[test]
    fn a_conversation_in_its_own_window_has_no_labels_button() {
        let apart = On {
            detached: true,
            ..on(Holds::Message)
        };
        assert!(!shows(Labels, apart));
        assert_eq!(groups(apart).last(), Some(&vec![Flag]));
    }

    #[test]
    fn tags_join_the_filing_capsule_only_where_the_account_keeps_them() {
        let tagged = On {
            tags: true,
            ..on(Holds::Message)
        };
        assert_eq!(groups(tagged)[2], vec![Labels, Tags]);
        assert!(!shows(Tags, on(Holds::Message)));
        let apart = On {
            detached: true,
            ..tagged
        };
        assert!(!shows(Tags, apart), "tags stay with the main window");
        let narrow = On {
            compact: true,
            ..tagged
        };
        assert!(!shows(Tags, narrow));
    }

    #[test]
    fn nothing_open_shows_nothing() {
        assert!(groups(on(Holds::Nothing)).is_empty());
        assert!(!more_shows(on(Holds::Nothing)));
    }

    #[test]
    fn read_never_shows_in_the_header() {
        for holds in [Holds::Nothing, Holds::Many, Holds::Message, Holds::Draft] {
            assert!(!shows(Read, on(holds)));
        }
    }
}
