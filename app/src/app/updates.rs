//! Checking for a newer release, installing it, and restarting into it.
//! The decisions live in `crate::update`; this is where they meet the
//! window, the tray, and the clock.

use std::os::unix::process::CommandExt;
use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use adw::prelude::*;
use gtk::{gio, glib};
use mailrs_domain::translate::{fill, gettext};

use super::App;
use crate::settings::Change;
use crate::ui::window::Notice;
use crate::update::{self, Blockers, Restart, State, github, install, version};

/// The first timed check waits for the app to settle after it starts.
const FIRST_CHECK: u32 = 5 * 60;
/// How often the timer looks at the clock. The check itself runs once a day;
/// see `update::due`.
const TICK: u32 = 60 * 60;
/// How long an installed update waits for sync or Undo Send before it
/// looks again.
const RETRY_RESTART: Duration = Duration::from_secs(10);

impl App {
    /// Starts the daily check, and listens for the Install button on the
    /// notification that announces a release.
    pub(super) fn start_update_checks(self: &Rc<Self>) {
        if !self.can_update() {
            return;
        }
        self.install_update_actions();
        let weak = Rc::downgrade(self);
        glib::timeout_add_seconds_local_once(FIRST_CHECK, move || {
            if let Some(app) = weak.upgrade() {
                app.check_for_updates(false);
            }
        });
        let weak = Rc::downgrade(self);
        glib::timeout_add_seconds_local(TICK, move || match weak.upgrade() {
            Some(app) => {
                app.check_for_updates(false);
                glib::ControlFlow::Continue
            }
            None => glib::ControlFlow::Break,
        });
    }

    fn install_update_actions(self: &Rc<Self>) {
        let add = |name: &str, run: fn(&Rc<App>)| {
            let action = gio::SimpleAction::new(name, None);
            let weak = Rc::downgrade(self);
            action.connect_activate(move |_, _| {
                if let Some(app) = weak.upgrade() {
                    run(&app);
                }
            });
            self.gio.add_action(&action);
        };
        add("check-for-updates", |app| app.check_for_updates(true));
        add("install-update", |app| app.install_update());
        add("restart-for-update", |app| app.restart_for_update(true));
        add("update-log", |app| {
            if let Some(State::Failed { log, .. }) = app.updater.as_ref().map(|u| u.state()) {
                open(&gio::File::for_path(log).uri());
            }
        });
        add("update-notes", |app| app.open_release_notes());
    }

