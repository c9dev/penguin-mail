//! An address field that holds its recipients as chips. What the writer
//! types becomes a chip on Enter, a comma, or as soon as focus leaves, so
//! a long list stays readable and a wrong address stands out in red.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, glib};
use mailrs_domain::Address;

use crate::compose::{format_recipients, is_address, parse_recipients};
use crate::ui::autocomplete::{self, Contacts};
use crate::ui::name;
use mailrs_domain::translate::{fill, gettext};

pub struct Recipients {
    /// The chips and the entry, wrapping onto as many lines as they need.
    pub field: gtk::FlowBox,
    pub entry: gtk::Entry,
    /// The entry's slot in the field, which outlives every rebuild.
    holder: gtk::FlowBoxChild,
    placeholder: String,
    addresses: RefCell<Vec<Address>>,
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
        false => fill(&gettext("{recipient}, not an address"), &[("recipient", &full)]),
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

    /// Puts the last chip back into the entry, for a quick correction.
    fn take_back(self: &Rc<Self>) {
        let Some(last) = self.addresses.borrow_mut().pop() else {
            return;
        };
        self.entry
            .set_text(&format_recipients(std::slice::from_ref(&last)));
        self.entry.set_position(-1);
        self.rebuild();
        self.announce();
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
            let weak = Rc::downgrade(self);
            remove.connect_clicked(move |_| {
                if let Some(field) = weak.upgrade() {
                    field.remove(index);
                }
            });
            chip.append(&remove);
            let holder = gtk::FlowBoxChild::builder()
                .child(&chip)
                .focusable(false)
                .build();
            name(&holder, &chip_name(&label, &address.email, valid));
            self.field.append(&holder);
        }
        self.field.append(&self.holder);
        if typing {
            self.entry.grab_focus();
        }
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
                    field.take_back();
                    glib::Propagation::Stop
                }
                gdk::Key::Tab | gdk::Key::ISO_Left_Tab => {
                    field.commit();
                    let step = if key == gdk::Key::Tab {
                        field.next.borrow()
                    } else {
                        field.previous.borrow()
                    };
                    match step.as_ref() {
                        Some(go) => {
                            go();
                            glib::Propagation::Stop
                        }
                        None => glib::Propagation::Proceed,
                    }
                }
                _ => glib::Propagation::Proceed,
            }
        });
        self.entry.add_controller(keys);

        let focus = gtk::EventControllerFocus::new();
        let weak = Rc::downgrade(self);
        focus.connect_leave(move |_| {
            if let Some(field) = weak.upgrade() {
                field.commit();
            }
        });
        self.entry.add_controller(focus);
    }
}

#[cfg(test)]
mod tests {
    use super::chip_name;

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
    fn a_chip_the_colour_marks_as_wrong_says_so() {
        assert_eq!(
            chip_name("not an email", "not an email", false),
            "not an email, not an address"
        );
    }
}
