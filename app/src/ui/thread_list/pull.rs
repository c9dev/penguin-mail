//! A deliberate overscroll at the top runs the same action as Check for Mail.
use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, glib};
use mailrs_domain::translate::gettext;

const DISTANCE: f64 = 72.0;

#[derive(Default)]
struct Gesture {
    active: bool,
    distance: f64,
    fired: bool,
}

impl Gesture {
    fn begin(&mut self) {
        *self = Self {
            active: true,
            ..Self::default()
        };
    }

    fn scroll(&mut self, at_top: bool, dx: f64, dy: f64) -> bool {
        if !self.active || self.fired {
            return false;
        }
        if !at_top || !dy.is_finite() || dx.abs() > dy.abs() {
            self.distance = 0.0;
            return false;
        }
        self.distance = (self.distance - dy).clamp(0.0, DISTANCE);
        self.fired = self.distance >= DISTANCE;
        self.fired
    }
}

pub(super) struct Pull {
    pub widget: gtk::Overlay,
    gesture: RefCell<Gesture>,
    indicator: gtk::Box,
    progress: gtk::ProgressBar,
    label: gtk::Label,
}

impl Pull {
    pub fn new(stack: &gtk::Stack, scroller: &gtk::ScrolledWindow) -> Rc<Self> {
        let indicator = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Start)
            .margin_top(8)
            .can_target(false)
            .css_classes(["card"])
            .visible(false)
            .build();
        let label = gtk::Label::builder()
            .margin_start(12)
            .margin_end(12)
            .margin_top(8)
            .build();
        let progress = gtk::ProgressBar::builder()
            .margin_start(12)
            .margin_end(12)
            .margin_bottom(8)
            .build();
        super::super::name(&progress, &gettext("Pull to check for mail"));
        indicator.append(&label);
        indicator.append(&progress);
        let widget = gtk::Overlay::builder().child(stack).build();
        widget.add_overlay(&indicator);
        let this = Rc::new(Self {
            widget,
            indicator,
            progress,
            label,
            gesture: RefCell::new(Gesture::default()),
        });
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
        scroll.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(&this);
        scroll.connect_scroll_begin(move |_| {
            if let Some(this) = weak.upgrade() {
                this.reset();
                this.gesture.borrow_mut().begin();
            }
        });
        let weak = Rc::downgrade(&this);
        let (stack, scroller) = (stack.downgrade(), scroller.downgrade());
        scroll.connect_scroll(move |controller, dx, dy| {
            let (Some(this), Some(stack), Some(scroller)) =
                (weak.upgrade(), stack.upgrade(), scroller.upgrade())
            else {
                return glib::Propagation::Proceed;
            };
            let empty = stack.visible_child_name().as_deref() == Some("empty");
            let at_top = empty
                || (stack.visible_child_name().as_deref() == Some("list")
                    && scroller.vadjustment().value() <= scroller.vadjustment().lower() + 0.5);
            // Wheel units on Linux touchpads describe lines, surface units pixels.
            let scale = if controller.unit() == gdk::ScrollUnit::Wheel {
                40.0
            } else {
                1.0
            };
            let fire = this
                .gesture
                .borrow_mut()
                .scroll(at_top, dx * scale, dy * scale);
            let distance = this.gesture.borrow().distance;
            this.indicator.set_visible(distance > 0.0);
            this.progress.set_fraction(distance / DISTANCE);
            this.label.set_label(&if this.gesture.borrow().fired {
                gettext("Checking for mail")
            } else {
                gettext("Pull to check for mail")
            });
            if fire {
                let _ = this.widget.activate_action("win.check", None);
            }
            glib::Propagation::Proceed
        });
        let weak = Rc::downgrade(&this);
        scroll.connect_scroll_end(move |_| {
            if let Some(this) = weak.upgrade() {
                this.reset();
            }
        });
        this.widget.add_controller(scroll);
        let weak = Rc::downgrade(&this);
        this.widget.connect_unmap(move |_| {
            if let Some(this) = weak.upgrade() {
                this.reset();
            }
        });
        this
    }

    pub fn reset(&self) {
        *self.gesture.borrow_mut() = Gesture::default();
        self.indicator.set_visible(false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pull_shows_progress_then_fires_once_until_the_next_gesture() {
        let mut pull = Gesture::default();
        assert!(
            !pull.scroll(true, 0.0, -100.0),
            "wheel events and momentum do not begin a pull"
        );
        pull.begin();
        assert!(!pull.scroll(true, 0.0, -30.0));
        assert_eq!(pull.distance, 30.0);
        assert!(pull.scroll(true, 0.0, -42.0));
        assert!(!pull.scroll(true, 0.0, -100.0));
        pull.begin();
        assert!(pull.scroll(true, 0.0, -72.0));
    }

    #[test]
    fn normal_scrolling_horizontal_swipes_and_reversals_do_not_trigger_sync() {
        let mut pull = Gesture::default();
        pull.begin();
        assert!(!pull.scroll(false, 0.0, -200.0));
        assert_eq!(pull.distance, 0.0);
        assert!(!pull.scroll(true, 80.0, -20.0));
        assert!(!pull.scroll(true, 0.0, -40.0));
        assert!(!pull.scroll(true, 0.0, 50.0));
        assert_eq!(pull.distance, 0.0);
        assert!(!pull.scroll(true, 0.0, -40.0));
        assert!(!pull.scroll(false, 0.0, -40.0));
        assert_eq!(pull.distance, 0.0);
    }
}

#[cfg(test)]
pub(super) mod checks {
    use super::*;
    use gtk::gio;
    use std::cell::Cell;

    pub fn run() {
        let stack = gtk::Stack::new();
        let scroller = gtk::ScrolledWindow::new();
        stack.add_named(&scroller, Some("list"));
        let pull = Pull::new(&stack, &scroller);
        let calls = Rc::new(Cell::new(0));
        let fired = calls.clone();
        let action = gio::SimpleAction::new("check", None);
        action.connect_activate(move |_, _| fired.set(fired.get() + 1));
        let actions = gio::SimpleActionGroup::new();
        actions.add_action(&action);
        pull.widget.insert_action_group("win", Some(&actions));
        let controller = pull
            .widget
            .observe_controllers()
            .item(0)
            .unwrap()
            .downcast::<gtk::EventControllerScroll>()
            .unwrap();
        assert!(
            controller
                .flags()
                .contains(gtk::EventControllerScrollFlags::BOTH_AXES),
            "GTK must preserve both deltas for the direction check"
        );
        controller.emit_by_name::<()>("scroll-begin", &[]);
        for _ in 0..4 {
            controller.emit_by_name::<bool>("scroll", &[&100.0f64, &-20.0f64]);
        }
        assert!(!pull.indicator.is_visible());
        assert_eq!(calls.get(), 0);
        controller.emit_by_name::<()>("scroll-end", &[]);
        controller.emit_by_name::<()>("scroll-begin", &[]);
        controller.emit_by_name::<bool>("scroll", &[&0.0f64, &-0.25f64]);
        assert!(pull.indicator.is_visible());
        assert_eq!(calls.get(), 0);
        controller.emit_by_name::<bool>("scroll", &[&0.0f64, &-100.0f64]);
        controller.emit_by_name::<bool>("scroll", &[&0.0f64, &-100.0f64]);
        assert_eq!(calls.get(), 1);
        controller.emit_by_name::<()>("scroll-end", &[]);
        assert!(!pull.indicator.is_visible());
    }
}
