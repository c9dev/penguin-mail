//! What the calendar's header gives up when its room runs short. The
//! week number goes first, then the year beside the month, then the view
//! switch folds into a drop-down that names the view on screen. Today,
//! New Event, Search and the window's buttons stay. The bold part of the
//! title ellipsizes only after all three.

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
    Switch,
}

impl Extra {
    pub const ALL: [Extra; 3] = [Extra::Week, Extra::Year, Extra::Switch];
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

/// The width an extra `natural` pixels wide, with `gap` beside it, gives
/// back when it goes: all of it, or less the width of the `fallback`
/// that takes its place.
pub fn saving(natural: i32, gap: i32, fallback: Option<i32>) -> i32 {
    natural + gap - fallback.map_or(0, |width| width + gap)
}

/// [`keeps`] with memory, so a steady width gives a steady answer.
///
/// The header's core width is worked out from its natural width less the
/// extras showing. A centred title makes that estimate come out
/// differently with an extra shown and with it hidden, so at some widths
/// the plain rule dropped an extra, then found room for it, then dropped
/// it again, and the search button blinked. Once an extra goes at a
/// width, it comes back only when the header is wider than that width.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Steady {
    /// The width at which each extra was last dropped, while it stays
    /// dropped.
    dropped_at: [Option<i32>; 3],
}

impl Steady {
    /// Which extras to keep in `room`, given the estimate `core` and the
    /// widths in `extras`, remembering what this decided before.
    pub fn decide(&mut self, room: i32, core: i32, extras: [i32; 3]) -> [bool; 3] {
        let mut kept = keeps(room, core, extras);
        for (index, keep) in kept.iter_mut().enumerate() {
            match self.dropped_at[index] {
                Some(width) if *keep && room <= width => *keep = false,
                Some(_) if *keep => self.dropped_at[index] = None,
                None if !*keep => self.dropped_at[index] = Some(room),
                _ => {}
            }
        }
        kept
    }
}

/// One extra as the header holds it: the widget, and the gap beside it
/// that goes with it.
struct Held {
    widget: gtk::Widget,
    gap: i32,
    /// What shows in the extra's place once it goes, such as a drop-down
    /// for the view switch. The extra then saves only the difference.
    fallback: Option<gtk::Widget>,
    /// Whether it has anything to show at all, such as a week number in
    /// a view that has one.
    wanted: Cell<bool>,
    /// Its natural width the last time it showed. A hidden widget
    /// measures nothing, and the header needs to know what showing it
    /// again would take.
    natural: Cell<i32>,
    /// The fallback's natural width the last time it showed, or before
    /// it first did.
    fallback_natural: Cell<i32>,
}

impl Held {
    /// The width the fallback takes in the extra's place, gap included,
    /// or nothing when the extra has none.
    fn fallback_width(&self) -> i32 {
        self.fallback
            .as_ref()
            .map_or(0, |_| self.fallback_natural.get() + self.gap)
    }
}

/// The header's natural width with its start and end side by side. The
/// bar lays them out in a `gtk::CenterBox`, which asks for twice its
/// wider side so a title could sit in the middle, even with no title; so
/// hiding a widget on the narrower side changed nothing, and the estimate
/// of the room the extras need moved with what showed.
fn sides_natural(header: &adw::HeaderBar) -> i32 {
    let measure = |widget: &gtk::Widget| widget.measure(gtk::Orientation::Horizontal, -1);
    let (whole_min, whole, _, _) = measure(header.upcast_ref());
    let mut at = header.first_child();
    for _ in 0..3 {
        let Some(widget) = at else { break };
        if let Some(center) = widget.downcast_ref::<gtk::CenterBox>() {
            let sides: i32 = [center.start_widget(), center.end_widget()]
                .iter()
                .flatten()
                .map(|side| measure(side).1)
                .sum();
            // The bar's own padding. Its measured widths leave it out
            // (the natural width came out 2 px below the centre box's),
            // so read it from the last allocation once there is one.
            let padding = match center.width() {
                0 => whole_min - measure(center.upcast_ref()).0,
                inside => header.width() - inside,
            };
            return padding + sides;
        }
        at = widget.first_child();
    }
    whole
}

