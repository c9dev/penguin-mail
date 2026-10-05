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

/// Runs `refocus`, which gives the keyboard focus back to a page whose
/// blocks were just built again, and keeps `scroller` where it was.
///
/// The viewport around the hours follows the window's focus, and a block
/// built a moment ago has no place yet: its bounds put it at the top of
/// the day, or wherever the last layout left them. Setting the value
/// back without animation also stops the scroll GTK has started.
pub(super) fn refocus_in_place(scroller: &gtk::ScrolledWindow, refocus: impl FnOnce()) {
    let adjustment = scroller.vadjustment();
    let kept = adjustment.value();
    refocus();
    adjustment.set_value(kept);
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
        focus_on_a_new_block_leaves_the_hours_where_they_were();
    }

    /// Answering an invitation reads the week again and builds its blocks
    /// anew, and the focus goes back to the event before GTK has laid
    /// the new block out. The viewport follows the window's focus and
    /// scrolled to the bounds the block had so far, at the top of the
    /// day, so the week sprang back to 00:00.
    fn focus_on_a_new_block_leaves_the_hours_where_they_were() {
        let hours = gtk::Fixed::builder().height_request(2000).width_request(300).build();
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&hours)
            .build();
        let window = gtk::Window::builder()
            .default_width(400)
            .default_height(300)
            .child(&scroller)
            .build();
        let block = gtk::Button::with_label("Design review");
        hours.put(&block, 10.0, 1000.0);
        window.present();
        spin(Duration::from_millis(300));
        scroller.vadjustment().set_value(900.0);
        spin(Duration::from_millis(300));
        assert_eq!(scroller.vadjustment().value(), 900.0, "the hours start where the check put them");

        // The refill puts a new block where the old one was, and the focus
        // goes to it.
        hours.remove(&block);
        let again = gtk::Button::with_label("Design review");
        hours.put(&again, 10.0, 1000.0);
        refocus_in_place(&scroller, || {
            again.grab_focus();
            // A check's window is never the active one, so GTK does not
            // follow its focus here. In the app it scrolls toward the new
            // block's bounds before layout, the top of the day.
            scroller.vadjustment().set_value(0.0);
        });
        spin(Duration::from_millis(600));

        assert_eq!(scroller.vadjustment().value(), 900.0, "the hours moved under the reader");
        window.destroy();
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
