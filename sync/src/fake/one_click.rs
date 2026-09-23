//! The one-click fake: it remembers each URL it was asked to post to, and
//! refuses when a test says so.

use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::{BackendError, SyncError};

#[derive(Default)]
pub struct FakeOneClick {
    state: Mutex<Posts>,
}

#[derive(Default)]
struct Posts {
    posted: Vec<String>,
    refusals: usize,
}

impl FakeOneClick {
    /// The URLs posted to, oldest first.
    pub fn posted(&self) -> Vec<String> {
        self.lock().posted.clone()
    }

    /// Makes the next post fail, as a list's server that answers 500 does.
    pub fn refuse_next(&self) {
        self.lock().refusals += 1;
    }

    pub(crate) fn post(&self, url: &str) -> Result<(), SyncError> {
        let mut posts = self.lock();
        if posts.refusals > 0 {
            posts.refusals -= 1;
            return Err(BackendError::Refused(format!("{url} answered 500")).into());
        }
        posts.posted.push(url.to_string());
        Ok(())
    }

    fn lock(&self) -> MutexGuard<'_, Posts> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
