//! Roving focus: a row of controls that is one stop on the Tab chain, with
//! the arrow keys moving between its members. The formatting bar works
//! this way, so a writer tabbing from the Subject to the body passes the
//! bar once rather than eleven times.
//!
//! Only the member that last held the focus can take it from Tab; the
//! others stay out of the chain until an arrow key reaches them.

use std::cell::Cell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, glib};

/// Where an arrow key asks the focus to go within a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Move {
    Back,
    Forward,
    First,
    Last,
}

impl Move {
    /// The move a key asks for. In a right-to-left language the row runs
    /// the other way, so Left moves forward.
    pub fn from_key(key: gdk::Key, rtl: bool) -> Option<Move> {
        let (back, forward) = match rtl {
            false => (Move::Back, Move::Forward),
            true => (Move::Forward, Move::Back),
        };
        match key {
            gdk::Key::Left | gdk::Key::KP_Left => Some(back),
            gdk::Key::Right | gdk::Key::KP_Right => Some(forward),
            gdk::Key::Home | gdk::Key::KP_Home => Some(Move::First),
            gdk::Key::End | gdk::Key::KP_End => Some(Move::Last),
            _ => None,
        }
    }
}

/// The member `go` lands on from `from` in a row of `len`, passing over
/// the ones `usable` turns down. The row stops at its ends rather than
/// wrapping, so holding Left settles on the first member. `None` when
/// there is nowhere to go.
pub fn target(from: usize, len: usize, go: Move, usable: impl Fn(usize) -> bool) -> Option<usize> {
    let found = match go {
        Move::Back => (0..from.min(len)).rev().find(|&i| usable(i)),
        Move::Forward => (from + 1..len).find(|&i| usable(i)),
        Move::First => (0..len).find(|&i| usable(i)),
        Move::Last => (0..len).rev().find(|&i| usable(i)),
    };
    found.filter(|&i| i != from)
}

/// Whether `widget` can take the focus as things stand.
fn usable(widget: &gtk::Widget) -> bool {
    widget.is_visible() && widget.is_sensitive()
}

struct Toolbar {
    items: Vec<gtk::Widget>,
    current: Cell<usize>,
}

impl Toolbar {
    /// Hands the Tab stop to member `index` and puts the focus there.
    fn focus(&self, index: usize) {
        self.items[index].set_can_focus(true);
        self.items[index].grab_focus();
        self.settle(index);
    }

    /// Leaves member `index` as the only one Tab can reach.
    fn settle(&self, index: usize) {
        self.current.set(index);
        for (i, item) in self.items.iter().enumerate() {
            item.set_can_focus(i == index);
        }
    }

    /// The member holding the focus, if one does. A popover a member opens
    /// has a surface of its own, and the keys pressed in it are its own
    /// business even though they pass through the bar on their way.
    fn focused(&self, bar: &gtk::Widget) -> Option<usize> {
        let focus = bar.root()?.focus()?;
        if focus.native() != bar.native() {
            return None;
        }
        self.items
            .iter()
            .position(|item| focus == *item || focus.is_ancestor(item))
    }
}

/// Makes `items`, which sit inside `bar`, one stop on the Tab chain with
/// the arrow keys, Home and End moving between them. A click leaves the
/// focus where it was, so pressing Bold with the mouse keeps the cursor in
/// the text.
pub fn toolbar(bar: &impl IsA<gtk::Widget>, items: Vec<gtk::Widget>) {
    if items.is_empty() {
        return;
    }
    for item in &items {
        item.set_focus_on_click(false);
    }
    let toolbar = Rc::new(Toolbar {
        items,
        current: Cell::new(0),
    });
    toolbar.settle(0);

    let bar = bar.as_ref().clone();
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let (here, weak_bar) = (Rc::clone(&toolbar), bar.downgrade());
    keys.connect_key_pressed(move |_, key, _, modifiers| {
        let Some(bar) = weak_bar.upgrade() else {
            return glib::Propagation::Proceed;
        };
        if modifiers.intersects(
            gdk::ModifierType::CONTROL_MASK
                | gdk::ModifierType::ALT_MASK
                | gdk::ModifierType::SHIFT_MASK,
        ) {
            return glib::Propagation::Proceed;
        }
        let rtl = bar.direction() == gtk::TextDirection::Rtl;
        let (Some(go), Some(from)) = (Move::from_key(key, rtl), here.focused(&bar)) else {
            return glib::Propagation::Proceed;
        };
        let items = &here.items;
        if let Some(to) = target(from, items.len(), go, |i| usable(&items[i])) {
            here.focus(to);
        }
        // An arrow at the end of the row goes nowhere rather than out of
        // it, which is what GTK would do with the key otherwise.
        glib::Propagation::Stop
    });
    bar.add_controller(keys);

    // A member can come to hold the focus some other way, such as a menu
    // button taking it back when its menu closes. Whichever one has it is
    // the one Tab comes back to.
    let focus = gtk::EventControllerFocus::new();
    let (here, weak_bar) = (Rc::clone(&toolbar), bar.downgrade());
    focus.connect_enter(move |_| {
        if let Some(bar) = weak_bar.upgrade()
            && let Some(index) = here.focused(&bar)
            && index != here.current.get()
        {
            here.settle(index);
        }
    });
    bar.add_controller(focus);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(_: usize) -> bool {
        true
    }

    #[test]
    fn the_arrows_step_along_the_row_and_stop_at_its_ends() {
        assert_eq!(target(2, 5, Move::Forward, all), Some(3));
        assert_eq!(target(2, 5, Move::Back, all), Some(1));
        assert_eq!(target(4, 5, Move::Forward, all), None);
        assert_eq!(target(0, 5, Move::Back, all), None);
    }

    #[test]
    fn home_and_end_go_to_the_ends() {
        assert_eq!(target(2, 5, Move::First, all), Some(0));
        assert_eq!(target(2, 5, Move::Last, all), Some(4));
        assert_eq!(target(0, 5, Move::First, all), None);
    }

    #[test]
    fn a_member_that_cannot_take_the_focus_is_passed_over() {
        let not_two = |i: usize| i != 2;
        assert_eq!(target(1, 5, Move::Forward, not_two), Some(3));
        assert_eq!(target(3, 5, Move::Back, not_two), Some(1));
        let not_ends = |i: usize| i != 0 && i != 4;
        assert_eq!(target(2, 5, Move::First, not_ends), Some(1));
        assert_eq!(target(2, 5, Move::Last, not_ends), Some(3));
    }

    #[test]
    fn left_and_right_swap_in_a_right_to_left_language() {
        assert_eq!(Move::from_key(gdk::Key::Left, false), Some(Move::Back));
        assert_eq!(Move::from_key(gdk::Key::Left, true), Some(Move::Forward));
        assert_eq!(Move::from_key(gdk::Key::Home, true), Some(Move::First));
        assert_eq!(Move::from_key(gdk::Key::Tab, false), None);
    }
}
