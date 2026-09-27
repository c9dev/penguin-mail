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

    /// Shows the popover for `when` to `end`, on `side` of `rect` in
    /// `anchor`'s own coordinates: `anchor` is the time grid or the
    /// month grid, translated into the card's coordinates, which is
    /// where the popover itself is parented. `on_save` runs with the
    /// title on Enter or Save; `on_more` on More Details.
    #[expect(clippy::too_many_arguments, reason = "each is a separate part of what the popover shows")]
    pub fn show(
        self: &Rc<Self>,
        anchor: &gtk::Widget,
        rect: &gdk::Rectangle,
        side: gtk::PositionType,
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
        self.popover.set_position(side);
        self.popover.popup();
        self.title.grab_focus();
    }

    /// Closes the popover, such as when the view's range changes under
    /// it.
    pub fn hide(&self) {
        self.popover.popdown();
    }
}

/// The scroll that shows a slot from `top` to `bottom` in a view `page`
/// tall scrolled to `value`, over content `upper` tall: `value` itself
/// when the slot already shows, else the slot in the middle of the view,
/// within the content's ends.
pub fn reveal(top: f64, bottom: f64, value: f64, page: f64, upper: f64) -> f64 {
    if top >= value && bottom <= value + page {
        return value;
    }
    let middle = (top + bottom) / 2.0 - page / 2.0;
    middle.min(upper - page).max(0.0)
}

/// The side of its slot quick create opens on: the right, unless the
/// slot's middle `x` sits in the right half of a grid `width` wide,
/// where the popover would run out of the window.
pub fn side(x: f64, width: f64) -> gtk::PositionType {
    if x > width / 2.0 {
        gtk::PositionType::Left
    } else {
        gtk::PositionType::Right
    }
}

/// The part of a span from `y`, `height` tall, that falls inside a view
/// `page` tall, at least one pixel, so the popover's arrow points at
/// what shows.
pub fn clamp_span(y: f64, height: f64, page: f64) -> (f64, f64) {
    let top = y.clamp(0.0, (page - 1.0).max(0.0));
    let bottom = (y + height).min(page).max(top + 1.0);
    (top, bottom - top)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_slot_already_on_screen_leaves_the_scroll_alone() {
        assert_eq!(reveal(600.0, 650.0, 400.0, 500.0, 1800.0), 400.0);
    }

    #[test]
    fn a_slot_below_the_view_scrolls_it_to_the_middle() {
        // An evening slot at 21:15 with the grid showing 08:00 to 18:00.
        assert_eq!(reveal(1275.0, 1335.0, 480.0, 600.0, 1440.0), 840.0);
    }

    #[test]
    fn a_reveal_never_scrolls_past_either_end() {
        assert_eq!(reveal(1400.0, 1440.0, 0.0, 600.0, 1440.0), 840.0);
        assert_eq!(reveal(0.0, 60.0, 700.0, 600.0, 1440.0), 0.0);
    }

    #[test]
    fn the_popover_opens_toward_the_wider_side_of_the_grid() {
        assert_eq!(side(200.0, 1000.0), gtk::PositionType::Right);
        assert_eq!(side(800.0, 1000.0), gtk::PositionType::Left);
    }

    #[test]
    fn the_rect_a_popover_points_at_stays_inside_the_view() {
        assert_eq!(clamp_span(-30.0, 60.0, 500.0), (0.0, 30.0));
        assert_eq!(clamp_span(480.0, 60.0, 500.0), (480.0, 20.0));
        assert_eq!(clamp_span(100.0, 60.0, 500.0), (100.0, 60.0));
    }
}
