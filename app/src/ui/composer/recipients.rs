//! An address field that holds its recipients as chips. What the writer
//! types becomes a chip on Enter, a comma, or as soon as focus leaves, so
//! a long list stays readable and a wrong address stands out in red.
//!
//! The chips are reachable from the keyboard without joining the Tab
//! chain: Left at the start of the entry steps onto the last chip, the
//! arrows move along them, and Delete or Backspace takes the one with the
//! focus away. See `roving` for the same idea on the formatting bar.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use mailrs_domain::Address;

use crate::compose::{is_address, parse_recipients};
use crate::ui::autocomplete::{self, Contacts};
use crate::ui::roving::{self, Move};
use crate::ui::{describe, name};
use mailrs_domain::translate::{fill, gettext};

pub struct Recipients {
    /// The chips and the entry, wrapping onto as many lines as they need.
    pub field: gtk::FlowBox,
    pub entry: gtk::Entry,
    /// The entry's slot in the field, which outlives every rebuild.
    holder: gtk::FlowBoxChild,
    placeholder: String,
    addresses: RefCell<Vec<Address>>,
    /// One slot per address, in the same order, rebuilt with them.
    chips: RefCell<Vec<gtk::FlowBoxChild>>,
    changed: RefCell<Option<Box<dyn Fn()>>>,
    /// Where Tab and Shift+Tab go from here. The field holds the focus
    /// itself, so it has to hand it on.
    next: RefCell<Option<Box<dyn Fn()>>>,
    previous: RefCell<Option<Box<dyn Fn()>>>,
}

/// What one chip says out loud: the name it shows, the address behind it
/// when they differ, and whether the address is one that could be sent to.
/// Red on the chip is the only sign of that on screen.
fn chip_name(shown: &str, email: &str, valid: bool) -> String {
    let full = match shown == email {
        true => shown.to_string(),
        false => fill(
            &gettext("{name}, {address}"),
            &[("name", shown), ("address", email)],
        ),
    };
    match valid {
        true => full,
        false => fill(
            &gettext("{recipient}, not an address"),
            &[("recipient", &full)],
        ),
    }
}

/// Which chip takes the focus once chip `index` of `before` is gone:
/// `None` means the entry. Backspace moves back to the chip before it and
/// Delete on to the one after, the way the two keys move through text.
fn after_removal(index: usize, before: usize, backspace: bool) -> Option<usize> {
    let left = before.saturating_sub(1);
    match (left, backspace) {
        (0, _) => None,
        (_, true) => Some(index.saturating_sub(1)),
        (_, false) => (index < left).then_some(index),
    }
}

impl Recipients {
    /// A field holding `addresses`, with `placeholder` shown while empty.
    pub fn new(placeholder: &str, addresses: &[Address], contacts: Contacts) -> Rc<Recipients> {
        let entry = gtk::Entry::builder()
            .placeholder_text(placeholder)
            .hexpand(true)
            .width_chars(18)
            .has_frame(false)
            .css_classes(["recipient-entry"])
            .build();
        let field = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .column_spacing(4)
            .row_spacing(4)
            .max_children_per_line(30)
            .min_children_per_line(1)
            .hexpand(true)
            .homogeneous(false)
            .build();
        // The placeholder is what the row is called; an entry's
        // placeholder is not its name, so it is given as one too.
        name(&entry, placeholder);
        autocomplete::attach(&entry, contacts);
        let holder = gtk::FlowBoxChild::builder()
            .child(&entry)
            .focusable(false)
            .build();
        let field = Rc::new(Recipients {
            field,
            entry,
            holder,
            placeholder: placeholder.to_string(),
            addresses: RefCell::new(addresses.to_vec()),
            chips: RefCell::new(Vec::new()),
            changed: RefCell::new(None),
            next: RefCell::new(None),
            previous: RefCell::new(None),
        });
        field.rebuild();
        field.wire();
        field
    }

    /// Calls `run` whenever the recipients change.
    pub fn on_change(self: &Rc<Self>, run: impl Fn() + 'static) {
        *self.changed.borrow_mut() = Some(Box::new(run));
    }

