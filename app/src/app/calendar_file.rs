//! A calendar file handed to the app: from Files, by the desktop entry's
//! `text/calendar`, or by a second launch that the running copy answers.

use std::path::Path;
use std::rc::Rc;

use gtk::gio::prelude::ApplicationExtManual;
use gtk::prelude::WidgetExt;

use super::App;

impl App {
    /// Opens `path` in a window of its own, counted among the windows that
    /// keep the process from shedding its memory in the background.
    pub(super) fn open_calendar_file(self: &Rc<Self>, path: &Path) {
        crate::ensure_gtk();
        self.apply_style();
        self.window_opened();
        // The application is a plain GApplication, which quits when nothing
        // holds it, and a window of ours holds nothing. The hold lasts as
        // long as the window does.
        let hold = std::cell::RefCell::new(Some(self.gio.hold()));
        let window = crate::ui::ics_file::open(self, path);
        let app = Rc::downgrade(self);
        window.connect_destroy(move |_| {
            hold.borrow_mut().take();
            if let Some(app) = app.upgrade() {
                app.window_closed();
            }
        });
    }
}
