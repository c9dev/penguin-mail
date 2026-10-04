//! The account's address book over Graph.

use mailrs_gmail::{ConnectionsPage, ContactFields, Person};

use super::{GraphApi, Microsoft};
use crate::BackendError;
use crate::services::ContactsService;

impl<G: GraphApi> ContactsService for Microsoft<G> {
    async fn connections(
        &self,
        _page_token: Option<&str>,
        _sync_token: Option<&str>,
    ) -> Result<ConnectionsPage, BackendError> {
        Err(BackendError::Unsupported)
    }

    async fn contact_photo(&self, _url: &str) -> Result<Vec<u8>, BackendError> {
        Err(BackendError::Unsupported)
    }

    async fn create_contact(&self, _fields: &ContactFields) -> Result<Person, BackendError> {
        Err(BackendError::Unsupported)
    }

    async fn update_contact(&self, _resource: &str, _fields: &ContactFields) -> Result<Person, BackendError> {
        Err(BackendError::Unsupported)
    }
}
