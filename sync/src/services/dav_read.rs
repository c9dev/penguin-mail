//! Reading a DAV collection a page at a time. The first call of a read
//! lists what changed; the hrefs wait here, per collection, and each call
//! hands out the next batch by a page token that names the read. One read
//! per collection is kept, so a read the copy dropped half way is replaced
//! by the next one and never piles up.

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, PoisonError};

use mailrs_dav::DavError;

use crate::BackendError;

pub(super) const TOKEN_SYNC: &str = "sync:";
pub(super) const TOKEN_CTAG: &str = "ctag:";

#[derive(Default)]
pub(super) struct Reads {
    next: u64,
    by_collection: HashMap<String, Pending>,
}

struct Pending {
    serial: u64,
    queue: VecDeque<String>,
    removed: Vec<String>,
    token: String,
}

pub(super) struct Taken {
    pub hrefs: Vec<String>,
    /// Handed out with the first batch only.
    pub removed: Vec<String>,
    pub last: bool,
    pub token: String,
}

impl Reads {
    pub(super) fn start(
        &mut self,
        collection: &str,
        hrefs: Vec<String>,
        removed: Vec<String>,
        token: String,
    ) -> u64 {
        self.next += 1;
        let serial = self.next;
        self.by_collection.insert(
            collection.to_string(),
            Pending { serial, queue: hrefs.into(), removed, token },
        );
        serial
    }

    pub(super) fn take(&mut self, collection: &str, serial: u64, batch: usize) -> Option<Taken> {
        let pending = self.by_collection.get_mut(collection).filter(|p| p.serial == serial)?;
        let count = batch.min(pending.queue.len());
        let hrefs: Vec<String> = pending.queue.drain(..count).collect();
        let removed = std::mem::take(&mut pending.removed);
        let last = pending.queue.is_empty();
        let token = pending.token.clone();
        if last {
            self.by_collection.remove(collection);
        }
        Some(Taken { hrefs, removed, last, token })
    }
}

/// A DAV failure as a backend error. A refused login is recorded in
/// `refused` for the Preferences row and held as `NeedsReauth`, which
/// keeps the calendar's queue waiting rather than dropping its changes.
pub(super) fn backend(err: DavError, refused: &Mutex<Option<String>>) -> BackendError {
    match err {
        DavError::Unauthorized => {
            *refused.lock().unwrap_or_else(PoisonError::into_inner) = Some(err.to_string());
            BackendError::NeedsReauth
        }
        DavError::Forbidden(words) => BackendError::Refused(words),
        DavError::NotFound => BackendError::NotFound,
        DavError::Changed => BackendError::Changed,
        DavError::InvalidSyncToken | DavError::NoSyncCollection => BackendError::StateLost,
        DavError::Busy(wait) => BackendError::RateLimited(wait),
        DavError::Network(detail) => BackendError::Offline(detail),
        DavError::Http { status, detail } if status >= 500 => {
            BackendError::Offline(format!("HTTP {status}: {detail}"))
        }
        other => BackendError::Refused(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_read_hands_out_batches_then_forgets_itself() {
        let mut reads = Reads::default();
        let serial = reads.start(
            "/cal/",
            (0..5).map(|n| format!("/cal/{n}.ics")).collect(),
            vec!["/cal/gone.ics".into()],
            "sync:t".into(),
        );
        let first = reads.take("/cal/", serial, 2).unwrap();
        assert_eq!((first.hrefs.len(), first.removed.len(), first.last), (2, 1, false));
        let rest = reads.take("/cal/", serial, 10).unwrap();
        assert!(rest.last && rest.removed.is_empty());
        assert!(reads.take("/cal/", serial, 10).is_none(), "a finished read is gone");
    }

    #[test]
    fn a_new_read_of_a_collection_replaces_the_old_one() {
        let mut reads = Reads::default();
        let old = reads.start("/cal/", vec!["/cal/a.ics".into()], Vec::new(), String::new());
        reads.start("/cal/", vec!["/cal/b.ics".into()], Vec::new(), String::new());
        assert!(reads.take("/cal/", old, 10).is_none());
    }

    #[test]
    fn a_refused_login_is_recorded_and_held_for_a_new_password() {
        let refused = Mutex::new(None);
        assert!(matches!(backend(DavError::Unauthorized, &refused), BackendError::NeedsReauth));
        assert!(refused.lock().unwrap().is_some());
        assert!(matches!(backend(DavError::InvalidSyncToken, &refused), BackendError::StateLost));
        assert!(matches!(
            backend(DavError::Http { status: 502, detail: "bad gateway".into() }, &refused),
            BackendError::Offline(_)
        ));
    }
}
