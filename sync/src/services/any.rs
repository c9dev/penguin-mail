//! The closed enums over each service's adapters. Every adapter is known
//! when the app compiles, so each enum forwards a call to the adapter it
//! holds with a `match`: no boxed futures and no trait objects. Each
//! adapter runs over a client enum from `clients.rs`, the real client or
//! under the `fake` feature the in-memory one, so it appears here once:
//! `Google` over Gmail, `Imap` over IMAP and SMTP, `GmailImap` over
//! Gmail's IMAP and SMTP, `Microsoft` over Graph,
//! `Pop3` over POP3 and SMTP, `Dav` over CalDAV or CardDAV, and `Sieve`
//! over ManageSieve.

use std::ops::RangeInclusive;
use std::time::Duration;

use mailrs_domain::calendar as model;
use mailrs_domain::invitation::Answer;
use mailrs_domain::{EpochMillis, Filter, MailSet, RemoteMailbox, Role, Vacation};
use mailrs_gmail::{ConnectionsPage, ContactFields, LabelColor, Person};
use mailrs_mime::Parts;

use super::beside::RulesBeside;
use super::clients::{AnyDav, AnyGmail, AnyGraph, AnyImap, AnyJmap, AnyPop3, AnySieve, AnySmtp};
use super::local::LocalRules;
use super::{
    AutoReplyService, Backfill, CalDav, CalendarService, CardDav, Changes, Claimed,
    ContactsService, Found, GmailImap, Google, Held, IdentityService, Imap, Jmap, JmapSieve,
    KeywordsPage, Look, MailBackend, MailCapabilities, Microsoft, NoMail, Pop3, RawMessage,
    ReceiptOut, Refused, Relocated, RemoteRef, RulesPlace, RulesService, SearchQuery,
    SendAsAddress, Settled, SieveRules, Storage, SyncState, Unapplied, Unheld, Want, Withheld,
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
            $enum::Jmap(adapter) => adapter.$method($($arg),*).await,
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
            AnyRules::JmapSieve(adapter) => adapter.$method($($arg),*).await,
            AnyRules::Local(adapter) => adapter.$method($($arg),*).await,
            AnyRules::Beside(adapter) => adapter.$method($($arg),*).await,
        }
    };
}

/// As `forward_dav!`, for the automatic reply: Gmail, Graph, a Sieve
/// script over ManageSieve or JMAP, or JMAP's own reply.
macro_rules! forward_reply {
    ($self:ident, $method:ident($($arg:expr),*)) => {
        match $self {
            AnyAutoReply::Google(adapter) => adapter.$method($($arg),*).await,
            AnyAutoReply::Microsoft(adapter) => adapter.$method($($arg),*).await,
            AnyAutoReply::Sieve(adapter) => adapter.$method($($arg),*).await,
            AnyAutoReply::JmapSieve(adapter) => adapter.$method($($arg),*).await,
            AnyAutoReply::Jmap(adapter) => adapter.$method($($arg),*).await,
        }
    };
}

/// As `forward_dav!`, for an enum that holds every mail adapter, and
/// the one that stands in for an account with no mail.
macro_rules! forward_all {
    ($enum:ident, $self:ident, $method:ident($($arg:expr),*)) => {
        match $self {
            $enum::Google(adapter) => adapter.$method($($arg),*).await,
            $enum::Imap(adapter) => adapter.$method($($arg),*).await,
            $enum::GmailImap(adapter) => adapter.$method($($arg),*).await,
            $enum::Jmap(adapter) => adapter.$method($($arg),*).await,
            $enum::Microsoft(adapter) => adapter.$method($($arg),*).await,
            $enum::Pop3(adapter) => adapter.$method($($arg),*).await,
            $enum::NoMail(adapter) => adapter.$method($($arg),*).await,
        }
    };
}

