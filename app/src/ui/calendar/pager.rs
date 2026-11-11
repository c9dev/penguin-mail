//! The carousel's three pages: the range before, the one on screen and
//! the one after.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, glib};

/// Shows `views` as the carousel's three pages, the middle one on
/// screen, and gives back the holders they sit in. Pass the holders the
/// last call gave back, or none the first time.
///
/// The holders stay in the carousel for good and only their views
/// change. A page removed from an `adw::Carousel` leaves it on page 0
/// once the removal has played out, after the caller's own guard has
/// ended, and the calendar reads a late page change at either end as a
/// swipe to that range.
pub(super) fn show_views(
    carousel: &adw::Carousel,
    holders: Vec<adw::Bin>,
    views: [gtk::Widget; 3],
) -> Vec<adw::Bin> {
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

/// Whether the carousel is somewhere a page change can be read from. With
/// no width it holds no position and reports its first page, whatever
/// page it was sent to: while it is off screen, and also once it is on
/// screen and not laid out yet, as when the calendar is shown in the pass
/// that puts its page there. A report read then would pass for a swipe
/// to the range before.
pub(super) fn rests(carousel: &adw::Carousel) -> bool {
    holds_a_position(carousel.is_mapped(), carousel.width())
}

fn holds_a_position(mapped: bool, width: i32) -> bool {
    mapped && width > 0
}

/// Sends the carousel to a page once it has a width, one wait at a time.
/// A carousel sent to a page before then stays on its first one. The wait
/// listens to the paints of the carousel's frame clock rather than asking
/// for frames, so a carousel left on screen with no width costs nothing
/// until a layout gives it one.
#[derive(Default)]
pub(super) struct LaidOut(Rc<RefCell<Option<(gdk::FrameClock, glib::SignalHandlerId)>>>);

impl LaidOut {
    /// Runs `send`, which sends the carousel to a page: at once when it is
    /// laid out, otherwise after the first paint that finds it so. A wait
    /// still running ends first, so a carousel shown twice before its
    /// first frame is sent once.
    pub(super) fn then(&self, carousel: &adw::Carousel, send: impl Fn() + 'static) {
        self.stop();
        if carousel.width() > 0 {
            return send();
        }
        // Only a carousel off screen has no clock, and it waits for its
        // map, which asks again.
        let Some(clock) = carousel.frame_clock() else {
            return;
        };
        let (waiting, carousel) = (Rc::clone(&self.0), carousel.downgrade());
        let id = clock.connect_after_paint(move |_| match carousel.upgrade() {
            // Not laid out yet: the next paint looks again.
            Some(carousel) if carousel.width() <= 0 => {}
            Some(_) => {
                stop(&waiting);
                send();
            }
            // A carousel that is gone has no page to go to.
            None => stop(&waiting),
        });
        *self.0.borrow_mut() = Some((clock, id));
    }

    /// Ends the wait, as when the carousel leaves the screen before it is
    /// laid out: the next time it is shown waits anew.
    pub(super) fn stop(&self) {
        stop(&self.0);
    }
}

/// A view dropped while its carousel still waits takes the wait with it,
/// so nothing is left listening to the frame clock.
impl Drop for LaidOut {
    fn drop(&mut self) {
        self.stop();
    }
}

fn stop(waiting: &RefCell<Option<(gdk::FrameClock, glib::SignalHandlerId)>>) {
    if let Some((clock, id)) = waiting.borrow_mut().take() {
        clock.disconnect(id);
    }
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
        a_carousel_shown_before_it_is_laid_out_rests_on_its_middle_page();
    }

    /// The calendar shown in the pass that puts its page on screen: the
    /// carousel is mapped with no width, and in the app a scroll to its
    /// middle page then left it on the first, the week before. The scroll
    /// has to wait for the width, and then happen once, however many
    /// times the carousel was shown before its first frame.
    fn a_carousel_shown_before_it_is_laid_out_rests_on_its_middle_page() {
        let carousel = adw::Carousel::builder().allow_long_swipes(false).build();
        let stack = gtk::Stack::new();
        stack.add_named(&gtk::Label::new(Some("first run")), Some("first"));
        stack.add_named(&carousel, Some("calendar"));
        let window = gtk::Window::builder()
            .default_width(800)
            .default_height(600)
            .child(&stack)
            .build();
        window.present();
        spin_until(|| stack.width() > 0);
        let holders = show_views(&carousel, Vec::new(), labels("week"));

        stack.set_visible_child_name("calendar");
        assert_eq!(
            carousel.width(),
            0,
            "the check needs a carousel that is not laid out yet"
        );
        let sent = Rc::new(Cell::new(0));
        let laid_out = LaidOut::default();
        let send_to = |carousel: &adw::Carousel, page: &adw::Bin| {
            let (again, page, count) = (carousel.clone(), page.clone(), Rc::clone(&sent));
            laid_out.then(carousel, move || {
                count.set(count.get() + 1);
                again.scroll_to(&page, false);
            });
        };
        // Mapped, unmapped and mapped again before a frame.
        send_to(&carousel, &holders[2]);
        send_to(&carousel, &holders[1]);
        assert_eq!(
            sent.get(),
            0,
            "a carousel with no width was sent to a page it cannot hold"
        );
        spin_until(|| sent.get() > 0 && carousel.position() == 1.0);
        // A frame more, for a second send that should not come.
        spin_frames(&carousel, 1);

        assert!(carousel.width() > 0);
        assert_eq!(sent.get(), 1, "once it has a width, and once only");
        assert_eq!(carousel.position(), 1.0);

        // One that is laid out already goes at once.
        send_to(&carousel, &holders[0]);
        assert_eq!(sent.get(), 2);

        // One that leaves the screen before its first frame is not sent.
        let other = adw::Carousel::new();
        let other_holders = show_views(&other, Vec::new(), labels("day"));
        stack.add_named(&other, Some("other"));
        stack.set_visible_child_name("other");
        assert_eq!(
            other.width(),
            0,
            "the check needs a carousel that is not laid out yet"
        );
        send_to(&other, &other_holders[1]);
        laid_out.stop();
        spin_until(|| other.width() > 0);
        spin_frames(&other, 1);
        assert_eq!(sent.get(), 2, "the wait ended with the unmap");
        window.destroy();
    }

    /// Answering an invitation reads the week again and builds its blocks
    /// anew, and the focus goes back to the event before GTK has laid
    /// the new block out. The viewport follows the window's focus and
    /// scrolled to the bounds the block had so far, at the top of the
    /// day, so the week sprang back to 00:00.
    fn focus_on_a_new_block_leaves_the_hours_where_they_were() {
        let hours = gtk::Fixed::builder()
            .height_request(2000)
            .width_request(300)
            .build();
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
        spin_until(|| scroller.vadjustment().upper() >= 2000.0);
        scroller.vadjustment().set_value(900.0);
        spin_until(|| scroller.vadjustment().value() == 900.0);
        assert_eq!(
            scroller.vadjustment().value(),
            900.0,
            "the hours start where the check put them"
        );

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
        // The new block is laid out, and a frame more for the viewport
        // to follow the focus if it was going to.
        spin_until(|| again.height() > 0);
        spin_frames(&scroller, 2);

        assert_eq!(
            scroller.vadjustment().value(),
            900.0,
            "the hours moved under the reader"
        );
        window.destroy();
    }

    /// Runs the main loop for `time`, so the carousel lays out, animates
    /// and reports what it does after the call that changed it returned.
    /// Runs the main loop until `done` holds, for at most two seconds; the
    /// assertion after it says what went wrong if it never did.
    fn spin_until(done: impl Fn() -> bool) {
        let context = gtk::glib::MainContext::default();
        let end = Instant::now() + Duration::from_secs(2);
        while !done() && Instant::now() < end {
            while context.iteration(false) {}
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Runs the main loop until `widget` has drawn `frames` more frames, so
    /// whatever a change set off in the next layout has happened.
    fn spin_frames(widget: &impl IsA<gtk::Widget>, frames: u32) {
        let Some(clock) = widget.frame_clock() else {
            return spin(Duration::from_millis(100));
        };
        let drawn = Rc::new(Cell::new(0));
        let count = Rc::clone(&drawn);
        let handler = clock.connect_after_paint(move |_| count.set(count.get() + 1));
        widget.queue_draw();
        spin_until(|| drawn.get() >= frames);
        clock.disconnect(handler);
    }

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
        spin_until(|| carousel.width() > 0);
        carousel.scroll_to(&holders[1], false);
        spin_until(|| carousel.position() == 1.0);
        arranging.set(false);
        assert_eq!(
            carousel.position(),
            1.0,
            "the first pages start on the middle one"
        );

        arranging.set(true);
        let holders = show_views(&carousel, holders, labels("second"));
        arranging.set(false);
        spin(Duration::from_millis(800));

        assert_eq!(
            *changes.borrow(),
            Vec::<u32>::new(),
            "page changes after new views"
        );
        assert_eq!(carousel.n_pages(), 3);
        assert_eq!(carousel.position(), 1.0);
        assert!(carousel.nth_page(1) == holders[1].clone().upcast::<gtk::Widget>());
        window.destroy();
    }
}

#[cfg(test)]
mod tests {
    use super::holds_a_position;

    /// Showing the calendar in the pass that puts its page on screen maps
    /// the carousel before it is laid out. It then reported its first page
    /// again and again, and each report was read as a swipe to the week
    /// before, so the calendar opened months in the past.
    #[test]
    fn a_carousel_without_a_width_holds_no_position() {
        assert!(!holds_a_position(false, 0), "not on screen");
        assert!(
            !holds_a_position(false, 1032),
            "not on screen, whatever it last measured"
        );
        assert!(!holds_a_position(true, 0), "on screen and not laid out yet");
        assert!(holds_a_position(true, 1032));
    }
}
