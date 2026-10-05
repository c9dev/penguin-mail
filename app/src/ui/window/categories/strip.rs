//! The chips over an inbox, laid out in lines that wrap. Every chip shows
//! its name while the chips fit on three lines; past that, and in the phone
//! layout, only the chosen chip keeps its name and the others show icons.
//! `chip::names_for` makes that call from the widths measured here. Every
//! chip keeps its spoken name and its tooltip either way, so the name is
//! never lost to a screen reader or to the pointer.

use std::cell::{Cell, RefCell};

use adw::prelude::*;
use gtk::subclass::prelude::*;
use gtk::glib;

use super::chip::{GAP, Names, lines, names_for, spread};

mod imp {
    use super::*;

    pub struct CategoryStrip {
        /// The chips, in reading order.
        pub chips: RefCell<Vec<gtk::ToggleButton>>,
        /// Each chip's name and its count.
        pub names: RefCell<Vec<gtk::Revealer>>,
        /// Each chip's corner badge, shown while its name is folded.
        /// Worded tabs have none.
        pub corners: RefCell<Vec<Option<gtk::Label>>>,
        /// The chosen chip, an index into `chips`.
        pub chosen: Cell<usize>,
        /// The names the last width handed over held.
        pub shown: Cell<Names>,
        /// Focused and Other: names with no icon to fall back to.
        pub worded: Cell<bool>,
        /// The window shows the list alone, as on a phone.
        pub phone: Cell<bool>,
    }

    impl Default for CategoryStrip {
        fn default() -> Self {
            CategoryStrip {
                chips: RefCell::default(),
                names: RefCell::default(),
                corners: RefCell::default(),
                chosen: Cell::new(0),
                shown: Cell::new(Names::Every),
                worded: Cell::new(false),
                phone: Cell::new(false),
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
            // so a chip can reach past the edge for one frame. Clip it
            // rather than draw over the list's edges.
            self.obj().set_overflow(gtk::Overflow::Hidden);
        }

        fn dispose(&self) {
            for chip in self.chips.take() {
                chip.unparent();
            }
        }
    }

    impl WidgetImpl for CategoryStrip {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::HeightForWidth
        }

        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let (named, folded) = self.widths();
            if orientation == gtk::Orientation::Horizontal {
                // The widest chip in the fold sets how narrow the row may
                // get; one line of every name is what it would like.
                let least = folded.iter().copied().max().unwrap_or(0);
                let natural = named.iter().sum::<i32>() + GAP * (named.len() as i32 - 1).max(0);
                return (least, natural.max(least), -1, -1);
            }
            let line = self.line_height();
            let count = match for_size < 0 {
                true => 1,
                false => {
                    let widths = match self.names_at(for_size, &named) {
                        Names::Every => named,
                        Names::Chosen => folded,
                    };
                    lines(&widths, for_size).len().max(1) as i32
                }
            };
            let height = count * line + (count - 1) * GAP;
            (height, height, -1, -1)
        }

        fn size_allocate(&self, width: i32, _height: i32, _baseline: i32) {
            let (named, _) = self.widths();
            let names = self.names_at(width, &named);
            if names != self.shown.replace(names) {
                // Revealing a name changes the size a chip asks for, and
                // GTK does not take a new size request in the middle of
                // handing out space. Change it once this pass is over.
                let strip = self.obj().downgrade();
                glib::idle_add_local_once(move || {
                    if let Some(strip) = strip.upgrade() {
                        strip.imp().show_names();
                        strip.queue_resize();
                    }
                });
            }
            // Lay the chips out as they are now, line by line. One line sits
            // flush left; once they wrap, each line shares its spare room
            // among its chips, so the lines meet both edges rather than
            // leaving a ragged right side.
            let chips = self.chips.borrow();
            let now: Vec<i32> = chips.iter().map(natural_width).collect();
            let line = self.line_height();
            let rows = lines(&now, width);
            let wrapped = rows.len() > 1;
            let mut index = 0;
            for (row, count) in rows.into_iter().enumerate() {
                let (mut x, y) = (0, row as i32 * (line + GAP));
                let widths = match wrapped {
                    true => spread(&now[index..index + count], width),
                    false => now[index..index + count].to_vec(),
                };
                for (chip, chip_width) in chips[index..index + count].iter().zip(widths) {
                    let chip_width = chip_width.min(width);
                    chip.size_allocate(&gtk::Allocation::new(x, y, chip_width, line), -1);
                    x += chip_width + GAP;
                }
                index += count;
            }
        }
    }

    impl CategoryStrip {
        /// Each chip's width with its name open, and folded the way
        /// [`Names::Chosen`] draws it. A name is measured on its own, open
        /// or not, so the answer does not change with the names showing.
        fn widths(&self) -> (Vec<i32>, Vec<i32>) {
            let horizontal = gtk::Orientation::Horizontal;
            let (chips, names) = (self.chips.borrow(), self.names.borrow());
            let mut named = Vec::with_capacity(chips.len());
            let mut folded = Vec::with_capacity(chips.len());
            for (index, (chip, name)) in chips.iter().zip(names.iter()).enumerate() {
                let bare = natural_width(chip) - name.measure(horizontal, -1).1;
                let open = bare + name.child().map_or(0, |n| n.measure(horizontal, -1).1);
                named.push(open);
                folded.push(match index == self.chosen.get() {
                    true => open,
                    false => bare,
                });
            }
            (named, folded)
        }

        fn names_at(&self, width: i32, named: &[i32]) -> Names {
            names_for(width, named, self.phone.get(), self.worded.get())
        }

        fn line_height(&self) -> i32 {
            self.chips
                .borrow()
                .iter()
                .map(|chip| chip.measure(gtk::Orientation::Vertical, -1).1)
                .max()
                .unwrap_or(0)
        }

        /// Opens the names the room allows and closes the others. A chip
        /// whose name is closed shows its count on its corner. A name that
        /// was closed fades in.
        pub fn show_names(&self) {
            let (chosen, shown) = (self.chosen.get(), self.shown.get());
            let corners = self.corners.borrow();
            for (index, name) in self.names.borrow().iter().enumerate() {
                let open = shown == Names::Every || index == chosen;
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

fn natural_width(widget: &impl IsA<gtk::Widget>) -> i32 {
    widget.measure(gtk::Orientation::Horizontal, -1).1
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
    /// Holds `chips`, which carry `names` and `corners` in the same order.
    /// `worded` says the chips have no icons, so their names always show.
    pub fn new(
        chips: Vec<gtk::ToggleButton>,
        names: Vec<gtk::Revealer>,
        corners: Vec<Option<gtk::Label>>,
        worded: bool,
    ) -> CategoryStrip {
        let strip: CategoryStrip = glib::Object::new();
        strip.add_css_class("category-chips");
        for chip in &chips {
            chip.set_parent(&strip);
        }
        let imp = strip.imp();
        imp.chips.replace(chips);
        imp.names.replace(names);
        imp.corners.replace(corners);
        imp.worded.set(worded);
        imp.show_names();
        strip
    }

    /// Makes the chip at `index` the one that keeps its name when not
    /// every name fits.
    pub fn choose(&self, index: usize) {
        self.imp().chosen.set(index);
        self.imp().show_names();
        // A shorter or longer name may fit where the last one did not.
        self.queue_resize();
    }

    /// The window shows the list alone, as on a phone, which keeps the
    /// chips to the chosen name and icons.
    pub fn set_phone(&self, phone: bool) {
        if self.imp().phone.replace(phone) != phone {
            self.queue_resize();
        }
    }
}
