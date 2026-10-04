//! The account's calendar over Graph.

use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use mailrs_domain::EpochMillis;
use mailrs_domain::calendar as model;
use mailrs_domain::invitation::Answer;
use mailrs_gmail::{Answered, Busy, Event, EventFields, Series};

use super::{GraphApi, Microsoft};
use crate::BackendError;
use crate::services::CalendarService;

impl<G: GraphApi> CalendarService for Microsoft<G> {
    async fn answer_invitation(
        &self,
        _ical_uid: &str,
        _me: &str,
        _answer: Answer,
        _occurrence: Option<EpochMillis>,
        _note: Option<&str>,
    ) -> Result<Answered, BackendError> {
        Err(BackendError::Unsupported)
    }

    async fn busy_between(&self, _from: EpochMillis, _to: EpochMillis) -> Result<Vec<Busy>, BackendError> {
        Err(BackendError::Unsupported)
    }

    async fn series(&self, _ical_uid: &str, _from: EpochMillis) -> Result<Option<Series>, BackendError> {
        Err(BackendError::Unsupported)
    }

    async fn events_between(&self, _from: EpochMillis, _to: EpochMillis) -> Result<Vec<Event>, BackendError> {
        Err(BackendError::Unsupported)
    }

    async fn create_event(&self, _fields: &EventFields) -> Result<Event, BackendError> {
        Err(BackendError::Unsupported)
    }

    async fn update_event(&self, _id: &str, _fields: &EventFields) -> Result<Event, BackendError> {
        Err(BackendError::Unsupported)
    }

    async fn delete_event(&self, _id: &str) -> Result<(), BackendError> {
        Err(BackendError::Unsupported)
    }

    async fn calendars(&self) -> Result<Vec<model::Calendar>, BackendError> {
        Err(BackendError::Unsupported)
    }

    async fn event_changes(
        &self,
        _calendar: &str,
        _token: Option<&str>,
        _page: Option<&str>,
        _from: EpochMillis,
    ) -> Result<model::EventPage, BackendError> {
        Err(BackendError::Unsupported)
    }

    async fn event_range(
        &self,
        _calendar: &str,
        _from: EpochMillis,
        _to: EpochMillis,
        _page: Option<&str>,
    ) -> Result<model::EventPage, BackendError> {
        Err(BackendError::Unsupported)
    }

    async fn put_event(
        &self,
        _event: &model::Event,
        _etag: Option<&str>,
        _create: bool,
        _notify: model::Notify,
    ) -> Result<model::Event, BackendError> {
        Err(BackendError::Unsupported)
    }

    async fn remove_event(
        &self,
        _calendar: &str,
        _id: &str,
        _etag: Option<&str>,
        _notify: model::Notify,
    ) -> Result<(), BackendError> {
        Err(BackendError::Unsupported)
    }

    async fn import_event(&self, _event: &model::Event) -> Result<model::Event, BackendError> {
        Err(BackendError::Unsupported)
    }

    async fn upload_attachment(
        &self,
        _file: &model::Attachment,
        _sent: Arc<AtomicU64>,
    ) -> Result<model::Attachment, BackendError> {
        Err(BackendError::Unsupported)
    }

    async fn share_file(&self, _file_id: &str, _email: &str) -> Result<(), BackendError> {
        Err(BackendError::Unsupported)
    }

    async fn move_event(
        &self,
        _event: &model::Event,
        _destination: &str,
        _notify: model::Notify,
    ) -> Result<model::Event, BackendError> {
        Err(BackendError::Unsupported)
    }

    async fn answer_event(
        &self,
        _calendar: &str,
        _id: &str,
        _me: &str,
        _answer: Answer,
        _note: Option<&str>,
    ) -> Result<model::Event, BackendError> {
        Err(BackendError::Unsupported)
    }

    async fn edit_list(
        &self,
        _calendar: &str,
        _edit: &model::list::ListEdit,
    ) -> Result<Option<model::Calendar>, BackendError> {
        Err(BackendError::Unsupported)
    }
}
