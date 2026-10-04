//! How often a Microsoft account looks at its mail: the Inbox every 30
//! seconds while the window is open, every other synced folder every 5
//! minutes, and everything every 15 minutes while only the tray runs.
//! Graph cannot push to the app, so the looks are all there is.

use std::time::Duration;

use tokio::time::Instant;

use super::{GraphApi, Microsoft};
use mailrs_domain::Role;

const INBOX_EVERY: Duration = Duration::from_secs(30);
const OTHERS_EVERY: Duration = Duration::from_secs(5 * 60);
const TRAY_EVERY: Duration = Duration::from_secs(15 * 60);

impl<G: GraphApi> Microsoft<G> {
    pub(super) fn poll_every(&self) -> Duration {
        match self.known().window_open {
            true => INBOX_EVERY,
            false => TRAY_EVERY,
        }
    }

    /// The folders this look covers, and whether the slow poll is due: the
    /// Inbox each time, the rest when the slow poll comes round, and all of
    /// them at every look while only the tray runs.
    pub(super) fn due(&self, synced: Vec<String>) -> (Vec<String>, bool) {
        let known = self.known();
        let slow = !known.window_open || known.last_slow.is_none_or(|at| at.elapsed() >= OTHERS_EVERY);
        let inbox = known.roles.get(&Role::Inbox).cloned();
        let due = match slow {
            true => synced,
            false => synced.into_iter().filter(|f| Some(f) == inbox.as_ref()).collect(),
        };
        (due, slow)
    }

    pub(super) fn slow_poll_done(&self) {
        self.known().last_slow = Some(Instant::now());
    }
}
