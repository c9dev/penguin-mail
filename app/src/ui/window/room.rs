//! How the window shares its width between the mailboxes, the space on
//! screen (the mail's list and conversation, or the calendar) and the
//! assistant's panel.
//!
//! libadwaita's split views give each pane its own width and clip what
//! does not fit, so a panel beside a space that cannot shrink any further
//! runs off the window's right edge. [`arrange`] decides from the widths
//! each part needs, in pixels, so that whatever sits side by side fits:
//! the panel goes beside the space while both fit, the mailboxes fold
//! away first to make that room, and below that the panel slides over
//! the space instead of squeezing it. [`Room`] holds the window's panes
//! and arranges them each time GTK hands it a width.

use std::cell::{Cell, RefCell};

use adw::prelude::*;
use gtk::glib;
use gtk::subclass::prelude::*;

/// The narrowest width each part can take, in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Needs {
    /// The mailboxes, or the calendar's own sidebar.
    pub mailboxes: i32,
    /// The mail's list and conversation, or the calendar, without the
    /// window's buttons.
    pub space: i32,
    /// The window's own buttons (minimize, maximize, close), which sit
    /// at the end of the space's header unless the panel beside it takes
    /// them into its own.
    pub controls: i32,
    /// The assistant's panel.
    pub panel: i32,
}

/// What the window folds away or lays over the rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Arrangement {
    /// The mailboxes leave the side of the space and wait behind the
    /// list's sidebar button.
    pub mailboxes_fold: bool,
    /// The panel, open or not, slides over the space rather than sitting
    /// beside it.
    pub panel_overlays: bool,
    /// The panel's width.
    pub panel_width: i32,
}

/// The panel's narrowest width, where its suggestions still read well
/// and its entry and Send button fit.
pub const PANEL_LEAST: i32 = 320;

/// The share of the window the panel takes while that fits.
const PANEL_SHARE: f64 = 0.3;
/// The panel grows no wider than this, however wide the window.
const PANEL_WIDEST: i32 = 460;

/// Arranges a window `width` pixels wide. While the panel is closed, the
/// answer still says how it would open, so it opens in place rather than
/// opening beside the space and then jumping over it.
pub fn arrange(width: i32, panel_open: bool, needs: Needs) -> Arrangement {
    let share = (f64::from(width) * PANEL_SHARE) as i32;
    let panel_natural = share.clamp(needs.panel, PANEL_WIDEST.max(needs.panel));
    let with_mailboxes = needs.mailboxes + needs.space;
    // Without the panel beside it, the space's own header holds the
    // window's buttons.
    let mailboxes_fit_alone = width >= with_mailboxes + needs.controls;
    if width - needs.panel >= with_mailboxes {
        Arrangement {
            mailboxes_fold: !panel_open && !mailboxes_fit_alone,
            panel_overlays: false,
            panel_width: panel_natural.min(width - with_mailboxes),
        }
    } else if width - needs.panel >= needs.space {
        // The mailboxes stay while the panel is closed and fold only
        // when it opens and needs their room.
        Arrangement {
            mailboxes_fold: panel_open || !mailboxes_fit_alone,
            panel_overlays: false,
            panel_width: panel_natural.min(width - needs.space),
        }
    } else {
        Arrangement {
            mailboxes_fold: !mailboxes_fit_alone,
            panel_overlays: true,
            panel_width: panel_natural.min(width),
        }
    }
}

