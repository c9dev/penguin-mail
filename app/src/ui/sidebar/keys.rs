//! The mailbox list's keys, worked out without a widget.
//!
//! The list is one Tab stop, as GNOME's sidebars are: Tab and Shift+Tab
//! leave it, the arrows move the focus between the rows a person can act
//! on, and moving opens nothing. Enter or Space opens the mailbox under
//! the focus, and the Menu key or Shift+F10 opens its menu. GTK's list
//! would select, and so load, every row the focus passed, and would take
//! one Tab per row, about 70 with six accounts.

use gtk::gdk;

/// What a key press in the mailbox list asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListKey {
    /// Leave the list, forward (true) or back.
    Leave { forward: bool },
    /// Move the focus this many reachable rows down, or up when negative.
    Step(i32),
    /// Open the mailbox under the focus.
    Open,
    /// Open the focused row's menu: its options button's, or the one a
    /// right click opens.
    Menu,
}

/// Where the keyboard focus sits when a key reaches the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// On a row of the list, or on a control inside one.
    InList,
    /// Inside a popover the list holds, such as a row's options menu or
    /// the menu a right click opens. Its keys reach the list's capture
    /// handler first, because the popover is a child of a row.
    InPopover,
}

/// What a key press asks of the list, given where the focus is. A menu
/// open over the list keeps its own keys, so its items can be walked.
pub fn route(key: gdk::Key, state: gdk::ModifierType, focus: Focus) -> Option<ListKey> {
    match focus {
        Focus::InPopover => None,
        Focus::InList => list_key(key, state),
    }
}

/// The list's own reading of `key`, or `None` for a key it leaves to
/// GTK and the window, such as Ctrl+Tab or a letter.
pub fn list_key(key: gdk::Key, state: gdk::ModifierType) -> Option<ListKey> {
    let chord = gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK | gdk::ModifierType::SUPER_MASK;
    if state.intersects(chord) {
        return None;
    }
    let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
    match key {
        gdk::Key::Menu => Some(ListKey::Menu),
        gdk::Key::F10 if shift => Some(ListKey::Menu),
        gdk::Key::Tab | gdk::Key::KP_Tab => Some(ListKey::Leave { forward: !shift }),
        gdk::Key::ISO_Left_Tab => Some(ListKey::Leave { forward: false }),
        gdk::Key::Down | gdk::Key::KP_Down => Some(ListKey::Step(1)),
        gdk::Key::Up | gdk::Key::KP_Up => Some(ListKey::Step(-1)),
        gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::ISO_Enter | gdk::Key::space | gdk::Key::KP_Space => {
            Some(ListKey::Open)
        }
        _ => None,
    }
}

/// The row the focus moves to from `from` by `step`, among rows whose
/// `reachable` flag says a person can act on them (a shown mailbox or
/// account heading, not a section title or a hidden row). Stays put at
/// either end.
pub fn step_to(reachable: &[bool], from: usize, step: i32) -> Option<usize> {
    let mut at = from;
    let mut left = step.unsigned_abs();
    while left > 0 {
        let next = if step > 0 { at.checked_add(1) } else { at.checked_sub(1) };
        let next = next.filter(|n| *n < reachable.len())?;
        at = next;
        if reachable[at] {
            left -= 1;
        }
    }
    (at != from).then_some(at)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_and_shift_tab_leave_the_list() {
        let none = gdk::ModifierType::empty();
        assert_eq!(list_key(gdk::Key::Tab, none), Some(ListKey::Leave { forward: true }));
        assert_eq!(
            list_key(gdk::Key::ISO_Left_Tab, gdk::ModifierType::SHIFT_MASK),
            Some(ListKey::Leave { forward: false })
        );
    }

    #[test]
    fn the_arrows_move_and_enter_or_space_opens() {
        let none = gdk::ModifierType::empty();
        assert_eq!(list_key(gdk::Key::Down, none), Some(ListKey::Step(1)));
        assert_eq!(list_key(gdk::Key::Up, none), Some(ListKey::Step(-1)));
        assert_eq!(list_key(gdk::Key::Return, none), Some(ListKey::Open));
        assert_eq!(list_key(gdk::Key::space, none), Some(ListKey::Open));
    }

    #[test]
    fn the_menu_key_and_shift_f10_open_the_rows_menu() {
        // The row's options button is no longer a Tab stop of its own, so
        // the keyboard reaches its menu this way.
        assert_eq!(list_key(gdk::Key::Menu, gdk::ModifierType::empty()), Some(ListKey::Menu));
        assert_eq!(list_key(gdk::Key::F10, gdk::ModifierType::SHIFT_MASK), Some(ListKey::Menu));
        assert_eq!(list_key(gdk::Key::F10, gdk::ModifierType::empty()), None);
    }

    #[test]
    fn a_chord_or_a_letter_is_left_to_the_window() {
        assert_eq!(list_key(gdk::Key::Tab, gdk::ModifierType::CONTROL_MASK), None);
        assert_eq!(list_key(gdk::Key::e, gdk::ModifierType::empty()), None);
    }

    #[test]
    fn keys_inside_an_open_menu_are_left_to_the_menu() {
        // A row's options menu is a popover inside the list, so its keys
        // pass the list's capture handler first. The arrows must move
        // between the menu's items, and Tab must stay in the menu.
        let none = gdk::ModifierType::empty();
        for key in [gdk::Key::Down, gdk::Key::Up, gdk::Key::Tab, gdk::Key::Return, gdk::Key::Menu] {
            assert_eq!(route(key, none, Focus::InPopover), None, "{key:?}");
        }
        assert_eq!(route(gdk::Key::Down, none, Focus::InList), Some(ListKey::Step(1)));
    }

    #[test]
    fn a_step_skips_section_titles_and_hidden_rows() {
        // All Inboxes, Flagged, "Mailboxes", Sent: the title is skipped.
        let rows = [true, true, false, true];
        assert_eq!(step_to(&rows, 1, 1), Some(3));
        assert_eq!(step_to(&rows, 3, -1), Some(1));
    }

    #[test]
    fn a_step_past_either_end_stays_put() {
        let rows = [false, true, true, false];
        assert_eq!(step_to(&rows, 2, 1), None);
        assert_eq!(step_to(&rows, 1, -1), None);
    }
}
