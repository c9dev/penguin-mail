//! The carousel's three pages: the range before, the one on screen and
//! the one after.

use adw::prelude::*;

/// Shows `views` as the carousel's three pages, the middle one on
/// screen, and gives back the holders they sit in. Pass the holders the
/// last call gave back, or none the first time.
///
/// The holders stay in the carousel for good and only their views
/// change. A page removed from an `adw::Carousel` leaves it on page 0
/// once the removal has played out, after the caller's own guard has
/// ended, and the calendar reads a late page change at either end as a
/// swipe to that range.
pub(super) fn show_views(carousel: &adw::Carousel, holders: Vec<adw::Bin>, views: [gtk::Widget; 3]) -> Vec<adw::Bin> {
    let holders = match holders.len() {
        3 => holders,
        _ => {
            for holder in &holders {
                carousel.remove(holder);
            }
            (0..3)
                .map(|_| {
                    let holder = adw::Bin::builder().hexpand(true).vexpand(true).build();
                    carousel.append(&holder);
                    holder
                })
                .collect()
        }
    };
    for (holder, view) in holders.iter().zip(&views) {
        holder.set_child(Some(view));
    }
    carousel.scroll_to(&holders[1], false);
    holders
}

/// The pager's checks. They run from the one GTK test in `richbuffer`,
/// because GTK belongs to the thread that starts it and the test harness
/// gives each test a thread of its own.
#[cfg(test)]
pub(crate) mod checks {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;
    use std::time::{Duration, Instant};

    pub fn run() {
        new_views_leave_the_middle_page_on_screen();
    }

    /// Runs the main loop for `time`, so the carousel lays out, animates
    /// and reports what it does after the call that changed it returned.
    fn spin(time: Duration) {
        let context = gtk::glib::MainContext::default();
        let end = Instant::now() + time;
        while Instant::now() < end {
            while context.iteration(false) {}
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn labels(prefix: &str) -> [gtk::Widget; 3] {
        [0, 1, 2].map(|n| gtk::Label::new(Some(&format!("{prefix} {n}"))).upcast())
    }

    /// Switching Day to Week gives the pages new views. The carousel then
    /// has to stay on the middle page and report no page change of its
    /// own: the calendar reads a change at either end as a step back or
    /// forward, and a late one walked the week back months at a time.
    fn new_views_leave_the_middle_page_on_screen() {
        let carousel = adw::Carousel::builder().allow_long_swipes(false).build();
        let window = gtk::Window::builder()
            .default_width(800)
            .default_height(600)
            .child(&carousel)
            .build();
        let arranging = Rc::new(Cell::new(true));
        let changes = Rc::new(RefCell::new(Vec::new()));
        let holders = show_views(&carousel, Vec::new(), labels("first"));
        let (seen, flag) = (Rc::clone(&changes), Rc::clone(&arranging));
        carousel.connect_page_changed(move |_, index| {
            if !flag.get() {
                seen.borrow_mut().push(index);
            }
        });
        window.present();
        spin(Duration::from_millis(500));
        carousel.scroll_to(&holders[1], false);
        spin(Duration::from_millis(200));
        arranging.set(false);
        assert_eq!(carousel.position(), 1.0, "the first pages start on the middle one");

        arranging.set(true);
        let holders = show_views(&carousel, holders, labels("second"));
        arranging.set(false);
        spin(Duration::from_millis(800));

        assert_eq!(*changes.borrow(), Vec::<u32>::new(), "page changes after new views");
        assert_eq!(carousel.n_pages(), 3);
        assert_eq!(carousel.position(), 1.0);
        assert!(carousel.nth_page(1) == holders[1].clone().upcast::<gtk::Widget>());
        window.destroy();
    }
}
