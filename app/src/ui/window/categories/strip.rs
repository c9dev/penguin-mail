//! The row that holds the category toggles. It shows every chip's name
//! while the whole row fits, then only the chosen chip's name, then icons
//! alone, the way a libadwaita view switcher drops its labels in a narrow
//! window. `chip::names_for` makes that call from the widths measured
//! here. Every toggle keeps its spoken label and its tooltip either way,
//! so the name is never lost to a screen reader or to the pointer.

use std::cell::{Cell, RefCell};

use adw::prelude::*;
use gtk::subclass::prelude::*;
use gtk::glib;

use super::chip::{Names, Needs, names_for};

mod imp {
    use super::*;

    pub struct CategoryStrip {
        pub group: RefCell<Option<adw::ToggleGroup>>,
        /// Each category's name and its count, in toggle order.
        pub names: RefCell<Vec<gtk::Revealer>>,
        /// Each category's corner badge, shown while its name is folded.
        /// Worded tabs have none.
        pub corners: RefCell<Vec<Option<gtk::Label>>>,
        /// The chosen category, an index into `names`.
        pub chosen: Cell<usize>,
        /// The names the last width handed over held.
        pub shown: Cell<Names>,
        /// Focused and Other: names with no icon to fall back to.
        pub worded: Cell<bool>,
    }

    impl Default for CategoryStrip {
        fn default() -> Self {
            CategoryStrip {
                group: RefCell::default(),
                names: RefCell::default(),
                corners: RefCell::default(),
                chosen: Cell::new(0),
                shown: Cell::new(Names::Every),
                worded: Cell::new(false),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for CategoryStrip {
        const NAME: &'static str = "MailrsCategoryStrip";
        type Type = super::CategoryStrip;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for CategoryStrip {
        fn constructed(&self) {
            self.parent_constructed();
            // After the row gets narrower the names close on the next idle,
            // so the group is wider than its place for one frame. Clip it
            // rather than draw over the list's edges.
            self.obj().set_overflow(gtk::Overflow::Hidden);
        }

        fn dispose(&self) {
            if let Some(group) = self.group.take() {
                group.unparent();
            }
        }
    }

    impl WidgetImpl for CategoryStrip {
        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let Some(group) = self.group.borrow().clone() else {
                return (0, 0, -1, -1);
            };
            if orientation == gtk::Orientation::Vertical {
                return group.measure(orientation, -1);
            }
            let needs = self.needs(&group, for_size);
            let least = match self.worded.get() {
                true => needs.every,
                false => needs.icons,
            };
            (least, needs.every, -1, -1)
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            let Some(group) = self.group.borrow().clone() else {
                return;
            };
            let names = names_for(width, self.needs(&group, height), self.worded.get());
            if names != self.shown.replace(names) {
                // Revealing a name changes the size the group asks for,
                // and GTK does not take a new size request in the middle
                // of handing out space. Change it once this pass is over.
                let strip = self.obj().downgrade();
                glib::idle_add_local_once(move || {
                    if let Some(strip) = strip.upgrade() {
                        strip.imp().show_names();
                    }
                });
            }
            let (min, natural, _, _) = group.measure(gtk::Orientation::Horizontal, height);
            let own = natural.min(width).max(min);
            // The chips sit flush at the row's start, under the list
            // header's title, rather than centred: `own` only bounds how
            // much of the width the group actually takes.
            group.allocate(own, height, baseline, None);
        }
    }

    impl CategoryStrip {
        /// The width the row needs with icons alone, with the chosen name
        /// beside its icon, and with every name. Each name is measured on
        /// its own, open or not, so the answer does not change with the
        /// names that happen to be showing.
        fn needs(&self, group: &adw::ToggleGroup, for_size: i32) -> Needs {
            let horizontal = gtk::Orientation::Horizontal;
            let (_, natural, _, _) = group.measure(horizontal, for_size);
            let names = self.names.borrow();
            let showing: i32 = names.iter().map(|n| n.measure(horizontal, -1).1).sum();
            let icons = natural - showing;
            let width = |name: &gtk::Revealer| {
                name.child()
                    .map_or(0, |name| name.measure(horizontal, -1).1)
            };
            let chosen = names.get(self.chosen.get()).map_or(0, width);
            Needs {
                icons,
                chosen: icons + chosen,
                every: icons + names.iter().map(width).sum::<i32>(),
            }
        }

        /// Opens the names the row has room for and closes the others. A
        /// chip whose name is closed shows its count on its corner. A name
        /// that was closed fades in.
        pub fn show_names(&self) {
            let (chosen, shown) = (self.chosen.get(), self.shown.get());
            let corners = self.corners.borrow();
            for (index, name) in self.names.borrow().iter().enumerate() {
                let open = match shown {
                    Names::Every => true,
                    Names::Chosen => index == chosen,
                    Names::Icons => false,
                };
                if open
                    && !name.reveals_child()
                    && let Some(label) = name.child()
                {
                    fade_in(&label);
                }
                name.set_reveal_child(open);
                if let Some(Some(corner)) = corners.get(index) {
                    corner.set_visible(!open);
                }
            }
        }
    }
}

/// How long a category's name takes to fade in, in milliseconds.
const FADE_MS: u32 = 150;

/// Fades `label` in from nothing. libadwaita jumps an animation to its end
/// when the desktop has animations turned off, and keeps it alive while it
/// plays, so nothing here holds on to it.
fn fade_in(label: &gtk::Widget) {
    let target = adw::PropertyAnimationTarget::new(label, "opacity");
    let fade = adw::TimedAnimation::new(label, 0.0, 1.0, FADE_MS, target);
    fade.set_easing(adw::Easing::EaseOutCubic);
    fade.play();
}

glib::wrapper! {
    pub struct CategoryStrip(ObjectSubclass<imp::CategoryStrip>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl CategoryStrip {
    /// Holds `group`, whose toggles carry `names` and `corners` in the
    /// same order. `worded` says the toggles have no icons, so their names
    /// always show.
    pub fn new(
        group: &adw::ToggleGroup,
        names: Vec<gtk::Revealer>,
        corners: Vec<Option<gtk::Label>>,
        worded: bool,
    ) -> CategoryStrip {
        let strip: CategoryStrip = glib::Object::new();
        group.set_parent(&strip);
        let imp = strip.imp();
        imp.group.replace(Some(group.clone()));
        imp.names.replace(names);
        imp.corners.replace(corners);
        imp.worded.set(worded);
        imp.show_names();
        strip
    }

    /// Makes the name at `index` the one that stays when not every name
    /// fits.
    pub fn choose(&self, index: usize) {
        self.imp().chosen.set(index);
        self.imp().show_names();
        // A shorter or longer name may fit where the last one did not, and
        // with every name closed nothing else would ask for a new layout.
        self.queue_resize();
    }
}
