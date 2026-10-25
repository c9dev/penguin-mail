//! The banner at the top of the mail list that says what Online Accounts
//! needs for an account that reads through it. One widget, whatever the
//! notice: its words and buttons follow.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;

use crate::goa_notice::{Action, NoticeContent};

/// What the banner runs when one of its buttons is pressed.
type OnPress = Rc<RefCell<Option<Rc<dyn Fn(Action)>>>>;

pub struct Bar {
    root: gtk::Revealer,
    title: gtk::Label,
    buttons: gtk::Box,
    shown: Rc<RefCell<Option<NoticeContent>>>,
    on_press: OnPress,
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
    /// The same notice again changes nothing, so a focused button keeps
    /// the focus.
    pub fn show(&self, wanted: Option<NoticeContent>) {
        if *self.shown.borrow() == wanted {
            return;
        }
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

#[cfg(test)]
pub(crate) mod checks {
    use super::*;
    use crate::goa_notice::{Notice, shown};

    pub fn run() {
        a_removed_account_shows_its_button_and_keeps_one_widget();
        the_banner_hides_when_no_notice_is_left();
        a_press_carries_the_button_s_action();
    }

    fn removed() -> NoticeContent {
        shown(&Notice::Removed {
            address: "dana@example.com".into(),
        })
    }

    fn a_removed_account_shows_its_button_and_keeps_one_widget() {
        let bar = Bar::new();
        let before = bar.widget().clone();
        bar.show(Some(removed()));
        assert_eq!(bar.labels(), ["Add It Again"]);
        assert!(bar.is_shown());
        bar.show(Some(shown(&Notice::Unreachable)));
        assert!(bar.labels().is_empty());
        assert_eq!(&before, bar.widget());
    }

    fn the_banner_hides_when_no_notice_is_left() {
        let bar = Bar::new();
        bar.show(Some(removed()));
        bar.show(None);
        assert!(!bar.is_shown());
        assert!(bar.labels().is_empty());
    }

    fn a_press_carries_the_button_s_action() {
        let pressed: Rc<RefCell<Vec<Action>>> = Rc::default();
        let sink = Rc::clone(&pressed);
        let bar = Bar::new();
        bar.connect_press(move |action| sink.borrow_mut().push(action));
        bar.show(Some(removed()));
        let mut child = bar.buttons.last_child();
        while let Some(widget) = child {
            if let Some(button) = widget.downcast_ref::<gtk::Button>() {
                button.emit_clicked();
            }
            child = widget.prev_sibling();
        }
        assert_eq!(*pressed.borrow(), [Action::OpenOnlineAccounts]);
    }
}
