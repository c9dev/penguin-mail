//! The banner at the top of the mail list that says what Online Accounts
//! needs for an account that reads through it. One widget, whatever the
//! notice: its words and buttons follow.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;

use crate::goa_notice::{Action, Shown};

pub struct Bar {
    root: gtk::Revealer,
    title: gtk::Label,
    buttons: gtk::Box,
    shown: Rc<RefCell<Option<Shown>>>,
    on_press: Rc<RefCell<Option<Rc<dyn Fn(Action)>>>>,
}

impl Bar {
    pub fn new() -> Bar {
        // The title holds an address, which may carry an "&".
        let title = gtk::Label::builder()
            .use_markup(false)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .hexpand(true)
            .xalign(0.0)
            .build();
        let buttons = gtk::Box::builder()
            .spacing(8)
            .valign(gtk::Align::Center)
            .build();
        let row = gtk::Box::builder()
            .spacing(12)
            .css_classes(["goa-banner"])
            .build();
        row.append(&title);
        row.append(&buttons);
        let root = gtk::Revealer::builder()
            .reveal_child(false)
            .child(&row)
            .build();
        Bar {
            root,
            title,
            buttons,
            shown: Rc::default(),
            on_press: Rc::default(),
        }
    }

    pub fn widget(&self) -> &gtk::Revealer {
        &self.root
    }

    /// Runs `on_press` with the action of the button pressed.
    pub fn connect_press(&self, on_press: impl Fn(Action) + 'static) {
        *self.on_press.borrow_mut() = Some(Rc::new(on_press));
    }

    /// Shows `wanted`, or hides the banner when nothing is left to say.
    pub fn show(&self, wanted: Option<Shown>) {
        while let Some(child) = self.buttons.first_child() {
            self.buttons.remove(&child);
        }
        if let Some(wanted) = &wanted {
            self.title.set_label(&wanted.title);
            for (i, (action, label)) in wanted.buttons.iter().enumerate() {
                let button = gtk::Button::builder().label(label).build();
                if i == 0 {
                    button.add_css_class("suggested-action");
                }
                let (action, on_press) = (*action, Rc::clone(&self.on_press));
                button.connect_clicked(move |_| {
                    let handler = on_press.borrow().clone();
                    if let Some(handler) = handler {
                        handler(action);
                    }
                });
                self.buttons.append(&button);
            }
        }
        self.root.set_reveal_child(wanted.is_some());
        self.shown.replace(wanted);
    }

    #[cfg(test)]
    pub fn labels(&self) -> Vec<String> {
        let mut found = Vec::new();
        let mut child = self.buttons.first_child();
        while let Some(widget) = child {
            if let Some(button) = widget.downcast_ref::<gtk::Button>() {
                found.push(button.label().map(|l| l.to_string()).unwrap_or_default());
            }
            child = widget.next_sibling();
        }
        found
    }

    #[cfg(test)]
    pub fn is_shown(&self) -> bool {
        self.root.reveals_child()
    }
}
