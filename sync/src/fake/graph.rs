//! An in-memory Microsoft Graph for sync's tests and the demo's Outlook
//! account. It keeps a change log per resource the way Graph keeps delta
//! state, so a delta link answers what changed since it, a move shows as a
//! removal from one folder and an addition to another under the same id,
//! and a link older than `expire_links` answers `SyncStateLost`. Each area
//! lives in its own file: `mail`, `calendar`, `contacts`, `settings`.

mod calendar;
mod contacts;
mod mail;
mod settings;

use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, PoisonError};

use mailrs_graph::{
    AutomaticReplies, ContactFolder, DeltaPage, GraphCalendar, GraphContact, GraphError,
    GraphEvent, Granted, Listing, MailFolder, MasterCategory, Me, Message, MessageBody,
    MessageRule, Override, Page, Response, SCOPES, Write,
};
use serde_json::Value;

use crate::services::microsoft::GraphApi;

pub use mail::FakeMail;

type Answer<T> = Result<T, GraphError>;

/// The part of Graph a refusal covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Area {
    Mail,
    Calendar,
    Contacts,
    Rules,
    Replies,
}

/// A mail folder.
#[derive(Debug, Clone)]
pub struct FakeFolder {
    pub name: String,
    pub parent: Option<String>,
}

/// A message the fake holds: where it sits, what Graph says about it, its
/// MIME, and its files with their bytes.
#[derive(Debug, Clone)]
pub struct FakeMessage {
    pub folder: String,
    pub message: Message,
    pub raw: Vec<u8>,
    pub files: Vec<(mailrs_graph::AttachmentInfo, Vec<u8>)>,
}

/// One entry of a change log: `id` changed in `place` at `seq`. A delta
/// round over `place` reads the entries after its link's `seq` and
/// answers each id as it stands now, or as removed when it left.
#[derive(Debug, Clone)]
pub struct Logged {
    pub seq: u64,
    pub place: String,
    pub id: String,
}

/// An answer to an invitation, kept so a test can read the note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answered {
    pub event: String,
    pub response: Response,
    pub comment: Option<String>,
}

/// A query whose next link the fake must be able to carry on.
#[derive(Debug, Clone)]
pub enum Pending {
    Listing(Listing),
    View { calendar: String, start: String, end: String },
}

/// A file being uploaded onto a draft.
#[derive(Debug, Clone, Default)]
pub struct Upload {
    pub message: String,
    pub name: String,
    pub size: u64,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Default)]
pub struct GraphState {
    pub me: String,
    pub folders: BTreeMap<String, FakeFolder>,
    /// Well-known name to folder id.
    pub well_known: BTreeMap<String, String>,
    pub categories: Vec<MasterCategory>,
    pub messages: BTreeMap<String, FakeMessage>,
    pub mail_log: Vec<Logged>,
    pub sent: Vec<Vec<u8>>,
    pub uploads: HashMap<String, Upload>,
    pub calendars: Vec<GraphCalendar>,
    /// Event id to its calendar and the event.
    pub events: BTreeMap<String, (String, GraphEvent)>,
    pub event_log: Vec<Logged>,
    /// `transactionId` to the event it made, so a retried create finds it.
    pub transactions: HashMap<String, String>,
    pub responses: Vec<Answered>,
    /// The body of every event create and change, as sent.
    pub event_bodies: Vec<serde_json::Value>,
    pub contact_folders: Vec<ContactFolder>,
    /// Contact id to its folder and the contact.
    pub contacts: BTreeMap<String, (String, GraphContact)>,
    pub contact_log: Vec<Logged>,
    pub photos: HashMap<String, Vec<u8>>,
    pub rules: Vec<MessageRule>,
    pub replies: AutomaticReplies,
    pub overrides: Vec<Override>,
    /// Queries a next link points back to, by position.
    pub pending: Vec<Pending>,
    /// The log position below which every link is refused.
    pub expired_before: u64,
    pub seq: u64,
    pub next_id: u64,
    pub refused: HashMap<Area, GraphError>,
    pub granted: Option<Granted>,
}