    /// Where Tab and Shift+Tab move to from this field.
    pub fn on_tab(self: &Rc<Self>, next: impl Fn() + 'static, previous: impl Fn() + 'static) {
        *self.next.borrow_mut() = Some(Box::new(next));
        *self.previous.borrow_mut() = Some(Box::new(previous));
    }

    /// The addresses in the field, including one being typed.
    pub fn addresses(&self) -> Vec<Address> {
        let mut list = self.addresses.borrow().clone();
        list.extend(parse_recipients(&self.entry.text()));
        list
    }

    pub fn is_empty(&self) -> bool {
        self.addresses.borrow().is_empty() && self.entry.text().trim().is_empty()
    }

    fn announce(&self) {
        if let Some(run) = self.changed.borrow().as_ref() {
            run();
        }
    }

    /// Turns what is typed into chips.
    fn commit(self: &Rc<Self>) {
        let text = self.entry.text();
        if text.trim().is_empty() {
            if !text.is_empty() {
                self.entry.set_text("");
            }
            return;
        }
        let found = parse_recipients(&text);
        if found.is_empty() {
            return;
        }
        self.addresses.borrow_mut().extend(found);
        self.entry.set_text("");
        self.rebuild();
        self.announce();
    }

    /// Deletes the last chip. Backspace in an empty entry means the same
    /// here as it does anywhere else: take the thing before the cursor
    /// away. Putting the address back into the entry instead left the
    /// chip gone and its text still in the field, where whatever was
    /// typed next ran into it.
    fn remove_last(self: &Rc<Self>) {
        let last = self.addresses.borrow().len().checked_sub(1);
        if let Some(index) = last {
            self.remove(index);
        }
    }

    fn remove(self: &Rc<Self>, index: usize) {
        let mut list = self.addresses.borrow_mut();
        if index < list.len() {
            list.remove(index);
        }
        drop(list);
        self.rebuild();
        self.announce();
    }

    /// Removes chip `index`. When it held the focus, the focus moves to
    /// its neighbour rather than falling out of the window.
    fn take_away(self: &Rc<Self>, index: usize, backspace: bool) {
        let count = self.chips.borrow().len();
        let focused = self.focused_chip();
        self.remove(index);
        // Every chip is built again, so whichever one held the focus, the
        // focus is put back by position.
        if let Some(at) = focused {
            self.focus_chip(match at.cmp(&index) {
                std::cmp::Ordering::Less => Some(at),
                std::cmp::Ordering::Equal => after_removal(index, count, backspace),
                std::cmp::Ordering::Greater => Some(at - 1),
            });
        }
    }

    /// Puts the focus on chip `index`, or in the entry at its start when
    /// there is no such chip.
    fn focus_chip(&self, index: Option<usize>) {
        let chip = index.and_then(|i| self.chips.borrow().get(i).cloned());
        match chip {
            Some(chip) => {
                chip.set_focusable(true);
                chip.grab_focus();
            }
            None => {
                self.entry.grab_focus();
                self.entry.set_position(0);
            }
        }
    }

    /// The chip holding the focus, if one does.
    fn focused_chip(&self) -> Option<usize> {
        self.chips.borrow().iter().position(|chip| chip.has_focus())
    }

    /// What a key does on a chip with the focus.
    fn chip_key(self: &Rc<Self>, index: usize, key: gdk::Key) -> glib::Propagation {
        let count = self.chips.borrow().len();
        let rtl = self.field.direction() == gtk::TextDirection::Rtl;
        if let Some(go) = Move::from_key(key, rtl) {
            // The entry is the last stop on the row, after every chip.
            if let Some(to) = roving::target(index, count + 1, go, |_| true) {
                self.focus_chip((to < count).then_some(to));
            }
            return glib::Propagation::Stop;
        }
        match key {
            gdk::Key::Delete | gdk::Key::KP_Delete | gdk::Key::BackSpace => {
                self.take_away(index, key == gdk::Key::BackSpace);
                glib::Propagation::Stop
            }
            gdk::Key::Tab | gdk::Key::ISO_Left_Tab => self.tab(key == gdk::Key::Tab),
            _ => glib::Propagation::Proceed,
        }
    }

