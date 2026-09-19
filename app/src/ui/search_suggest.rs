//! The suggestion list under the search field.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, glib, pango};

use super::autocomplete::Contacts;
use crate::search::{Suggestion, suggestions};

struct Suggest {
    entry: gtk::SearchEntry,
    popover: gtk::Popover,
    list: gtk::ListBox,
    contacts: Contacts,
    labels: Box<dyn Fn() -> Vec<String>>,
    shown: RefCell<Vec<Suggestion>>,
    /// Set once the arrows moved into the list, so Enter picks a row
    /// instead of running the plain search.
    chosen: Cell<bool>,
    /// Set while the entry text is being replaced by a pick.
    picking: Cell<bool>,
}

/// Suggests searches under `entry`. `labels` gives label names to offer.
pub fn attach(
    entry: &gtk::SearchEntry,
    contacts: Contacts,
    labels: impl Fn() -> Vec<String> + 'static,
) {
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::Single)
        .css_classes(["navigation-sidebar"])
        .can_focus(false)
        .build();
    let popover = gtk::Popover::builder()
        .child(&list)
        .autohide(false)
        .has_arrow(false)
        .can_focus(false)
        .position(gtk::PositionType::Bottom)
        .halign(gtk::Align::Start)
        .build();
    popover.set_parent(entry);
    let suggest = Rc::new(Suggest {
        entry: entry.clone(),
        popover,
        list,
        contacts,
        labels: Box::new(labels),
        shown: RefCell::new(Vec::new()),
        chosen: Cell::new(false),
        picking: Cell::new(false),
    });

    let weak = Rc::downgrade(&suggest);
    entry.connect_search_changed(move |_| {
        if let Some(s) = weak.upgrade() {
            s.update();
        }
    });
    let weak = Rc::downgrade(&suggest);
    suggest.list.connect_row_activated(move |_, row| {
        if let Some(s) = weak.upgrade() {
            s.pick(row.index() as usize);
        }
    });
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak = Rc::downgrade(&suggest);
    keys.connect_key_pressed(move |_, key, _, _| {
        let Some(s) = weak.upgrade() else {
            return glib::Propagation::Proceed;
        };
        if !s.popover.is_visible() {
            return glib::Propagation::Proceed;
        }
        match key {
            gdk::Key::Down => s.step(1),
            gdk::Key::Up => s.step(-1),
            gdk::Key::Return | gdk::Key::KP_Enter if s.chosen.get() => {
                let index = s.list.selected_row().map_or(0, |r| r.index() as usize);
                s.pick(index);
            }
            gdk::Key::Return | gdk::Key::KP_Enter => {
                s.popover.popdown();
                return glib::Propagation::Proceed;
            }
            gdk::Key::Escape => s.popover.popdown(),
            _ => return glib::Propagation::Proceed,
        }
        glib::Propagation::Stop
    });
    entry.add_controller(keys);
    let focus = gtk::EventControllerFocus::new();
    let weak = Rc::downgrade(&suggest);
    focus.connect_leave(move |_| {
        let later = weak.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(150), move || {
            if let Some(s) = later.upgrade()
                && !s
                    .entry
                    .state_flags()
                    .contains(gtk::StateFlags::FOCUS_WITHIN)
            {
                s.popover.popdown();
            }
        });
    });
    entry.add_controller(focus);
    let keep = Rc::clone(&suggest);
    entry.connect_destroy(move |_| keep.popover.unparent());
}

impl Suggest {
    fn update(&self) {
        if self.picking.get()
            || !self
                .entry
                .state_flags()
                .contains(gtk::StateFlags::FOCUS_WITHIN)
        {
            return self.popover.popdown();
        }
        let contacts = Rc::clone(&self.contacts.borrow());
        let found = suggestions(&self.entry.text(), &contacts, &(self.labels)());
        if found.is_empty() {
            return self.popover.popdown();
        }
        self.list.remove_all();
        for suggestion in &found {
            self.list.append(
                &gtk::ListBoxRow::builder()
                    .child(
                        &gtk::Label::builder()
                            .label(&suggestion.label)
                            .xalign(0.0)
                            .ellipsize(pango::EllipsizeMode::End)
                            .margin_top(4)
                            .margin_bottom(4)
                            .build(),
                    )
                    .can_focus(false)
                    .build(),
            );
        }
        self.list.unselect_all();
        self.chosen.set(false);
        *self.shown.borrow_mut() = found;
        self.list
            .set_size_request(self.entry.width().clamp(260, 480), -1);
        self.popover
            .set_pointing_to(Some(&gdk::Rectangle::new(0, 0, 1, self.entry.height())));
        self.popover.popup();
    }

    fn step(&self, delta: i32) {
        let count = self.shown.borrow().len() as i32;
        let current = self.list.selected_row().map_or(-1, |r| r.index());
        let next = (current + delta).clamp(0, count - 1);
        self.list.select_row(self.list.row_at_index(next).as_ref());
        self.chosen.set(true);
    }

    /// Runs suggestion `index` as the search.
    fn pick(&self, index: usize) {
        let Some(suggestion) = self.shown.borrow().get(index).cloned() else {
            return;
        };
        self.popover.popdown();
        self.picking.set(true);
        self.entry.set_text(&suggestion.query);
        self.entry.set_position(-1);
        self.picking.set(false);
        self.entry.emit_by_name::<()>("activate", &[]);
    }
}
