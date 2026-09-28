//! A box that strokes a dashed outline on its edge, for the invitation's
//! own block on the day strip. CSS draws `dashed` borders with short
//! dashes and 1 px gaps, and the mockup draws dashes of 5 and gaps of 4,
//! as the calendar's unanswered blocks have them.

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk};

/// The mockup's stroke on the strip: 1.6 px, dashes of 5 and gaps of 4,
/// on the block's 7 px corners.
const WIDTH: f32 = 1.6;
const DASH: [f32; 2] = [5.0, 4.0];
const CORNER: f32 = 7.0;

mod imp {
    use std::cell::Cell;

    use super::*;

    #[derive(Default)]
    pub struct Outlined {
        pub colour: Cell<Option<gdk::RGBA>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Outlined {
        const NAME: &'static str = "MailrsOutlinedBox";
        type Type = super::Outlined;
        type ParentType = gtk::Box;
    }

    impl ObjectImpl for Outlined {}

    impl WidgetImpl for Outlined {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            self.parent_snapshot(snapshot);
            let Some(colour) = self.colour.get() else {
                return;
            };
            let widget = self.obj();
            // The stroke sits on the edge, half in and half out, as an
            // SVG stroke does. GTK hands this the content box, inside any
            // CSS padding, so the stylesheet gives an outlined block none.
            let bounds =
                graphene::Rect::new(0.0, 0.0, widget.width() as f32, widget.height() as f32);
            let path = gsk::PathBuilder::new();
            path.add_rounded_rect(&gsk::RoundedRect::from_rect(bounds, CORNER));
            let stroke = gsk::Stroke::new(WIDTH);
            stroke.set_dash(&DASH);
            snapshot.append_stroke(&path.to_path(), &stroke, &colour);
        }
    }

    impl BoxImpl for Outlined {}
}

glib::wrapper! {
    pub struct Outlined(ObjectSubclass<imp::Outlined>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl Outlined {
    /// A horizontal box with `spacing`, outlined in `colour` when given.
    pub fn new(spacing: i32, colour: Option<gdk::RGBA>) -> Outlined {
        let outlined: Outlined = glib::Object::builder()
            .property("orientation", gtk::Orientation::Horizontal)
            .property("spacing", spacing)
            .build();
        outlined.imp().colour.set(colour);
        outlined
    }
}

/// The colour a block's outline takes: its own `#rrggbb`, or the accent
/// for anything else, as `.cal-accent` does in the stylesheet.
pub fn colour_of(colour: &str) -> gdk::RGBA {
    match crate::ui::calendar::tint::parse_hex(colour) {
        Some((r, g, b)) => gdk::RGBA::new(
            f32::from(r) / 255.0,
            f32::from(g) / 255.0,
            f32::from(b) / 255.0,
            1.0,
        ),
        None => adw::StyleManager::default().accent_color_rgba(),
    }
}