/// As `forward_all!`, for a method that answers at once.
macro_rules! forward_all_now {
    ($enum:ident, $self:ident, $method:ident($($arg:expr),*)) => {
        match $self {
            $enum::Google(adapter) => adapter.$method($($arg),*),
            $enum::Imap(adapter) => adapter.$method($($arg),*),
            $enum::GmailImap(adapter) => adapter.$method($($arg),*),
            $enum::Jmap(adapter) => adapter.$method($($arg),*),
            $enum::Microsoft(adapter) => adapter.$method($($arg),*),
            $enum::Pop3(adapter) => adapter.$method($($arg),*),
            $enum::NoMail(adapter) => adapter.$method($($arg),*),
        }
    };
}

type GoogleAdapter = Google<AnyGmail>;
type ImapAdapter = Imap<AnyImap, AnySmtp>;
type GmailImapAdapter = GmailImap<AnyImap, AnySmtp>;
type JmapAdapter = Jmap<AnyJmap>;
type MicrosoftAdapter = Microsoft<AnyGraph>;
type Pop3Adapter = Pop3<AnySmtp, AnyPop3>;

/// An account's mail.
#[derive(Clone)]
pub enum AnyMail {
    Google(GoogleAdapter),
    Imap(ImapAdapter),
    /// A Gmail account whose mail comes over IMAP.
    GmailImap(GmailImapAdapter),
    Jmap(JmapAdapter),
    Microsoft(MicrosoftAdapter),
    Pop3(Pop3Adapter),
    /// A calendar account, which has none.
    NoMail(NoMail),
}

/// An account's calendar.
#[derive(Clone)]
pub enum AnyCalendar {
    Google(GoogleAdapter),
    Microsoft(MicrosoftAdapter),
    /// A CalDAV server, whose replies go out through the account's mail
    /// where it has mail.
    Dav(CalDav<AnyDav>),
    /// A JMAP server's own calendars.
    Jmap(JmapAdapter),
}

/// An account's address book.
#[derive(Clone)]
pub enum AnyContacts {
    Google(GoogleAdapter),
    Microsoft(MicrosoftAdapter),
    Dav(CardDav<AnyDav>),
    Jmap(JmapAdapter),
}

/// The rules an account's server runs, or this computer does.
#[derive(Clone)]
pub enum AnyRules {
    Google(GoogleAdapter),
    Microsoft(MicrosoftAdapter),
    Sieve(SieveRules<AnySieve>),
    JmapSieve(SieveRules<JmapSieve<AnyJmap>>),
    Local(LocalRules),
    /// A JMAP server's Sieve script beside local rules a switch from IMAP
    /// could not move into it.
    Beside(RulesBeside),
}

/// An account's automatic reply.
#[derive(Clone)]
pub enum AnyAutoReply {
    Google(GoogleAdapter),
    Microsoft(MicrosoftAdapter),
    Sieve(SieveRules<AnySieve>),
    /// The reply inside the rules script on a server that runs Sieve over
    /// JMAP, which then never uses JMAP's own reply.
    JmapSieve(SieveRules<JmapSieve<AnyJmap>>),
    Jmap(JmapAdapter),
}

/// The addresses an account sends as.
#[derive(Clone)]
pub enum AnyIdentities {
    Google(GoogleAdapter),
    Imap(ImapAdapter),
    GmailImap(GmailImapAdapter),
    Jmap(JmapAdapter),
    Microsoft(MicrosoftAdapter),
    Pop3(Pop3Adapter),
    /// A calendar account, which sends nothing.
    NoMail(NoMail),
}

impl AnyMail {
    /// Whether this is mail at all, and not what stands in for it in a
    /// calendar account.
    pub fn serves_mail(&self) -> bool {
        !matches!(self, AnyMail::NoMail(_))
    }

    /// A label of the account can take a colour. Gmail's IMAP keeps none.
    pub fn label_colors(&self) -> bool {
        !matches!(self, AnyMail::GmailImap(_))
    }

    /// The account's session offers Masked Email.
    pub fn masked_email(&self) -> bool {
        match self {
            AnyMail::Jmap(adapter) => adapter.masked_email(),
            _ => false,
        }
    }

    /// Whether `address` is one of the account's masked addresses: `None`
    /// until a JMAP account has listed them, `Some(false)` on an account
    /// that makes none.
    pub fn is_masked(&self, address: &str) -> Option<bool> {
        match self {
            AnyMail::Jmap(adapter) => adapter.is_masked(address),
            _ => Some(false),
        }
    }

