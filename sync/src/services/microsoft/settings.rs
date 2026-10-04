//! The automatic reply, which Outlook keeps in the mailbox settings.

use mailrs_domain::Vacation;

use super::{GraphApi, Microsoft};
use crate::BackendError;
use crate::services::AutoReplyService;

impl<G: GraphApi> AutoReplyService for Microsoft<G> {
    async fn vacation(&self) -> Result<Vacation, BackendError> {
        Err(BackendError::Unsupported)
    }

    async fn set_vacation(&self, _vacation: &Vacation) -> Result<(), BackendError> {
        Err(BackendError::Unsupported)
    }
}
