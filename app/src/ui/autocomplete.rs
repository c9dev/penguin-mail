//! Recipient suggestions under the composer's address fields. Typing shows
//! matching correspondents; arrows move, Enter or Tab picks, Esc closes.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, glib, pango};
use mailrs_store::contacts::Contact;

use crate::compose::{format_recipients, parse_recipients};
use crate::contacts::{current_token, suggest};

/// Shared, replaceable list of known correspondents.
pub type Contacts = Rc<RefCell<Rc<Vec<Contact>>>>;

const SHOWN: usize = 6;

struct Completion {
    entry: gtk::Entry,
    popover: gtk::Popover,
    list: gtk::ListBox,
    contacts: Contacts,
    shown: RefCell<Vec<Contact>>,
}

/// Adds suggestions to `entry`.
pub fn attach(entry: &gtk::Entry, contacts: Contacts) {
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::Single)
        .css_classes(["navigation-sidebar"])
        .can_focus(false)
        .build();
    let scroller = gtk::ScrolledWindow::builder()
        .child(&list)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .max_content_height(320)
        .build();
    let popover = gtk::Popover::builder()
        .child(&scroller)
        .autohide(false)
        .has_arrow(false)
        .can_focus(false)
        .position(gtk::PositionType::Bottom)
        .halign(gtk::Align::Start)
        .css_classes(["recipient-suggestions"])
        .build();
    popover.set_parent(entry);
    let completion = Rc::new(Completion {
        entry: entry.clone(),
        popover,
        list,
        contacts,
        shown: RefCell::new(Vec::new()),
    });

    let weak = Rc::downgrade(&completion);
    entry.connect_changed(move |_| {
        if let Some(c) = weak.upgrade() {
            c.update();
        }
    });
    let weak = Rc::downgrade(&completion);
    completion.list.connect_row_activated(move |_, row| {
        if let Some(c) = weak.upgrade() {
            c.accept(row.index() as usize);
        }
    });

    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak = Rc::downgrade(&completion);
    keys.connect_key_pressed(move |_, key, _, _| {
        let Some(c) = weak.upgrade() else {
            return glib::Propagation::Proceed;
        };
        if !c.popover.is_visible() {
            return glib::Propagation::Proceed;
        }
        match key {
            gdk::Key::Down => c.step(1),
            gdk::Key::Up => c.step(-1),
            gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::Tab => {
                let index = c.list.selected_row().map_or(0, |r| r.index() as usize);
                c.accept(index);
            }
            gdk::Key::Escape => c.popover.popdown(),
            _ => return glib::Propagation::Proceed,
        }
        glib::Propagation::Stop
    });
    entry.add_controller(keys);

    let focus = gtk::EventControllerFocus::new();
    let weak = Rc::downgrade(&completion);
    focus.connect_leave(move |_| {
        if let Some(c) = weak.upgrade() {
            // A click on a suggestion moves focus first; let it land.
            let later = Rc::downgrade(&c);
            glib::timeout_add_local_once(std::time::Duration::from_millis(150), move || {
                if let Some(c) = later.upgrade()
                    && !focused(&c.entry)
                {
                    c.popover.popdown();
                }
            });
        }
    });
    entry.add_controller(focus);

    // The popover belongs to the entry; it must go before the entry does.
    let keep = Rc::clone(&completion);
    entry.connect_destroy(move |_| {
        keep.popover.unparent();
    });
}

impl Completion {
    fn update(&self) {
        let text = self.entry.text();
        if !focused(&self.entry) {
            self.popover.popdown();
            return;
        }
        let (start, token) = current_token(&text);
        let entered: Vec<String> = parse_recipients(&text[..start])
            .into_iter()
            .map(|a| a.email)
            .collect();
        let contacts = Rc::clone(&self.contacts.borrow());
        let found: Vec<Contact> = suggest(&contacts, token, &entered, SHOWN)
            .into_iter()
            .cloned()
            .collect();
        if found.is_empty() {
            self.popover.popdown();
            return;
        }
        self.list.remove_all();
        for contact in &found {
            self.list.append(&row(contact));
        }
        self.list.select_row(self.list.row_at_index(0).as_ref());
        *self.shown.borrow_mut() = found;
        self.list
            .set_size_request(self.entry.width().clamp(280, 460), -1);
        self.popover
            .set_pointing_to(Some(&gdk::Rectangle::new(0, 0, 1, self.entry.height())));
        self.popover.popup();
    }

    fn step(&self, delta: i32) {
        let count = self.shown.borrow().len() as i32;
        let current = self.list.selected_row().map_or(-1, |r| r.index());
        let next = (current + delta).clamp(0, count - 1);
        self.list.select_row(self.list.row_at_index(next).as_ref());
    }

    /// Replaces what is being typed with suggestion `index`.
    fn accept(&self, index: usize) {
        let Some(contact) = self.shown.borrow().get(index).cloned() else {
            return;
        };
        let text = self.entry.text().to_string();
        let (start, _) = current_token(&text);
        let head = text[..start].trim_end();
        let chosen = format_recipients(&[contact.address()]);
        let joined = if head.is_empty() {
            format!("{chosen}, ")
        } else {
            format!("{head} {chosen}, ")
        };
        self.popover.popdown();
        self.entry.set_text(&joined);
        self.entry.set_position(-1);
        self.entry.grab_focus_without_selecting();
    }
}

/// Whether typing goes to `entry`. Its inner text widget holds the focus.
fn focused(entry: &gtk::Entry) -> bool {
    entry.state_flags().contains(gtk::StateFlags::FOCUS_WITHIN)
}

fn row(contact: &Contact) -> gtk::ListBoxRow {
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .margin_top(4)
        .margin_bottom(4)
        .build();
    let name = contact.name.as_deref().unwrap_or(&contact.email);
    content.append(
        &gtk::Label::builder()
            .label(name)
            .xalign(0.0)
            .ellipsize(pango::EllipsizeMode::End)
            .css_classes(["heading"])
            .build(),
    );
    if contact.name.is_some() {
        content.append(
            &gtk::Label::builder()
                .label(&contact.email)
                .xalign(0.0)
                .ellipsize(pango::EllipsizeMode::End)
                .css_classes(["dim-label", "caption"])
                .build(),
        );
    }
    gtk::ListBoxRow::builder()
        .child(&content)
        .can_focus(false)
        .build()
}
