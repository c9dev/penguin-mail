//! Moving, marking, tagging and erasing mail, and making and changing folders.

use mailrs_domain::RemoteMailbox;
use mailrs_gmail::LabelColor;

use super::{GraphApi, Microsoft};
use crate::services::{Relocated, Unapplied};
use crate::{BackendError, MailOp};

impl<G: GraphApi> Microsoft<G> {
    pub(super) async fn write(&self, _messages: &[String], _ops: &[MailOp]) -> Result<Vec<Relocated>, Unapplied> {
        Err(Unapplied { taken: 0, error: BackendError::Unsupported, relocated: Vec::new() })
    }

    pub(super) async fn make_folder(&self, _name: &str) -> Result<RemoteMailbox, BackendError> {
        Err(BackendError::Unsupported)
    }

    pub(super) async fn rename_folder(&self, _id: &str, _name: &str) -> Result<RemoteMailbox, BackendError> {
        Err(BackendError::Unsupported)
    }

    pub(super) async fn remove_folder(&self, _id: &str) -> Result<(), BackendError> {
        Err(BackendError::Unsupported)
    }

    pub(super) async fn color_tag(&self, _id: &str, _color: &LabelColor) -> Result<RemoteMailbox, BackendError> {
        Err(BackendError::Unsupported)
    }
}
