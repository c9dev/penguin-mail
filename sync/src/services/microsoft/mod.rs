//! The Microsoft adapter: every service an Outlook.com or Microsoft 365
//! account offers, over Microsoft Graph. `G` is the Graph client, the real
//! `mailrs_graph::Graph` or `FakeGraph`. Mail syncs through each synced
//! folder's delta link; Graph keeps a message's id when it moves because
//! every call asks for immutable ids, so the store's id is Graph's and no
//! remote ref is kept. Outlook's categories are tags; Focused Inbox is the
//! category `FOCUS_OTHER`.

mod api;
mod cadence;
mod calendar;
mod contacts;
mod errors;
mod feed;
mod fetch;
mod folders;
mod outgoing;
mod rules;
mod search;
mod settings;
mod writes;

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::ops::RangeInclusive;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use mailrs_domain::mailbox::keyword::{FLAGGED, SEEN};
use mailrs_domain::{MailSet, RemoteMailbox, Role};
use mailrs_gmail::LabelColor;
use mailrs_mime::Parts;
use mailrs_graph::Granted;
use tokio::time::Instant;

pub use api::GraphApi;
pub(crate) use errors::backend;

use super::{
    Backfill, Changes, Found, IdentityService, KeywordsPage, MailBackend, MailCapabilities, RawMessage, Refused,
    Relocated, RemoteRef, SearchQuery, SendAsAddress, SyncState, Unapplied, Want, Withheld,
};
use crate::api::{DraftRef, SavedDraft};
use crate::{BackendError, MailOp};

/// What a Microsoft account's mail service can do.
const MICROSOFT: MailCapabilities = MailCapabilities {
    labels: false,
    server_threads: true,
    files_sent_mail: true,
    categories: false,
    delete_forever: true,
    batch_limit: mailrs_graph::BATCH_LIMIT,
    keywords: &[SEEN, FLAGGED],
    native_search: false,
    renames: false,
    restates: true,
    tags: true,
    focus: true,
};

/// What the adapter needs to know about its account.
#[derive(Debug, Clone)]
pub struct MicrosoftSettings {
    pub address: String,
    /// "Outlook" or "Microsoft 365".
    pub provider_name: String,
    pub window_days: i64,
}

/// The services the organization refused this run, each learned from a
/// 403 on a call whose scope the token carries.
#[derive(Debug, Default)]
struct RefusedNow {
    calendar: AtomicBool,
    contacts: AtomicBool,
    rules: AtomicBool,
    auto_reply: AtomicBool,
}

/// A service a tenant can refuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Service {
    Calendar,
    Contacts,
    Rules,
    AutoReply,
}

impl Service {
    /// The scope the service's calls need.
    fn scope(self) -> &'static str {
        match self {
            Service::Calendar => "Calendars.ReadWrite",
            Service::Contacts => "Contacts.ReadWrite",
            Service::Rules | Service::AutoReply => "MailboxSettings.ReadWrite",
        }
    }
}

/// What the adapter learned from its last listing and keeps between
/// looks. Its size follows the account's folders and tags, never its
/// mail.
#[derive(Debug, Default)]
pub(super) struct Known {
    /// A role to its folder's id.
    pub roles: BTreeMap<Role, String>,
    /// A folder's id to its path, `Parent/Child`.
    pub names: HashMap<String, String>,
    /// A folder's id to the messages Graph says it holds.
    pub totals: HashMap<String, u64>,
    /// A category's name to its tag id.
    pub tags: BTreeMap<String, String>,
    /// A category's name to Graph's id for it, which a colour change names.
    pub category_ids: BTreeMap<String, String>,
    /// Folders a person opened, which the feed follows beside the roles.
    pub followed: BTreeSet<String>,
    pub listed: bool,
    pub window_open: bool,
    pub last_slow: Option<Instant>,
}

/// The attachment ids of the last structures fetched, by part path, so a
/// file opened after its message costs one call.
#[derive(Debug, Default)]
struct Handles {
    recent: VecDeque<(String, Vec<(String, String)>)>,
}

impl Handles {
    const KEPT: usize = 64;

    fn remember(&mut self, id: &str, handles: Vec<(String, String)>) {
        self.recent.retain(|(m, _)| m != id);
        if self.recent.len() == Self::KEPT {
            self.recent.pop_front();
        }
        self.recent.push_back((id.to_string(), handles));
    }

    fn get(&self, id: &str, path: &str) -> Option<String> {
        let (_, handles) = self.recent.iter().find(|(m, _)| m == id)?;
        handles.iter().find(|(p, _)| p == path).map(|(_, h)| h.clone())
    }
}

pub struct Microsoft<G> {
    graph: Arc<G>,
    settings: Arc<MicrosoftSettings>,
    known: Arc<Mutex<Known>>,
    handles: Arc<Mutex<Handles>>,
    refused: Arc<RefusedNow>,
}

