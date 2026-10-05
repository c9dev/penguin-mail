//! Recipient suggestions under the composer's address fields and the event
//! editor's Guests field. Typing shows matching correspondents; arrows
//! move, Enter or Tab picks, Esc closes. Each row says which accounts'
//! contacts hold the person, or that only mail found them.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, glib, pango};
use mailrs_domain::AccountId;
use mailrs_domain::translate::{fill, gettext};
use mailrs_store::contacts::Suggestion;

use crate::compose::{format_recipients, parse_recipients};
use crate::contacts::{cue, current_token, suggest};
use crate::format::{account_color_index, account_label};

/// Shared, replaceable list of people to suggest.
pub type Contacts = Rc<RefCell<Rc<Vec<Suggestion>>>>;

/// The account a message goes out from, which the field's owner keeps
/// current. Its contacts come first in the list.
pub type Sending = Rc<Cell<Option<AccountId>>>;

/// Runs with the field's text once a suggestion is picked, in place of
/// the field showing it.
type OnPick = dyn Fn(&str);

const SHOWN: usize = 6;

struct Completion {
    entry: gtk::Entry,
    popover: gtk::Popover,
    list: gtk::ListBox,
    contacts: Contacts,
    sending: Sending,
    shown: RefCell<Vec<Suggestion>>,
    on_pick: Option<Box<OnPick>>,
}

/// Adds suggestions to `entry`, for a message that goes out from the
/// account `sending` holds. A pick writes the address into the field,
/// followed by a comma, for the field to read.
pub fn attach(entry: &gtk::Entry, contacts: Contacts, sending: Sending) {
    attach_with(entry, contacts, sending, None);
}

/// Adds suggestions to `entry`, and hands the field's text with the
/// picked address in it to `on_pick` instead of writing it, for a field
/// that turns an address into something else at once, such as the event
/// editor's guest list.
pub fn attach_picking(entry: &gtk::Entry, contacts: Contacts, on_pick: impl Fn(&str) + 'static) {
    attach_with(entry, contacts, Sending::default(), Some(Box::new(on_pick)));
}

fn attach_with(
    entry: &gtk::Entry,
    contacts: Contacts,
    sending: Sending,
    on_pick: Option<Box<OnPick>>,
) {
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
        sending,
        shown: RefCell::new(Vec::new()),
        on_pick,
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
        let from = self.sending.get();
        let found: Vec<Suggestion> = suggest(&contacts, token, &entered, SHOWN, from)
            .into_iter()
            .cloned()
            .collect();
        if found.is_empty() {
            self.popover.popdown();
            return;
        }
        self.list.remove_all();
        for contact in &found {
            self.list.append(&row(contact, from));
        }
        self.list.select_row(self.list.row_at_index(0).as_ref());
        *self.shown.borrow_mut() = found;
        self.list
            .set_size_request(self.entry.width().clamp(360, 460), -1);
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

    /// Replaces what is being typed with suggestion `index`, or hands the
    /// result to `on_pick`.
    fn accept(&self, index: usize) {
        let Some(contact) = self.shown.borrow().get(index).cloned() else {
            return;
        };
        let joined = picked_text(&self.entry.text(), &format_recipients(&[contact.address()]));
        self.popover.popdown();
        match &self.on_pick {
            Some(on_pick) => on_pick(&joined),
            None => {
                self.entry.set_text(&joined);
                self.entry.set_position(-1);
            }
        }
        self.entry.grab_focus_without_selecting();
    }
}

/// The field's text once `chosen`, a formatted address, replaces the word
/// being typed in `text`, followed by a comma for the next one.
pub fn picked_text(text: &str, chosen: &str) -> String {
    let (start, _) = current_token(text);
    let head = text[..start].trim_end();
    if head.is_empty() { format!("{chosen}, ") } else { format!("{head} {chosen}, ") }
}

/// Whether typing goes to `entry`. Its inner text widget holds the focus.
fn focused(entry: &gtk::Entry) -> bool {
    entry.state_flags().contains(gtk::StateFlags::FOCUS_WITHIN)
}

/// One suggestion: the name, then the address with the account cue at the
/// end of the same line. A person without a name shows the address on top
/// and the cue alone below it.
fn row(contact: &Suggestion, from: Option<AccountId>) -> gtk::ListBoxRow {
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .margin_top(4)
        .margin_bottom(4)
        .build();
    let named = contact.name.as_deref().filter(|n| !n.trim().is_empty());
    content.append(
        &gtk::Label::builder()
            .label(named.unwrap_or(&contact.email))
            .xalign(0.0)
            .ellipsize(pango::EllipsizeMode::End)
            .css_classes(["heading"])
            .build(),
    );
    let detail = gtk::Box::builder().spacing(12).build();
    if named.is_some() {
        detail.append(
            &gtk::Label::builder()
                .label(&contact.email)
                .xalign(0.0)
                .hexpand(true)
                .ellipsize(pango::EllipsizeMode::End)
                .css_classes(["dim-label", "caption"])
                .build(),
        );
    }
    let shown = cue(&contact.accounts, from, account_label);
    let source = gtk::Box::builder()
        .spacing(4)
        .hexpand(named.is_none())
        .halign(gtk::Align::End)
        .css_classes(["suggestion-source"])
        .build();
    if let Some(all) = &shown.tooltip {
        source.set_tooltip_text(Some(all));
    }
    let dots = gtk::Box::builder()
        .spacing(2)
        .valign(gtk::Align::Center)
        .build();
    for account in &shown.dots {
        dots.append(
            &gtk::Box::builder()
                .valign(gtk::Align::Center)
                .css_classes([
                    "account-dot".to_string(),
                    format!("account-{}", account_color_index(*account)),
                ])
                .build(),
        );
    }
    if !shown.dots.is_empty() {
        source.append(&dots);
    }
    source.append(
        &gtk::Label::builder()
            .label(&shown.text)
            .ellipsize(pango::EllipsizeMode::Middle)
            .max_width_chars(24)
            // Held at its full width up to that cap, so the address
            // beside it gives way first.
            .width_chars(shown.text.chars().count().min(24) as i32)
            .css_classes(["dim-label", "caption"])
            .build(),
    );
    detail.append(&source);
    content.append(&detail);
    let row = gtk::ListBoxRow::builder()
        .child(&content)
        .can_focus(false)
        .build();
    let person = match named {
        Some(name) => fill(
            &gettext("{name}, {address}"),
            &[("name", name), ("address", &contact.email)],
        ),
        None => contact.email.clone(),
    };
    super::name(
        &row,
        &fill(
            &gettext("{person}, {source}"),
            &[("person", &person), ("source", &shown.spoken)],
        ),
    );
    row
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pick_replaces_the_word_being_typed() {
        assert_eq!(picked_text("Lo", "Love <love@example.com>"), "Love <love@example.com>, ");
    }

    #[test]
    fn a_pick_keeps_the_addresses_before_it() {
        assert_eq!(
            picked_text("ann@example.com, Lo", "Love <love@example.com>"),
            "ann@example.com, Love <love@example.com>, "
        );
    }
}
