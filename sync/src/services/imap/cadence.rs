//! How often an IMAP account looks at its mail. One connection waits in
//! IDLE on the Inbox (RFC 2177) and wakes the engine when the server
//! reports a change; the other mailboxes wait for the slow poll, every
//! five minutes while the window is open and every fifteen while only the
//! tray runs. A server without IDLE has its Inbox polled every minute.

use std::time::Duration;

use mailrs_domain::Role;
use mailrs_imap::Woke;
use tokio::time::Instant;

use super::{Imap, ImapApi, Submit};
use crate::BackendError;
use crate::services::MailBackend;

/// How often the Inbox is looked at on a server without IDLE.
const INBOX_POLL: Duration = Duration::from_secs(60);

/// How long one IDLE lasts before it is issued again. RFC 2177 asks for
/// less than 29 minutes, since servers drop a quiet connection at 30.
const IDLE_FOR: Duration = Duration::from_secs(25 * 60);

/// How long the first failed watch in a row waits before it lets the
/// Inbox be looked at, which makes a broken IDLE a poll each minute
/// rather than a busy loop.
const WATCH_RETRY: Duration = Duration::from_secs(60);

/// The longest a repeated IDLE failure waits: the slow poll's own longest
/// pace, past which IDLE would buy nothing a plain poll does not already
/// give.
const WATCH_RETRY_CAP: Duration = Duration::from_secs(15 * 60);

/// How long the watch waits after `failures` IDLE attempts in a row have
/// failed (0 for the first), doubling each time and capped at
/// [`WATCH_RETRY_CAP`], so a server that refuses or drops every IDLE is
/// asked less and less often instead of in a busy loop.
fn watch_retry(failures: u32) -> Duration {
    WATCH_RETRY
        .saturating_mul(1u32 << failures.min(8))
        .min(WATCH_RETRY_CAP)
}

/// How often mailboxes other than the Inbox are looked at.
pub(super) fn slow_poll(tray_only: bool) -> Duration {
    match tray_only {
        true => Duration::from_secs(15 * 60),
        false => Duration::from_secs(5 * 60),
    }
}

/// How often the engine reads the feed: at the slow poll's pace where IDLE
/// watches the Inbox, and every minute where nothing does.
pub(super) fn poll_every(idle: bool, tray_only: bool) -> Duration {
    match idle {
        true => slow_poll(tray_only),
        false => INBOX_POLL,
    }
}

impl<I: ImapApi, S: Submit> Imap<I, S> {
    /// The mailboxes this look covers, and whether it is the slow poll's
    /// turn: the Inbox every time, the rest when the slow poll is due.
    pub(super) async fn due(&self) -> Result<(Vec<String>, bool), BackendError> {
        let synced = self.synced().await?;
        let slow = {
            let known = self.known();
            known.every_look
                || known
                    .last_slow
                    .is_none_or(|at| at.elapsed() >= slow_poll(known.tray_only))
        };
        let due = match slow {
            true => synced,
            false => synced
                .into_iter()
                .filter(|m| m.eq_ignore_ascii_case("INBOX"))
                .collect(),
        };
        Ok((due, slow))
    }

    /// Notes that the slow poll has looked at every synced mailbox.
    pub(super) fn slow_poll_done(&self) {
        self.known().last_slow = Some(Instant::now());
    }

    /// Waits in IDLE on the Inbox until the server reports a change,
    /// issuing IDLE again every 25 minutes. A server without IDLE never
    /// ends this wait. A watch that fails waits and ends, so the look it
    /// wakes is the poll; repeated failures wait longer each time, so a
    /// server that keeps refusing or dropping IDLE is asked less and less
    /// rather than in a busy loop. An IDLE the guard dropped past its
    /// budget ends the watch at once, so the engine syncs what the server
    /// reported, and counts as a failure: the next watch waits before it
    /// issues IDLE again. A change or a timeout forgets the failures.
    pub(super) async fn watch_inbox(&self) {
        let pause = self.known().idle_pause.take();
        if let Some(pause) = pause {
            tokio::time::sleep(pause).await;
        }
        let idle = match self.capabilities_now().await {
            Ok(capabilities) => capabilities.idle,
            Err(err) => {
                tracing::warn!(%err, "could not read what the server offers to watch the Inbox");
                tokio::time::sleep(WATCH_RETRY).await;
                return;
            }
        };
        if !idle {
            return std::future::pending().await;
        }
        let inbox = self
            .mailbox_for(Role::Inbox)
            .unwrap_or_else(|| "INBOX".into());
        loop {
            match self.api.idle(&inbox, IDLE_FOR).await {
                Ok(Woke::Dropped) => {
                    let mut known = self.known();
                    known.idle_failures = known.idle_failures.saturating_add(1);
                    let wait = watch_retry(known.idle_failures - 1);
                    known.idle_pause = Some(wait);
                    tracing::warn!(
                        ?wait,
                        "the server sent more during IDLE than the client takes; syncing, then waiting before the next IDLE"
                    );
                    return;
                }
                Ok(woke) => {
                    self.known().idle_failures = 0;
                    match woke {
                        Woke::TimedOut => continue,
                        Woke::Changed | Woke::Dropped => return,
                    }
                }
                Err(err) => {
                    let err = BackendError::from(err);
                    let failures = {
                        let mut known = self.known();
                        known.idle_failures = known.idle_failures.saturating_add(1);
                        known.idle_failures
                    };
                    let wait = watch_retry(failures - 1);
                    tracing::warn!(%err, ?wait, "the Inbox watch failed; polling it instead");
                    tokio::time::sleep(wait).await;
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{poll_every, slow_poll, watch_retry};

    #[test]
    fn the_inbox_is_watched_or_polled_each_minute_and_the_rest_waits_longer() {
        assert_eq!(slow_poll(false), Duration::from_secs(5 * 60));
        assert_eq!(slow_poll(true), Duration::from_secs(15 * 60));
        assert_eq!(poll_every(true, false), Duration::from_secs(5 * 60));
        assert_eq!(poll_every(true, true), Duration::from_secs(15 * 60));
        assert_eq!(poll_every(false, false), Duration::from_secs(60));
        assert_eq!(poll_every(false, true), Duration::from_secs(60));
    }

    #[test]
    fn a_watch_that_keeps_failing_waits_longer_each_time_up_to_the_slow_pace() {
        assert_eq!(watch_retry(0), Duration::from_secs(60));
        assert_eq!(watch_retry(1), Duration::from_secs(120));
        assert_eq!(watch_retry(2), Duration::from_secs(240));
        assert_eq!(watch_retry(10), Duration::from_secs(15 * 60));
    }
}
