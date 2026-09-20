//! The protection card: what the engine made of a message, on a strip
//! above it.
//!
//! One card covers both standards, because a reader cares what happened to
//! the message rather than which of them carried it. It sits where the
//! event card sits and says the same kind of thing: one line for what
//! happened, one for how much it is worth. It has no buttons, because
//! nothing here is the reader's to answer.

use std::rc::Rc;

use adw::prelude::*;

use crate::pgp::{Mark, Tone};

pub struct PgpCard {
    pub widget: gtk::Box,
    icon: gtk::Image,
    title: gtk::Label,
    detail: gtk::Label,
    inside: gtk::Box,
}

impl PgpCard {
    pub fn new() -> Rc<PgpCard> {
        let icon = gtk::Image::builder().valign(gtk::Align::Start).build();
        let title = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .css_classes(["heading"])
            .build();
        let detail = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .css_classes(["dim-label", "caption"])
            .build();
        let lines = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(2)
            .hexpand(true)
            .build();
        lines.append(&title);
        lines.append(&detail);

        let inside = gtk::Box::builder()
            .spacing(12)
            .css_classes(["card", "pgp-card"])
            .build();
        inside.append(&icon);
        inside.append(&lines);

        let widget = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .visible(false)
            .css_classes(["pgp-area"])
            .build();
        widget.append(&inside);

        Rc::new(PgpCard {
            widget,
            icon,
            title,
            detail,
            inside,
        })
    }

    /// Puts one mark on the card and shows it.
    pub fn show(&self, mark: &Mark) {
        self.title.set_text(&mark.title);
        match &mark.detail {
            Some(detail) => {
                self.detail.set_text(detail);
                self.detail.set_visible(true);
            }
            None => self.detail.set_visible(false),
        }
        self.icon.set_icon_name(Some(icon(mark.tone)));
        self.inside
            .set_css_classes(&["card", "pgp-card", tone(mark.tone)]);
        self.widget.set_visible(true);
    }

    pub fn hide(&self) {
        self.widget.set_visible(false);
    }
}

/// The colour the card takes. A signature nothing could check is a
/// different thing from one that failed, so the two never look alike.
fn tone(tone: Tone) -> &'static str {
    match tone {
        Tone::Good => "good",
        Tone::Bad => "bad",
        Tone::Unchecked => "unchecked",
    }
}

fn icon(tone: Tone) -> &'static str {
    match tone {
        Tone::Good => "channel-secure-symbolic",
        Tone::Bad => "dialog-warning-symbolic",
        Tone::Unchecked => "channel-insecure-symbolic",
    }
}