impl GraphState {
    fn next_seq(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    fn new_id(&mut self, kind: &str) -> String {
        self.next_id += 1;
        format!("AAMk-{kind}-{}", self.next_id)
    }

    fn refuses(&self, area: Area) -> Answer<()> {
        match self.refused.get(&area) {
            Some(err) => Err(err.clone()),
            None => Ok(()),
        }
    }

    fn remember(&mut self, query: Pending) -> String {
        self.pending.push(query);
        (self.pending.len() - 1).to_string()
    }
}

pub struct FakeGraph {
    state: Mutex<GraphState>,
}

impl Default for FakeGraph {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeGraph {
    /// A mailbox with Outlook's six well-known folders, an empty default
    /// calendar and contact folder, every scope granted, and automatic
    /// replies off.
    pub fn new() -> FakeGraph {
        let mut state = GraphState {
            me: "me@outlook.com".into(),
            granted: Some(Granted::parse(&SCOPES.join(" "))),
            replies: AutomaticReplies {
                status: "disabled".into(),
                external_audience: "all".into(),
                ..AutomaticReplies::default()
            },
            ..GraphState::default()
        };
        for (name, shown) in [
            ("inbox", "Inbox"),
            ("sentitems", "Sent Items"),
            ("drafts", "Drafts"),
            ("deleteditems", "Deleted Items"),
            ("junkemail", "Junk Email"),
            ("archive", "Archive"),
        ] {
            let id = format!("AAMk-{name}");
            state.folders.insert(id.clone(), FakeFolder { name: shown.into(), parent: None });
            state.well_known.insert(name.into(), id);
        }
        state.calendars.push(GraphCalendar {
            id: "cal-1".into(),
            name: "Calendar".into(),
            hex_color: Some("#0078d4".into()),
            can_edit: true,
            is_default_calendar: true,
            ..GraphCalendar::default()
        });
        state
            .contact_folders
            .push(ContactFolder { id: "contacts-1".into(), display_name: "Contacts".into() });
        FakeGraph { state: Mutex::new(state) }
    }

    pub fn with<R>(&self, f: impl FnOnce(&mut GraphState) -> R) -> R {
        f(&mut self.state.lock().unwrap_or_else(PoisonError::into_inner))
    }

    pub fn folder_id(&self, well_known: &str) -> String {
        self.with(|s| s.well_known.get(well_known).cloned().unwrap_or_default())
    }

    pub fn add_folder(&self, name: &str, parent: Option<&str>) -> String {
        self.with(|s| {
            let id = s.new_id("folder");
            s.folders.insert(
                id.clone(),
                FakeFolder { name: name.into(), parent: parent.map(str::to_string) },
            );
            id
        })
    }

    pub fn add_category(&self, name: &str, color: &str) {
        self.with(|s| {
            let id = s.new_id("category");
            s.categories.push(MasterCategory { id, display_name: name.into(), color: color.into() });
        });
    }

    /// Makes every link handed out so far answer `SyncStateLost`, as Graph
    /// does once it drops a delta's state. A round started afterwards gets
    /// links past the mark, so it keeps its place.
    pub fn expire_links(&self) {
        self.with(|s| {
            s.expired_before = s.next_seq();
        });
    }

    pub fn withhold(&self, scope: &str) {
        self.with(|s| {
            let kept: Vec<&str> = SCOPES
                .iter()
                .copied()
                .filter(|sc| !sc.eq_ignore_ascii_case(scope))
                .filter(|sc| s.granted.as_ref().is_some_and(|g| g.has(sc)))
                .collect();
            s.granted = Some(Granted::parse(&kept.join(" ")));
        });
    }