    /// The server keeps an automatic reply this account's sign-in cannot
    /// reach. Only a JMAP account signed in with a token can say so.
    pub fn auto_reply_needs_sign_in(&self) -> bool {
        match self {
            AnyMail::Jmap(adapter) => adapter.auto_reply_needs_sign_in(),
            _ => false,
        }
    }

    /// What Google's account withholds, read from the scopes it granted.
    /// An IMAP account withholds nothing: it grants Google nothing to
    /// begin with.
    pub fn withheld(&self) -> Withheld {
        match self {
            AnyMail::Google(adapter) => adapter.withheld(),
            AnyMail::Imap(_) | AnyMail::GmailImap(_) => Withheld::NONE,
            AnyMail::Jmap(_) => Withheld::NONE,
            AnyMail::Microsoft(adapter) => adapter.withheld(),
            AnyMail::Pop3(_) => Withheld::NONE,
            AnyMail::NoMail(_) => Withheld::NONE,
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

    async fn destroy_checked(
        &self,
        messages: &[String],
        mailbox: &str,
    ) -> Vec<Result<(), BackendError>> {
        forward_all!(AnyMail, self, destroy_checked(messages, mailbox))
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

    async fn mailbox_page(
        &self,
        mailbox: &str,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<Backfill, BackendError> {
        forward_all!(AnyMail, self, mailbox_page(mailbox, cursor, limit))
    }

    async fn unread_page(
        &self,
        mailbox: &str,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<Backfill, BackendError> {
        forward_all!(AnyMail, self, unread_page(mailbox, cursor, limit))
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

    async fn schedule(&self, raw: &[u8], at: EpochMillis) -> Result<Held, BackendError> {
        forward_all!(AnyMail, self, schedule(raw, at))
    }

    async fn find_hold(&self, message_id: &str) -> Result<Option<Held>, BackendError> {
        forward_all!(AnyMail, self, find_hold(message_id))
    }

    async fn hold_of_copy(&self, email_id: &str) -> Result<Option<Claimed>, BackendError> {
        forward_all!(AnyMail, self, hold_of_copy(email_id))
    }

    async fn unschedule(
        &self,
        handle: &str,
        message_id: &str,
        send_at: EpochMillis,
    ) -> Result<Unheld, BackendError> {
        forward_all!(AnyMail, self, unschedule(handle, message_id, send_at))
    }

    async fn settle(
        &self,
        handle: &str,
        message_id: &str,
        send_at: EpochMillis,
    ) -> Result<Settled, BackendError> {
        forward_all!(AnyMail, self, settle(handle, message_id, send_at))
    }

    async fn storage(&self) -> Result<Option<Storage>, BackendError> {
        forward_all!(AnyMail, self, storage())
    }

    async fn send_receipt(&self, receipt: &ReceiptOut) -> Result<(), BackendError> {
        forward_all!(AnyMail, self, send_receipt(receipt))
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

    fn reset_follow(&self, mailbox: &str) {
        forward_all_now!(AnyMail, self, reset_follow(mailbox))
    }

    fn set_window_open(&self, open: bool) {
        forward_all_now!(AnyMail, self, set_window_open(open))
    }

    fn check_now(&self) {
        forward_all_now!(AnyMail, self, check_now())
    }

    fn poll_interval(&self) -> Option<Duration> {
        forward_all_now!(AnyMail, self, poll_interval())
    }

    async fn watch(&self) {
        forward_all!(AnyMail, self, watch())
    }

    async fn look_for_mail(&self) -> Look {
        forward_all!(AnyMail, self, look_for_mail())
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
        forward_dav!(
            AnyCalendar,
            self,
            event_changes(calendar, token, page, from)
        )
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
        forward_dav!(
            AnyCalendar,
            self,
            answer_event(calendar, id, me, answer, note)
        )
    }

    async fn edit_list(
        &self,
        calendar: &str,
        edit: &model::list::ListEdit,
    ) -> Result<Option<model::Calendar>, BackendError> {
        forward_dav!(AnyCalendar, self, edit_list(calendar, edit))
    }
}

/// As `forward_dav!`, for an address book, which a JMAP server can hold too.
macro_rules! forward_contacts {
    ($self:ident, $method:ident($($arg:expr),*)) => {
        match $self {
            AnyContacts::Google(adapter) => adapter.$method($($arg),*).await,
            AnyContacts::Microsoft(adapter) => adapter.$method($($arg),*).await,
            AnyContacts::Dav(adapter) => adapter.$method($($arg),*).await,
            AnyContacts::Jmap(adapter) => adapter.$method($($arg),*).await,
        }
    };
}

impl ContactsService for AnyContacts {
    async fn connections(
        &self,
        page_token: Option<&str>,
        sync_token: Option<&str>,
    ) -> Result<ConnectionsPage, BackendError> {
        forward_contacts!(self, connections(page_token, sync_token))
    }

    async fn contact_photo(&self, url: &str) -> Result<Vec<u8>, BackendError> {
        forward_contacts!(self, contact_photo(url))
    }

    async fn create_contact(&self, fields: &ContactFields) -> Result<Person, BackendError> {
        forward_contacts!(self, create_contact(fields))
    }

    async fn update_contact(
        &self,
        resource: &str,
        fields: &ContactFields,
    ) -> Result<Person, BackendError> {
        forward_contacts!(self, update_contact(resource, fields))
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
            AnyRules::JmapSieve(adapter) => adapter.queues_offline(),
            AnyRules::Beside(adapter) => adapter.queues_offline(),
            _ => false,
        }
    }
}

impl AnyRules {
    /// Brings a Sieve script in line with folders renamed, moved or
    /// deleted; `moved` pairs old ids with new ones. Other rules name their
    /// folders by an id that a rename keeps, or run on this computer.
    pub async fn follow_folders(&self, moved: &[(String, String)]) -> Result<(), BackendError> {
        match self {
            AnyRules::Sieve(rules) => rules.follow_folders(moved).await,
            AnyRules::JmapSieve(rules) => rules.follow_folders(moved).await,
            AnyRules::Beside(rules) => rules.server().follow_folders(moved).await,
            _ => Ok(()),
        }
    }

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
            AnyRules::JmapSieve(adapter) => adapter.listing().await,
            AnyRules::Beside(adapter) => adapter.listing().await,
            other => Ok((other.filters().await?, Vec::new())),
        }
    }

    /// Whether some of the rules run on this computer.
    pub fn runs_here(&self) -> bool {
        matches!(self, AnyRules::Local(_) | AnyRules::Beside(_))
    }

    /// The ids of the rules that run on this computer beside a server's.
    pub async fn kept_here(&self) -> Result<Vec<String>, BackendError> {
        match self {
            AnyRules::Beside(adapter) => adapter.kept_here().await,
            _ => Ok(Vec::new()),
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

    /// Whether an answer to an invitation reaches its organizer. Google
    /// and Microsoft send it themselves, and a JMAP account always has
    /// mail to carry it; a CalDAV server may leave it to the account's
    /// mail, which a calendar account lacks.
    pub fn tells_organizer(&self) -> bool {
        match self {
            AnyCalendar::Google(_) | AnyCalendar::Microsoft(_) | AnyCalendar::Jmap(_) => true,
            AnyCalendar::Dav(adapter) => adapter.tells_organizer(),
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
            AnyAutoReply::JmapSieve(adapter) => adapter.keeps_subject(),
            AnyAutoReply::Jmap(adapter) => adapter.keeps_subject(),
        }
    }

    fn limits_to_contacts(&self) -> bool {
        match self {
            AnyAutoReply::Google(adapter) => adapter.limits_to_contacts(),
            AnyAutoReply::Microsoft(adapter) => adapter.limits_to_contacts(),
            AnyAutoReply::Sieve(adapter) => adapter.limits_to_contacts(),
            AnyAutoReply::JmapSieve(adapter) => adapter.limits_to_contacts(),
            AnyAutoReply::Jmap(adapter) => adapter.limits_to_contacts(),
        }
    }
}

impl IdentityService for AnyIdentities {
    async fn identities(&self) -> Result<Vec<SendAsAddress>, BackendError> {
        forward_all!(AnyIdentities, self, identities())
    }
}
