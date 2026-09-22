//! Finding a newer release, installing it the way this copy was installed,
//! and restarting into it. The app owns one [`Updater`] unless it runs as
//! the demo or from a cargo build, which never update. A Flatpak or a snap
//! has one that leaves everything to its store.

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

/// What the window shows for an update: the main menu's entry, and the
/// banner above the panes when there is news. Each button runs an app
/// action, so the window needs no callbacks of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shown {
    pub menu: MenuEntry,
    pub banner: Option<Banner>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuEntry {
    pub label: String,
    /// None greys the entry out, which is right while a check or an
    /// install is running.
    pub action: Option<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Banner {
    pub title: String,
    /// The button's label and the app action it runs.
    pub button: Option<(String, &'static str)>,
}

/// How the window shows `state`.
pub fn shown(state: &State) -> Shown {
    let entry = |label: String, action| MenuEntry { label, action };
    let menu = match state {
        State::Available(release) => entry(
            fill(
                &gettext("Install Update {version}"),
                &[("version", &release.version.to_string())],
            ),
            Some("app.install-update"),
        ),
        State::Installing(_) => entry(gettext("Installing Update…"), None),
        State::Installed(_) => entry(gettext("Restart to Update"), Some("app.restart-for-update")),
        State::Failed { .. } => entry(gettext("Show Update Log"), Some("app.update-log")),
        State::Checking => entry(gettext("Checking for Updates…"), None),
        State::Idle | State::Current | State::Unreachable => {
            entry(gettext("Check for Updates"), Some("app.check-for-updates"))
        }
    };
    let banner = match state {
        // These answer a check; the About window and a toast say so.
        State::Idle | State::Checking | State::Current | State::Unreachable => None,
        State::Available(release) => Some(Banner {
            title: fill(
                &gettext("Penguin Mail {version} is available"),
                &[("version", &release.version.to_string())],
            ),
            button: Some((gettext("Install"), "app.install-update")),
        }),
        State::Installing(version) => Some(Banner {
            title: fill(
                &gettext("Installing Penguin Mail {version}"),
                &[("version", &version.to_string())],
            ),
            button: None,
        }),
        State::Installed(_) => Some(Banner {
            title: gettext("Restart to finish updating"),
            button: Some((gettext("Restart"), "app.restart-for-update")),
        }),
        State::Failed { version, .. } => Some(Banner {
            title: fill(
                &gettext("The update to {version} failed"),
                &[("version", &version.to_string())],
            ),
            button: Some((gettext("Show Log"), "app.update-log")),
        }),
    };
    Shown { menu, banner }
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
        let appimage = std::env::var_os("APPIMAGE").map(PathBuf::from);
        install::method_for(crate::packaging::BUILT_FOR, &exe, appimage.as_deref())
            .map(Updater::new)
    }

    /// Whether this copy fetches and installs releases itself. A store
    /// install has an updater that does nothing, so the window, the tray
    /// and Preferences can ask one question.
    pub fn updates_itself(&self) -> bool {
        self.method.store().is_none()
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

    fn version(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn a_release_on_offer_shows_in_the_menu_and_the_banner() {
        let release = Release {
            version: version("0.2.0"),
            page: String::new(),
            assets: Vec::new(),
        };
        let shown = shown(&State::Available(release));
        assert_eq!(shown.menu.label, "Install Update 0.2.0");
        assert_eq!(shown.menu.action, Some("app.install-update"));
        let banner = shown.banner.expect("a banner");
        assert_eq!(banner.title, "Penguin Mail 0.2.0 is available");
        assert_eq!(
            banner.button,
            Some(("Install".to_string(), "app.install-update"))
        );
    }

    #[test]
    fn an_answered_check_leaves_the_banner_down_and_offers_another() {
        for state in [State::Idle, State::Current, State::Unreachable] {
            let shown = shown(&state);
            assert_eq!(shown.banner, None, "{state:?}");
            assert_eq!(shown.menu.label, "Check for Updates");
            assert_eq!(shown.menu.action, Some("app.check-for-updates"));
        }
    }

    #[test]
    fn running_work_greys_the_menu_entry_out() {
        let checking = shown(&State::Checking);
        assert_eq!(checking.menu.action, None);
        assert_eq!(checking.banner, None);
        let installing = shown(&State::Installing(version("0.2.0")));
        assert_eq!(installing.menu.action, None);
        let banner = installing.banner.expect("a banner");
        assert_eq!(banner.title, "Installing Penguin Mail 0.2.0");
        assert_eq!(banner.button, None);
    }

    #[test]
    fn an_installed_update_offers_a_restart_and_a_failed_one_its_log() {
        let installed = shown(&State::Installed(version("0.2.0")));
        assert_eq!(installed.menu.action, Some("app.restart-for-update"));
        assert_eq!(
            installed.banner.and_then(|b| b.button),
            Some(("Restart".to_string(), "app.restart-for-update"))
        );
        let failed = shown(&State::Failed {
            version: version("0.2.0"),
            log: PathBuf::from("/tmp/install.log"),
        });
        assert_eq!(failed.menu.label, "Show Update Log");
        let banner = failed.banner.expect("a banner");
        assert_eq!(banner.title, "The update to 0.2.0 failed");
        assert_eq!(
            banner.button,
            Some(("Show Log".to_string(), "app.update-log"))
        );
    }

    #[test]
    fn a_timed_check_waits_a_day_after_the_last() {
        assert!(due(None, 1_000));
        assert!(!due(Some(1_000), 1_000 + 3_600));
        assert!(due(Some(1_000), 1_000 + DAY));
    }
}