/// Whether `widget` shows inside `header`: it and every parent up to the
/// header are visible. The view switch can sit in the bottom bar
/// instead, where it takes none of the header's room.
fn shows_in(widget: &gtk::Widget, header: &adw::HeaderBar) -> bool {
    let header = header.upcast_ref::<gtk::Widget>();
    let mut at = Some(widget.clone());
    while let Some(widget) = at {
        if &widget == header {
            return true;
        }
        if !widget.get_visible() {
            return false;
        }
        at = widget.parent();
    }
    false
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct HeaderRoom {
        pub(super) header: RefCell<Option<adw::HeaderBar>>,
        pub(super) extras: RefCell<Vec<Held>>,
        pub(super) kept: Cell<[bool; 3]>,
        pub(super) steady: Cell<Steady>,
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
            // gone, or in its fallback's form, so the window never counts
            // an extra as room it must find, and for its full width as
            // the width it would like.
            let (min, _, _, _) = header.measure(orientation, for_size);
            let (core, extras) = self.widths(&header);
            let mut least = min;
            for held in self.extras.borrow().iter() {
                if shows_in(&held.widget, &header) {
                    least -= held.widget.measure(orientation, -1).0 + held.gap;
                    if held.fallback.is_some() {
                        least += held.fallback_width();
                    }
                }
            }
            (least, core + extras.iter().sum::<i32>(), -1, -1)
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            let Some(header) = self.header.borrow().clone() else {
                return;
            };
            let (core, extras) = self.widths(&header);
            let mut steady = self.steady.get();
            let kept = steady.decide(width, core, extras);
            self.steady.set(steady);
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
        /// The header's natural width with every extra gone or in its
        /// fallback's form, and what each extra adds to that with its
        /// gap, in the order of [`Extra::ALL`].
        fn widths(&self, header: &adw::HeaderBar) -> (i32, [i32; 3]) {
            let natural = sides_natural(header);
            let mut extras = [0; 3];
            let mut core = natural;
            for (index, held) in self.extras.borrow().iter().enumerate() {
                if shows_in(&held.widget, header) {
                    let width = held.widget.measure(gtk::Orientation::Horizontal, -1).1;
                    held.natural.set(width);
                    core -= width + held.gap;
                }
                if let Some(fallback) = &held.fallback
                    && shows_in(fallback, header)
                {
                    let width = fallback.measure(gtk::Orientation::Horizontal, -1).1;
                    held.fallback_natural.set(width);
                    core -= width + held.gap;
                }
                if held.wanted.get() {
                    core += held.fallback_width();
                    let fallback = held.fallback.as_ref().map(|_| held.fallback_natural.get());
                    extras[index] = saving(held.natural.get(), held.gap, fallback);
                }
            }
            (core, extras)
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
    /// [`Extra::ALL`], each with the gap beside it and what shows in its
    /// place once it goes, if anything.
    pub fn new(
        header: &adw::HeaderBar,
        extras: [(gtk::Widget, i32, Option<gtk::Widget>); 3],
    ) -> HeaderRoom {
        let room: HeaderRoom = glib::Object::new();
        header.set_parent(&room);
        room.imp().header.replace(Some(header.clone()));
        let natural = |widget: &gtk::Widget| widget.measure(gtk::Orientation::Horizontal, -1).1;
        room.imp().extras.replace(
            extras
                .into_iter()
                .map(|(widget, gap, fallback)| Held {
                    natural: Cell::new(natural(&widget)),
                    fallback_natural: Cell::new(fallback.as_ref().map_or(0, natural)),
                    widget,
                    gap,
                    fallback,
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

    /// The header's core width is worked out from its natural width,
    /// which a centred title makes come out differently with an extra
    /// shown and with it hidden. At a steady width the two estimates
    /// flipped the answer every frame and the search button blinked.
    #[test]
    fn a_steady_width_settles_instead_of_blinking() {
        let extras = [30, 30, 40];
        let room = 600;
        // With the extras shown the estimate is high; with them hidden,
        // low enough that the plain rule would bring them straight back.
        let core_shown = 560;
        let core_hidden = 500;
        assert_eq!(keeps(room, core_hidden, extras), [true, true, true]);
        let mut state = Steady::default();
        let first = state.decide(room, core_shown, extras);
        assert_ne!(first, [true, true, true], "no room for every extra at the high estimate");
        for _ in 0..5 {
            assert_eq!(state.decide(room, core_hidden, extras), first, "nothing comes back at the same width");
        }
    }

    #[test]
    fn a_dropped_extra_comes_back_when_the_header_widens() {
        let extras = [30, 30, 40];
        let mut state = Steady::default();
        assert_ne!(state.decide(600, 560, extras), [true, true, true]);
        assert_eq!(state.decide(700, 500, extras), [true, true, true]);
    }

    #[test]
    fn a_narrower_header_still_drops_at_once() {
        let extras = [30, 30, 40];
        let mut state = Steady::default();
        assert_eq!(state.decide(1000, 500, extras), [true, true, true]);
        assert_eq!(state.decide(560, 500, extras), [false, false, true]);
    }

    #[test]
    fn an_extra_with_a_fallback_saves_only_the_difference() {
        assert_eq!(saving(273, 0, Some(79)), 194);
    }

    #[test]
    fn an_extra_without_a_fallback_saves_itself_and_its_gap() {
        assert_eq!(saving(32, 8, None), 40);
    }

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
