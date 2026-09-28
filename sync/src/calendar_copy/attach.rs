//! Files from this computer attached to events. The editor uploads a
//! picked file at once through [`CalendarCopy::upload`], with a byte
//! count for its progress bar. A file the editor could not upload, for
//! want of a network, stays in the event as a waiting attachment that
//! names its path; the queue uploads it before it sends the event
//! ([`CalendarCopy::prepare_attachments`]).
//!
//! A file that will not upload does not hold anything else up. Without
//! Drive access it waits, marked, and the queue tries again once the
//! account grants it; a file that moved or cannot be read, or one Drive
//! refuses, stays marked until the person takes it off. The event's other
//! changes go out either way, and so does every other queued write. Only
//! a failure that may pass, such as the network going, keeps the change
//! queued, as it would keep the write itself.
//!
//! Before the event goes out, each guest who cannot open a file the app
//! uploaded yet is made a reader of it, unless the person turned sharing
//! off for that file. A sharing failure never stops the write; the next
//! save tries again.
//!
//! The upload and sharing need `drive.file`, which sign-in asks for.
//! Reading the attachments an event already has needs only the calendar
//! permission.

use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use mailrs_domain::AccountId;
use mailrs_domain::calendar::{self, Attachment, Event, Notify, UploadProblem};
use mailrs_gmail::GmailError;
use mailrs_store::calendar as store;

use super::CalendarCopy;
use crate::settings::Permitted;
use crate::{Accounts, AnyCalendar, BackendError, CalendarService, SyncError};

impl<A: Accounts> CalendarCopy<A> {
    /// Uploads `file`, a waiting attachment the person just picked, to the
    /// account's Drive and answers it linked and shared with the guests.
    /// `sent` counts the bytes as they go out. An account that has not
    /// granted Drive answers `NeedsPermission` without sending anything; a
    /// network failure answers the error, and the editor then keeps the
    /// file waiting for the queue.
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
            Ok(done) => Ok(Permitted::Done(Attachment { share: file.share.or(Some(true)), ..done })),
            Err(BackendError::NeedsPermission) => Ok(Permitted::NeedsPermission),
            Err(err) => Err(err.into()),
        }
    }

    /// Queues again, quietly, each event holding a file that waited for
    /// Drive access, once the account has granted it, so the next walk of
    /// the queue uploads the file.
    pub(super) async fn retry_waiting_for_access(&self, account_id: AccountId) -> Result<(), SyncError> {
        if self.drive_withheld(account_id)? {
            return Ok(());
        }
        self.db
            .write(move |c| {
                for (calendar, id) in store::waiting_for_access(c, account_id)? {
                    let Some(mut event) = store::event(c, account_id, &calendar, &id)? else { continue };
                    for file in event.attachments.iter_mut().flatten() {
                        if file.problem == Some(UploadProblem::NeedsAccess) {
                            file.problem = None;
                        }
                    }
                    event.pending = true;
                    store::save_events(c, account_id, std::slice::from_ref(&event), crate::now_millis())?;
                    // The guests heard of the event when it went out; the
                    // file joining it is no news worth a second mail.
                    store::enqueue_after(c, account_id, store::ChangeKind::Save, &event, None, None, Notify::Nobody)?;
                }
                Ok(())
            })
            .await?;
        Ok(())
    }

    /// Readies queued change `change`'s `body` to send: uploads its waiting
    /// files, marks the ones that will not go, and makes the guests readers
    /// of the files the app uploaded. Answers the body to send and the
    /// titles of files no longer at their paths, for the window to name.
    /// What changed is written to the queue and the copy before anything
    /// else, so a write that then fails repeats none of it. The inner error
    /// is a failure that may pass, which keeps the change queued.
    pub(super) async fn prepare_attachments(
        &self,
        calendar: &AnyCalendar,
        change: &store::QueuedChange,
        mut body: Event,
    ) -> Result<Result<(Event, Vec<String>), BackendError>, SyncError> {
        let Some(mut files) = body.attachments.clone() else {
            return Ok(Ok((body, Vec::new())));
        };
        let withheld = self.drive_withheld(change.account_id)?;
        let mut left_out = Vec::new();
        let mut failed = None;
        for file in files.iter_mut().filter(|file| file.waiting.is_some() && file.problem.is_none()) {
            if withheld {
                file.problem = Some(UploadProblem::NeedsAccess);
                continue;
            }
            match calendar.upload_attachment(file, Arc::default()).await {
                Ok(done) => *file = Attachment { share: file.share.or(Some(true)), ..done },
                Err(BackendError::FileMissing(_) | BackendError::Gmail(GmailError::File(_))) => {
                    file.problem = Some(UploadProblem::NotFound);
                    left_out.push(file.title.clone());
                }
                Err(BackendError::NeedsPermission) => file.problem = Some(UploadProblem::NeedsAccess),
                Err(err) if err.is_transient() || matches!(err, BackendError::NeedsReauth) => {
                    failed = Some(err);
                    break;
                }
                Err(err) => file.problem = Some(UploadProblem::Refused(err.to_string())),
            }
        }
        if failed.is_none() && !withheld {
            self.share(calendar, &mut files, &body.guests).await;
        }
        if Some(&files) != body.attachments.as_ref() {
            let (account_id, seq) = (change.account_id, change.seq);
            let (cal, id, list) = (change.calendar.clone(), change.event.clone(), files.clone());
            self.db
                .write(move |c| store::replace_attachments(c, account_id, seq, &cal, &id, &list))
                .await?;
            body.attachments = Some(files);
        }
        Ok(match failed {
            Some(err) => Err(err),
            None => Ok((body, left_out)),
        })
    }

    /// Makes each guest a reader of every file in `files` the app uploaded
    /// and may share, and notes who got it. A refusal leaves that guest
    /// for the next save.
    async fn share(&self, calendar: &AnyCalendar, files: &mut [Attachment], guests: &[calendar::Guest]) {
        for file in files.iter_mut() {
            let wanted: Vec<String> = calendar::to_share(file, guests).into_iter().map(str::to_string).collect();
            for email in wanted {
                match calendar.share_file(&file.file_id, &email).await {
                    Ok(()) => file.shared_with.push(email),
                    Err(err) => {
                        tracing::warn!(error = %err, "could not share an attached file with a guest");
                        if err.is_transient() || matches!(err, BackendError::NeedsPermission) {
                            return;
                        }
                    }
                }
            }
        }
    }

    fn drive_withheld(&self, account_id: AccountId) -> Result<bool, SyncError> {
        Ok(self.accounts.services(account_id).ok_or(SyncError::UnknownAccount(account_id))?.withheld().drive)
    }
}