    /// Hands the focus on to the field before or after this one.
    fn tab(&self, forward: bool) -> glib::Propagation {
        let step = match forward {
            true => self.next.borrow(),
            false => self.previous.borrow(),
        };
        match step.as_ref() {
            Some(go) => {
                go();
                glib::Propagation::Stop
            }
            None => glib::Propagation::Proceed,
        }
    }

    fn rebuild(self: &Rc<Self>) {
        // The placeholder only has to say what the row is for while it is
        // empty; chips say it afterwards.
        self.entry.set_placeholder_text(
            self.addresses
                .borrow()
                .is_empty()
                .then_some(self.placeholder.as_str()),
        );
        let typing = self
            .entry
            .state_flags()
            .contains(gtk::StateFlags::FOCUS_WITHIN);
        // The entry comes out whole, so the field can be built again around
        // it without the cursor losing its place.
        if self.holder.parent().is_some() {
            self.field.remove(&self.holder);
        }
        self.field.remove_all();
        let mut chips = Vec::new();
        for (index, address) in self.addresses.borrow().iter().enumerate() {
            let valid = is_address(&address.email);
            let chip = gtk::Box::builder()
                .spacing(4)
                .css_classes(if valid {
                    vec!["recipient-chip"]
                } else {
                    vec!["recipient-chip", "invalid"]
                })
                .build();
            let label = address
                .name
                .clone()
                .filter(|n| !n.trim().is_empty())
                .unwrap_or_else(|| address.email.clone());
            chip.append(
                &gtk::Label::builder()
                    .label(&label)
                    .tooltip_text(&address.email)
                    .ellipsize(gtk::pango::EllipsizeMode::Middle)
                    .max_width_chars(28)
                    .build(),
            );
            let remove = gtk::Button::builder()
                .icon_name("window-close-symbolic")
                .css_classes(["flat", "circular"])
                .tooltip_text(gettext("Remove"))
                .can_focus(false)
                .build();
            name(
                &remove,
                &fill(&gettext("Remove {recipient}"), &[("recipient", &label)]),
            );
            remove.set_action_name(Some("recipient.remove"));
            chip.append(&remove);
            // Out of the Tab chain until an arrow key reaches it, and back
            // out once the focus moves on, so Tab still takes one press
            // to cross the field.
            let holder = gtk::FlowBoxChild::builder()
                .child(&chip)
                .focusable(false)
                .build();
            describe(
                &holder,
                &chip_name(&label, &address.email, valid),
                &gettext("Press Delete to remove"),
            );
            let leave = gtk::EventControllerFocus::new();
            leave.connect_leave(|focus| {
                if let Some(chip) = focus.widget() {
                    chip.set_focusable(false);
                }
            });
            holder.add_controller(leave);
            // The chip carries its own Remove action, which a screen
            // reader offers on the chip itself, and the button runs it.
            let actions = gio::SimpleActionGroup::new();
            let action = gio::SimpleAction::new("remove", None);
            let weak = Rc::downgrade(self);
            action.connect_activate(move |_, _| {
                if let Some(field) = weak.upgrade() {
                    field.take_away(index, false);
                }
            });
            actions.add_action(&action);
            holder.insert_action_group("recipient", Some(&actions));
            self.field.append(&holder);
            chips.push(holder);
        }
        *self.chips.borrow_mut() = chips;
        self.field.append(&self.holder);
        if typing {
            self.entry.grab_focus();
        }
    }

    /// Whether `key`, pressed in the entry, moves the focus back onto the
    /// chips: the key toward the start of the line, with the cursor already
    /// there and nothing selected.
    fn leaves_for_chips(&self, key: gdk::Key) -> bool {
        let rtl = self.entry.direction() == gtk::TextDirection::Rtl;
        Move::from_key(key, rtl) == Some(Move::Back)
            && self.entry.position() == 0
            && self.entry.selection_bounds().is_none()
            && !self.chips.borrow().is_empty()
    }

