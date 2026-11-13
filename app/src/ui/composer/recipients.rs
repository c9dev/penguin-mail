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
use mailrs_domain::{AccountId, Address};

use crate::compose::{is_address, parse_recipients};
use crate::ui::autocomplete::{self, Contacts, Sending};
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
    /// The account the message goes out from, whose contacts the
    /// suggestions put first.
    sending: Sending,
    /// Shared with the sibling fields of one composer, so a chip thrown
    /// from one of them can be caught by another. See `Moves`.
    moves: RefCell<Option<Rc<Moves>>>,
    /// Where a thrown chip rides under the pointer. Owned by the
    /// composer, which hands it to each of its rows.
    ghost_stage: RefCell<Option<gtk::Fixed>>,
}

/// What the three rows of one composer share: each other, and which
/// chip is currently under the pointer. A chip is thrown with an
/// ordinary pointer gesture rather than the drag-and-drop protocol,
/// because the move never leaves this window and Wayland compositors
/// have been measured killing the drag session at the moment of the
/// lift; the pointer is the one thing that cannot get lost. The entry
/// keeps its own drag-and-drop answer, so text dropped from outside
/// still lands in the row.
pub struct Moves {
    /// The chip being thrown, from the row it was lifted out of.
    moving: RefCell<Option<Moving>>,
    /// The three rows, so the release can ask which one stands under
    /// the pointer. Weak: the rows outlive the gesture, not the other
    /// way around.
    siblings: RefCell<Vec<std::rc::Weak<Recipients>>>,
}

impl Moves {
    pub fn new() -> Rc<Moves> {
        Rc::new(Moves {
            moving: RefCell::new(None),
            siblings: RefCell::new(Vec::new()),
        })
    }
}

/// The chip being thrown and the row it came from. The whole address
/// rides, not the chip's shown text, so a named address keeps its
/// name through the move.
pub struct Moving {
    address: Address,
    from: Rc<Recipients>,
}

/// The pill that rides under the pointer while a chip is thrown. The
/// Remove button stays home with the chip: its action would not resolve
/// out here, and a second unnamed button is noise on the accessibility
/// bus.
fn ghost_pill(address: &Address) -> gtk::Box {
    let shown = address
        .name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .unwrap_or(&address.email);
    let pill = gtk::Box::builder()
        .css_classes(["recipient-chip", "drag-ghost"])
        .build();
    pill.append(
        &gtk::Label::builder()
            .label(shown)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .max_width_chars(28)
            .build(),
    );
    pill
}

/// Which row of the book stands under the pointer at its offset from
/// the gesture's start, if any.
fn caught(
    gesture: &gtk::GestureDrag,
    offset_x: f64,
    offset_y: f64,
    moves: &Moves,
) -> Option<Rc<Recipients>> {
    let (x, y) = gesture.start_point()?;
    let widget = gesture.widget()?;
    let root = widget.root()?;
    let at = widget.compute_point(
        &root,
        &gtk::graphene::Point::new((x + offset_x) as f32, (y + offset_y) as f32),
    )?;
    moves
        .siblings
        .borrow()
        .iter()
        .filter_map(|s| s.upgrade())
        .find(|sibling| covers(&sibling.field, at.x(), at.y()))
}

