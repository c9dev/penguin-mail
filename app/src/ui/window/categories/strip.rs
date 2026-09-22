//! The row that holds the category toggles. It shows the chosen
//! category's name while the whole row fits, and folds that name away to
//! leave icons alone when it does not, the way a libadwaita view switcher
//! drops its labels in a narrow window. Every toggle keeps its spoken
//! label and its tooltip either way, so the name is never lost to a
//! screen reader or to the pointer.

use std::cell::{Cell, RefCell};

use adw::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{glib, graphene, gsk};

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct CategoryStrip {
        pub group: RefCell<Option<adw::ToggleGroup>>,
        /// Each category's sliding name, in toggle order.
        pub names: RefCell<Vec<gtk::Revealer>>,
        /// The name of the chosen category, an index into `names`.
        pub chosen: Cell<usize>,
        /// Whether the last width handed over held the chosen name too.
        pub roomy: Cell<bool>,
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
            self.roomy.set(true);
            // While a name slides shut after the row got narrower, the
            // group is wider than its place for a few frames. Clip it
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
            let (icons, roomy) = self.widths(&group, for_size);
            (icons, roomy, -1, -1)
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            let Some(group) = self.group.borrow().clone() else {
                return;
            };
            let (_, roomy) = self.widths(&group, height);
            let fits = width >= roomy;
            if fits != self.roomy.replace(fits) {
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
            let x = ((width - own) / 2).max(0) as f32;
            group.allocate(
                own,
                height,
                baseline,
                Some(gsk::Transform::new().translate(&graphene::Point::new(x, 0.0))),
            );
        }
    }

    impl CategoryStrip {
        /// The width the row needs with icons alone, and with the chosen
        /// name beside its icon as well. A name part way through its
        /// slide counts as closed, so the answer holds still while the
        /// names move.
        fn widths(&self, group: &adw::ToggleGroup, for_size: i32) -> (i32, i32) {
            let horizontal = gtk::Orientation::Horizontal;
            let (_, natural, _, _) = group.measure(horizontal, for_size);
            let names = self.names.borrow();
            let showing: i32 = names.iter().map(|n| n.measure(horizontal, -1).1).sum();
            let icons = natural - showing;
            let chosen = names
                .get(self.chosen.get())
                .and_then(|n| n.child())
                .map_or(0, |name| name.measure(horizontal, -1).1);
            (icons, icons + chosen)
        }

        /// Opens the chosen category's name when the row has room for it
        /// and closes every other.
        pub fn show_names(&self) {
            let (chosen, roomy) = (self.chosen.get(), self.roomy.get());
            for (index, name) in self.names.borrow().iter().enumerate() {
                name.set_reveal_child(roomy && index == chosen);
            }
        }
    }
}

glib::wrapper! {
    pub struct CategoryStrip(ObjectSubclass<imp::CategoryStrip>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl CategoryStrip {
    /// Holds `group`, whose toggles carry `names` in the same order.
    pub fn new(group: &adw::ToggleGroup, names: Vec<gtk::Revealer>) -> CategoryStrip {
        let strip: CategoryStrip = glib::Object::new();
        group.set_parent(&strip);
        strip.imp().group.replace(Some(group.clone()));
        strip.imp().names.replace(names);
        strip
    }

    /// Makes the name at `index` the one shown while it fits.
    pub fn choose(&self, index: usize) {
        self.imp().chosen.set(index);
        self.imp().show_names();
        // A shorter or longer name may fit where the last one did not, and
        // with every name closed nothing else would ask for a new layout.
        self.queue_resize();
    }
}
