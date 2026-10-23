//! A mail file handed to the app: from Files, by the desktop entry's
//! `message/rfc822`, or by a second launch that the running copy answers.
//!
//! The message opens in a window of its own over the main window, which
//! comes up first when none is open: everything a reading window does,
//! from Reply to the attachment rows, hangs off the main window, and the
//! person chose to see it rather than have those lifted off it.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use gtk::gio::prelude::ApplicationExtManual;
use gtk::prelude::WidgetExt;
use gtk::{gio, glib};
use mailrs_domain::translate::gettext;

use super::App;
use crate::open_thread::{Loose, Origin};
use crate::ui::eml_file;

/// How long a file waits for the first account load before going on
/// with what is known: a store that does not answer by then will not.
const ACCOUNTS_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

impl App {
    /// Resolves once the first account load has answered, or after
    /// [`ACCOUNTS_WAIT`] when it has not.
    async fn accounts_loaded(&self) {
        let started = std::time::Instant::now();
        while !self.accounts_read.get() && started.elapsed() < ACCOUNTS_WAIT {
            glib::timeout_future(std::time::Duration::from_millis(50)).await;
        }
    }

    /// Opens `path` as a loose message, acting as the default account,
    /// or says why the file will not open.
    pub(super) fn open_mail_file(self: &Rc<Self>, path: &Path) {
        crate::ensure_gtk();
        self.apply_style();
        let path = path.to_path_buf();
        let (this, hold) = (Rc::clone(self), self.gio.hold());
        glib::spawn_future_local(async move {
            let reading = path.clone();
            let read = gio::spawn_blocking(move || eml_file::read_message(&reading)).await;
            match read {
                Ok(Ok(raw)) => {
                    // An open main window is left as it is; one is made
                    // only when there is none to hang the message on.
                    let window = this.window().unwrap_or_else(|| this.show_window());
                    // The window keeps the account it opens with, so the
                    // first account load must have answered: a launch on
                    // a file asks before it has.
                    this.accounts_loaded().await;
                    // The wait is long enough to close a window in, and
                    // the one held here may be gone from the app by now.
                    let window =
                        host_after_wait(window, this.window(), &mut this.parked.borrow_mut());
                    let account = this.default_account(None);
                    window.open_loose(Loose {
                        raw: Arc::new(raw),
                        account_id: account.unwrap_or_default(),
                        has_account: account.is_some(),
                        origin: Origin::File(path),
                    });
                }
                Ok(Err(why)) => this.show_unreadable(&path, &why),
                Err(_) => this.show_unreadable(&path, &gettext("This file could not be read.")),
            }
            drop(hold);
        });
    }

    /// A window saying the file could not be opened, counted among the
    /// windows that keep the process from shedding its memory, and
    /// holding the application for as long as it is up, as the calendar
    /// file's window does.
    fn show_unreadable(self: &Rc<Self>, path: &Path, why: &str) {
        self.window_opened();
        let hold = RefCell::new(Some(self.gio.hold()));
        let window = eml_file::show_unreadable(path, why);
        let app = Rc::downgrade(self);
        window.connect_destroy(move |_| {
            hold.borrow_mut().take();
            if let Some(app) = app.upgrade() {
                app.window_closed();
            }
        });
    }
}

/// The window a file opens on once its wait is over: the main window
/// open now, when there is one, since the person may have closed and
/// reopened it meanwhile. Otherwise `held`, the window that was open when
/// the wait began. It closed during the wait with no window of its own
/// to keep it parked, so it is parked here, as `forget_window` would have
/// done, before it hosts one. The loose window lets it go when it closes.
fn host_after_wait<T>(held: Rc<T>, current: Option<Rc<T>>, parked: &mut Vec<Rc<T>>) -> Rc<T> {
    if let Some(current) = current {
        return current;
    }
    if !parked.iter().any(|p| Rc::ptr_eq(p, &held)) {
        parked.push(Rc::clone(&held));
    }
    held
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use super::host_after_wait;

    #[test]
    fn the_main_window_open_after_the_wait_hosts_the_file() {
        let (held, current) = (Rc::new("held"), Rc::new("current"));
        let mut parked = Vec::new();
        let host = host_after_wait(Rc::clone(&held), Some(Rc::clone(&current)), &mut parked);
        assert!(Rc::ptr_eq(&host, &current));
        assert!(parked.is_empty());
    }

    #[test]
    fn a_main_window_closed_during_the_wait_is_parked_and_hosts_the_file() {
        let held = Rc::new("held");
        let mut parked = Vec::new();
        let host = host_after_wait(Rc::clone(&held), None, &mut parked);
        assert!(Rc::ptr_eq(&host, &held));
        assert_eq!(parked.len(), 1);
        assert!(Rc::ptr_eq(&parked[0], &held));
    }

    #[test]
    fn a_window_already_parked_is_not_parked_twice() {
        let held = Rc::new("held");
        let mut parked = vec![Rc::clone(&held)];
        host_after_wait(Rc::clone(&held), None, &mut parked);
        assert_eq!(parked.len(), 1);
    }
}