    /// Asks GitHub for the latest release. A check the person asked for
    /// always runs and always answers; a timed one runs once a day at most
    /// and speaks only when there is something to install.
    pub(crate) fn check_for_updates(self: &Rc<Self>, asked: bool) {
        let Some(updater) = self.updater.clone().filter(|u| u.updates_itself()) else {
            return;
        };
        if matches!(updater.state(), State::Installing(_) | State::Installed(_)) {
            return;
        }
        let settings = self.settings();
        let now = now();
        if !asked && !(settings.check_for_updates && update::due(settings.last_update_check, now)) {
            return;
        }
        self.change_settings(Change::UpdateChecked(now));
        let before = updater.state();
        if asked {
            self.set_update_state(State::Checking);
        }
        let client = updater.client();
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let found = this
                .core
                .call(async move { github::latest(&client).await })
                .await;
            let running = version::Version::running();
            match found {
                Ok(Some(release)) if release.version > running => {
                    if version::pick(&release, updater.method()).is_none() {
                        tracing::info!(version = %release.version, "the release has no file for this install");
                        return;
                    }
                    let shown = release.version.to_string();
                    let new = this.settings().announced_update.as_deref() != Some(shown.as_str());
                    if new || asked {
                        this.change_settings(Change::UpdateAnnounced(shown));
                        let (install, clicked) = async_channel::bounded(1);
                        update::announce(release.version, install);
                        let app = Rc::clone(&this);
                        glib::spawn_future_local(async move {
                            if clicked.recv().await.is_ok() {
                                app.install_update();
                            }
                        });
                    }
                    this.set_update_state(State::Available(release));
                }
                Ok(_) => {
                    this.set_update_state(State::Current);
                    if asked {
                        this.answer_update_check(fill(
                            &gettext("Penguin Mail {version} is up to date"),
                            &[("version", &running.to_string())],
                        ));
                    }
                }
                Err(err) => {
                    tracing::info!(error = %err, "could not check for updates");
                    if asked {
                        this.set_update_state(State::Unreachable);
                        this.answer_update_check(gettext(
                            "Could not reach GitHub to check for updates",
                        ));
                    } else {
                        // A check nobody asked for fails quietly and leaves
                        // whatever was offered before on offer.
                        this.set_update_state(before);
                    }
                }
            }
        });
    }

    /// Downloads the release this copy needs, checks it, and installs it.
    pub(crate) fn install_update(self: &Rc<Self>) {
        let Some(updater) = self.updater.clone() else {
            return;
        };
        let State::Available(release) = updater.state() else {
            return;
        };
        let Some(files) = version::pick(&release, updater.method()) else {
            return;
        };
        let version = release.version;
        let (package_name, package_asset, sums_asset) = (
            files.package.name.clone(),
            files.package.clone(),
            files.sums.clone(),
        );
        let work = glib::user_cache_dir()
            .join(mailrs_sync::config::DIR_NAME)
            .join("update")
            .join(version.to_string());
        self.set_update_state(State::Installing(version));
        let method = updater.method().clone();
        let client = updater.client();
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let outcome = this
                .core
                .call(async move {
                    // A leftover from an earlier try would be unpacked over.
                    let _ = tokio::fs::remove_dir_all(&work).await;
                    tokio::fs::create_dir_all(&work).await?;
                    let log = work.join("install.log");
                    let failed = |reason: String| install::Failed {
                        reason,
                        log: log.clone(),
                    };
                    let package = work.join(&package_name);
                    let fetched = async {
                        github::download(&client, &package_asset, &package).await?;
                        github::text(&client, &sums_asset).await
                    }
                    .await;
                    let result = match fetched {
                        Err(err) => Err(failed(err.to_string())),
                        Ok(sums) => match install::verify(&package, &sums, &package_name) {
                            Err(reason) => Err(failed(reason)),
                            Ok(()) => install::run(&method, version, &package, &work).await,
                        },
                    };
                    if let Err(failed) = &result {
                        // The log says why even when the installer never ran.
                        let _ = tokio::fs::write(&failed.log, format!("{}\n", failed.reason)).await;
                    }
                    anyhow::Ok(result)
                })
                .await;
            match outcome {
                Ok(Ok(())) => {
                    tracing::info!(%version, "installed the update");
                    this.set_update_state(State::Installed(version));
                    this.restart_for_update(false);
                }
                Ok(Err(failed)) => {
                    tracing::warn!(%version, reason = %failed.reason, "the update failed");
                    this.set_update_state(State::Failed {
                        version,
                        log: failed.log,
                    });
                }
                Err(err) => {
                    tracing::warn!(%version, error = %err, "the update failed");
                    this.set_update_state(State::Available(release));
                }
            }
        });
    }

    /// Starts the new binary in this process's place. Sync and Undo Send
    /// finish first; an open composer waits for the person, since drafts
    /// save only when they ask. `asked` means they clicked Restart.
    fn restart_for_update(self: &Rc<Self>, asked: bool) {
        let Some(updater) = &self.updater else {
            return;
        };
        if !matches!(updater.state(), State::Installed(_)) {
            return;
        }
        let window_open = self.window.borrow().is_some();
        let blockers = Blockers {
            syncing: self.core.busy(),
            sending: self.pending_sends.get() > 0,
            composing: self.open_windows.get() > usize::from(window_open),
        };
        let decision = match update::when_to_restart(blockers) {
            Restart::Ask if asked => Restart::Later,
            decision => decision,
        };
        match decision {
            Restart::Ask => {}
            Restart::Later => {
                let weak = Rc::downgrade(self);
                glib::timeout_add_local_once(RETRY_RESTART, move || {
                    if let Some(app) = weak.upgrade() {
                        app.restart_for_update(asked);
                    }
                });
            }
            Restart::Now => {
                let Ok(exe) = crate::exe::launcher() else {
                    return;
                };
                // exec keeps the process, so the new copy takes over the
                // app's D-Bus name without racing the old one for it.
                let mut command = std::process::Command::new(exe);
                if !window_open {
                    command.arg("--background");
                }
                tracing::info!("restarting into the update");
                let err = command.exec();
                tracing::warn!(error = %err, "could not restart into the update");
            }
        }
    }

    /// Where an update stands, when this copy updates at all.
    pub fn update_state(&self) -> Option<State> {
        self.updater.as_ref().map(|u| u.state())
    }

    /// Answers a check the person asked for: in the window when it is open,
    /// in a notification when they asked from the tray with no window.
    fn answer_update_check(&self, text: String) {
        match self.window() {
            Some(window) => window.notice(Notice::UpdateChecked(text)),
            None => update::tell(text),
        }
    }

    /// Whether this copy updates itself: not the demo, not a cargo build,
    /// and not a Flatpak or a snap, whose store does it.
    pub fn can_update(&self) -> bool {
        self.updater.as_ref().is_some_and(|u| u.updates_itself())
    }

    pub(crate) fn open_release_notes(&self) {
        if let Some(State::Available(release)) = self.updater.as_ref().map(|u| u.state()) {
            open(&release.page);
        }
    }

    fn set_update_state(self: &Rc<Self>, state: State) {
        let Some(updater) = &self.updater else {
            return;
        };
        updater.set(state.clone());
        self.tell_window(Notice::Update(&state));
        let label = match &state {
            State::Available(release) => Some(release.version.to_string()),
            _ => None,
        };
        let handle = self.tray.lock().expect("tray slot poisoned").clone();
        if let Some(handle) = handle {
            self.core.spawn(async move {
                handle
                    .update(move |tray: &mut crate::tray::MailTray| tray.update = label)
                    .await;
            });
        }
    }
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

fn open(uri: &str) {
    if let Err(err) = gio::AppInfo::launch_default_for_uri(uri, None::<&gio::AppLaunchContext>) {
        tracing::warn!(error = %err, uri, "could not open");
    }
}