// By hand: a derive would ask for `G: Clone`, and the client is shared.
impl<G> Clone for Microsoft<G> {
    fn clone(&self) -> Self {
        Microsoft {
            graph: Arc::clone(&self.graph),
            settings: Arc::clone(&self.settings),
            known: Arc::clone(&self.known),
            handles: Arc::clone(&self.handles),
            refused: Arc::clone(&self.refused),
        }
    }
}

/// The tag id for an Outlook category. A message names its categories by
/// name, so the name is the id; the prefix keeps it apart from a folder's.
pub(super) fn tag_id(name: &str) -> String {
    format!("category:{name}")
}

pub(super) fn tag_name(id: &str) -> Option<&str> {
    id.strip_prefix("category:")
}

impl<G: GraphApi> Microsoft<G> {
    pub fn new(graph: Arc<G>, settings: MicrosoftSettings) -> Self {
        Microsoft {
            graph,
            settings: Arc::new(settings),
            known: Arc::new(Mutex::new(Known::default())),
            handles: Arc::new(Mutex::new(Handles::default())),
            refused: Arc::new(RefusedNow::default()),
        }
    }

    pub(super) fn graph(&self) -> &G {
        &self.graph
    }

    pub(super) fn settings(&self) -> &MicrosoftSettings {
        &self.settings
    }

    pub(super) fn known(&self) -> MutexGuard<'_, Known> {
        self.known.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// What the account's consent leaves out.
    pub fn withheld(&self) -> Withheld {
        withheld(self.graph.granted().as_ref())
    }

    pub fn refused(&self) -> Refused {
        let on = |flag: &AtomicBool| flag.load(Ordering::Relaxed);
        Refused {
            calendar: on(&self.refused.calendar),
            contacts: on(&self.refused.contacts),
            rules: on(&self.refused.rules),
            auto_reply: on(&self.refused.auto_reply),
        }
    }

    /// The backend error for a service call's failure. A 403 on a call
    /// whose scope the token carries is the organization's refusal: the
    /// service turns off for the run and answers `Unsupported`, which
    /// Preferences explains. Without the scope it is a permission to ask
    /// for.
    pub(super) fn service_error(&self, service: Service, err: mailrs_graph::GraphError) -> crate::BackendError {
        let granted = self.graph.granted().is_none_or(|g| g.has(service.scope()));
        if matches!(err, mailrs_graph::GraphError::AccessDenied { .. }) && granted {
            let flag = match service {
                Service::Calendar => &self.refused.calendar,
                Service::Contacts => &self.refused.contacts,
                Service::Rules => &self.refused.rules,
                Service::AutoReply => &self.refused.auto_reply,
            };
            flag.store(true, Ordering::Relaxed);
            tracing::info!(?service, "the organization refuses this service");
            return crate::BackendError::Unsupported;
        }
        backend(err)
    }

    fn remember(&self, id: &str, handles: Vec<(String, String)>) {
        self.handles.lock().unwrap_or_else(PoisonError::into_inner).remember(id, handles);
    }

    fn remembered(&self, id: &str, path: &str) -> Option<String> {
        self.handles.lock().unwrap_or_else(PoisonError::into_inner).get(id, path)
    }
}

/// What `granted` leaves out, by the scope each feature needs. `None`
/// means the grant is not known yet, and then nothing is withheld.
pub fn withheld(granted: Option<&Granted>) -> Withheld {
    let Some(granted) = granted else {
        return Withheld::NONE;
    };
    let lacks = |scope: &str| !granted.has(scope);
    Withheld {
        settings: lacks("MailboxSettings.ReadWrite"),
        delete: lacks("Mail.ReadWrite"),
        contacts: lacks("Contacts.ReadWrite"),
        change_contacts: lacks("Contacts.ReadWrite"),
        calendar: lacks("Calendars.ReadWrite"),
        calendar_list: lacks("Calendars.ReadWrite"),
        calendars: lacks("Calendars.ReadWrite"),
        change_calendar_list: lacks("Calendars.ReadWrite"),
        // Graph needs no Drive scope; the window gates event files by
        // `Offers::event_files`.
        drive: false,
    }
}

impl<G: GraphApi> MailBackend for Microsoft<G> {
    fn capabilities(&self) -> MailCapabilities {
        MICROSOFT
    }

    fn provider_name(&self) -> &str {
        &self.settings.provider_name
    }

    fn person_waiting(&self) -> bool {
        false
    }

    async fn stand_by(&self, wait: Duration) {
        tokio::time::sleep(wait).await;
    }

    fn made_by_person(&self, id: &str) -> bool {
        !self.known().roles.values().any(|r| r == id)
    }

    async fn mailboxes(&self) -> Result<Vec<RemoteMailbox>, BackendError> {
        self.list_mailboxes().await
    }

    async fn changes(&self, since: Option<&SyncState>) -> Result<Changes, BackendError> {
        self.feed(since).await
    }

    async fn create_mailbox(&self, name: &str) -> Result<RemoteMailbox, BackendError> {
        self.make_folder(name).await
    }

    async fn rename_mailbox(&self, id: &str, name: &str) -> Result<RemoteMailbox, BackendError> {
        self.rename_folder(id, name).await
    }

