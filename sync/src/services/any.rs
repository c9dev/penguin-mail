//! The closed enums over each service's adapters. Every adapter is known
//! when the app compiles, so each enum forwards a call to the adapter it
//! holds with a `match`: no boxed futures and no trait objects. `Google`
//! holds the adapter over the real client; `Fake` holds the same adapter
//! over `FakeGmail`, for tests and the demo.

use std::time::Duration;

use mailrs_domain::invitation::Answer;
use mailrs_domain::{EpochMillis, Filter, MessageBody, MessageMeta, Vacation};
use mailrs_gmail::{
    Answered, Busy, ConnectionsPage, ContactFields, Event, EventFields, HistoryPage, LabelColor,
    MessagePage, Person, Profile, RemoteLabel, Series,
};

use super::{
    AutoReplyService, CalendarService, ContactsService, Google, IdentityService, MailBackend,
    MailCapabilities, RulesService, SendAsAddress,
};
use crate::BackendError;
use crate::api::{AccountClient, DraftRef, SavedDraft};
#[cfg(any(test, feature = "fake"))]
use crate::fake::FakeGmail;

/// Awaits `$method` on whichever adapter `$self`, an `$enum`, holds.
macro_rules! forward {
    ($enum:ident, $self:ident, $method:ident($($arg:expr),*)) => {
        match $self {
            $enum::Google(adapter) => adapter.$method($($arg),*).await,
            #[cfg(any(test, feature = "fake"))]
            $enum::Fake(adapter) => adapter.$method($($arg),*).await,
        }
    };
}

/// An account's mail.
#[derive(Clone)]
pub enum AnyMail {
    Google(Google<AccountClient>),
    #[cfg(any(test, feature = "fake"))]
    Fake(Google<FakeGmail>),
}

/// An account's calendar.
#[derive(Clone)]
pub enum AnyCalendar {
    Google(Google<AccountClient>),
    #[cfg(any(test, feature = "fake"))]
    Fake(Google<FakeGmail>),
}

/// An account's address book.
#[derive(Clone)]
pub enum AnyContacts {
    Google(Google<AccountClient>),
    #[cfg(any(test, feature = "fake"))]
    Fake(Google<FakeGmail>),
}

/// The rules an account's server runs.
#[derive(Clone)]
pub enum AnyRules {
    Google(Google<AccountClient>),
    #[cfg(any(test, feature = "fake"))]
    Fake(Google<FakeGmail>),
}

/// An account's automatic reply.
#[derive(Clone)]
pub enum AnyAutoReply {
    Google(Google<AccountClient>),
    #[cfg(any(test, feature = "fake"))]
    Fake(Google<FakeGmail>),
}

/// The addresses an account sends as.
#[derive(Clone)]
pub enum AnyIdentities {
    Google(Google<AccountClient>),
    #[cfg(any(test, feature = "fake"))]
    Fake(Google<FakeGmail>),
}

impl MailBackend for AnyMail {
    fn capabilities(&self) -> MailCapabilities {
        match self {
            AnyMail::Google(adapter) => adapter.capabilities(),
            #[cfg(any(test, feature = "fake"))]
            AnyMail::Fake(adapter) => adapter.capabilities(),
        }
    }

    fn person_waiting(&self) -> bool {
        match self {
            AnyMail::Google(adapter) => adapter.person_waiting(),
            #[cfg(any(test, feature = "fake"))]
            AnyMail::Fake(adapter) => adapter.person_waiting(),
        }
    }

    async fn stand_by(&self, wait: Duration) {
        forward!(AnyMail, self, stand_by(wait))
    }

    async fn profile(&self) -> Result<Profile, BackendError> {
        forward!(AnyMail, self, profile())
    }

    async fn labels(&self) -> Result<Vec<RemoteLabel>, BackendError> {
        forward!(AnyMail, self, labels())
    }

    async fn list_messages(
        &self,
        query: &str,
        page_token: Option<&str>,
        page_size: u32,
    ) -> Result<MessagePage, BackendError> {
        forward!(AnyMail, self, list_messages(query, page_token, page_size))
    }

    async fn list_labelled(
        &self,
        label_id: &str,
        query: &str,
        page_token: Option<&str>,
        page_size: u32,
    ) -> Result<MessagePage, BackendError> {
        forward!(
            AnyMail,
            self,
            list_labelled(label_id, query, page_token, page_size)
        )
    }

    async fn message_metadata(&self, id: &str) -> Result<MessageMeta, BackendError> {
        forward!(AnyMail, self, message_metadata(id))
    }

    async fn thread_metadata(&self, thread_id: &str) -> Result<Vec<MessageMeta>, BackendError> {
        forward!(AnyMail, self, thread_metadata(thread_id))
    }

    async fn message_body(&self, id: &str) -> Result<MessageBody, BackendError> {
        forward!(AnyMail, self, message_body(id))
    }

    async fn history(
        &self,
        start_history_id: u64,
        page_token: Option<&str>,
    ) -> Result<HistoryPage, BackendError> {
        forward!(AnyMail, self, history(start_history_id, page_token))
    }

    async fn modify_labels(
        &self,
        id: &str,
        add: &[String],
        remove: &[String],
    ) -> Result<(), BackendError> {
        forward!(AnyMail, self, modify_labels(id, add, remove))
    }

    async fn batch_modify(
        &self,
        ids: &[String],
        add: &[String],
        remove: &[String],
    ) -> Result<(), BackendError> {
        forward!(AnyMail, self, batch_modify(ids, add, remove))
    }

