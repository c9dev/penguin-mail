//! Saved choices and visibility of the conversation toolbar.

pub mod customize;

use serde::{Deserialize, Serialize};

/// One button of the header bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
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
    Print,
    Source,
}

impl Slot {
    pub const ALL: [Slot; 13] = [
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
        Slot::Print,
        Slot::Source,
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
    /// One message from its own bytes, in no mailbox: an `.eml` file or
    /// a message attached to another. Only the replies apply to it, and
    /// only while there is an account to answer from.
    Loose { answerable: bool },
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
    let replies = matches!(on.holds, Holds::Message | Holds::Loose { answerable: true });
    match slot {
        Slot::Reply => replies,
        Slot::ReplyAll | Slot::Forward => replies && wide,
        Slot::Edit => on.holds == Holds::Draft,
        Slot::Archive | Slot::Trash => any,
        Slot::Junk => any && wide,
        Slot::Read => any && wide,
        Slot::Print | Slot::Source => {
            wide && matches!(
                on.holds,
                Holds::Message | Holds::Draft | Holds::Loose { .. }
            )
        }
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
    matches!(on.holds, Holds::Message | Holds::Loose { .. })
}

/// Saved layouts keep the first occurrence of each button.
pub fn layout(saved: Option<&[Slot]>) -> Vec<Slot> {
    let mut slots = Vec::new();
    for &slot in saved.unwrap_or(DEFAULT) {
        if !slots.contains(&slot) {
            slots.push(slot);
        }
    }
    slots
}

pub const DEFAULT: &[Slot] = &[
    Slot::Reply,
    Slot::ReplyAll,
    Slot::Forward,
    Slot::Edit,
    Slot::Archive,
    Slot::Trash,
    Slot::Junk,
    Slot::Labels,
    Slot::Tags,
    Slot::Flag,
];

/// Adjacent buttons from the same family share a capsule.
pub fn capsules(saved: Option<&[Slot]>) -> Vec<Vec<Slot>> {
    let family = |slot: Slot| CAPSULES.iter().position(|group| group.contains(&slot));
    let mut groups: Vec<Vec<Slot>> = Vec::new();
    for slot in layout(saved) {
        if let Some(last) = groups.last_mut()
            && family(slot).is_some()
            && family(last[0]) == family(slot)
        {
            last.push(slot);
        } else {
            groups.push(vec![slot]);
        }
    }
    groups
}

#[cfg(test)]
fn groups(on: On) -> Vec<Vec<Slot>> {
    capsules(None)
        .into_iter()
        .map(|group| {
            group
                .into_iter()
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
    fn a_loose_message_keeps_only_the_replies() {
        let loose = On {
            detached: true,
            ..on(Holds::Loose { answerable: true })
        };
        assert_eq!(groups(loose), vec![vec![Reply, ReplyAll, Forward]]);
        assert!(more_shows(loose));
        let narrow = On {
            compact: true,
            ..loose
        };
        assert_eq!(groups(narrow), vec![vec![Reply]]);
    }

    /// With no account there is nothing to answer from, so the replies go
    /// too and only the More menu stays.
    #[test]
    fn a_loose_message_with_no_account_has_no_replies() {
        let alone = On {
            detached: true,
            ..on(Holds::Loose { answerable: false })
        };
        assert!(groups(alone).is_empty());
        assert!(more_shows(alone));
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
    fn custom_order_can_include_read_print_and_source_without_duplicates() {
        assert_eq!(
            layout(Some(&[Print, Read, Source, Print])),
            [Print, Read, Source]
        );
        assert!(layout(Some(&[])).is_empty());
        assert_eq!(layout(None), DEFAULT);
        for slot in [Print, Source] {
            assert!(shows(slot, on(Holds::Message)));
            assert!(shows(slot, on(Holds::Loose { answerable: false })));
            assert!(!shows(slot, on(Holds::Many)));
            assert!(!shows(
                slot,
                On {
                    compact: true,
                    ..on(Holds::Message)
                }
            ));
        }
        assert!(!shows(Read, on(Holds::Loose { answerable: true })));
        assert_eq!(
            capsules(Some(&[Trash, Archive, Print, ReplyAll, Reply])),
            [vec![Trash, Archive], vec![Print], vec![ReplyAll, Reply]]
        );
    }

    #[test]
    fn nothing_open_shows_nothing() {
        assert!(groups(on(Holds::Nothing)).is_empty());
        assert!(!more_shows(on(Holds::Nothing)));
    }

    #[test]
    fn read_can_be_added_to_the_header() {
        for holds in [Holds::Nothing, Holds::Many, Holds::Message, Holds::Draft] {
            assert_eq!(shows(Read, on(holds)), holds != Holds::Nothing);
        }
    }
}
