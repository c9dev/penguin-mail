//! Follows a native page's GTK anchor without keeping its window animating.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, glib};

#[derive(Default)]
struct Watch(RefCell<Option<(gdk::FrameClock, glib::SignalHandlerId)>>);

impl Watch {
    fn stop(&self) {
        if let Some((clock, handler)) = self.0.borrow_mut().take() {
            clock.disconnect(handler);
        }
    }

    fn start(&self, anchor: &gtk::Widget, placed: Rc<dyn Fn()>) {
        self.stop();
        let Some(clock) = anchor.frame_clock() else {
            return;
        };
        // A native page needs the allocation GTK has just painted. A tick
        // runs before layout and requests more frames even while idle.
        let handler = clock.connect_after_paint(move |_| placed());
        *self.0.borrow_mut() = Some((clock, handler));
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.stop();
    }
}

pub(super) fn follow(anchor: &gtk::Widget, placed: impl Fn() + 'static) {
    let watch = Rc::new(Watch::default());
    let placed: Rc<dyn Fn()> = Rc::new(placed);
    if anchor.is_mapped() {
        watch.start(anchor, placed.clone());
    }
    let mapped = watch.clone();
    anchor.connect_map(move |anchor| mapped.start(anchor, placed.clone()));
    anchor.connect_unmap(move |_| watch.stop());
}

#[cfg(any(test, all(target_os = "macos", debug_assertions)))]
pub(crate) async fn check() {
    use std::cell::Cell;
    use std::rc::Rc;
    use std::time::Duration;

    async fn settle() {
        gtk::glib::timeout_future(Duration::from_millis(250)).await;
    }

    let anchor = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let window = gtk::Window::builder()
        .default_width(400)
        .default_height(300)
        .child(&anchor)
        .build();
    let placed = Rc::new(Cell::new(0));
    let count = placed.clone();
    let last_width = Rc::new(Cell::new(0));
    let width = last_width.clone();
    let weak = anchor.downgrade();
    follow(anchor.upcast_ref(), move || {
        count.set(count.get() + 1);
        width.set(weak.upgrade().unwrap().width());
    });
    window.present();
    settle().await;
    let before = placed.get();
    assert!(before > 0, "a mapped page follows the initial layout");
    settle().await;
    assert_eq!(
        placed.get(),
        before,
        "an idle native page requests no frames"
    );
    let stale = Rc::new(Cell::new(false));
    let missed = stale.clone();
    let weak = anchor.downgrade();
    let clock = anchor.frame_clock().unwrap();
    let handler = clock.connect_after_paint(move |_| {
        if let Some(anchor) = weak.upgrade() {
            missed.set(missed.get() || last_width.get() != anchor.width());
        }
    });
    anchor.set_margin_start(30);
    settle().await;
    assert!(
        placed.get() > before,
        "a changed layout places the page again"
    );
    assert!(!stale.get(), "the native page uses this frame's allocation");
    clock.disconnect(handler);
    anchor.set_visible(false);
    let hidden = placed.get();
    window.queue_draw();
    settle().await;
    assert_eq!(
        placed.get(),
        hidden,
        "an unmapped page stops following frames"
    );
    anchor.set_visible(true);
    settle().await;
    assert!(
        placed.get() > hidden,
        "a remapped page follows its window again"
    );
    window.close();
}
