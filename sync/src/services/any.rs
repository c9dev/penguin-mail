//! The closed enums over each service's adapters. Every adapter is known
//! when the app compiles, so each enum forwards a call to the adapter it
//! holds with a `match`: no boxed futures and no trait objects. Each
//! adapter runs over a client enum from `clients.rs`, the real client or
//! under the `fake` feature the in-memory one, so it appears here once:
//! `Google` over Gmail, `Imap` over IMAP and SMTP, `Microsoft` over Graph,
//! `Pop3` over POP3 and SMTP, `Dav` over CalDAV or CardDAV, and `Sieve`
//! over ManageSieve.

use std::ops::RangeInclusive;
use std::time::Duration;

use mailrs_domain::calendar as model;
use mailrs_domain::invitation::Answer;
use mailrs_domain::{EpochMillis, Filter, MailSet, RemoteMailbox, Role, Vacation};
use mailrs_gmail::{ConnectionsPage, ContactFields, LabelColor, Person};
use mailrs_mime::Parts;

use super::clients::{AnyDav, AnyGmail, AnyGraph, AnyImap, AnyPop3, AnySieve, AnySmtp};
use super::local::LocalRules;
use super::{
    AutoReplyService, Backfill, CalDav, CalendarService, CardDav, Changes, ContactsService, Found, Google,
    IdentityService, Imap, KeywordsPage, MailBackend, MailCapabilities, Microsoft, Pop3, RawMessage, Refused,
    Relocated, RemoteRef, RulesPlace, RulesService, SearchQuery, SendAsAddress, SieveRules, SyncState, Unapplied,
    Want, Withheld,
};
use crate::api::{DraftRef, SavedDraft};
use crate::{BackendError, MailOp};

/// Awaits `$method` on whichever adapter `$self`, a calendar or an
/// address book, holds: Google, Microsoft, or a CalDAV or CardDAV server.
macro_rules! forward_dav {
    ($enum:ident, $self:ident, $method:ident($($arg:expr),*)) => {
        match $self {
            $enum::Google(adapter) => adapter.$method($($arg),*).await,
            $enum::Microsoft(adapter) => adapter.$method($($arg),*).await,
            $enum::Dav(adapter) => adapter.$method($($arg),*).await,
        }
    };
}

/// As `forward_dav!`, for the rules: Gmail, Graph, ManageSieve, or this
/// computer.
macro_rules! forward_rules {
    ($self:ident, $method:ident($($arg:expr),*)) => {
        match $self {
            AnyRules::Google(adapter) => adapter.$method($($arg),*).await,
            AnyRules::Microsoft(adapter) => adapter.$method($($arg),*).await,
            AnyRules::Sieve(adapter) => adapter.$method($($arg),*).await,
            AnyRules::Local(adapter) => adapter.$method($($arg),*).await,
        }
    };
}

/// As `forward_dav!`, for the automatic reply: Gmail, Graph or
/// ManageSieve.
macro_rules! forward_reply {
    ($self:ident, $method:ident($($arg:expr),*)) => {
        match $self {
            AnyAutoReply::Google(adapter) => adapter.$method($($arg),*).await,
            AnyAutoReply::Microsoft(adapter) => adapter.$method($($arg),*).await,
            AnyAutoReply::Sieve(adapter) => adapter.$method($($arg),*).await,
        }
    };
}

/// As `forward_dav!`, for an enum that holds every mail adapter.
macro_rules! forward_all {
    ($enum:ident, $self:ident, $method:ident($($arg:expr),*)) => {
        match $self {
            $enum::Google(adapter) => adapter.$method($($arg),*).await,
            $enum::Imap(adapter) => adapter.$method($($arg),*).await,
            $enum::Microsoft(adapter) => adapter.$method($($arg),*).await,
            $enum::Pop3(adapter) => adapter.$method($($arg),*).await,
        }
    };
}

/// As `forward_all!`, for a method that answers at once.
macro_rules! forward_all_now {
    ($enum:ident, $self:ident, $method:ident($($arg:expr),*)) => {
        match $self {
            $enum::Google(adapter) => adapter.$method($($arg),*),
            $enum::Imap(adapter) => adapter.$method($($arg),*),
            $enum::Microsoft(adapter) => adapter.$method($($arg),*),
            $enum::Pop3(adapter) => adapter.$method($($arg),*),
        }
    };
}

type GoogleAdapter = Google<AnyGmail>;
type ImapAdapter = Imap<AnyImap, AnySmtp>;
type MicrosoftAdapter = Microsoft<AnyGraph>;
type Pop3Adapter = Pop3<AnySmtp, AnyPop3>;

