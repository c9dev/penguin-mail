//! `Quick`, the small popover a drag across empty time or N opens: the
//! time, a title field with the cursor in it, the calendar it goes on,
//! and More Details for the editor. Enter saves.
//!
//! One popover serves the whole view, parented to the calendar's card
//! the way the event popover is (`popover.rs`'s own doc comment): a
//! reload or a carousel step can destroy the block or the day cell
//! `show` last pointed at, but never the card. `show` repositions and
//! refills the one popover rather than building a new one each time.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, graphene};
use mailrs_domain::calendar::Calendar;
use mailrs_domain::translate::gettext;

use super::tint;
use crate::ui::name;

type OnTitle = dyn Fn(String);

pub struct Quick {
    pub popover: gtk::Popover,
    /// The stable widget `show`'s bounds are computed against, and the
    /// popover is parented to.
    parent: gtk::Widget,
    time: gtk::Label,
    title: gtk::Entry,
    dot: gtk::Box,
    calendar_label: gtk::Label,
    save: gtk::Button,
    more: gtk::Button,
    on_save: RefCell<Option<Box<OnTitle>>>,
    on_more: RefCell<Option<Box<OnTitle>>>,
}

impl Quick {
    /// Builds the one popover this view will ever open, parented to
    /// `parent`, so it survives a reload; `show` repositions it at
    /// whichever slot a press or N marked.
    pub fn new(parent: &impl IsA<gtk::Widget>) -> Rc<Quick> {
        let time = gtk::Label::builder()
            .xalign(0.0)
            .css_classes(["dim-label", "caption"])
            .build();
        let title = gtk::Entry::builder()
            .placeholder_text(gettext("Add title"))
            .activates_default(false)
            .width_chars(26)
            .build();
        name(&title, &gettext("Title"));
        let dot = gtk::Box::builder()
            .css_classes(["checked-dot"])
            .valign(gtk::Align::Center)
            .build();
        let calendar_label = gtk::Label::builder()
            .css_classes(["dim-label", "caption"])
            .build();
        let where_to = gtk::Box::builder().spacing(6).build();
        where_to.append(&dot);
        where_to.append(&calendar_label);
        let more = gtk::Button::builder()
            .label(gettext("More Details"))
            .css_classes(["flat"])
            .build();
        let save = gtk::Button::builder()
            .label(gettext("Save"))
            .css_classes(["suggested-action"])
            .sensitive(false)
            .build();
        let buttons = gtk::Box::builder()
            .spacing(6)
            .halign(gtk::Align::End)
            .build();
        buttons.append(&more);
        buttons.append(&save);
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .margin_top(10)
            .margin_bottom(10)
            .margin_start(10)
            .margin_end(10)
            .build();
        for child in [
            time.upcast_ref::<gtk::Widget>(),
            title.upcast_ref(),
            where_to.upcast_ref(),
            buttons.upcast_ref(),
        ] {
            content.append(child);
        }
        let popover = gtk::Popover::builder()
            .child(&content)
            .has_arrow(true)
            .position(gtk::PositionType::Right)
            .build();
        popover.set_parent(parent);

        let this = Rc::new(Quick {
            popover,
            parent: parent.clone().upcast(),
            time,
            title,
            dot,
            calendar_label,
            save,
            more,
            on_save: RefCell::new(None),
            on_more: RefCell::new(None),
        });

        let (t, s) = (this.title.clone(), this.save.clone());
        t.connect_changed(move |t| s.set_sensitive(!t.text().trim().is_empty()));
        let weak = Rc::downgrade(&this);
        this.title.connect_activate(move |t| {
            let Some(this) = weak.upgrade() else { return };
            let text = t.text().trim().to_string();
            if !text.is_empty() {
                this.popover.popdown();
                if let Some(f) = this.on_save.borrow().as_ref() {
                    f(text);
                }
            }
        });
        let weak = Rc::downgrade(&this);
        this.save.connect_clicked(move |_| {
            let Some(this) = weak.upgrade() else { return };
            let text = this.title.text().trim().to_string();
            this.popover.popdown();
            if let Some(f) = this.on_save.borrow().as_ref() {
                f(text);
            }
        });
        let weak = Rc::downgrade(&this);
        this.more.connect_clicked(move |_| {
            let Some(this) = weak.upgrade() else { return };
            let text = this.title.text().to_string();
            this.popover.popdown();
            if let Some(f) = this.on_more.borrow().as_ref() {
                f(text);
            }
        });
        this
    }

    /// Shows the popover for `when` to `end`, pointed at `rect` in
    /// `anchor`'s own coordinates: `anchor` is the time grid or the
    /// month grid, translated into the card's coordinates, which is
    /// where the popover itself is parented. `on_save` runs with the
    /// title on Enter or Save; `on_more` on More Details.
    pub fn show(
        self: &Rc<Self>,
        anchor: &gtk::Widget,
        rect: &gdk::Rectangle,
        when: &str,
        calendar: &Calendar,
        on_save: impl Fn(String) + 'static,
        on_more: impl Fn(String) + 'static,
    ) {
        self.time.set_label(when);
        self.dot
            .set_css_classes(&["checked-dot", &tint::css_class(&calendar.color)]);
        self.calendar_label.set_label(&calendar.name);
        self.title.set_text("");
        self.save.set_sensitive(false);
        self.on_save.replace(Some(Box::new(on_save)));
        self.on_more.replace(Some(Box::new(on_more)));

        if let Some(point) = anchor.compute_point(
            &self.parent,
            &graphene::Point::new(rect.x() as f32, rect.y() as f32),
        ) {
            let translated = gdk::Rectangle::new(
                point.x().round() as i32,
                point.y().round() as i32,
                rect.width(),
                rect.height(),
            );
            self.popover.set_pointing_to(Some(&translated));
        }
        self.popover.popup();
        self.title.grab_focus();
    }

    /// Closes the popover, such as when the view's range changes under
    /// it.
    pub fn hide(&self) {
        self.popover.popdown();
    }
}