    async fn delete_mailbox(&self, id: &str) -> Result<(), BackendError> {
        self.remove_folder(id).await
    }

    async fn set_mailbox_color(&self, id: &str, color: &LabelColor) -> Result<RemoteMailbox, BackendError> {
        self.color_tag(id, color).await
    }

    async fn mailbox_threads(&self, id: &str) -> Result<u64, BackendError> {
        self.known().totals.get(id).copied().ok_or(BackendError::NotFound)
    }

    fn follow(&self, mailbox: &str) -> bool {
        self.known().followed.insert(mailbox.to_string())
    }

    fn set_window_open(&self, open: bool) {
        self.known().window_open = open;
    }

    fn poll_interval(&self) -> Option<Duration> {
        Some(self.poll_every())
    }

    /// Graph cannot push to the app, so the looks are all there is.
    async fn watch(&self) {
        std::future::pending::<()>().await;
    }

    /// The feed names every flag change, so there is nothing to page.
    async fn keywords_in(&self, _: &str, _: u32, _: RangeInclusive<u32>) -> Result<KeywordsPage, BackendError> {
        Ok(KeywordsPage::default())
    }

    async fn uidvalidity(&self, _: &str) -> Result<Option<u32>, BackendError> {
        Ok(None)
    }

    async fn keywords_stored(&self, _: &str) -> Result<&'static [&'static str], BackendError> {
        Ok(MICROSOFT.keywords)
    }

    async fn backfill(&self, days: i64, cursor: Option<&str>) -> Result<Backfill, BackendError> {
        self.backfill_page(days, cursor).await
    }

    async fn window_ids(&self, days: i64, mailbox: Option<&str>) -> Result<Vec<RemoteRef>, BackendError> {
        self.ids_in(Some(days), mailbox.map(str::to_string)).await
    }

    async fn inbox_ids(&self) -> Result<Vec<RemoteRef>, BackendError> {
        let inbox = self.known().roles.get(&Role::Inbox).cloned();
        self.ids_in(None, inbox).await
    }

    async fn search(&self, query: &SearchQuery, limit: usize) -> Result<Vec<RemoteRef>, BackendError> {
        self.find(query, limit).await
    }

    async fn find_sent(&self, message_id: &str) -> Result<Option<String>, BackendError> {
        self.sent_with(message_id).await
    }

    async fn fetch(&self, wants: Vec<Want>) -> Result<Found, BackendError> {
        self.fetch_metas(wants).await
    }

    async fn fetch_whole(&self, threads: Vec<String>) -> Result<Found, BackendError> {
        self.fetch_threads(threads).await
    }

    async fn fetch_raw(&self, ids: &[String]) -> Result<Vec<RawMessage>, BackendError> {
        self.raws(ids).await
    }

    /// Graph files what it sends, and a draft goes through `save_draft`.
    async fn append(&self, _: &[u8], _: &str) -> Result<String, BackendError> {
        Err(BackendError::Unsupported)
    }

    async fn fetch_structure(&self, id: &str) -> Result<Parts, BackendError> {
        self.structure(id).await
    }

    async fn fetch_part(&self, id: &str, path: &str) -> Result<Vec<u8>, BackendError> {
        self.part(id, path).await
    }

    fn mailbox_for(&self, role: Role) -> Option<String> {
        self.known().roles.get(&role).cloned()
    }

    fn set_of(&self, id: &str) -> MailSet {
        let role = self.known().roles.iter().find(|(_, folder)| *folder == id).map(|(r, _)| *r);
        match role {
            Some(role) => MailSet::Role(role),
            None => MailSet::Mailbox(id.to_string()),
        }
    }

    async fn apply(&self, messages: &[String], ops: &[MailOp]) -> Result<Vec<Relocated>, Unapplied> {
        self.write(messages, ops).await
    }

    async fn send(&self, raw: &[u8], _thread_id: Option<&str>) -> Result<String, BackendError> {
        self.send_raw(raw).await
    }

    /// The thread id is Graph's to choose, so it is not used.
    async fn save_draft(
        &self,
        draft_id: Option<&str>,
        raw: &[u8],
        _thread_id: Option<&str>,
    ) -> Result<SavedDraft, BackendError> {
        self.save(draft_id, raw).await
    }

    async fn send_draft(&self, draft_id: &str) -> Result<String, BackendError> {
        self.send_saved(draft_id).await
    }

    async fn delete_draft(&self, draft_id: &str) -> Result<(), BackendError> {
        self.drop_draft(draft_id).await
    }

    async fn list_drafts(&self) -> Result<Vec<DraftRef>, BackendError> {
        self.drafts().await
    }
}

impl<G: GraphApi> IdentityService for Microsoft<G> {
    /// The account's own address. The name stays whatever the person gave
    /// Preferences, as for an IMAP account.
    async fn identities(&self) -> Result<Vec<SendAsAddress>, BackendError> {
        Ok(vec![SendAsAddress {
            email: self.settings.address.clone(),
            name: None,
            signature: String::new(),
            default: true,
        }])
    }
}