    /// Every call in `area` answers `err` from now on, as a tenant that
    /// blocks the feature does.
    pub fn refuse(&self, area: Area, err: GraphError) {
        self.with(|s| s.refused.insert(area, err));
    }
}

/// A link of the fake's own: `fake:<kind>:<place>:<seq>[:<offset>]`. The
/// place never holds a colon.
fn link(kind: &str, place: &str, seq: u64, offset: Option<usize>) -> String {
    match offset {
        Some(o) => format!("fake:{kind}:{place}:{seq}:{o}"),
        None => format!("fake:{kind}:{place}:{seq}"),
    }
}

/// The `(kind, place, seq, offset)` of one of the fake's links.
fn read_link(text: &str) -> Option<(String, String, u64, usize)> {
    let mut parts = text.strip_prefix("fake:")?.splitn(4, ':');
    let kind = parts.next()?.to_string();
    let place = parts.next()?.to_string();
    let seq = parts.next()?.parse().ok()?;
    let offset = parts.next().map_or(Some(0), |o| o.parse().ok())?;
    Some((kind, place, seq, offset))
}

/// One delta page over a change log: the ids logged in `place` after
/// `since`, each as `present` finds it now or as removed, fifty at a time.
fn log_page<T>(
    log: &[Logged],
    place: &str,
    since: u64,
    offset: usize,
    now: u64,
    present: impl Fn(&str) -> Option<T>,
    removed: impl Fn(&str) -> T,
) -> (Vec<T>, Option<usize>) {
    let mut ids: Vec<&str> = Vec::new();
    for entry in log.iter().filter(|e| e.place == place && e.seq > since && e.seq <= now) {
        ids.retain(|id| *id != entry.id);
        ids.push(&entry.id);
    }
    let page: Vec<T> = ids
        .iter()
        .skip(offset)
        .take(mailrs_graph::PAGE_SIZE as usize)
        .map(|id| present(id).unwrap_or_else(|| removed(id)))
        .collect();
    let next = (offset + page.len() < ids.len()).then_some(offset + page.len());
    (page, next)
}

/// A whole delta round for `place`. A first round lists `start()` as it
/// stands, fifty at a time; a later one replays the log after its link.
/// The pages of a first round carry the log position they began at, so a
/// change logged while the round pages is answered by the next round.
struct Round<'a> {
    log: &'a [Logged],
    expired_before: u64,
    now: u64,
    place: &'a str,
}

impl Round<'_> {
    fn run<T: Clone>(
        &self,
        link_text: Option<&str>,
        start: impl FnOnce() -> Vec<T>,
        present: impl Fn(&str) -> Option<T>,
        removed: impl Fn(&str) -> T,
    ) -> Answer<DeltaPage<T>> {
        let (since, offset, starting) = match link_text {
            None => (self.now, 0, true),
            Some(text) => {
                let (kind, place, seq, offset) = read_link(text).ok_or(GraphError::SyncStateLost)?;
                if place != self.place || seq < self.expired_before {
                    return Err(GraphError::SyncStateLost);
                }
                (seq, offset, kind == "start")
            }
        };
        if starting {
            let all = start();
            let page: Vec<T> =
                all.iter().skip(offset).take(mailrs_graph::PAGE_SIZE as usize).cloned().collect();
            let more = offset + page.len() < all.len();
            return Ok(DeltaPage {
                next_link: more.then(|| link("start", self.place, since, Some(offset + page.len()))),
                delta_link: (!more).then(|| link("delta", self.place, since, None)),
                value: page,
            });
        }
        let (value, next) =
            log_page(self.log, self.place, since, offset, self.now, present, removed);
        Ok(DeltaPage {
            next_link: next.map(|o| link("delta", self.place, since, Some(o))),
            delta_link: next.is_none().then(|| link("delta", self.place, self.now, None)),
            value,
        })
    }
}

impl GraphApi for FakeGraph {
    fn granted(&self) -> Option<Granted> {
        self.with(|s| s.granted.clone())
    }

    async fn me(&self) -> Answer<Me> {
        Ok(self.with(|s| Me {
            display_name: Some("Me".into()),
            mail: Some(s.me.clone()),
            user_principal_name: Some(s.me.clone()),
        }))
    }

    async fn well_known(&self, names: &[&str]) -> Answer<Vec<Option<MailFolder>>> {
        self.with(|s| mail::well_known(s, names))
    }

    async fn folders(&self, parent: Option<&str>, next: Option<&str>) -> Answer<Page<MailFolder>> {
        self.with(|s| mail::folders(s, parent, next))
    }

    async fn categories(&self) -> Answer<Vec<MasterCategory>> {
        self.with(mail::categories)
    }