/// An account's mail.
#[derive(Clone)]
pub enum AnyMail {
    Google(GoogleAdapter),
    Imap(ImapAdapter),
    Microsoft(MicrosoftAdapter),
    Pop3(Pop3Adapter),
}

/// An account's calendar.
#[derive(Clone)]
pub enum AnyCalendar {
    Google(GoogleAdapter),
    Microsoft(MicrosoftAdapter),
    /// A CalDAV server, whose replies go out through the account's mail.
    Dav(CalDav<AnyDav>),
}

/// An account's address book.
#[derive(Clone)]
pub enum AnyContacts {
    Google(GoogleAdapter),
    Microsoft(MicrosoftAdapter),
    Dav(CardDav<AnyDav>),
}

/// The rules an account's server runs, or this computer does.
#[derive(Clone)]
pub enum AnyRules {
    Google(GoogleAdapter),
    Microsoft(MicrosoftAdapter),
    Sieve(SieveRules<AnySieve>),
    Local(LocalRules),
}

/// An account's automatic reply.
#[derive(Clone)]
pub enum AnyAutoReply {
    Google(GoogleAdapter),
    Microsoft(MicrosoftAdapter),
    Sieve(SieveRules<AnySieve>),
}

/// The addresses an account sends as.
#[derive(Clone)]
pub enum AnyIdentities {
    Google(GoogleAdapter),
    Imap(ImapAdapter),
    Microsoft(MicrosoftAdapter),
    Pop3(Pop3Adapter),
}

impl AnyMail {
    /// What Google's account withholds, read from the scopes it granted.
    /// An IMAP account withholds nothing: it grants Google nothing to
    /// begin with.
    pub fn withheld(&self) -> Withheld {
        match self {
            AnyMail::Google(adapter) => adapter.withheld(),
            AnyMail::Imap(_) => Withheld::NONE,
            AnyMail::Microsoft(adapter) => adapter.withheld(),
            AnyMail::Pop3(_) => Withheld::NONE,
        }
    }

