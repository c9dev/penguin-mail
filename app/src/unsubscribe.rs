//! Leaving a mailing list lives in `mailrs_sync::unsubscribe`. The app
//! keeps these names at their old path for the assistant's tools.

pub use mailrs_sync::unsubscribe::{Unsubscribe, choose_with_body};

/// Where the mail asking a list to let go stands once the outbox has
/// taken it. A list hears nothing until the mail leaves, so only `Sent`
/// counts as unsubscribed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestSent {
    Sent,
    /// Gmail could not take it yet, and it waits in the Outbox.
    Waiting,
}
