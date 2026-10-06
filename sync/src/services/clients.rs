//! The closed enums over each client an adapter runs on: the real client,
//! or under the `fake` feature the in-memory one tests and the demo use.
//! Each adapter in `any.rs` then is one type, `Google<AnyGmail>` say,
//! and each service enum lists it once. Every call is a `match`, as in
//! `any.rs`: nothing is boxed. A fake sits behind an `Arc` so a test can
//! keep its own handle on it and read what the adapter did.

#[cfg(any(test, feature = "fake"))]
use std::sync::Arc;
use std::time::Duration;

#[cfg(any(test, feature = "fake"))]
use futures::future::Either;

use mailrs_dav::{Collection, CollectionState, DavApi, DavClient, DavError, Fetched, Homes, Kind, Member, Precondition, Synced};
use mailrs_domain::calendar;
use mailrs_domain::invitation::Answer;
use mailrs_domain::{EpochMillis, Filter, MessageMeta, Vacation};
use mailrs_gmail::model::Message;
use mailrs_gmail::{
    AccountQuota, ConnectionsPage, ContactFields, GmailError, Granted, HistoryPage, LabelColor, MessagePage, Person,
    Profile, RemoteLabel, SendAs,
};
use mailrs_graph::{
    AutomaticReplies, ContactFolder, DeltaPage, Graph, GraphCalendar, GraphContact, GraphError, GraphEvent, Listing,
    MailFolder, MasterCategory, Me, MessageBody, MessageRule, Override, Page, Response, Write,
};
use mailrs_imap::{
    AppendUid, BodyStructure, Capabilities, CopyUid, FlagsOf, ImapClient, ImapError, Listed, Selected, Since,
    SmtpClient, UidSet, Woke,
};
use mailrs_pop3::{ListItem, Pop3Api, Pop3Client, Pop3Error, Stat, UidlListing};
use mailrs_sieve::client::{ManageSieveApi, ManageSieveClient, SieveError};
use serde_json::Value;

use super::{GraphApi, ImapApi, Submit};
use crate::api::{AccountClient, DraftRef, GmailApi, SavedDraft};
#[cfg(any(test, feature = "fake"))]
use crate::fake::{FakeDav, FakeGmail, FakeGraph, FakeImap, FakePop3, FakeSieve, FakeSmtp};

