//! The day headings over a Day or Week grid, one to a column. A heading
//! names the place a day is worked from ("MON 28 Home") while every
//! heading fits its column with it, and drops the places together once
//! one does not, so a narrow window keeps seven columns of "MON 28"
//! rather than running the week off its right edge.

use std::cell::{Cell, RefCell};

use adw::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{glib, graphene, gsk};

/// Whether the places fit: each heading's width with its place, `full`,
/// is no wider than a column `column` pixels wide.
pub fn places_fit(column: i32, full: &[i32]) -> bool {
    full.iter().all(|&width| width <= column)
}

/// The width a row of `count` equal columns needs, the widest of `each`
/// in every column.
pub fn row_width(count: usize, each: impl Iterator<Item = i32>) -> i32 {
    each.max().unwrap_or(0) * count as i32
}

/// One heading: the button, and the place label inside it if the day
/// has one.
struct Heading {
    button: gtk::Button,
    place: Option<gtk::Label>,
    /// The button's natural width with its place, from the last time
    /// the place showed.
    full: Cell<i32>,
}

impl Heading {
    fn place_showing(&self) -> bool {
        self.place.as_ref().is_some_and(|place| place.get_visible())
    }

    /// The button's minimum and natural widths without its place, and
    /// its natural width with it.
    fn widths(&self) -> (i32, i32, i32) {
        let (min, natural, _, _) = self.button.measure(gtk::Orientation::Horizontal, -1);
        match &self.place {
            Some(place) if place.get_visible() => {
                let (place_min, place_natural, _, _) =
                    place.measure(gtk::Orientation::Horizontal, -1);
                self.full.set(natural);
                (min - place_min - SPACING, natural - place_natural - SPACING, natural)
            }
            _ => (min, natural, self.full.get().max(natural)),
        }
    }
}

/// The gap between a heading's weekday, date and place.
pub const SPACING: i32 = 8;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct DayHeadings {
        pub(super) headings: RefCell<Vec<Heading>>,
        pub(super) queued: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for DayHeadings {
        const NAME: &'static str = "MailrsDayHeadings";
        type Type = super::DayHeadings;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for DayHeadings {
        fn dispose(&self) {
            for heading in self.headings.take() {
                heading.button.unparent();
            }
        }
    }

    impl WidgetImpl for DayHeadings {
        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            let headings = self.headings.borrow();
            if orientation == gtk::Orientation::Vertical {
                let (min, natural) = headings.iter().fold((0, 0), |(min, natural), h| {
                    let (m, n, _, _) = h.button.measure(orientation, -1);
                    (min.max(m), natural.max(n))
                });
                return (min, natural, -1, -1);
            }
            // The row needs each column as wide as the widest heading
            // without its place, and would like it wide enough for the
            // widest with one.
            let widths: Vec<(i32, i32, i32)> = headings.iter().map(Heading::widths).collect();
            let count = widths.len();
            (
                row_width(count, widths.iter().map(|w| w.0)),
                row_width(count, widths.iter().map(|w| w.2)),
                -1,
                -1,
            )
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            let headings = self.headings.borrow();
            if headings.is_empty() {
                return;
            }
            let column = width as f32 / headings.len() as f32;
            let mut full = Vec::with_capacity(headings.len());
            for (index, heading) in headings.iter().enumerate() {
                let (min, natural, _, _) = heading.button.measure(gtk::Orientation::Horizontal, -1);
                let (_, _, whole) = heading.widths();
                full.push(whole);
                let given = natural.min(column.floor() as i32).max(min);
                let x = index as f32 * column + (column - given as f32) / 2.0;
                heading.button.allocate(
                    given,
                    height,
                    baseline,
                    Some(gsk::Transform::new().translate(&graphene::Point::new(x.round(), 0.0))),
                );
            }
            let fit = places_fit(column.floor() as i32, &full);
            let showing = headings.iter().any(Heading::place_showing);
            let has_places = headings.iter().any(|h| h.place.is_some());
            drop(headings);
            if has_places && fit != showing && !self.queued.replace(true) {
                // Showing or hiding a place changes the size a heading
                // asks for, which GTK does not take in the middle of
                // handing out space. Do it once this pass is over.
                let row = self.obj().downgrade();
                glib::idle_add_local_once(move || {
                    let Some(row) = row.upgrade() else { return };
                    row.imp().queued.set(false);
                    for heading in row.imp().headings.borrow().iter() {
                        if let Some(place) = &heading.place {
                            place.set_visible(fit);
                        }
                    }
                });
            }
        }
    }
}

glib::wrapper! {
    /// The day headings over a grid, one to a column, dropping their
    /// places when the columns are too narrow for them.
    pub struct DayHeadings(ObjectSubclass<imp::DayHeadings>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl DayHeadings {
    pub fn new() -> DayHeadings {
        glib::Object::new()
    }

    /// Replaces the headings with `headings`, each a button and the place
    /// label inside it, if any. Places start shown; the next allocation
    /// hides them again if they do not fit.
    pub fn replace(&self, headings: Vec<(gtk::Button, Option<gtk::Label>)>) {
        for old in self.imp().headings.take() {
            old.button.unparent();
        }
        let held = headings
            .into_iter()
            .map(|(button, place)| {
                button.set_parent(self);
                Heading {
                    button,
                    place,
                    full: Cell::new(0),
                }
            })
            .collect();
        self.imp().headings.replace(held);
        self.queue_resize();
    }
}

impl Default for DayHeadings {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn places_show_while_every_heading_fits_its_column() {
        assert!(places_fit(100, &[70, 100, 96]));
    }

    #[test]
    fn one_heading_too_wide_for_its_column_drops_every_place() {
        assert!(!places_fit(90, &[70, 100, 64]));
    }

    #[test]
    fn a_row_needs_the_widest_heading_in_every_column() {
        assert_eq!(row_width(7, [60, 82, 71].into_iter()), 574);
    }

    #[test]
    fn an_empty_row_needs_nothing() {
        assert_eq!(row_width(0, std::iter::empty()), 0);
    }
}