    async fn message_delta(&self, folder: &str, link: Option<&str>, received_since: &str) -> Answer<DeltaPage<Message>> {
        self.with(|s| mail::message_delta(s, folder, link, received_since))
    }

    async fn list_messages(&self, listing: &Listing, next: Option<&str>) -> Answer<Page<Message>> {
        self.with(|s| mail::list_messages(s, listing, next))
    }

    async fn messages(&self, ids: &[String]) -> Answer<Vec<Answer<Message>>> {
        self.with(|s| mail::messages(s, ids))
    }

    async fn raw(&self, id: &str, limit: usize) -> Answer<Vec<u8>> {
        self.with(|s| mail::raw(s, id, limit))
    }

    async fn body(&self, id: &str) -> Answer<MessageBody> {
        self.with(|s| mail::body(s, id))
    }

    async fn attachment(&self, message: &str, attachment: &str, limit: usize) -> Answer<Vec<u8>> {
        self.with(|s| mail::attachment(s, message, attachment, limit))
    }

    async fn apply(&self, writes: &[Write]) -> Answer<Vec<Answer<()>>> {
        self.with(|s| mail::apply(s, writes))
    }

    async fn create_folder(&self, parent: Option<&str>, name: &str) -> Answer<MailFolder> {
        self.with(|s| mail::create_folder(s, parent, name))
    }

    async fn rename_folder(&self, id: &str, name: &str) -> Answer<MailFolder> {
        self.with(|s| mail::rename_folder(s, id, name))
    }

    async fn delete_folder(&self, id: &str) -> Answer<()> {
        self.with(|s| mail::delete_folder(s, id))
    }

    async fn set_category_color(&self, id: &str, color: &str) -> Answer<MasterCategory> {
        self.with(|s| mail::set_category_color(s, id, color))
    }

    async fn send_mime(&self, raw: &[u8]) -> Answer<()> {
        self.with(|s| mail::send_mime(s, raw))
    }

    async fn create_draft_mime(&self, raw: &[u8]) -> Answer<Message> {
        self.with(|s| mail::create_draft_mime(s, raw))
    }

    async fn create_draft(&self, draft: &Value) -> Answer<Message> {
        self.with(|s| mail::create_draft(s, draft))
    }

    async fn upload_session(&self, message: &str, name: &str, size: u64) -> Answer<String> {
        self.with(|s| mail::upload_session(s, message, name, size))
    }

    async fn upload_chunk(&self, url: &str, offset: u64, total: u64, bytes: &[u8]) -> Answer<bool> {
        self.with(|s| mail::upload_chunk(s, url, offset, total, bytes))
    }

    async fn send_draft(&self, id: &str) -> Answer<()> {
        self.with(|s| mail::send_draft(s, id))
    }

    async fn delete_message(&self, id: &str) -> Answer<()> {
        self.with(|s| mail::delete_message(s, id))
    }


    async fn calendars(&self) -> Answer<Vec<GraphCalendar>> {
        self.with(calendar::calendars)
    }

    async fn calendar_view_delta(&self, calendar: &str, link: Option<&str>, start: &str, end: &str) -> Answer<DeltaPage<GraphEvent>> {
        self.with(|s| calendar::calendar_view_delta(s, calendar, link, start, end))
    }

    async fn event(&self, id: &str) -> Answer<GraphEvent> {
        self.with(|s| calendar::event(s, id))
    }

    async fn instances(&self, series: &str, start: &str, end: &str) -> Answer<Vec<GraphEvent>> {
        self.with(|s| calendar::instances(s, series, start, end))
    }

    async fn calendar_view(&self, start: &str, end: &str) -> Answer<Vec<GraphEvent>> {
        self.with(|s| calendar::calendar_view(s, start, end))
    }

    async fn events_by_uid(&self, uid: &str) -> Answer<Vec<GraphEvent>> {
        self.with(|s| calendar::events_by_uid(s, uid))
    }

    async fn create_event(&self, calendar: &str, body: &Value) -> Answer<GraphEvent> {
        self.with(|s| calendar::create_event(s, calendar, body))
    }

