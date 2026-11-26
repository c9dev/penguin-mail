//! Visible confirmation on a copy button.
//!
//! A screen reader hears an announcement when something is copied, but a
//! person looking at the screen saw nothing change and took the button for
//! broken. `Copied::show` swaps the button's icon for a check mark and its
//! label for "Copied" for a moment, then puts both back.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;
use mailrs_domain::translate::gettext;

/// How long the button stays in its copied state.
const HOLD: Duration = Duration::from_secs(2);

/// One pending "put it back" timer. Starting another replaces it.
#[derive(Default)]
struct Hold {
    pending: RefCell<Option<glib::SourceId>>,
}

impl Hold {
    fn is_running(&self) -> bool {
        self.pending.borrow().is_some()
    }

    /// Runs `expire` after `delay`, dropping any timer still waiting.
    fn restart(self: &Rc<Self>, delay: Duration, expire: impl FnOnce() + 'static) {
        if let Some(old) = self.pending.take() {
            old.remove();
        }
        let this = Rc::clone(self);
        let id = glib::timeout_add_local_once(delay, move || {
            this.pending.take();
            expire();
        });
        self.pending.replace(Some(id));
    }
}

/// What a button showed before it said "Copied".
enum Original {
    Content { icon: String, label: String },
    Icon(String),
    Label(String),
}

struct Saved {
    original: Original,
    width_request: i32,
}

/// The copied state of one button. Make it once, next to the button, and
/// call `show` each time the button copies something.
pub struct Copied {
    button: glib::WeakRef<gtk::Button>,
    hold: Rc<Hold>,
    saved: Rc<RefCell<Option<Saved>>>,
}

impl Copied {
    pub fn new(button: &gtk::Button) -> Self {
        Self {
            button: button.downgrade(),
            hold: Rc::default(),
            saved: Rc::default(),
        }
    }

    /// Shows the copied state, or keeps showing it for another two seconds.
    /// Does nothing once the button is gone.
    pub fn show(&self) {
        let Some(button) = self.button.upgrade() else {
            return;
        };
        if !self.hold.is_running() {
            self.saved.replace(Some(Saved {
                original: read(&button),
                width_request: button.width_request(),
            }));
            // The button keeps the width it had, so "Copied" being
            // shorter than its label does not shift the row around it.
            let width = button.width();
            if width > 0 {
                button.set_width_request(width);
            }
            write(&button, "penguin-mail-object-select-symbolic", &gettext("Copied"));
        }
        let (weak, saved) = (self.button.clone(), Rc::clone(&self.saved));
        self.hold.restart(HOLD, move || {
            let (Some(button), Some(saved)) = (weak.upgrade(), saved.take()) else {
                return;
            };
            restore(&button, saved);
        });
    }
}

fn content(button: &gtk::Button) -> Option<adw::ButtonContent> {
    button.child().and_downcast::<adw::ButtonContent>()
}

fn read(button: &gtk::Button) -> Original {
    if let Some(content) = content(button) {
        Original::Content {
            icon: content.icon_name().to_string(),
            label: content.label().to_string(),
        }
    } else if let Some(icon) = button.icon_name() {
        Original::Icon(icon.to_string())
    } else {
        Original::Label(button.label().unwrap_or_default().to_string())
    }
}

fn write(button: &gtk::Button, icon: &str, label: &str) {
    if let Some(content) = content(button) {
        content.set_icon_name(icon);
        content.set_label(label);
    } else if button.icon_name().is_some() {
        button.set_icon_name(icon);
    } else {
        button.set_label(label);
    }
}

fn restore(button: &gtk::Button, saved: Saved) {
    button.set_width_request(saved.width_request);
    match saved.original {
        Original::Content { icon, label } => write(button, &icon, &label),
        Original::Icon(icon) => button.set_icon_name(&icon),
        Original::Label(label) => button.set_label(&label),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hold_is_not_running_before_it_starts() {
        assert!(!Hold::default().is_running());
    }
}

/// Checks that need the default main context, which the GTK checks' thread
/// owns. A timer set from a test's own thread fires on that thread instead.
#[cfg(test)]
pub(crate) mod checks {
    use super::*;
    use std::cell::Cell;

    pub fn run() {
        restarting_the_hold_runs_only_the_last_expiry();
    }

    fn wait(context: &glib::MainContext, until: impl Fn() -> bool) {
        let end = std::time::Instant::now() + Duration::from_secs(3);
        while !until() && std::time::Instant::now() < end {
            context.iteration(false);
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn restarting_the_hold_runs_only_the_last_expiry() {
        let context = glib::MainContext::default();
        let hold = Rc::new(Hold::default());
        let ran = Rc::new(Cell::new(0));
        for mark in [1, 2] {
            let ran = Rc::clone(&ran);
            hold.restart(Duration::from_millis(20), move || ran.set(mark));
        }
        assert!(hold.is_running());
        wait(&context, || !hold.is_running());
        assert_eq!(ran.get(), 2);
    }
}
