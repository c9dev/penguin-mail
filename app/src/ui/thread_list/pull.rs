//! A deliberate overscroll at the top runs the same action as Check for Mail.
use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::{gdk, glib};
use mailrs_domain::translate::gettext;

const DISTANCE: f64 = 140.0;
const MIN_PULL_TIME: Duration = Duration::from_millis(350);

#[derive(Default)]
struct Gesture {
    active: bool,
    distance: f64,
    since: Option<Instant>,
    fired: bool,
}

impl Gesture {
    fn begin(&mut self) {
        *self = Self {
            active: true,
            ..Self::default()
        };
    }

    fn scroll(&mut self, at_top: bool, dx: f64, dy: f64, now: Instant) -> bool {
        if !self.active || self.fired {
            return false;
        }
        if !at_top || !dx.is_finite() || !dy.is_finite() || dy >= 0.0 || dx.abs() > dy.abs() {
            self.distance = 0.0;
            self.since = None;
            return false;
        }
        let since = *self.since.get_or_insert(now);
        self.distance = (self.distance - dy).min(DISTANCE);
        self.fired = self.distance >= DISTANCE && now.duration_since(since) >= MIN_PULL_TIME;
        self.fired
    }
}

pub(super) struct Pull {
    pub widget: gtk::Overlay,
    gesture: RefCell<Gesture>,
    scroll: gtk::EventControllerScroll,
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
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
        scroll.set_propagation_phase(gtk::PropagationPhase::None);
        let this = Rc::new(Self {
            widget,
            indicator,
            progress,
            label,
            gesture: RefCell::new(Gesture::default()),
            scroll: scroll.clone(),
        });
        let weak = Rc::downgrade(&this);
        scroll.connect_scroll_begin(move |controller| {
            if controller.propagation_phase() == gtk::PropagationPhase::Capture
                && let Some(this) = weak.upgrade()
            {
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
            let fire =
                this.gesture
                    .borrow_mut()
                    .scroll(at_top, dx * scale, dy * scale, Instant::now());
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

    pub fn set_enabled(&self, enabled: bool) {
        self.reset();
        self.scroll.set_propagation_phase(if enabled {
            gtk::PropagationPhase::Capture
        } else {
            gtk::PropagationPhase::None
        });
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
    fn a_pull_needs_both_distance_and_time_and_fires_once() {
        let mut pull = Gesture::default();
        let start = Instant::now();
        assert!(!pull.scroll(true, 0.0, -500.0, start));
        pull.begin();
        assert!(!pull.scroll(true, 0.0, -70.0, start));
        assert!(!pull.scroll(true, 0.0, -69.0, start + MIN_PULL_TIME));
        assert!(pull.scroll(true, 0.0, -1.0, start + MIN_PULL_TIME));
        assert!(!pull.scroll(true, 0.0, -500.0, start + MIN_PULL_TIME));
    }

    #[test]
    fn a_fast_flick_never_checks_even_when_it_travels_far() {
        let mut pull = Gesture::default();
        let start = Instant::now();
        pull.begin();
        assert!(!pull.scroll(true, 0.0, -400.0, start));
        assert!(!pull.scroll(true, 0.0, -400.0, start + Duration::from_millis(100)));
        pull = Gesture::default();
        assert!(!pull.scroll(true, 0.0, -400.0, start + Duration::from_secs(1)));
    }

    #[test]
    fn scrolling_to_the_top_does_not_count_toward_the_pull_time() {
        let mut pull = Gesture::default();
        let start = Instant::now();
        pull.begin();
        assert!(!pull.scroll(false, 0.0, -500.0, start));
        let top = start + Duration::from_secs(2);
        assert!(!pull.scroll(true, 0.0, -500.0, top));
        assert!(!pull.scroll(true, 0.0, -10.0, top + Duration::from_millis(100)));
        assert!(pull.scroll(true, 0.0, -10.0, top + MIN_PULL_TIME));
    }

    #[test]
    fn changing_direction_or_leaving_the_top_resets_distance_and_time() {
        let start = Instant::now();
        for (at_top, dx, dy) in [
            (true, 80.0, -20.0),
            (true, 0.0, 1.0),
            (false, 0.0, -5.0),
            (true, f64::NAN, -5.0),
        ] {
            let mut pull = Gesture::default();
            pull.begin();
            assert!(!pull.scroll(true, 0.0, -100.0, start));
            assert!(!pull.scroll(at_top, dx, dy, start + MIN_PULL_TIME));
            assert_eq!(pull.distance, 0.0);
            assert!(pull.since.is_none());
            assert!(!pull.scroll(true, 0.0, -500.0, start + MIN_PULL_TIME));
        }
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
        assert_eq!(controller.propagation_phase(), gtk::PropagationPhase::None);
        controller.emit_by_name::<()>("scroll-begin", &[]);
        controller.emit_by_name::<bool>("scroll", &[&0.0f64, &-500.0f64]);
        assert_eq!(calls.get(), 0);
        assert!(!pull.indicator.is_visible());
        pull.set_enabled(true);
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
        controller.emit_by_name::<bool>("scroll", &[&0.0f64, &-500.0f64]);
        assert_eq!(calls.get(), 0, "a fast flick does not check");
        pull.gesture.borrow_mut().since = Some(Instant::now() - MIN_PULL_TIME);
        controller.emit_by_name::<bool>("scroll", &[&0.0f64, &-1.0f64]);
        assert_eq!(calls.get(), 1);
        pull.set_enabled(false);
        assert!(!pull.indicator.is_visible());
        controller.emit_by_name::<()>("scroll-begin", &[]);
        controller.emit_by_name::<bool>("scroll", &[&0.0f64, &-500.0f64]);
        assert_eq!(calls.get(), 1);
        pull.set_enabled(true);
        controller.emit_by_name::<bool>("scroll", &[&0.0f64, &-500.0f64]);
        assert_eq!(calls.get(), 1, "enabling needs a fresh gesture");
        controller.emit_by_name::<()>("scroll-end", &[]);
        assert!(!pull.indicator.is_visible());
    }
}
