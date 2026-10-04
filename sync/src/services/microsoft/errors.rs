//! Graph's errors as the backend errors every provider shares.

use mailrs_domain::translate::gettext;
use mailrs_graph::GraphError;

use crate::BackendError;

pub(crate) fn backend(err: GraphError) -> BackendError {
    match err {
        GraphError::NeedsReauth => BackendError::NeedsReauth,
        GraphError::Network(detail) => BackendError::Offline(detail),
        GraphError::Throttled { retry_after } => BackendError::RateLimited(retry_after),
        GraphError::NotFound => BackendError::NotFound,
        GraphError::SyncStateLost => BackendError::StateLost,
        GraphError::PreconditionFailed | GraphError::Conflict => BackendError::Changed,
        GraphError::AccessDenied { .. } => BackendError::NeedsPermission,
        GraphError::MailboxOnPremises => BackendError::Refused(gettext(
            "This mailbox is on your organization's own Exchange server, which Penguin Mail cannot reach.",
        )),
        other => BackendError::Refused(other.to_string()),
    }
}
