//! What the calendar's header drops when its room runs short. The view
//! switch keeps its full width: the week number goes first, then the
//! year beside the month, then the search button, which Ctrl+F still
//! opens. The bold part of the title ellipsizes only after all three.

use std::cell::{Cell, RefCell};

use adw::prelude::*;
use gtk::glib;
use gtk::subclass::prelude::*;

/// A part of the header that can leave when the room runs short, in the
/// order it leaves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Extra {
    Week,
    Year,
    Search,
}

impl Extra {
    pub const ALL: [Extra; 3] = [Extra::Week, Extra::Year, Extra::Search];
}

/// Which of [`Extra::ALL`] the header keeps in `room` pixels, when the
/// parts that always stay take `core` pixels at their natural width and
/// each extra takes the width beside it in `extras`, spacing included.
pub fn keeps(room: i32, core: i32, extras: [i32; 3]) -> [bool; 3] {
    let mut kept = [true; 3];
    let mut used = core + extras.iter().sum::<i32>();
    for (index, width) in extras.iter().enumerate() {
        if used <= room {
            break;
        }
        kept[index] = false;
        used -= width;
    }
    kept
}

/// One extra as the header holds it: the widget, and the gap beside it
/// that goes with it.
struct Held {
    widget: gtk::Widget,
    gap: i32,
    /// Whether it has anything to show at all, such as a week number in
    /// a view that has one.
    wanted: Cell<bool>,
    /// Its natural width the last time it showed. A hidden widget
    /// measures nothing, and the header needs to know what showing it
    /// again would take.
    natural: Cell<i32>,
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct HeaderRoom {
        pub(super) header: RefCell<Option<adw::HeaderBar>>,
        pub(super) extras: RefCell<Vec<Held>>,
        pub(super) kept: Cell<[bool; 3]>,
        pub(super) queued: Cell<bool>,
        pub(super) on_change: RefCell<Option<Box<dyn Fn()>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for HeaderRoom {
        const NAME: &'static str = "MailrsCalendarHeaderRoom";
        type Type = super::HeaderRoom;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for HeaderRoom {
        fn constructed(&self) {
            self.parent_constructed();
            self.kept.set([true; 3]);
            // After the header gets narrower an extra leaves on the next
            // idle, so for one frame the bar can be wider than its place.
            self.obj().set_overflow(gtk::Overflow::Hidden);
        }

        fn dispose(&self) {
            if let Some(header) = self.header.take() {
                header.unparent();
            }
        }
    }

    impl WidgetImpl for HeaderRoom {
        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let Some(header) = self.header.borrow().clone() else {
                return (0, 0, -1, -1);
            };
            if orientation == gtk::Orientation::Vertical {
                return header.measure(orientation, -1);
            }
            // The header asks for the width it takes with every extra
            // gone, so the window never counts an extra as room it must
            // find, and for its full width as the width it would like.
            let (min, _, _, _) = header.measure(orientation, for_size);
            let (core, extras) = self.widths(&header);
            let showing: i32 = self
                .extras
                .borrow()
                .iter()
                .filter(|held| held.widget.get_visible())
                .map(|held| held.widget.measure(orientation, -1).0 + held.gap)
                .sum();
            (min - showing, core + extras.iter().sum::<i32>(), -1, -1)
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            let Some(header) = self.header.borrow().clone() else {
                return;
            };
            let (core, extras) = self.widths(&header);
            let kept = keeps(width, core, extras);
            if kept != self.kept.replace(kept) && !self.queued.replace(true) {
                // Showing or hiding a widget changes the size the header
                // asks for, which GTK does not take in the middle of
                // handing out space. Do it once this pass is over.
                let room = self.obj().downgrade();
                glib::idle_add_local_once(move || {
                    if let Some(room) = room.upgrade() {
                        room.imp().queued.set(false);
                        let change = room.imp().on_change.take();
                        if let Some(change) = &change {
                            change();
                        }
                        room.imp().on_change.replace(change);
                    }
                });
            }
            let (min, _, _, _) = header.measure(gtk::Orientation::Horizontal, height);
            header.allocate(width.max(min), height, baseline, None);
        }
    }

    impl HeaderRoom {
        /// The header's natural width without the extras, and each
        /// extra's width with its gap, in the order of [`Extra::ALL`].
        fn widths(&self, header: &adw::HeaderBar) -> (i32, [i32; 3]) {
            let (_, natural, _, _) = header.measure(gtk::Orientation::Horizontal, -1);
            let mut extras = [0; 3];
            let mut showing = 0;
            for (index, held) in self.extras.borrow().iter().enumerate() {
                if held.widget.get_visible() {
                    let width = held.widget.measure(gtk::Orientation::Horizontal, -1).1;
                    held.natural.set(width);
                    showing += width + held.gap;
                }
                if held.wanted.get() {
                    extras[index] = held.natural.get() + held.gap;
                }
            }
            (natural - showing, extras)
        }
    }
}

glib::wrapper! {
    /// Holds the calendar's header bar and hides its extras, one by one,
    /// when the bar has less room than it would like.
    pub struct HeaderRoom(ObjectSubclass<imp::HeaderRoom>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl HeaderRoom {
    /// Holds `header`, whose extras are `extras` in the order of
    /// [`Extra::ALL`], each with the gap beside it.
    pub fn new(header: &adw::HeaderBar, extras: [(gtk::Widget, i32); 3]) -> HeaderRoom {
        let room: HeaderRoom = glib::Object::new();
        header.set_parent(&room);
        room.imp().header.replace(Some(header.clone()));
        room.imp().extras.replace(
            extras
                .into_iter()
                .map(|(widget, gap)| Held {
                    natural: Cell::new(widget.measure(gtk::Orientation::Horizontal, -1).1),
                    widget,
                    gap,
                    wanted: Cell::new(true),
                })
                .collect(),
        );
        room
    }

    /// Whether the header has room for `extra`.
    pub fn keeps(&self, extra: Extra) -> bool {
        let index = Extra::ALL.iter().position(|e| *e == extra).unwrap_or(0);
        self.imp().kept.get()[index]
    }

    /// Tells the header whether `extra` has anything to show, so an
    /// empty one takes no room in the sums.
    pub fn want(&self, extra: Extra, wanted: bool) {
        let index = Extra::ALL.iter().position(|e| *e == extra).unwrap_or(0);
        if let Some(held) = self.imp().extras.borrow().get(index)
            && held.wanted.replace(wanted) != wanted
        {
            self.queue_resize();
        }
    }

    /// Runs `change` once the header has decided to keep or drop an
    /// extra; it should show each extra the header keeps and hide the
    /// rest.
    pub fn connect_change(&self, change: impl Fn() + 'static) {
        self.imp().on_change.replace(Some(Box::new(change)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXTRAS: [i32; 3] = [40, 60, 40];

    #[test]
    fn a_roomy_header_keeps_everything() {
        assert_eq!(keeps(800, 500, EXTRAS), [true, true, true]);
    }

    #[test]
    fn the_week_number_leaves_first() {
        assert_eq!(keeps(610, 500, EXTRAS), [false, true, true]);
    }

    #[test]
    fn the_year_leaves_after_the_week_number() {
        assert_eq!(keeps(560, 500, EXTRAS), [false, false, true]);
    }

    #[test]
    fn the_search_button_leaves_last() {
        assert_eq!(keeps(520, 500, EXTRAS), [false, false, false]);
    }

    #[test]
    fn a_header_exactly_as_wide_as_everything_keeps_it_all() {
        assert_eq!(keeps(640, 500, EXTRAS), [true, true, true]);
    }
}