/// Declares `$enum` over a real client and its fake, and implements
/// `$trait` on it by passing each call to the client it holds. `now`
/// lists the methods that answer at once, `later` the ones that answer
/// a future. The trait's own list is the check: a method left out here
/// fails to compile.
macro_rules! either_client {
    (
        $(#[$doc:meta])*
        $enum:ident($real:ty, $fake:ty): $trait:path;
        now { $(fn $now:ident($($na:ident: $nt:ty),*) -> $nr:ty;)* }
        later { $(fn $later:ident($($la:ident: $lt:ty),*) -> $lr:ty;)* }
    ) => {
        $(#[$doc])*
        pub enum $enum {
            Real($real),
            #[cfg(any(test, feature = "fake"))]
            Fake(Arc<$fake>),
        }

        impl From<$real> for $enum {
            fn from(client: $real) -> Self {
                $enum::Real(client)
            }
        }

        #[cfg(any(test, feature = "fake"))]
        impl From<Arc<$fake>> for $enum {
            fn from(fake: Arc<$fake>) -> Self {
                $enum::Fake(fake)
            }
        }

        impl $trait for $enum {
            $(fn $now(&self, $($na: $nt),*) -> $nr {
                match self {
                    $enum::Real(client) => <$real as $trait>::$now(client, $($na),*),
                    #[cfg(any(test, feature = "fake"))]
                    $enum::Fake(client) => <$fake as $trait>::$now(client, $($na),*),
                }
            })*
            // The client's own future, in `Either` when there are two, so
            // a call adds no state machine of its own around it and a
            // debug build's poll goes no deeper than the client's.
            $(
            #[cfg(any(test, feature = "fake"))]
            fn $later(&self, $($la: $lt),*) -> impl Future<Output = $lr> + Send {
                match self {
                    $enum::Real(client) => Either::Left(<$real as $trait>::$later(client, $($la),*)),
                    $enum::Fake(client) => Either::Right(<$fake as $trait>::$later(client, $($la),*)),
                }
            }
            #[cfg(not(any(test, feature = "fake")))]
            fn $later(&self, $($la: $lt),*) -> impl Future<Output = $lr> + Send {
                let $enum::Real(client) = self;
                <$real as $trait>::$later(client, $($la),*)
            }
            )*
        }
    };
}

type Gmail<T> = Result<T, GmailError>;

either_client! {
    /// Gmail, the Calendar API and the People API: Google's client for one
    /// account, or the in-memory Gmail.
    // Google's client is far larger than an `Arc` to the fake, but the
    // enum lives behind the adapter's own `Arc`, once per account.
    #[cfg_attr(any(test, feature = "fake"), expect(clippy::large_enum_variant))]
    AnyGmail(AccountClient, FakeGmail): GmailApi;
    now {
        fn quota() -> Option<&AccountQuota>;
        fn granted() -> Option<Granted>;
        fn workspace() -> bool;
    }
    later {
        fn profile() -> Gmail<Profile>;
        fn labels() -> Gmail<Vec<RemoteLabel>>;
        fn list_messages(query: &str, page_token: Option<&str>, page_size: u32) -> Gmail<MessagePage>;
        fn list_labelled(label_id: &str, query: &str, page_token: Option<&str>, page_size: u32) -> Gmail<MessagePage>;
        fn message_metadata(id: &str) -> Gmail<MessageMeta>;
        fn thread_metadata(thread_id: &str) -> Gmail<Vec<MessageMeta>>;
        fn message_structure(id: &str) -> Gmail<Message>;
        fn history(start_history_id: u64, page_token: Option<&str>) -> Gmail<HistoryPage>;
        fn modify_labels(id: &str, add: &[String], remove: &[String]) -> Gmail<()>;
        fn batch_modify(ids: &[String], add: &[String], remove: &[String]) -> Gmail<()>;
        fn delete_messages(ids: &[String]) -> Gmail<()>;
        fn send(raw: &[u8], thread_id: Option<&str>) -> Gmail<String>;
        fn save_draft(draft_id: Option<&str>, raw: &[u8], thread_id: Option<&str>) -> Gmail<SavedDraft>;
        fn send_draft(draft_id: &str) -> Gmail<String>;
        fn delete_draft(draft_id: &str) -> Gmail<()>;
        fn list_drafts() -> Gmail<Vec<DraftRef>>;
        fn send_as() -> Gmail<Vec<SendAs>>;
        fn attachment(message_id: &str, attachment_id: &str) -> Gmail<Vec<u8>>;
        fn raw_message(id: &str) -> Gmail<Vec<u8>>;
        fn filters() -> Gmail<Vec<Filter>>;
        fn create_filter(filter: &Filter) -> Gmail<Filter>;
        fn delete_filter(id: &str) -> Gmail<()>;
        fn create_label(name: &str) -> Gmail<RemoteLabel>;
        fn rename_label(id: &str, name: &str) -> Gmail<RemoteLabel>;
        fn delete_label(id: &str) -> Gmail<()>;
        fn set_label_color(id: &str, color: &LabelColor) -> Gmail<RemoteLabel>;
        fn label_threads(id: &str) -> Gmail<u64>;
        fn vacation() -> Gmail<Vacation>;
        fn set_vacation(vacation: &Vacation) -> Gmail<()>;
        fn connections(page_token: Option<&str>, sync_token: Option<&str>) -> Gmail<ConnectionsPage>;
        fn contact_photo(url: &str) -> Gmail<Vec<u8>>;
        fn create_contact(fields: &ContactFields) -> Gmail<Person>;
        fn update_contact(resource: &str, fields: &ContactFields) -> Gmail<Person>;
        fn calendars() -> Gmail<Vec<calendar::Calendar>>;
        fn event_changes(calendar: &str, token: Option<&str>, page: Option<&str>, from: EpochMillis) -> Gmail<calendar::EventPage>;
        fn event_range(calendar: &str, from: EpochMillis, to: EpochMillis, page: Option<&str>) -> Gmail<calendar::EventPage>;
        fn put_event(event: &calendar::Event, etag: Option<&str>, create: bool, notify: calendar::Notify) -> Gmail<calendar::Event>;
        fn remove_event(calendar: &str, id: &str, etag: Option<&str>, notify: calendar::Notify) -> Gmail<()>;
        fn import_event(event: &calendar::Event) -> Gmail<calendar::Event>;
        fn upload_to_drive(path: &std::path::Path, name: &str, mime_type: &str, sent: std::sync::Arc<std::sync::atomic::AtomicU64>) -> Gmail<calendar::Attachment>;
        fn share_file(file_id: &str, email: &str) -> Gmail<()>;
        fn move_event(event: &calendar::Event, destination: &str, notify: calendar::Notify) -> Gmail<calendar::Event>;
        fn answer_event(calendar: &str, id: &str, me: &str, answer: Answer, note: Option<&str>) -> Gmail<calendar::Event>;
        fn edit_calendar_list(id: &str, edit: &calendar::list::ListEdit) -> Gmail<Option<calendar::Calendar>>;
    }
}

type Graphed<T> = Result<T, GraphError>;

either_client! {
    /// Microsoft Graph: the client for one account, or the in-memory Graph.
    AnyGraph(Graph, FakeGraph): GraphApi;
    now {
        fn granted() -> Option<mailrs_graph::Granted>;
    }
    later {
        fn me() -> Graphed<Me>;
        fn well_known(names: &[&str]) -> Graphed<Vec<Option<MailFolder>>>;
        fn folders(parent: Option<&str>, next: Option<&str>) -> Graphed<Page<MailFolder>>;
        fn categories() -> Graphed<Vec<MasterCategory>>;
        fn message_delta(folder: &str, link: Option<&str>, received_since: &str) -> Graphed<DeltaPage<mailrs_graph::Message>>;
        fn list_messages(listing: &Listing, next: Option<&str>) -> Graphed<Page<mailrs_graph::Message>>;
        fn messages(ids: &[String]) -> Graphed<Vec<Graphed<mailrs_graph::Message>>>;
        fn raw(id: &str, limit: usize) -> Graphed<Vec<u8>>;
        fn body(id: &str) -> Graphed<MessageBody>;
        fn attachment(message: &str, attachment: &str, limit: usize) -> Graphed<Vec<u8>>;
        fn apply(writes: &[Write]) -> Graphed<Vec<Graphed<()>>>;
        fn create_folder(parent: Option<&str>, name: &str) -> Graphed<MailFolder>;
        fn rename_folder(id: &str, name: &str) -> Graphed<MailFolder>;
        fn delete_folder(id: &str) -> Graphed<()>;
        fn set_category_color(id: &str, color: &str) -> Graphed<MasterCategory>;
        fn send_mime(raw: &[u8]) -> Graphed<()>;
        fn create_draft_mime(raw: &[u8]) -> Graphed<mailrs_graph::Message>;
        fn create_draft(draft: &Value) -> Graphed<mailrs_graph::Message>;
        fn upload_session(message: &str, name: &str, size: u64, is_inline: bool, content_id: Option<&str>) -> Graphed<String>;
        fn upload_chunk(url: &str, offset: u64, total: u64, bytes: &[u8]) -> Graphed<bool>;
        fn send_draft(id: &str) -> Graphed<()>;
        fn delete_message(id: &str) -> Graphed<()>;
        fn calendars() -> Graphed<Vec<GraphCalendar>>;
        fn calendar_view_delta(calendar: &str, link: Option<&str>, start: &str, end: &str) -> Graphed<DeltaPage<GraphEvent>>;
        fn event(id: &str) -> Graphed<GraphEvent>;
        fn instances(series: &str, start: &str, end: &str) -> Graphed<Vec<GraphEvent>>;
        fn original_starts(ids: &[String]) -> Graphed<Vec<Graphed<GraphEvent>>>;
        fn events(ids: &[String]) -> Graphed<Vec<Graphed<GraphEvent>>>;
        fn events_by_uid(calendar: &str, uid: &str) -> Graphed<Vec<GraphEvent>>;
        fn create_event(calendar: &str, body: &Value) -> Graphed<GraphEvent>;
        fn update_event(id: &str, body: &Value, etag: Option<&str>) -> Graphed<GraphEvent>;
        fn delete_event(id: &str, etag: Option<&str>) -> Graphed<()>;
        fn respond(id: &str, response: Response, comment: Option<&str>) -> Graphed<()>;
        fn calendar_view_of(calendar: &str, start: &str, end: &str, link: Option<&str>) -> Graphed<Page<GraphEvent>>;
        fn create_calendar(name: &str, hex: &str) -> Graphed<GraphCalendar>;
        fn update_calendar(id: &str, body: &Value) -> Graphed<GraphCalendar>;
        fn delete_calendar(id: &str) -> Graphed<()>;
        fn contact_folders() -> Graphed<Vec<ContactFolder>>;
        fn default_contact_folder() -> Graphed<Option<String>>;
        fn contact_delta(folder: &str, link: Option<&str>) -> Graphed<DeltaPage<GraphContact>>;
        fn contact_photo(id: &str, limit: usize) -> Graphed<Option<Vec<u8>>>;
        fn create_contact(body: &Value) -> Graphed<GraphContact>;
        fn contact_name(id: &str) -> Graphed<GraphContact>;
        fn update_contact(id: &str, body: &Value) -> Graphed<GraphContact>;
        fn rules() -> Graphed<Vec<MessageRule>>;
        fn create_rule(rule: &MessageRule) -> Graphed<MessageRule>;
        fn delete_rule(id: &str) -> Graphed<()>;
        fn automatic_replies() -> Graphed<AutomaticReplies>;
        fn set_automatic_replies(replies: &AutomaticReplies) -> Graphed<AutomaticReplies>;
        fn overrides() -> Graphed<Vec<Override>>;
        fn set_override(address: &str, other: bool) -> Graphed<Override>;
        fn delete_override(id: &str) -> Graphed<()>;
    }
}

type Imapped<T> = Result<T, ImapError>;

either_client! {
    /// An IMAP server: the client, or the in-memory server.
    AnyImap(ImapClient, FakeImap): ImapApi;
    now {}
    later {
        fn capabilities() -> Imapped<Capabilities>;
        fn list() -> Imapped<Vec<Listed>>;
        fn select(mailbox: &str, since: Option<Since>) -> Imapped<Selected>;
        fn flags(mailbox: &str, uids: &UidSet, changed_since: Option<u64>) -> Imapped<Vec<FlagsOf>>;
        fn search(mailbox: &str, keys: &str) -> Imapped<Vec<u32>>;
        fn headers(mailbox: &str, uids: &UidSet) -> Imapped<Vec<mailrs_imap::Fetched>>;
        fn body(mailbox: &str, uid: u32, section: &str) -> Imapped<Option<Vec<u8>>>;
        fn structure(mailbox: &str, uid: u32) -> Imapped<Option<BodyStructure>>;
        fn store(mailbox: &str, uids: &UidSet, add: bool, flags: &[String]) -> Imapped<()>;
        fn move_to(mailbox: &str, uids: &UidSet, to: &str) -> Imapped<Option<CopyUid>>;
        fn copy_to(mailbox: &str, uids: &UidSet, to: &str) -> Imapped<Option<CopyUid>>;
        fn expunge(mailbox: &str, uids: &UidSet) -> Imapped<()>;
        fn append(mailbox: &str, flags: &[String], raw: &[u8]) -> Imapped<Option<AppendUid>>;
        fn create(mailbox: &str) -> Imapped<()>;
        fn rename(from: &str, to: &str) -> Imapped<()>;
        fn delete(mailbox: &str) -> Imapped<()>;
        fn idle(mailbox: &str, limit: Duration) -> Imapped<Woke>;
    }
}

either_client! {
    /// An SMTP submission server: the client, or the in-memory sink.
    AnySmtp(SmtpClient, FakeSmtp): Submit;
    now {}
    later {
        fn submit(from: &str, to: &[String], raw: &[u8]) -> Imapped<()>;
    }
}

either_client! {
    /// A POP3 server: the client, or the in-memory server.
    AnyPop3(Pop3Client, FakePop3): Pop3Api;
    now {}
    later {
        fn connect() -> Result<mailrs_pop3::Capabilities, Pop3Error>;
        fn stat() -> Result<Stat, Pop3Error>;
        fn uidl() -> Result<UidlListing, Pop3Error>;
        fn list() -> Result<Vec<ListItem>, Pop3Error>;
        fn retr(id: u32, octets: u64) -> Result<Vec<u8>, Pop3Error>;
        fn top(id: u32, lines: u32) -> Result<Vec<u8>, Pop3Error>;
        fn dele(id: u32) -> Result<(), Pop3Error>;
        fn quit() -> Result<(), Pop3Error>;
    }
}

either_client! {
    /// A CalDAV or CardDAV server: the client, or the in-memory server.
    AnyDav(DavClient, FakeDav): DavApi;
    now {
        fn host() -> &str;
    }
    later {
        fn homes() -> Result<Homes, DavError>;
        fn collections(home: &str, kind: Kind) -> Result<Vec<Collection>, DavError>;
        fn state(collection: &str) -> Result<CollectionState, DavError>;
        fn sync(collection: &str, token: &str) -> Result<Synced, DavError>;
        fn members(collection: &str, kind: Kind, range: Option<(EpochMillis, EpochMillis)>) -> Result<Vec<Member>, DavError>;
        fn fetch(collection: &str, kind: Kind, hrefs: &[String]) -> Result<Vec<Fetched>, DavError>;
        fn get(href: &str) -> Result<Fetched, DavError>;
        fn put(href: &str, body: &str, kind: Kind, when: Precondition) -> Result<Option<String>, DavError>;
        fn delete(href: &str, etag: Option<&str>) -> Result<(), DavError>;
        fn find_uid(collection: &str, uid: &str) -> Result<Option<Fetched>, DavError>;
        fn auto_schedule() -> Result<bool, DavError>;
    }
}

either_client! {
    /// A ManageSieve server: the client, or the in-memory server.
    AnySieve(ManageSieveClient, FakeSieve): ManageSieveApi;
    now {}
    later {
        fn capabilities() -> Result<mailrs_sieve::client::Capabilities, SieveError>;
        fn scripts() -> Result<Vec<mailrs_sieve::client::Listed>, SieveError>;
        fn get(name: &str) -> Result<String, SieveError>;
        fn put(name: &str, script: &str) -> Result<(), SieveError>;
        fn activate(name: &str) -> Result<(), SieveError>;
    }
}
