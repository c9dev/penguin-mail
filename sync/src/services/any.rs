//! The closed enums over each service's adapters. Every adapter is known
//! when the app compiles, so each enum forwards a call to the adapter it
//! holds with a `match`: no boxed futures and no trait objects. `Google`
//! holds the adapter over the real client; `Fake` holds the same adapter
//! over `FakeGmail`, for tests and the demo. `Imap` and `FakeImap` do the
//! same for an IMAP account's mail and identity.

use std::ops::RangeInclusive;
use std::time::Duration;

use mailrs_domain::calendar as model;
use mailrs_domain::invitation::Answer;
use mailrs_domain::{EpochMillis, Filter, MailSet, RemoteMailbox, Role, Vacation};
use mailrs_gmail::{
    Answered, Busy, ConnectionsPage, ContactFields, Event, EventFields, LabelColor, Person, Series,
};
use mailrs_imap::{ImapClient, SmtpClient};
use mailrs_mime::Parts;

use super::{
    AutoReplyService, Backfill, CalendarService, Changes, ContactsService, Found, Google,
    IdentityService, Imap, KeywordsPage, MailBackend, MailCapabilities, RawMessage, Relocated,
    RemoteRef, RulesService, SearchQuery, SendAsAddress, SyncState, Unapplied, Want, Withheld,
};
use crate::api::{AccountClient, DraftRef, SavedDraft};
#[cfg(any(test, feature = "fake"))]
use crate::fake::{FakeGmail, FakeImap, FakeSmtp};
use crate::{BackendError, MailOp};

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

/// As `forward!`, for an enum that also holds the IMAP adapter.
macro_rules! forward_all {
    ($enum:ident, $self:ident, $method:ident($($arg:expr),*)) => {
        match $self {
            $enum::Google(adapter) => adapter.$method($($arg),*).await,
            #[cfg(any(test, feature = "fake"))]
            $enum::Fake(adapter) => adapter.$method($($arg),*).await,
            $enum::Imap(adapter) => adapter.$method($($arg),*).await,
            #[cfg(any(test, feature = "fake"))]
            $enum::FakeImap(adapter) => adapter.$method($($arg),*).await,
        }
    };
}

/// As `forward_all!`, for a method that answers at once.
macro_rules! forward_all_now {
    ($enum:ident, $self:ident, $method:ident($($arg:expr),*)) => {
        match $self {
            $enum::Google(adapter) => adapter.$method($($arg),*),
            #[cfg(any(test, feature = "fake"))]
            $enum::Fake(adapter) => adapter.$method($($arg),*),
            $enum::Imap(adapter) => adapter.$method($($arg),*),
            #[cfg(any(test, feature = "fake"))]
            $enum::FakeImap(adapter) => adapter.$method($($arg),*),
        }
    };
}

/// An account's mail.
#[derive(Clone)]
pub enum AnyMail {
    Google(Google<AccountClient>),
    #[cfg(any(test, feature = "fake"))]
    Fake(Google<FakeGmail>),
    Imap(Imap<ImapClient, SmtpClient>),
    #[cfg(any(test, feature = "fake"))]
    FakeImap(Imap<FakeImap, FakeSmtp>),
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
    Imap(Imap<ImapClient, SmtpClient>),
    #[cfg(any(test, feature = "fake"))]
    FakeImap(Imap<FakeImap, FakeSmtp>),
}

impl AnyMail {
    /// What Google's account withholds, read from the scopes it granted.
    /// An IMAP account withholds nothing: it grants Google nothing to
    /// begin with.
    pub fn withheld(&self) -> Withheld {
        match self {
            AnyMail::Google(adapter) => adapter.withheld(),
            #[cfg(any(test, feature = "fake"))]
            AnyMail::Fake(adapter) => adapter.withheld(),
            AnyMail::Imap(_) => Withheld::NONE,
            #[cfg(any(test, feature = "fake"))]
            AnyMail::FakeImap(_) => Withheld::NONE,
        }
    }
}

impl MailBackend for AnyMail {
    fn capabilities(&self) -> MailCapabilities {
        forward_all_now!(AnyMail, self, capabilities())
    }

    fn provider_name(&self) -> &str {
        forward_all_now!(AnyMail, self, provider_name())
    }

    fn mailbox_for(&self, role: Role) -> Option<String> {
        forward_all_now!(AnyMail, self, mailbox_for(role))
    }

    fn set_of(&self, id: &str) -> MailSet {
        forward_all_now!(AnyMail, self, set_of(id))
    }

    async fn apply(
        &self,
        messages: &[String],
        ops: &[MailOp],
    ) -> Result<Vec<Relocated>, Unapplied> {
        forward_all!(AnyMail, self, apply(messages, ops))
    }

    fn person_waiting(&self) -> bool {
        forward_all_now!(AnyMail, self, person_waiting())
    }

    async fn stand_by(&self, wait: Duration) {
        forward_all!(AnyMail, self, stand_by(wait))
    }

    async fn backfill(&self, days: i64, cursor: Option<&str>) -> Result<Backfill, BackendError> {
        forward_all!(AnyMail, self, backfill(days, cursor))
    }

    async fn window_ids(
        &self,
        days: i64,
        mailbox: Option<&str>,
    ) -> Result<Vec<RemoteRef>, BackendError> {
        forward_all!(AnyMail, self, window_ids(days, mailbox))
    }

    async fn inbox_ids(&self) -> Result<Vec<RemoteRef>, BackendError> {
        forward_all!(AnyMail, self, inbox_ids())
    }