/// Whether a point in window coordinates falls inside a row.
fn covers(field: &gtk::FlowBox, x: f32, y: f32) -> bool {
    let widget = field.upcast_ref::<gtk::Widget>();
    let Some(root) = widget.root() else {
        return false;
    };
    let Some(at) = widget.compute_point(&root, &gtk::graphene::Point::new(0., 0.)) else {
        return false;
    };
    x >= at.x()
        && x < at.x() + field.width() as f32
        && y >= at.y()
        && y < at.y() + field.height() as f32
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
            // Small enough to share a line with a chip in a 360 px
            // composer; it grows into whatever room the line has left.
            .width_chars(8)
            .max_width_chars(8)
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
        let sending = Sending::default();
        autocomplete::attach(&entry, contacts, Rc::clone(&sending));
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
            sending,
            moves: RefCell::new(None),
            ghost_stage: RefCell::new(None),
        });
        field.rebuild();
        field.wire();
        field
    }

    /// Puts the contacts of `account`, the one the message goes out from,
    /// first among the suggestions.
    pub fn set_from(&self, account: Option<AccountId>) {
        self.sending.set(account);
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

    /// Gives this row the place a thrown chip rides in under the
    /// pointer. Without one, a move still works; it just shows no ghost.
    pub fn set_ghost_stage(&self, stage: &gtk::Fixed) {
        *self.ghost_stage.borrow_mut() = Some(stage.clone());
    }

    /// Joins this row to the book its siblings share. A lone field, as
    /// in the tests, holds none and never takes part in a move.
    pub fn share_moves(self: &Rc<Self>, moves: &Rc<Moves>) {
        if !self.moves.borrow().iter().any(|held| Rc::ptr_eq(held, moves)) {
            moves.siblings.borrow_mut().push(Rc::downgrade(self));
        }
        *self.moves.borrow_mut() = Some(Rc::clone(moves));
    }

    /// Adds one recipient to the end of the field, as if it had been
    /// typed and committed.
    pub fn add(self: &Rc<Self>, address: Address) {
        self.addresses.borrow_mut().push(address);
        self.rebuild();
        self.announce();
    }

    /// Takes the first chip holding this address away, saying whether
    /// there was one. Focus is left where it was: a moved chip lands in
    /// the field under the pointer, not the one it started in.
    pub fn remove_address(self: &Rc<Self>, email: &str) -> bool {
        let at = self.addresses.borrow().iter().position(|a| a.email == email);
        let Some(index) = at else { return false };
        self.addresses.borrow_mut().remove(index);
        self.rebuild();
        self.announce();
        true
    }

    /// Moves the thrown chip into this field. Letting go on the row the
    /// chip came from is a quiet no-op, as in Mac's Mail. The gesture
    /// and the field's checks both come through here, so the
    /// arithmetic of a move is read once.
    fn land(self: &Rc<Self>, moving: Moving) -> bool {
        if Rc::ptr_eq(&moving.from, self) {
            return true;
        }
        moving.from.remove_address(&moving.address.email);
        self.add(moving.address);
        // Ask for both rows again outright. A rebuild usually damages
        // enough on its own, but a chip that left its row under a
        // gesture has been seen to keep its last pixels in the frame.
        moving.from.field.queue_draw();
        self.field.queue_draw();
        true
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
            // Lifting a chip and letting go over a sibling row moves
            // the recipient there.
            let drag = gtk::GestureDrag::new();
            let carried = address.clone();
            let owner = Rc::downgrade(self);
            let in_hand = chip.clone();
            let lit: Rc<RefCell<Option<Rc<Recipients>>>> = Rc::default();
            let held: Rc<RefCell<Option<gtk::Box>>> = Rc::default();
            let held_begin = Rc::clone(&held);
            let held_seen = Rc::clone(&held);
            let held_landed = Rc::clone(&held);
            let held_lost = Rc::clone(&held);
            drag.connect_drag_begin(move |gesture, _, _| {
                let Some(field) = owner.upgrade() else { return };
                let Some(moves) = field.moves.borrow().clone() else { return };
                *moves.moving.borrow_mut() = Some(Moving {
                    address: carried.clone(),
                    from: Rc::clone(&field),
                });
                in_hand.add_css_class("dragging");
                if let (Some(widget), Some(cursor)) =
                    (gesture.widget(), gdk::Cursor::from_name("dnd-move", None))
                {
                    widget.set_cursor(Some(&cursor));
                }
                if let Some(stage) = field.ghost_stage.borrow().clone() {
                    let ghost = ghost_pill(&carried);
                    ghost.set_can_target(false);
                    stage.put(&ghost, 0., 0.);
                    *held_begin.borrow_mut() = Some(ghost);
                }
            });
            let owner = Rc::downgrade(self);
            drag.connect_drag_update({
                let lit = Rc::clone(&lit);
                let held = held_seen;
                move |gesture, offset_x, offset_y| {
                    let Some(field) = owner.upgrade() else { return };
                    let Some(moves) = field.moves.borrow().clone() else { return };
                    // The ghost rides wherever the pointer goes, whether
                    // or not the row under it just changed.
                    if let (Some(stage), Some(ghost), Some(widget), Some((x0, y0))) = (
                        field.ghost_stage.borrow().as_ref(),
                        held.borrow().as_ref(),
                        gesture.widget(),
                        gesture.start_point(),
                    ) && let Some(under) = widget.compute_point(
                        stage,
                        &gtk::graphene::Point::new(offset_x as f32, offset_y as f32),
                    ) {
                        stage.move_(ghost, f64::from(under.x()) - x0, f64::from(under.y()) - y0);
                    }
                    let now = caught(gesture, offset_x, offset_y, &moves);
                    let changed = match (&*lit.borrow(), &now) {
                        (None, None) => false,
                        (Some(old), Some(new)) => !Rc::ptr_eq(old, new),
                        _ => true,
                    };
                    if !changed {
                        return;
                    }
                    if let Some(previous) = lit.borrow().as_ref() {
                        previous.field.remove_css_class("recipient-drop");
                    }
                    if let Some(next) = now.as_ref() {
                        next.field.add_css_class("recipient-drop");
                    }
                    *lit.borrow_mut() = now;
                }
            });
            let owner = Rc::downgrade(self);
            let at_home = chip.clone();
            let lit3 = Rc::clone(&lit);
            let carried_ghost = held_landed;
            drag.connect_drag_end(move |gesture, offset_x, offset_y| {
                if let Some(previous) = lit3.borrow_mut().take() {
                    previous.field.remove_css_class("recipient-drop");
                }
                at_home.remove_css_class("dragging");
                if let Some(widget) = gesture.widget() {
                    widget.set_cursor(None as Option<&gdk::Cursor>);
                }
                let Some(field) = owner.upgrade() else { return };
                // Above the click guard below: a jittered click that
                // never became a throw must not strand a ghost.
                if let (Some(stage), Some(ghost)) =
                    (field.ghost_stage.borrow().as_ref(), carried_ghost.borrow_mut().take())
                {
                    stage.remove(&ghost);
                }
                let Some(moves) = field.moves.borrow().clone() else { return };
                let Some(moving) = moves.moving.borrow_mut().take() else { return };
                // Under five pixels of travel the pointer was clicking
                // the chip, not throwing it at another row.
                if offset_x.hypot(offset_y) < 5. {
                    return;
                }
                // Claude Code's read of the stale chip pixels: the
                // widget that leaves is torn down inside its own
                // gesture's emission, in the frame that also hides the
                // overlay, and Wayland damage can miss it. Let the
                // gesture finish, then move in a later frame.
                if let Some(onto) = caught(gesture, offset_x, offset_y, &moves) {
                    glib::idle_add_local_once(move || {
                        onto.land(moving);
                    });
                }
            });
            let owner = Rc::downgrade(self);
            let lit2 = Rc::clone(&lit);
            let dimmed = chip.clone();
            let lost_ghost = held_lost;
            drag.connect_cancel(move |gesture, _| {
                if let Some(previous) = lit2.borrow_mut().take() {
                    previous.field.remove_css_class("recipient-drop");
                }
                dimmed.remove_css_class("dragging");
                if let Some(widget) = gesture.widget() {
                    widget.set_cursor(None as Option<&gdk::Cursor>);
                }
                if let Some(field) = owner.upgrade() {
                    if let (Some(stage), Some(ghost)) =
                        (field.ghost_stage.borrow().as_ref(), lost_ghost.borrow_mut().take())
                    {
                        stage.remove(&ghost);
                    }
                    if let Some(moves) = field.moves.borrow().clone() {
                        *moves.moving.borrow_mut() = None;
                    }
                }
            });
            holder.add_controller(drag);
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
            if modifiers.intersects(
                gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK | crate::ui::PRIMARY,
            ) {
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

/// The field's checks. They run from the one GTK test in `richbuffer`,
/// because GTK belongs to the thread that starts it and the test
/// harness gives each test a thread of its own.
#[cfg(test)]
pub(super) mod checks {
    use super::*;

    fn no_contacts() -> Contacts {
        Rc::new(RefCell::new(Rc::new(Vec::new())))
    }

    fn an_address(name: Option<&str>, email: &str) -> Address {
        Address {
            name: name.map(str::to_string),
            email: email.to_string(),
        }
    }

    /// Whether one widget carries the named controller, in the shape
    /// the codebase already reads a widget's controllers in.
    fn carries<C: glib::object::Cast>(widget: &impl IsA<gtk::Widget>) -> bool {
        let list = widget.observe_controllers();
        (0..list.n_items())
            .filter_map(|i| list.item(i))
            .any(|c| c.is::<C>())
    }

    fn a_chip_offers_itself_to_be_lifted() {
        let to = Recipients::new("To", &[an_address(None, "bea@example.com")], no_contacts());
        let chip = to.chips.borrow()[0].clone();
        assert!(
            carries::<gtk::GestureDrag>(&chip),
            "a chip that cannot be lifted is a chip that cannot move"
        );
    }

    fn a_moved_recipient_leaves_one_field_and_lands_in_the_other() {
        let to = Recipients::new(
            "To",
            &[
                an_address(Some("Bea"), "bea@example.com"),
                an_address(None, "hazel@example.com"),
            ],
            no_contacts(),
        );
        let cc = Recipients::new("Cc", &[], no_contacts());
        let moves = Moves::new();
        to.share_moves(&moves);
        cc.share_moves(&moves);
        assert_eq!(
            moves.siblings.borrow().len(),
            2,
            "a joined row failed to join the book"
        );
        // Lift Bea out of To. The throw itself is GTK's device work;
        // the checks drive what the release calls.
        let lifted = || Moving {
            address: to.addresses.borrow()[0].clone(),
            from: Rc::clone(&to),
        };
        // Letting go on the field the chip came from moves nothing and
        // says the chip went somewhere.
        assert!(to.land(lifted()), "the chip's own field refused it");
        assert_eq!(to.addresses().len(), 2, "a quiet drop moved a chip");
        assert!(cc.land(lifted()), "the chip never reached the Cc field");
        assert_eq!(to.addresses(), vec![an_address(None, "hazel@example.com")]);
        assert_eq!(cc.addresses(), vec![an_address(Some("Bea"), "bea@example.com")]);
        // A row that shares no book has no chip to catch.
        let lone = Recipients::new("To", &[], no_contacts());
        assert!(
            lone.moves.borrow().is_none(),
            "a lone row joined a book it was never given"
        );
    }

    pub fn run() {
        a_chip_offers_itself_to_be_lifted();
        a_moved_recipient_leaves_one_field_and_lands_in_the_other();
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
