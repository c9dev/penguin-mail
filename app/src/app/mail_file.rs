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
    /// [`ACCOUNTS_WAIT`] when it has not. The load tells each waiter once
    /// it is done, so the wait wakes exactly then rather than polling.
    async fn accounts_loaded(&self) {
        if self.accounts_read.get() {
            return;
        }
        let (tell, told) = futures::channel::oneshot::channel();
        self.account_waiters.borrow_mut().push(tell);
        let _ = glib::future_with_timeout(ACCOUNTS_WAIT, told).await;
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
                    let window = match host_after_wait(window, this.window()) {
                        Host::Open(window) => window,
                        Host::Closed(window) => {
                            this.park(&window);
                            window
                        }
                    };
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

/// The window a file opens on once its wait is over.
enum Host<T> {
    /// A main window open now: the one held across the wait, or one the
    /// person opened after closing it.
    Open(Rc<T>),
    /// The window held across the wait, which closed meanwhile with no
    /// window of its own to keep it parked. It hosts the file once it is
    /// parked, as `forget_window` would have parked it had the file's
    /// window already been up; the file's window lets it go when it closes.
    Closed(Rc<T>),
}

fn host_after_wait<T>(held: Rc<T>, current: Option<Rc<T>>) -> Host<T> {
    match current {
        Some(current) => Host::Open(current),
        None => Host::Closed(held),
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use super::{Host, host_after_wait};

    #[test]
    fn the_main_window_open_after_the_wait_hosts_the_file() {
        let (held, current) = (Rc::new("held"), Rc::new("current"));
        match host_after_wait(held, Some(Rc::clone(&current))) {
            Host::Open(host) => assert!(Rc::ptr_eq(&host, &current)),
            Host::Closed(_) => panic!("the open window was passed over"),
        }
    }

    #[test]
    fn a_main_window_closed_during_the_wait_hosts_the_file_once_parked() {
        let held = Rc::new("held");
        match host_after_wait(Rc::clone(&held), None) {
            Host::Closed(host) => assert!(Rc::ptr_eq(&host, &held)),
            Host::Open(_) => panic!("a closed window was taken for open"),
        }
    }
}
