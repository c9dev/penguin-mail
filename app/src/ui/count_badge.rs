//! A count drawn beside a control that already announces the full number.

use std::cell::RefCell;

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{glib, graphene, pango};

mod imp {
    use super::*;

    #[derive(Default, glib::Properties)]
    #[properties(wrapper_type = super::CountBadge)]
    pub struct CountBadge {
        #[property(get, set = Self::set_label, explicit_notify)]
        label: RefCell<String>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for CountBadge {
        const NAME: &'static str = "MailrsCountBadge";
        type Type = super::CountBadge;
        type ParentType = gtk::Widget;

        fn class_init(class: &mut Self::Class) {
            class.set_accessible_role(gtk::AccessibleRole::Presentation);
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for CountBadge {}

    impl WidgetImpl for CountBadge {
        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            let (ink, line) = self.layout().pixel_extents();
            let size = if orientation == gtk::Orientation::Horizontal {
                ink.width().max(line.width())
            } else {
                ink.height().max(line.height())
            };
            (size, size, -1, -1)
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let badge = self.obj();
            let layout = self.layout();
            let (ink, _) = layout.extents();
            snapshot.save();
            snapshot.translate(&ink_origin(
                badge.width(),
                badge.height(),
                ink,
                badge.scale_factor(),
            ));
            snapshot.append_layout(&layout, &badge.color());
            snapshot.restore();
        }
    }

    impl CountBadge {
        fn set_label(&self, label: String) {
            if *self.label.borrow() == label {
                return;
            }
            self.label.replace(label);
            let badge = self.obj();
            badge.queue_resize();
            badge.notify_label();
        }

        fn layout(&self) -> pango::Layout {
            let layout = self.obj().create_pango_layout(Some(&self.label.borrow()));
            let attributes = pango::AttrList::new();
            attributes.insert(pango::AttrFontFeatures::new("tnum"));
            layout.set_attributes(Some(&attributes));
            layout
        }
    }
}

glib::wrapper! {
    pub struct CountBadge(ObjectSubclass<imp::CountBadge>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

// The line box reserves space for marks and descenders the badge does not use.
// The origin lands on a whole device pixel, so the digits stay sharp.
fn ink_origin(width: i32, height: i32, ink: pango::Rectangle, scale: i32) -> graphene::Point {
    let pixels = |units: i32| units as f32 / pango::SCALE as f32;
    let snap = |at: f32| (at * scale as f32).round() / scale as f32;
    graphene::Point::new(
        snap((width as f32 - pixels(ink.width())) / 2.0 - pixels(ink.x())),
        snap((height as f32 - pixels(ink.height())) / 2.0 - pixels(ink.y())),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn centers_ink_with_fractional_bearings() {
        let scale = pango::SCALE;
        let ink = pango::Rectangle::new(-scale / 4, 3 * scale / 2, 27 * scale / 2, 9 * scale);
        assert_eq!(ink_origin(22, 18, ink, 2), graphene::Point::new(4.5, 3.0));
        assert_eq!(ink_origin(22, 18, ink, 1), graphene::Point::new(5.0, 3.0));
    }
}