    /// The services an organization refused a Microsoft account this run.
    /// No other provider learns of such a refusal.
    pub fn refused(&self) -> Refused {
        match self {
            AnyMail::Microsoft(adapter) => adapter.refused(),
            _ => Refused::default(),
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

    async fn unread_counts(
        &self,
    ) -> Result<std::collections::HashMap<mailrs_domain::MailSet, i64>, BackendError> {
        forward_all!(AnyMail, self, unread_counts())
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
    async fn calendars(&self) -> Result<Vec<model::Calendar>, BackendError> {
        forward_dav!(AnyCalendar, self, calendars())
    }

    async fn event_changes(
        &self,
        calendar: &str,
        token: Option<&str>,
        page: Option<&str>,
        from: EpochMillis,
    ) -> Result<model::EventPage, BackendError> {
        forward_dav!(AnyCalendar, self, event_changes(calendar, token, page, from))
    }

    async fn event_range(
        &self,
        calendar: &str,
        from: EpochMillis,
        to: EpochMillis,
        page: Option<&str>,
    ) -> Result<model::EventPage, BackendError> {
        forward_dav!(AnyCalendar, self, event_range(calendar, from, to, page))
    }

    async fn put_event(
        &self,
        event: &model::Event,
        etag: Option<&str>,
        create: bool,
        notify: model::Notify,
    ) -> Result<model::Event, BackendError> {
        forward_dav!(AnyCalendar, self, put_event(event, etag, create, notify))
    }

    async fn remove_event(
        &self,
        calendar: &str,
        id: &str,
        etag: Option<&str>,
        notify: model::Notify,
    ) -> Result<(), BackendError> {
        forward_dav!(AnyCalendar, self, remove_event(calendar, id, etag, notify))
    }

    async fn import_event(&self, event: &model::Event) -> Result<model::Event, BackendError> {
        forward_dav!(AnyCalendar, self, import_event(event))
    }

    async fn upload_attachment(
        &self,
        file: &model::Attachment,
        sent: std::sync::Arc<std::sync::atomic::AtomicU64>,
    ) -> Result<model::Attachment, BackendError> {
        forward_dav!(AnyCalendar, self, upload_attachment(file, sent))
    }

    async fn share_file(&self, file_id: &str, email: &str) -> Result<(), BackendError> {
        forward_dav!(AnyCalendar, self, share_file(file_id, email))
    }

    async fn move_event(
        &self,
        event: &model::Event,
        destination: &str,
        notify: model::Notify,
    ) -> Result<model::Event, BackendError> {
        forward_dav!(AnyCalendar, self, move_event(event, destination, notify))
    }

    async fn answer_event(
        &self,
        calendar: &str,
        id: &str,
        me: &str,
        answer: Answer,
        note: Option<&str>,
    ) -> Result<model::Event, BackendError> {
        forward_dav!(AnyCalendar, self, answer_event(calendar, id, me, answer, note))
    }

    async fn edit_list(
        &self,
        calendar: &str,
        edit: &model::list::ListEdit,
    ) -> Result<Option<model::Calendar>, BackendError> {
        forward_dav!(AnyCalendar, self, edit_list(calendar, edit))
    }
}

impl ContactsService for AnyContacts {
    async fn connections(
        &self,
        page_token: Option<&str>,
        sync_token: Option<&str>,
    ) -> Result<ConnectionsPage, BackendError> {
        forward_dav!(AnyContacts, self, connections(page_token, sync_token))
    }

    async fn contact_photo(&self, url: &str) -> Result<Vec<u8>, BackendError> {
        forward_dav!(AnyContacts, self, contact_photo(url))
    }

    async fn create_contact(&self, fields: &ContactFields) -> Result<Person, BackendError> {
        forward_dav!(AnyContacts, self, create_contact(fields))
    }

    async fn update_contact(
        &self,
        resource: &str,
        fields: &ContactFields,
    ) -> Result<Person, BackendError> {
        forward_dav!(AnyContacts, self, update_contact(resource, fields))
    }
}

impl RulesService for AnyRules {
    async fn filters(&self) -> Result<Vec<Filter>, BackendError> {
        forward_rules!(self, filters())
    }

    async fn create_filter(&self, filter: &Filter) -> Result<Filter, BackendError> {
        forward_rules!(self, create_filter(filter))
    }

    async fn delete_filter(&self, id: &str) -> Result<(), BackendError> {
        forward_rules!(self, delete_filter(id))
    }

    async fn replace_filter(&self, old_id: &str, new: &Filter) -> Result<Filter, BackendError> {
        forward_rules!(self, replace_filter(old_id, new))
    }

    async fn take_over(&self) {
        forward_rules!(self, take_over())
    }

    fn queues_offline(&self) -> bool {
        match self {
            AnyRules::Sieve(adapter) => adapter.queues_offline(),
            _ => false,
        }
    }
}

impl AnyRules {
    /// Where the rules run, for the line under the Rules dialog's list.
    pub fn place(&self) -> RulesPlace {
        match self {
            AnyRules::Local(_) => RulesPlace::ThisComputer,
            _ => RulesPlace::Server,
        }
    }

    /// The rules, and the blocks a Sieve server holds that nobody here
    /// wrote.
    pub async fn listing(&self) -> Result<(Vec<Filter>, Vec<String>), BackendError> {
        match self {
            AnyRules::Sieve(adapter) => adapter.listing().await,
            other => Ok((other.filters().await?, Vec::new())),
        }
    }
}

impl AnyCalendar {
    /// Why the calendar server last refused the login, for Preferences.
    pub fn login_refused(&self) -> Option<String> {
        match self {
            AnyCalendar::Dav(adapter) => adapter.login_refused(),
            _ => None,
        }
    }
}

impl AnyContacts {
    /// Why the contacts server last refused the login, for Preferences.
    pub fn login_refused(&self) -> Option<String> {
        match self {
            AnyContacts::Dav(adapter) => adapter.login_refused(),
            _ => None,
        }
    }
}

impl AutoReplyService for AnyAutoReply {
    async fn vacation(&self) -> Result<Vacation, BackendError> {
        forward_reply!(self, vacation())
    }

    async fn set_vacation(&self, vacation: &Vacation) -> Result<(), BackendError> {
        forward_reply!(self, set_vacation(vacation))
    }

    fn keeps_subject(&self) -> bool {
        match self {
            AnyAutoReply::Google(adapter) => adapter.keeps_subject(),
            AnyAutoReply::Microsoft(adapter) => adapter.keeps_subject(),
            AnyAutoReply::Sieve(adapter) => adapter.keeps_subject(),
        }
    }

    fn limits_to_contacts(&self) -> bool {
        match self {
            AnyAutoReply::Google(adapter) => adapter.limits_to_contacts(),
            AnyAutoReply::Microsoft(adapter) => adapter.limits_to_contacts(),
            AnyAutoReply::Sieve(adapter) => adapter.limits_to_contacts(),
        }
    }
}

impl IdentityService for AnyIdentities {
    async fn identities(&self) -> Result<Vec<SendAsAddress>, BackendError> {
        forward_all!(AnyIdentities, self, identities())
    }
}
