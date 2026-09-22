//! The card above a queued message: when it goes, or why it has not gone
//! and when the next try is, with what the person can do about it. It
//! takes the translation card's shape and sits at the top of the pane.
//!
//! The buttons fire the window's own actions, so they act on what the
//! row menu acts on: the selected rows in the main window, or the message
//! in a window of its own. GTK greys one out wherever its action is off.

use adw::prelude::*;

use crate::open_thread::Unsent;
use mailrs_domain::translate::gettext;

pub struct QueuedCard {
    pub widget: gtk::Box,
    title: gtk::Label,
    detail: gtk::Label,
    /// Edit, Send Now and Delete, for a message in the Outbox.
    stuck: gtk::Box,
    /// Cancel Send, for a message waiting in Send Later.
    later: gtk::Button,
}

impl QueuedCard {
    pub fn new() -> QueuedCard {
        let icon = gtk::Image::builder()
            .icon_name("penguin-mail-outbox-symbolic")
            .valign(gtk::Align::Start)
            .build();
        let line = |classes: &[&str]| {
            gtk::Label::builder()
                .xalign(0.0)
                .wrap(true)
                .wrap_mode(gtk::pango::WrapMode::WordChar)
                .css_classes(classes.to_vec())
                .build()
        };
        let title = line(&["heading"]);
        let detail = line(&["dim-label", "caption"]);
        let lines = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(2)
            .hexpand(true)
            .build();
        lines.append(&title);
        lines.append(&detail);

        let button = |label: String, action: &str| {
            gtk::Button::builder()
                .label(label)
                .action_name(action)
                .valign(gtk::Align::Center)
                .build()
        };
        let stuck = gtk::Box::builder().spacing(6).margin_top(6).build();
        let send = button(gettext("Send Now"), "win.outbox-send");
        send.add_css_class("suggested-action");
        let delete = button(gettext("Delete"), "win.outbox-delete");
        delete.add_css_class("destructive-action");
        stuck.append(&button(gettext("Edit…"), "win.outbox-edit"));
        stuck.append(&send);
        stuck.append(&delete);
        // Delete in Send Later stops the message and leaves its draft in
        // Drafts, which is what this button says it does.
        let later = button(gettext("Cancel Send"), "win.trash");
        later.set_halign(gtk::Align::Start);
        later.set_margin_top(6);
        // The buttons go under the words, so a narrow pane wraps the words
        // and keeps every button whole.
        lines.append(&stuck);
        lines.append(&later);

        let inside = gtk::Box::builder()
            .spacing(12)
            .css_classes(["card", "queued-card"])
            .build();
        inside.append(&icon);
        inside.append(&lines);

        let widget = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .visible(false)
            .css_classes(["queued-area"])
            .build();
        widget.append(&inside);
        QueuedCard {
            widget,
            title,
            detail,
            stuck,
            later,
        }
    }

    pub fn show(&self, unsent: &Unsent) {
        self.title.set_text(&match unsent.stuck {
            true => gettext("This message has not been sent"),
            false => gettext("This message is waiting to be sent"),
        });
        self.detail.set_text(&unsent.line);
        self.stuck.set_visible(unsent.stuck);
        self.later.set_visible(!unsent.stuck);
        self.widget.set_visible(true);
    }

    pub fn hide(&self) {
        self.widget.set_visible(false);
    }
}
