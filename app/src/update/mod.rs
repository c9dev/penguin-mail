//! Finding a newer release, installing it the way this copy was installed,
//! and restarting into it. The app owns one [`Updater`] unless it runs as
//! the demo or from a cargo build, which never update.

pub mod github;
pub mod install;
pub mod version;

use std::cell::RefCell;
use std::path::PathBuf;

use mailrs_domain::translate::{fill, gettext};

use crate::APP_ID;
use version::{Method, Release, Version};

/// Where an update stands. The window's banner and the tray read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Idle,
    /// A check the person asked for is on its way.
    Checking,
    /// The last check found nothing newer than this copy.
    Current,
    /// The last check the person asked for could not reach GitHub.
    Unreachable,
    Available(Release),
    Installing(Version),
    /// Installed, and waiting for the app to restart into it.
    Installed(Version),
    Failed {
        version: Version,
        log: PathBuf,
    },
}

/// What keeps an installed update from restarting the app this moment.
#[derive(Debug, Clone, Copy)]
pub struct Blockers {
    pub syncing: bool,
    /// A message is inside its Undo Send delay.
    pub sending: bool,
    pub composing: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Restart {
    Now,
    /// Try again shortly: the work in the way finishes on its own.
    Later,
    /// Leave it to the person. Drafts save only when they ask, so restarting
    /// under an open composer would lose what they wrote.
    Ask,
}

pub fn when_to_restart(blockers: Blockers) -> Restart {
    if blockers.composing {
        Restart::Ask
    } else if blockers.syncing || blockers.sending {
        Restart::Later
    } else {
        Restart::Now
    }
}

const DAY: i64 = 86_400;

/// Whether a timed check should run. The app restarts itself whenever its
/// window has been closed a while, so a check on every start would ask
/// GitHub many times a day.
pub fn due(last: Option<i64>, now: i64) -> bool {
    last.is_none_or(|last| now - last >= DAY)
}

pub struct Updater {
    method: Method,
    client: reqwest::Client,
    state: RefCell<State>,
}

impl Updater {
    /// An updater for the running binary, or none when this copy never
    /// updates: the demo, and a cargo build. A demo pointed at a test
    /// release server with PENGUIN_MAIL_RELEASES_URL gets one, so the update
    /// screens can be seen and tested without a real account.
    pub fn for_this_copy(demo: bool) -> Option<Updater> {
        if demo && std::env::var_os("PENGUIN_MAIL_RELEASES_URL").is_none() {
            return None;
        }
        let exe = crate::exe::path().ok()?;
        install::method_for(&exe).map(Updater::new)
    }

    pub fn new(method: Method) -> Updater {
        Updater {
            method,
            client: github::client(),
            state: RefCell::new(State::Idle),
        }
    }

    pub fn method(&self) -> &Method {
        &self.method
    }

    pub fn client(&self) -> reqwest::Client {
        self.client.clone()
    }

    pub fn state(&self) -> State {
        self.state.borrow().clone()
    }

    pub fn set(&self, state: State) {
        *self.state.borrow_mut() = state;
    }
}

/// Says a release is out, with an Install button that sends `()` on
/// `install`. The daemon's reply arrives on its own thread, as it does for
/// new mail.
pub fn announce(version: Version, install: async_channel::Sender<()>) {
    std::thread::spawn(move || {
        let mut notification = notify_rust::Notification::new();
        notification
            .appname("Penguin Mail")
            .summary(&fill(
                &gettext("Penguin Mail {version} is available"),
                &[("version", &version.to_string())],
            ))
            .body(&gettext("Install it now, or later from the tray."))
            .icon(APP_ID)
            .hint(notify_rust::Hint::DesktopEntry(APP_ID.into()))
            .action("install", &gettext("Install"));
        match notification.show() {
            Ok(handle) => handle.wait_for_action(|key| {
                if key == "install" {
                    let _ = install.send_blocking(());
                }
            }),
            Err(err) => tracing::warn!(error = %err, "could not announce the update"),
        }
    });
}

/// A notification with nothing to click, for the answer to Check for
/// Updates.
pub fn tell(summary: String) {
    std::thread::spawn(move || {
        let shown = notify_rust::Notification::new()
            .appname("Penguin Mail")
            .summary(&summary)
            .icon(APP_ID)
            .hint(notify_rust::Hint::DesktopEntry(APP_ID.into()))
            .show();
        if let Err(err) = shown {
            tracing::warn!(error = %err, "could not show a notification");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_open_composer_asks_and_busy_work_waits() {
        let b = |syncing, sending, composing| Blockers {
            syncing,
            sending,
            composing,
        };
        assert_eq!(when_to_restart(b(false, false, false)), Restart::Now);
        assert_eq!(when_to_restart(b(true, false, false)), Restart::Later);
        assert_eq!(when_to_restart(b(false, true, false)), Restart::Later);
        assert_eq!(when_to_restart(b(true, true, true)), Restart::Ask);
    }

    #[test]
    fn a_timed_check_waits_a_day_after_the_last() {
        assert!(due(None, 1_000));
        assert!(!due(Some(1_000), 1_000 + 3_600));
        assert!(due(Some(1_000), 1_000 + DAY));
    }
}