    fn wire(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.entry.connect_activate(move |_| {
            if let Some(field) = weak.upgrade() {
                field.commit();
            }
        });
        // A picked suggestion arrives with a comma after it.
        let weak = Rc::downgrade(self);
        self.entry.connect_changed(move |entry| {
            let Some(field) = weak.upgrade() else { return };
            if entry.text().trim_end().ends_with(',') {
                field.commit();
            }
            field.announce();
        });

        let keys = gtk::EventControllerKey::new();
        // Ahead of the entry's own text handling, which swallows
        // Backspace before a bubbling controller ever sees it. Every key
        // this does not claim is passed straight on.
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, key, _, _| {
            let Some(field) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            match key {
                gdk::Key::comma | gdk::Key::semicolon => {
                    field.commit();
                    glib::Propagation::Stop
                }
                gdk::Key::BackSpace if field.entry.text().is_empty() => {
                    field.remove_last();
                    glib::Propagation::Stop
                }
                gdk::Key::Tab | gdk::Key::ISO_Left_Tab => {
                    field.commit();
                    field.tab(key == gdk::Key::Tab)
                }
                _ if field.leaves_for_chips(key) => {
                    // What is typed becomes a chip first, so the chip the
                    // focus lands on is the one just written.
                    field.commit();
                    let last = field.chips.borrow().len().checked_sub(1);
                    field.focus_chip(last);
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        });
        self.entry.add_controller(keys);

        // Keys on a chip. The flow box sees them before the chip does, and
        // lets every key through that lands anywhere else.
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, key, _, modifiers| {
            let Some(field) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            let Some(index) = field.focused_chip() else {
                return glib::Propagation::Proceed;
            };
            if modifiers.intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK) {
                return glib::Propagation::Proceed;
            }
            field.chip_key(index, key)
        });
        self.field.add_controller(keys);

        let focus = gtk::EventControllerFocus::new();
        let weak = Rc::downgrade(self);
        focus.connect_leave(move |_| {
            if let Some(field) = weak.upgrade() {
                field.commit();
            }
        });
        self.entry.add_controller(focus);

        // The flow box takes the press before the entry inside it ever
        // sees one, so clicking the row left the focus wherever it was:
        // click the body, click To, and the typing still went to the
        // body. Every click on the row puts the cursor in the entry,
        // which is also what clicking the empty space beside the chips
        // should do.
        let click = gtk::GestureClick::new();
        let weak = Rc::downgrade(self);
        click.connect_pressed(move |_, _, _, _| {
            if let Some(field) = weak.upgrade() {
                field.entry.grab_focus();
            }
        });
        self.field.add_controller(click);
    }
}

#[cfg(test)]
mod tests {
    use super::{after_removal, chip_name};

    #[test]
    fn a_chip_says_the_address_behind_the_name_it_shows() {
        assert_eq!(
            chip_name("Ann Lee", "ann@example.com", true),
            "Ann Lee, ann@example.com"
        );
        assert_eq!(
            chip_name("bo@example.com", "bo@example.com", true),
            "bo@example.com"
        );
    }

    #[test]
    fn backspace_moves_back_a_chip_and_delete_moves_on() {
        // Removing the third of five chips.
        assert_eq!(after_removal(2, 5, true), Some(1));
        assert_eq!(after_removal(2, 5, false), Some(2));
    }

    #[test]
    fn removing_at_either_end_stays_on_the_chips_while_there_are_any() {
        assert_eq!(after_removal(0, 3, true), Some(0));
        assert_eq!(after_removal(2, 3, true), Some(1));
        // Delete on the last chip has nothing after it but the entry.
        assert_eq!(after_removal(2, 3, false), None);
        assert_eq!(after_removal(0, 1, true), None);
        assert_eq!(after_removal(0, 1, false), None);
    }

    #[test]
    fn a_chip_the_colour_marks_as_wrong_says_so() {
        assert_eq!(
            chip_name("not an email", "not an email", false),
            "not an email, not an address"
        );
    }
}