    async fn update_event(&self, id: &str, body: &Value, etag: Option<&str>) -> Answer<GraphEvent> {
        self.with(|s| calendar::update_event(s, id, body, etag))
    }

    async fn delete_event(&self, id: &str, etag: Option<&str>) -> Answer<()> {
        self.with(|s| calendar::delete_event(s, id, etag))
    }

    async fn respond(&self, id: &str, response: Response, comment: Option<&str>) -> Answer<()> {
        self.with(|s| calendar::respond(s, id, response, comment))
    }

    async fn calendar_view_of(&self, calendar: &str, start: &str, end: &str, link: Option<&str>) -> Answer<Page<GraphEvent>> {
        self.with(|s| calendar::calendar_view_of(s, calendar, start, end, link))
    }

    async fn create_calendar(&self, name: &str, hex: &str) -> Answer<GraphCalendar> {
        self.with(|s| calendar::create_calendar(s, name, hex))
    }

    async fn update_calendar(&self, id: &str, body: &Value) -> Answer<GraphCalendar> {
        self.with(|s| calendar::update_calendar(s, id, body))
    }

    async fn delete_calendar(&self, id: &str) -> Answer<()> {
        self.with(|s| calendar::delete_calendar(s, id))
    }


    async fn contact_folders(&self) -> Answer<Vec<ContactFolder>> {
        self.with(contacts::contact_folders)
    }

    async fn default_contact_folder(&self) -> Answer<Option<String>> {
        self.with(contacts::default_contact_folder)
    }

    async fn contact_delta(&self, folder: &str, link: Option<&str>) -> Answer<DeltaPage<GraphContact>> {
        self.with(|s| contacts::contact_delta(s, folder, link))
    }

    async fn contact_photo(&self, id: &str, limit: usize) -> Answer<Option<Vec<u8>>> {
        self.with(|s| contacts::contact_photo(s, id, limit))
    }

    async fn create_contact(&self, body: &Value) -> Answer<GraphContact> {
        self.with(|s| contacts::create_contact(s, body))
    }

    async fn update_contact(&self, id: &str, body: &Value) -> Answer<GraphContact> {
        self.with(|s| contacts::update_contact(s, id, body))
    }


    async fn rules(&self) -> Answer<Vec<MessageRule>> {
        self.with(settings::rules)
    }

    async fn create_rule(&self, rule: &MessageRule) -> Answer<MessageRule> {
        self.with(|s| settings::create_rule(s, rule))
    }

    async fn delete_rule(&self, id: &str) -> Answer<()> {
        self.with(|s| settings::delete_rule(s, id))
    }

    async fn automatic_replies(&self) -> Answer<AutomaticReplies> {
        self.with(settings::automatic_replies)
    }

    async fn set_automatic_replies(&self, replies: &AutomaticReplies) -> Answer<()> {
        self.with(|s| settings::set_automatic_replies(s, replies))
    }

    async fn overrides(&self) -> Answer<Vec<Override>> {
        self.with(settings::overrides)
    }

    async fn set_override(&self, address: &str, other: bool) -> Answer<Override> {
        self.with(|s| settings::set_override(s, address, other))
    }

    async fn delete_override(&self, id: &str) -> Answer<()> {
        self.with(|s| settings::delete_override(s, id))
    }
}

#[cfg(test)]
mod tests {
    use mailrs_graph::GraphError;

    use crate::fake::{Area, FakeGraph};
    use crate::services::microsoft::GraphApi;

    #[tokio::test]
    async fn a_refused_area_answers_its_error_and_the_rest_work() {
        let fake = FakeGraph::new();
        fake.refuse(Area::Calendar, GraphError::AccessDenied { code: "ErrorAccessDenied".into() });
        assert!(matches!(fake.calendars().await, Err(GraphError::AccessDenied { .. })));
        assert!(fake.categories().await.is_ok());
    }

    #[test]
    fn a_withheld_scope_leaves_the_grant() {
        let fake = FakeGraph::new();
        assert!(fake.granted().unwrap().has("Calendars.ReadWrite"));
        fake.withhold("Calendars.ReadWrite");
        assert!(!fake.granted().unwrap().has("Calendars.ReadWrite"));
    }
}