    async fn delete_messages(&self, ids: &[String]) -> Result<(), BackendError> {
        forward!(AnyMail, self, delete_messages(ids))
    }

    async fn send(&self, raw: &[u8], thread_id: Option<&str>) -> Result<String, BackendError> {
        forward!(AnyMail, self, send(raw, thread_id))
    }

    async fn save_draft(
        &self,
        draft_id: Option<&str>,
        raw: &[u8],
        thread_id: Option<&str>,
    ) -> Result<SavedDraft, BackendError> {
        forward!(AnyMail, self, save_draft(draft_id, raw, thread_id))
    }

    async fn send_draft(&self, draft_id: &str) -> Result<String, BackendError> {
        forward!(AnyMail, self, send_draft(draft_id))
    }

    async fn delete_draft(&self, draft_id: &str) -> Result<(), BackendError> {
        forward!(AnyMail, self, delete_draft(draft_id))
    }

    async fn list_drafts(&self) -> Result<Vec<DraftRef>, BackendError> {
        forward!(AnyMail, self, list_drafts())
    }

    async fn attachment(
        &self,
        message_id: &str,
        attachment_id: &str,
    ) -> Result<Vec<u8>, BackendError> {
        forward!(AnyMail, self, attachment(message_id, attachment_id))
    }

    async fn raw_message(&self, id: &str) -> Result<Vec<u8>, BackendError> {
        forward!(AnyMail, self, raw_message(id))
    }

    async fn create_label(&self, name: &str) -> Result<RemoteLabel, BackendError> {
        forward!(AnyMail, self, create_label(name))
    }

    async fn rename_label(&self, id: &str, name: &str) -> Result<RemoteLabel, BackendError> {
        forward!(AnyMail, self, rename_label(id, name))
    }

    async fn delete_label(&self, id: &str) -> Result<(), BackendError> {
        forward!(AnyMail, self, delete_label(id))
    }

    async fn set_label_color(
        &self,
        id: &str,
        color: &LabelColor,
    ) -> Result<RemoteLabel, BackendError> {
        forward!(AnyMail, self, set_label_color(id, color))
    }

    async fn label_threads(&self, id: &str) -> Result<u64, BackendError> {
        forward!(AnyMail, self, label_threads(id))
    }
}

impl CalendarService for AnyCalendar {
    async fn answer_invitation(
        &self,
        ical_uid: &str,
        me: &str,
        answer: Answer,
        occurrence: Option<EpochMillis>,
    ) -> Result<Answered, BackendError> {
        forward!(
            AnyCalendar,
            self,
            answer_invitation(ical_uid, me, answer, occurrence)
        )
    }

    async fn busy_between(
        &self,
        from: EpochMillis,
        to: EpochMillis,
    ) -> Result<Vec<Busy>, BackendError> {
        forward!(AnyCalendar, self, busy_between(from, to))
    }

    async fn series(
        &self,
        ical_uid: &str,
        from: EpochMillis,
    ) -> Result<Option<Series>, BackendError> {
        forward!(AnyCalendar, self, series(ical_uid, from))
    }

    async fn events_between(
        &self,
        from: EpochMillis,
        to: EpochMillis,
    ) -> Result<Vec<Event>, BackendError> {
        forward!(AnyCalendar, self, events_between(from, to))
    }

    async fn create_event(&self, fields: &EventFields) -> Result<Event, BackendError> {
        forward!(AnyCalendar, self, create_event(fields))
    }

    async fn update_event(&self, id: &str, fields: &EventFields) -> Result<Event, BackendError> {
        forward!(AnyCalendar, self, update_event(id, fields))
    }

    async fn delete_event(&self, id: &str) -> Result<(), BackendError> {
        forward!(AnyCalendar, self, delete_event(id))
    }
}

impl ContactsService for AnyContacts {
    async fn connections(
        &self,
        page_token: Option<&str>,
        sync_token: Option<&str>,
    ) -> Result<ConnectionsPage, BackendError> {
        forward!(AnyContacts, self, connections(page_token, sync_token))
    }

    async fn contact_photo(&self, url: &str) -> Result<Vec<u8>, BackendError> {
        forward!(AnyContacts, self, contact_photo(url))
    }

    async fn create_contact(&self, fields: &ContactFields) -> Result<Person, BackendError> {
        forward!(AnyContacts, self, create_contact(fields))
    }

    async fn update_contact(
        &self,
        resource: &str,
        fields: &ContactFields,
    ) -> Result<Person, BackendError> {
        forward!(AnyContacts, self, update_contact(resource, fields))
    }
}

impl RulesService for AnyRules {
    async fn filters(&self) -> Result<Vec<Filter>, BackendError> {
        forward!(AnyRules, self, filters())
    }

    async fn create_filter(&self, filter: &Filter) -> Result<Filter, BackendError> {
        forward!(AnyRules, self, create_filter(filter))
    }

    async fn delete_filter(&self, id: &str) -> Result<(), BackendError> {
        forward!(AnyRules, self, delete_filter(id))
    }
}

impl AutoReplyService for AnyAutoReply {
    async fn vacation(&self) -> Result<Vacation, BackendError> {
        forward!(AnyAutoReply, self, vacation())
    }

    async fn set_vacation(&self, vacation: &Vacation) -> Result<(), BackendError> {
        forward!(AnyAutoReply, self, set_vacation(vacation))
    }
}

impl IdentityService for AnyIdentities {
    async fn identities(&self) -> Result<Vec<SendAsAddress>, BackendError> {
        forward!(AnyIdentities, self, identities())
    }
}