type Decide = Box<dyn Fn(i32) -> Arrangement>;
type Apply = Box<dyn Fn(Arrangement)>;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Room {
        pub(super) child: RefCell<Option<gtk::Widget>>,
        pub(super) decide: RefCell<Option<Decide>>,
        pub(super) apply: RefCell<Option<Apply>>,
        /// The width GTK last handed over.
        pub(super) width: Cell<i32>,
        /// What the last decision was, applied or waiting to be.
        pub(super) arranged: Cell<Option<Arrangement>>,
        pub(super) queued: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Room {
        const NAME: &'static str = "MailrsWindowRoom";
        type Type = super::Room;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for Room {
        fn constructed(&self) {
            self.parent_constructed();
            // A new arrangement lands on the next idle, so for one frame
            // the panes can be wider than the window.
            self.obj().set_overflow(gtk::Overflow::Hidden);
        }

        fn dispose(&self) {
            if let Some(child) = self.child.take() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for Room {
        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let Some(child) = self.child.borrow().clone() else {
                return (0, 0, -1, -1);
            };
            let measured = child.measure(orientation, for_size);
            match orientation {
                // The width the panes ask for follows the arrangement,
                // which follows the width: asking for it would let the
                // window grow each time the panel widens. The room takes
                // any width and arranges the panes to fit.
                gtk::Orientation::Horizontal => (0, measured.1, -1, -1),
                _ => measured,
            }
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            let Some(child) = self.child.borrow().clone() else {
                return;
            };
            self.width.set(width);
            self.decide();
            let (min, _, _, _) = child.measure(gtk::Orientation::Horizontal, height);
            child.allocate(width.max(min), height, baseline, None);
        }
    }

    impl Room {
        /// Decides again for the last width, and applies the answer on
        /// the next idle when it differs from the last one. Changing a
        /// split view's state changes the sizes its panes ask for, which
        /// GTK does not take in the middle of handing out space.
        pub(super) fn decide(&self) {
            let width = self.width.get();
            if width <= 0 {
                return;
            }
            let Some(arranged) = self.decide.borrow().as_ref().map(|decide| decide(width)) else {
                return;
            };
            if self.arranged.replace(Some(arranged)) == Some(arranged) || self.queued.replace(true)
            {
                return;
            }
            let room = self.obj().downgrade();
            glib::idle_add_local_once(move || {
                let Some(room) = room.upgrade() else { return };
                let imp = room.imp();
                imp.queued.set(false);
                let Some(arranged) = imp.arranged.get() else { return };
                // Applying can close the panel, which asks for a decision
                // of its own; no borrow may be held across it.
                let apply = imp.apply.take();
                if let Some(apply) = &apply {
                    apply(arranged);
                }
                imp.apply.replace(apply);
            });
        }
    }
}

glib::wrapper! {
    /// Holds the window's panes and arranges them for the width it gets.
    pub struct Room(ObjectSubclass<imp::Room>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Room {
    /// Holds `child`. Each time the width or what the panes need changes,
    /// `decide` works out an arrangement for the width, and `apply` puts
    /// a new one on screen.
    pub fn new(
        child: &impl IsA<gtk::Widget>,
        decide: impl Fn(i32) -> Arrangement + 'static,
        apply: impl Fn(Arrangement) + 'static,
    ) -> Room {
        let room: Room = glib::Object::new();
        child.set_parent(&room);
        let imp = room.imp();
        imp.child.replace(Some(child.clone().upcast()));
        imp.decide.replace(Some(Box::new(decide)));
        imp.apply.replace(Some(Box::new(apply)));
        room
    }

    /// Decides again, for a change GTK does not hand a new width for,
    /// such as the panel opening or closing.
    pub fn rearrange(&self) {
        self.imp().decide();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the demo's mail measures at the default text size, with a
    /// conversation open.
    const MAIL: Needs = Needs {
        mailboxes: 257,
        space: 767,
        controls: 108,
        panel: 320,
    };

    #[test]
    fn a_wide_window_keeps_everything_side_by_side() {
        let arranged = arrange(1600, true, MAIL);
        assert_eq!(
            arranged,
            Arrangement {
                mailboxes_fold: false,
                panel_overlays: false,
                panel_width: 460,
            }
        );
    }

    #[test]
    fn the_owners_window_folds_the_mailboxes_for_the_open_panel() {
        let arranged = arrange(1330, true, MAIL);
        assert_eq!(
            arranged,
            Arrangement {
                mailboxes_fold: true,
                panel_overlays: false,
                panel_width: 399,
            }
        );
    }

    #[test]
    fn the_space_needs_room_for_the_window_buttons_while_the_panel_is_closed() {
        assert!(arrange(1100, false, MAIL).mailboxes_fold);
    }

    #[test]
    fn the_mailboxes_stay_while_the_panel_is_closed() {
        assert!(!arrange(1330, false, MAIL).mailboxes_fold);
    }

    #[test]
    fn a_closed_panel_would_open_where_an_open_one_sits() {
        for width in [900, 1100, 1330, 1600] {
            assert_eq!(
                arrange(width, false, MAIL).panel_overlays,
                arrange(width, true, MAIL).panel_overlays,
                "at {width}"
            );
        }
    }

    #[test]
    fn the_panel_shrinks_to_its_minimum_before_it_overlays() {
        let arranged = arrange(1100, true, MAIL);
        assert_eq!(
            (arranged.panel_overlays, arranged.panel_width),
            (false, 330)
        );
    }

    #[test]
    fn the_panel_overlays_once_the_space_and_panel_do_not_fit() {
        assert!(arrange(1000, true, MAIL).panel_overlays);
    }

    #[test]
    fn the_mailboxes_fold_when_the_space_alone_does_not_fit_beside_them() {
        assert!(arrange(1000, false, MAIL).mailboxes_fold);
    }

    #[test]
    fn whatever_sits_side_by_side_fits_the_window() {
        for width in (360..2400).step_by(7) {
            for open in [false, true] {
                let arranged = arrange(width, open, MAIL);
                let mut used = MAIL.space;
                if !arranged.mailboxes_fold {
                    used += MAIL.mailboxes;
                }
                if open && !arranged.panel_overlays {
                    assert!(arranged.panel_width >= MAIL.panel, "at {width}");
                    used += arranged.panel_width;
                } else {
                    used += MAIL.controls;
                }
                // A space too wide for the window on its own has nothing
                // left to fold; every other arrangement fits.
                let least = MAIL.space + MAIL.controls;
                assert!(used <= width.max(least), "at {width}, open {open}");
            }
        }
    }
}
