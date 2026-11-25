//! A calendar file handed to the app: from Files, by the desktop entry's
//! `text/calendar`, or by a second launch that the running copy answers.

use std::path::Path;
use std::rc::Rc;

use gtk::gio::prelude::ApplicationExtManual;
use gtk::glib::object::ObjectExt;
use gtk::prelude::{GtkWindowExt, WidgetExt};

use super::App;

impl App {
    /// Opens `path` in a window of its own, counted among the windows that
    /// keep the process from shedding its memory in the background. A file
    /// that already has a window comes up in it: the card hands such a
    /// file back to the desktop whenever its events cannot be added here,
    /// and the desktop's own entry hands it straight back to this app, so
    /// opening another window every time would stack window upon window
    /// without ever adding anything.
    pub(super) fn open_calendar_file(self: &Rc<Self>, path: &Path) {
        let held = self
            .calendar_files
            .borrow()
            .iter()
            .find(|(shown, _)| shown.as_path() == path)
            .and_then(|(_, window)| window.upgrade());
        if let Some(window) = held {
            window.present();
            return;
        }
        crate::ensure_gtk();
        self.apply_style();
        self.window_opened();
        // The application is a plain GApplication, which quits when nothing
        // holds it, and a window of ours holds nothing. The hold lasts as
        // long as the window does.
        let hold = std::cell::RefCell::new(Some(self.gio.hold()));
        let window = crate::ui::ics_file::open(self, path);
        self.calendar_files
            .borrow_mut()
            .push((path.to_path_buf(), window.downgrade()));
        let app = Rc::downgrade(self);
        let shown = path.to_path_buf();
        window.connect_destroy(move |_| {
            hold.borrow_mut().take();
            if let Some(app) = app.upgrade() {
                // One window per file, so the entry that came with this
                // window is the one that leaves.
                app.calendar_files
                    .borrow_mut()
                    .retain(|(open, _)| open != &shown);
                app.window_closed();
            }
        });
    }
}
