//! Files from this computer attached to events. The editor uploads a
//! picked file at once through [`CalendarCopy::upload`], with a byte
//! count for its progress bar. A file the editor could not upload, for
//! want of a network, stays in the event as a waiting attachment that
//! names its path; the queue uploads it before it sends the event
//! ([`CalendarCopy::upload_waiting`]). A file that has moved by then
//! stays off the event, and the send says so.
//!
//! The upload needs `drive.file`, which sign-in asks for. Reading the
//! attachments an event already has needs only the calendar permission.

use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use mailrs_domain::AccountId;
use mailrs_domain::calendar::{self, Attachment, Event};
use mailrs_store::calendar as store;

use super::CalendarCopy;
use crate::settings::Permitted;
use crate::{Accounts, AnyCalendar, BackendError, CalendarService, SyncError};

impl<A: Accounts> CalendarCopy<A> {
    /// Uploads `file`, a waiting attachment the person just picked, to the
    /// account's Drive and answers it linked. `sent` counts the bytes as
    /// they go out. An account that has not granted Drive answers
    /// `NeedsPermission` without sending anything; a network failure
    /// answers the error, and the editor then keeps the file waiting for
    /// the queue.
    pub async fn upload(
        &self,
        account_id: AccountId,
        file: Attachment,
        sent: Arc<AtomicU64>,
    ) -> Result<Permitted<Attachment>, SyncError> {
        let services = self.accounts.services(account_id).ok_or(SyncError::UnknownAccount(account_id))?;
        if services.withheld().drive {
            return Ok(Permitted::NeedsPermission);
        }
        let calendar = services.calendar.ok_or(SyncError::Backend(BackendError::Unsupported))?;
        match calendar.upload_attachment(&file, sent).await {
            Ok(done) => Ok(Permitted::Done(done)),
            Err(BackendError::NeedsPermission) => Ok(Permitted::NeedsPermission),
            Err(err) => Err(err.into()),
        }
    }

    /// Uploads the waiting files in queued change `change`'s `body`, and
    /// answers the body to send with each one linked, and the titles of
    /// the files no longer at their paths, which stay off the event. What
    /// went up is written to the queue and the copy before anything else,
    /// so a write that then fails does not upload it again. The inner
    /// error is the upload's own, for the send to treat as it treats the
    /// write's.
    pub(super) async fn upload_waiting(
        &self,
        calendar: &AnyCalendar,
        change: &store::QueuedChange,
        mut body: Event,
    ) -> Result<Result<(Event, Vec<String>), BackendError>, SyncError> {
        let waiting: Vec<Attachment> =
            body.attachments.iter().flatten().filter(|file| file.waiting.is_some()).cloned().collect();
        if waiting.is_empty() {
            return Ok(Ok((body, Vec::new())));
        }
        let (mut uploaded, mut missing, mut left_out) = (Vec::new(), Vec::new(), Vec::new());
        let mut failed = None;
        for file in waiting {
            let path = file.waiting.clone().unwrap_or_default();
            match calendar.upload_attachment(&file, Arc::default()).await {
                Ok(done) => uploaded.push((path, done)),
                Err(BackendError::FileMissing(_)) => {
                    missing.push(path);
                    left_out.push(file.title);
                }
                Err(err) => {
                    failed = Some(err);
                    break;
                }
            }
        }
        let (account_id, seq) = (change.account_id, change.seq);
        let (cal, id) = (change.calendar.clone(), change.event.clone());
        let (settled, gone) = (uploaded.clone(), missing.clone());
        self.db
            .write(move |c| store::settle_uploads(c, account_id, seq, &cal, &id, &settled, &gone))
            .await?;
        if let Some(files) = &mut body.attachments {
            calendar::settle_uploads(files, &uploaded, &missing);
        }
        Ok(match failed {
            Some(err) => Err(err),
            None => Ok((body, left_out)),
        })
    }
}