    async fn search(
        &self,
        query: &SearchQuery,
        limit: usize,
    ) -> Result<Vec<RemoteRef>, BackendError> {
        forward_all!(AnyMail, self, search(query, limit))
    }

    async fn find_sent(&self, message_id: &str) -> Result<Option<String>, BackendError> {
        forward_all!(AnyMail, self, find_sent(message_id))
    }

    async fn fetch(&self, wants: Vec<Want>) -> Result<Found, BackendError> {
        forward_all!(AnyMail, self, fetch(wants))
    }

    async fn fetch_whole(&self, threads: Vec<String>) -> Result<Found, BackendError> {
        forward_all!(AnyMail, self, fetch_whole(threads))
    }

    async fn fetch_raw(&self, ids: &[String]) -> Result<Vec<RawMessage>, BackendError> {
        forward_all!(AnyMail, self, fetch_raw(ids))
    }

    async fn append(&self, raw: &[u8], mailbox: &str) -> Result<String, BackendError> {
        forward_all!(AnyMail, self, append(raw, mailbox))
    }

    async fn fetch_structure(&self, id: &str) -> Result<Parts, BackendError> {
        forward_all!(AnyMail, self, fetch_structure(id))
    }

    async fn fetch_part(&self, id: &str, path: &str) -> Result<Vec<u8>, BackendError> {
        forward_all!(AnyMail, self, fetch_part(id, path))
    }

    async fn send(&self, raw: &[u8], thread_id: Option<&str>) -> Result<String, BackendError> {
        forward_all!(AnyMail, self, send(raw, thread_id))
    }

    async fn save_draft(
        &self,
        draft_id: Option<&str>,
        raw: &[u8],
        thread_id: Option<&str>,
    ) -> Result<SavedDraft, BackendError> {
        forward_all!(AnyMail, self, save_draft(draft_id, raw, thread_id))
    }

    async fn send_draft(&self, draft_id: &str) -> Result<String, BackendError> {
        forward_all!(AnyMail, self, send_draft(draft_id))
    }

    async fn delete_draft(&self, draft_id: &str) -> Result<(), BackendError> {
        forward_all!(AnyMail, self, delete_draft(draft_id))
    }

    async fn list_drafts(&self) -> Result<Vec<DraftRef>, BackendError> {
        forward_all!(AnyMail, self, list_drafts())
    }

    fn made_by_person(&self, id: &str) -> bool {
        forward_all_now!(AnyMail, self, made_by_person(id))
    }

    async fn mailboxes(&self) -> Result<Vec<RemoteMailbox>, BackendError> {
        forward_all!(AnyMail, self, mailboxes())
    }

    async fn changes(&self, since: Option<&SyncState>) -> Result<Changes, BackendError> {
        forward_all!(AnyMail, self, changes(since))
    }

    async fn create_mailbox(&self, name: &str) -> Result<RemoteMailbox, BackendError> {
        forward_all!(AnyMail, self, create_mailbox(name))
    }

    async fn rename_mailbox(&self, id: &str, name: &str) -> Result<RemoteMailbox, BackendError> {
        forward_all!(AnyMail, self, rename_mailbox(id, name))
    }

    async fn delete_mailbox(&self, id: &str) -> Result<(), BackendError> {
        forward_all!(AnyMail, self, delete_mailbox(id))
    }

    async fn set_mailbox_color(
        &self,
        id: &str,
        color: &LabelColor,
    ) -> Result<RemoteMailbox, BackendError> {
        forward_all!(AnyMail, self, set_mailbox_color(id, color))
    }

    async fn mailbox_threads(&self, id: &str) -> Result<u64, BackendError> {
        forward_all!(AnyMail, self, mailbox_threads(id))
    }

    fn follow(&self, mailbox: &str) -> bool {
        forward_all_now!(AnyMail, self, follow(mailbox))
    }

    fn set_window_open(&self, open: bool) {
        forward_all_now!(AnyMail, self, set_window_open(open))
    }

    fn poll_interval(&self) -> Option<Duration> {
        forward_all_now!(AnyMail, self, poll_interval())
    }

    async fn watch(&self) {
        forward_all!(AnyMail, self, watch())
    }

    async fn uidvalidity(&self, mailbox: &str) -> Result<Option<u32>, BackendError> {
        forward_all!(AnyMail, self, uidvalidity(mailbox))
    }

    async fn keywords_stored(
        &self,
        mailbox: &str,
    ) -> Result<&'static [&'static str], BackendError> {
        forward_all!(AnyMail, self, keywords_stored(mailbox))
    }

    async fn keywords_in(
        &self,
        mailbox: &str,
        uidvalidity: u32,
        uids: RangeInclusive<u32>,
    ) -> Result<KeywordsPage, BackendError> {
        forward_all!(AnyMail, self, keywords_in(mailbox, uidvalidity, uids))
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

    async fn calendars(&self) -> Result<Vec<model::Calendar>, BackendError> {
        forward!(AnyCalendar, self, calendars())
    }

    async fn event_changes(
        &self,
        calendar: &str,
        token: Option<&str>,
        page: Option<&str>,
        from: EpochMillis,
    ) -> Result<model::EventPage, BackendError> {
        forward!(AnyCalendar, self, event_changes(calendar, token, page, from))
    }

    async fn put_event(&self, event: &model::Event, etag: Option<&str>, create: bool) -> Result<model::Event, BackendError> {
        forward!(AnyCalendar, self, put_event(event, etag, create))
    }

    async fn remove_event(&self, calendar: &str, id: &str, etag: Option<&str>) -> Result<(), BackendError> {
        forward!(AnyCalendar, self, remove_event(calendar, id, etag))
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
        forward_all!(AnyIdentities, self, identities())
    }
}
