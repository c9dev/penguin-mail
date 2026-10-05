//! The Google adapter: every service a Google account offers, over one
//! Gmail client. `G` is that client, the real `AccountClient` or
//! `FakeGmail` for tests and the demo. Gmail's errors become backend
//! errors here, and Gmail's quota and pacing show nowhere else in the
//! services.

mod changes;
mod fetch;
mod writes;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};
use std::ops::RangeInclusive;
use std::time::Duration;

use mailrs_domain::calendar as model;
use mailrs_domain::invitation::Answer;
use mailrs_domain::mailbox::keyword;
use mailrs_domain::{EpochMillis, Filter, MailSet, MailboxKind, RemoteMailbox, Role, Vacation};
use mailrs_gmail::labels as gmail;
use mailrs_gmail::{
    CALENDAR_LIST_SCOPE, CALENDAR_LIST_WRITE_SCOPE, CALENDAR_SCOPE, CALENDARS_SCOPE,
    CONTACTS_SCOPE, CONTACTS_WRITE_SCOPE,
    ConnectionsPage, ContactFields, DELETE_SCOPE, DRIVE_FILE_SCOPE, GmailError, Granted,
    LabelColor, Person, RemoteLabel, SETTINGS_SCOPE, SendAs, limiter, structure,
};
use mailrs_mime::Parts;
use mailrs_mime::html::html_to_text;

use super::{
    AutoReplyService, Backfill, CalendarService, Changes, ContactsService, Found, IdentityService,
    KeywordsPage, MailBackend, MailCapabilities, Priority, RawMessage, Relocated, RemoteRef,
    RulesService, SearchQuery, SendAsAddress, SyncState, Unapplied, Want, Withheld, priority,
};
use crate::api::{DraftRef, GmailApi, SavedDraft};
use crate::{BackendError, MailOp};

/// Page size for window listings, whose every id costs a metadata fetch
/// after it. A page of 100 is about two and a half seconds of an
/// account's budget, which is what paces backfill.
pub const LIST_PAGE_SIZE: u32 = 100;

/// Page size for a listing that needs only ids, such as the inbox check.
/// Gmail's most, for the same 5 units a call as a page of 100.
pub const ID_PAGE_SIZE: u32 = 500;

/// A Google account's services, all over the one client `G`, which spends
/// one quota bucket for all of them.
pub struct Google<G> {
    gmail: Arc<G>,
    /// The attachment handles of the last structures fetched, so a file
    /// opened right after its message costs one call.
    handles: Arc<Mutex<Handles>>,
    /// Capabilities that replace Gmail's, set only on a fake account.
    capabilities: Option<MailCapabilities>,
}

/// What Gmail's mail service can do.
const GMAIL: MailCapabilities = MailCapabilities {
    labels: true,
    server_threads: true,
    files_sent_mail: true,
    categories: true,
    delete_forever: true,
    // Gmail keeps these three as its UNREAD, STARRED and MUTE labels.
    keywords: &[keyword::SEEN, keyword::FLAGGED, keyword::MUTED],
    native_search: true,
    batch_limit: mailrs_gmail::BATCH_LIMIT,
    renames: false,
    restates: false,
    tags: false,
    focus: false,
    local_mailboxes: false,
    files_muted_replies: true,
};

impl<G> Google<G> {
    pub fn new(gmail: Arc<G>) -> Self {
        Google {
            gmail,
            handles: Arc::new(Mutex::new(Handles::default())),
            capabilities: None,
        }
    }

    /// Answers `caps` instead of Gmail's, for a fake account whose server
    /// does less.
    #[cfg(any(test, feature = "fake"))]
    pub fn with_capabilities(mut self, caps: MailCapabilities) -> Self {
        self.capabilities = Some(caps);
        self
    }

    /// The handle `fetch_structure` last saw at `path` of message `id`.
    fn remembered(&self, id: &str, path: &str) -> Option<String> {
        self.handles
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(id, path)
    }

    fn remember(&self, id: &str, handles: Vec<(String, String)>) {
        self.handles
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remember(id, handles);
    }
}

// By hand, since a derive would ask for `G: Clone` and the client is
// shared rather than copied.
impl<G> Clone for Google<G> {
    fn clone(&self) -> Self {
        Google {
            gmail: Arc::clone(&self.gmail),
            handles: Arc::clone(&self.handles),
            capabilities: self.capabilities,
        }
    }
}

/// The attachment handles of the last structures fetched, so a file
/// opened right after its message costs one call. Gmail's handle for a
/// part changes from fetch to fetch, and any recent one works.
#[derive(Default)]
struct Handles {
    /// Message id and its handles by path, oldest first.
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
        self.recent
            .iter()
            .find(|(m, _)| m == id)?
            .1
            .iter()
            .find(|(p, _)| p == path)
            .map(|(_, h)| h.clone())
    }
}

/// Runs one Gmail call at the priority of the work around it. Gmail's
/// quota bucket reads a task-local of its own, so background work is
/// marked there too.
async fn paced<T>(call: impl Future<Output = T>) -> T {
    match priority() {
        Priority::Background => limiter::background(call).await,
        Priority::Foreground => call.await,
    }
}

impl<G: GmailApi> Google<G> {
    /// What this account's own client believes Google withholds.
    pub fn withheld(&self) -> Withheld {
        withheld(self.gmail.granted().as_ref())
    }

    /// Whether the account is on Google Workspace, which keeps out of
    /// office and focus time; a personal account keeps neither.
    pub fn workspace(&self) -> bool {
        self.gmail.workspace()
    }
}

/// Whether `address` belongs to a Google Workspace account rather than a
/// personal one: Google gives a personal account an address at
/// `gmail.com` or `googlemail.com`. Any other domain may be Workspace; which
/// edition it runs cannot be read with the scopes the app holds, so a
/// save Google turns down still says why.
pub fn is_workspace(address: &str) -> bool {
    let domain = address.trim().rsplit_once('@').map_or("", |(_, domain)| domain);
    !domain.is_empty() && !domain.eq_ignore_ascii_case("gmail.com") && !domain.eq_ignore_ascii_case("googlemail.com")
}

/// What `granted` leaves out, by the scope table CONTEXT.md's Withheld
/// permission entry gives. `None` means the account's grants are not
/// known yet, and then nothing is withheld: an account still starting
/// offers everything until a read says otherwise.
pub fn withheld(granted: Option<&Granted>) -> Withheld {
    let Some(granted) = granted else {
        return Withheld::NONE;
    };
    Withheld {
        settings: !granted.has(SETTINGS_SCOPE),
        delete: !granted.has(DELETE_SCOPE),
        contacts: !granted.has(CONTACTS_SCOPE),
        change_contacts: !granted.has(CONTACTS_WRITE_SCOPE),
        calendar: !granted.has(CALENDAR_SCOPE),
        calendar_list: !granted.has(CALENDAR_LIST_SCOPE),
        calendars: !granted.has(CALENDARS_SCOPE),
        change_calendar_list: !granted.has(CALENDAR_LIST_WRITE_SCOPE),
        drive: !granted.has(DRIVE_FILE_SCOPE),
    }
}

impl<G: GmailApi> MailBackend for Google<G> {
    fn capabilities(&self) -> MailCapabilities {
        self.capabilities.unwrap_or(GMAIL)
    }

    fn provider_name(&self) -> &str {
        "Gmail"
    }

    fn mailbox_for(&self, role: Role) -> Option<String> {
        gmail::label_of_role(role).map(str::to_string)
    }

    fn set_of(&self, id: &str) -> MailSet {
        gmail::set_of(id)
    }

    /// Gmail never renames a message, so nothing is relocated.
    async fn apply(
        &self,
        messages: &[String],
        ops: &[MailOp],
    ) -> Result<Vec<Relocated>, Unapplied> {
        self.write(messages, ops).await.map(|()| Vec::new())
    }

    fn person_waiting(&self) -> bool {
        self.gmail
            .quota()
            .is_some_and(|quota| quota.foreground_waiting())
    }

    async fn stand_by(&self, wait: Duration) {
        // The guard counts the whole wait as the person's, so backfill
        // stands aside for all of it and not only while the retry asks
        // the bucket.
        let _waiting = self.gmail.quota().map(|quota| quota.waiting());
        tokio::time::sleep(wait).await;
    }

    async fn backfill(&self, days: i64, cursor: Option<&str>) -> Result<Backfill, BackendError> {
        match paced(
            self.gmail
                .list_messages(&fetch::window_query(days), cursor, LIST_PAGE_SIZE),
        )
        .await
        {
            Ok(page) => Ok(Backfill {
                refs: page.messages.into_iter().map(RemoteRef::from).collect(),
                next: page.next_page_token,
            }),
            // Gmail answers 400 for a page token it no longer takes.
            Err(GmailError::Http { status: 400, .. }) if cursor.is_some() => {
                Err(BackendError::StateLost)
            }
            Err(err) => Err(err.into()),
        }
    }

    async fn window_ids(
        &self,
        days: i64,
        mailbox: Option<&str>,
    ) -> Result<Vec<RemoteRef>, BackendError> {
        self.every_id(mailbox, &fetch::window_query(days)).await
    }

    async fn inbox_ids(&self) -> Result<Vec<RemoteRef>, BackendError> {
        self.every_id(None, "in:inbox").await
    }

    async fn search(
        &self,
        query: &SearchQuery,
        limit: usize,
    ) -> Result<Vec<RemoteRef>, BackendError> {
        // A tree prints as the text the folders and smart mailboxes sent
        // before they became trees, so Gmail answers the same search.
        let text = match query {
            SearchQuery::Native(text) => text.clone(),
            SearchQuery::Tree(tree) => mailrs_gmail::query::print(tree),
        };
        // One call of 5 units whatever the count, up to Gmail's page of 500.
        let size = u32::try_from(limit).unwrap_or(u32::MAX).min(ID_PAGE_SIZE);
        let page = paced(self.gmail.list_messages(&text, None, size)).await?;
        Ok(page
            .messages
            .into_iter()
            .take(limit)
            .map(RemoteRef::from)
            .collect())
    }

    async fn find_sent(&self, message_id: &str) -> Result<Option<String>, BackendError> {
        // A draft carries the same header as the message it becomes, so
        // the search asks for sent mail alone.
        let query = format!("in:sent rfc822msgid:{message_id}");
        let page = paced(self.gmail.list_messages(&query, None, 1)).await?;
        Ok(page.messages.into_iter().next().map(|m| m.id))
    }

    async fn fetch(&self, wants: Vec<Want>) -> Result<Found, BackendError> {
        self.fetch_planned(wants).await
    }

    async fn fetch_whole(&self, threads: Vec<String>) -> Result<Found, BackendError> {
        self.fetch_threads(threads).await
    }

    async fn fetch_raw(&self, ids: &[String]) -> Result<Vec<RawMessage>, BackendError> {
        let mut raws = Vec::with_capacity(ids.len());
        for id in ids {
            let bytes = paced(self.gmail.raw_message(id)).await?;
            raws.push(RawMessage {
                id: id.clone(),
                bytes,
            });
        }
        Ok(raws)
    }

    /// Gmail files a copy of what it sends under Sent itself, so nothing
    /// asks it to file one.
    async fn append(&self, _raw: &[u8], _mailbox: &str) -> Result<String, BackendError> {
        Err(BackendError::Unsupported)
    }

    /// Gmail sends a text part by reference when it carries a file name,
    /// which every Google Calendar invitation does, or when it is large.
    /// Each such part the body needs costs one more call. A failed fetch
    /// leaves the message readable without that part, and marks the parts
    /// incomplete so the body read from them is not kept.
    async fn fetch_structure(&self, id: &str) -> Result<Parts, BackendError> {
        let message = paced(self.gmail.message_structure(id)).await?;
        let payload = message.payload.ok_or(BackendError::NotFound)?;
        let mut parts = structure::parts_of(&payload);
        for (path, handle) in structure::text_by_reference(&payload) {
            match paced(self.gmail.attachment(id, &handle)).await {
                Ok(bytes) => parts.set_data(&path, bytes),
                Err(err) => {
                    tracing::warn!(message = id, %err, "could not fetch a text part sent by reference");
                    parts.incomplete = true;
                }
            }
        }
        self.remember(id, structure::handles(&payload));
        Ok(parts)
    }

    /// Gmail's attachment ids can change between fetches: a remembered
    /// handle Gmail refuses is retried once through a fresh structure
    /// fetch, rather than failing the whole read over a stale id.
    async fn fetch_part(&self, id: &str, path: &str) -> Result<Vec<u8>, BackendError> {
        let handle = match self.remembered(id, path) {
            Some(handle) => handle,
            None => {
                let parts = self.fetch_structure(id).await?;
                let part = parts.find(path);
                // A part Gmail sent inline came with the structure.
                if let Some(data) = part.and_then(|p| p.data.clone()) {
                    return Ok(data);
                }
                match self.remembered(id, path) {
                    Some(handle) => handle,
                    // Gmail sends a forwarded message's parts but gives
                    // the message itself no handle. Its bytes are only
                    // in the raw message, which costs the whole message.
                    None if part.is_some_and(|p| p.mime_type == "message/rfc822") => {
                        let raw = paced(self.gmail.raw_message(id)).await?;
                        return mailrs_mime::part(&raw, path).ok_or(BackendError::NotFound);
                    }
                    None => return Err(BackendError::NotFound),
                }
            }
        };
        match paced(self.gmail.attachment(id, &handle)).await {
            Err(GmailError::NotFound | GmailError::Http { status: 400, .. }) => {
                let parts = self.fetch_structure(id).await?;
                // The retry's structure may answer the part inline, as the
                // cold path's does.
                if let Some(data) = parts.find(path).and_then(|p| p.data.clone()) {
                    return Ok(data);
                }
                let retried = self.remembered(id, path).ok_or(BackendError::NotFound)?;
                Ok(paced(self.gmail.attachment(id, &retried)).await?)
            }
            other => Ok(other?),
        }
    }

    async fn send(&self, raw: &[u8], thread_id: Option<&str>) -> Result<String, BackendError> {
        Ok(paced(self.gmail.send(raw, thread_id)).await?)
    }

    async fn save_draft(
        &self,
        draft_id: Option<&str>,
        raw: &[u8],
        thread_id: Option<&str>,
    ) -> Result<SavedDraft, BackendError> {
        Ok(paced(self.gmail.save_draft(draft_id, raw, thread_id)).await?)
    }

    async fn send_draft(&self, draft_id: &str) -> Result<String, BackendError> {
        Ok(paced(self.gmail.send_draft(draft_id)).await?)
    }

    async fn delete_draft(&self, draft_id: &str) -> Result<(), BackendError> {
        Ok(paced(self.gmail.delete_draft(draft_id)).await?)
    }

    async fn list_drafts(&self) -> Result<Vec<DraftRef>, BackendError> {
        Ok(paced(self.gmail.list_drafts()).await?)
    }

    fn made_by_person(&self, id: &str) -> bool {
        gmail::kind_of(id) == MailboxKind::Label
    }

    async fn mailboxes(&self) -> Result<Vec<RemoteMailbox>, BackendError> {
        Ok(paced(self.gmail.labels())
            .await?
            .into_iter()
            .map(remote_mailbox)
            .collect())
    }

    async fn changes(&self, since: Option<&SyncState>) -> Result<Changes, BackendError> {
        self.history_changes(since).await
    }

    async fn create_mailbox(&self, name: &str) -> Result<RemoteMailbox, BackendError> {
        Ok(remote_mailbox(paced(self.gmail.create_label(name)).await?))
    }

    async fn rename_mailbox(&self, id: &str, name: &str) -> Result<RemoteMailbox, BackendError> {
        Ok(remote_mailbox(
            paced(self.gmail.rename_label(id, name)).await?,
        ))
    }

    async fn delete_mailbox(&self, id: &str) -> Result<(), BackendError> {
        Ok(paced(self.gmail.delete_label(id)).await?)
    }

    async fn set_mailbox_color(
        &self,
        id: &str,
        color: &LabelColor,
    ) -> Result<RemoteMailbox, BackendError> {
        Ok(remote_mailbox(
            paced(self.gmail.set_label_color(id, color)).await?,
        ))
    }

    async fn mailbox_threads(&self, id: &str) -> Result<u64, BackendError> {
        Ok(paced(self.gmail.label_threads(id)).await?)
    }

    /// Gmail's history speaks for every label, so there is nothing more to
    /// follow.
    fn follow(&self, _mailbox: &str) -> bool {
        false
    }

    /// The engine's own interval paces Gmail whether the window is open or
    /// not.
    fn set_window_open(&self, _open: bool) {}

    fn poll_interval(&self) -> Option<Duration> {
        None
    }

    /// Gmail pushes nothing to a desktop client, so the engine's poll is
    /// the only look.
    async fn watch(&self) {
        std::future::pending::<()>().await
    }

    /// Gmail's history names every flag change, so nobody asks.
    async fn keywords_in(
        &self,
        _mailbox: &str,
        _uidvalidity: u32,
        _uids: RangeInclusive<u32>,
    ) -> Result<KeywordsPage, BackendError> {
        Err(BackendError::Unsupported)
    }

    /// Gmail names a message by its own id, which no label renumbers.
    async fn uidvalidity(&self, _mailbox: &str) -> Result<Option<u32>, BackendError> {
        Ok(None)
    }

    /// Gmail stores the same keywords whatever the label.
    async fn keywords_stored(
        &self,
        _mailbox: &str,
    ) -> Result<&'static [&'static str], BackendError> {
        Ok(self.capabilities().keywords)
    }
}

/// A Gmail label as a server mailbox. Gmail lists its keyword and category
/// labels too (`STARRED`, `UNREAD`, `CATEGORY_SOCIAL`); they arrive as
/// system mailboxes without a role, which the store keeps for the label
/// list and never files mail under. Gmail's choice to hide a label from
/// its own list is not read yet, so `hidden` is false.
fn remote_mailbox(label: RemoteLabel) -> RemoteMailbox {
    RemoteMailbox {
        role: gmail::role_of(&label.id),
        kind: match label.kind.as_deref() {
            Some("system") => MailboxKind::System,
            _ => MailboxKind::Label,
        },
        color: label.color.map(|c| c.background_color),
        hidden: false,
        id: label.id,
        name: label.name,
    }
}

impl<G: GmailApi> CalendarService for Google<G> {
    async fn calendars(&self) -> Result<Vec<model::Calendar>, BackendError> {
        Ok(paced(self.gmail.calendars()).await?)
    }

    async fn event_changes(
        &self,
        calendar: &str,
        token: Option<&str>,
        page: Option<&str>,
        from: EpochMillis,
    ) -> Result<model::EventPage, BackendError> {
        match paced(self.gmail.event_changes(calendar, token, page, from)).await {
            // Only the call that reads changes can say the server lost
            // its place; elsewhere a 404 means an event is gone.
            Err(GmailError::ExpiredSyncToken) => Err(BackendError::StateLost),
            other => Ok(other?),
        }
    }

    async fn event_range(
        &self,
        calendar: &str,
        from: EpochMillis,
        to: EpochMillis,
        page: Option<&str>,
    ) -> Result<model::EventPage, BackendError> {
        Ok(paced(self.gmail.event_range(calendar, from, to, page)).await?)
    }

    async fn put_event(
        &self,
        event: &model::Event,
        etag: Option<&str>,
        create: bool,
        notify: model::Notify,
    ) -> Result<model::Event, BackendError> {
        paced(self.gmail.put_event(event, etag, create, notify)).await.map_err(calendar_write_error)
    }

    async fn remove_event(
        &self,
        calendar: &str,
        id: &str,
        etag: Option<&str>,
        notify: model::Notify,
    ) -> Result<(), BackendError> {
        paced(self.gmail.remove_event(calendar, id, etag, notify)).await.map_err(calendar_write_error)
    }

    async fn import_event(&self, event: &model::Event) -> Result<model::Event, BackendError> {
        paced(self.gmail.import_event(event)).await.map_err(calendar_write_error)
    }

    async fn upload_attachment(
        &self,
        file: &model::Attachment,
        sent: std::sync::Arc<std::sync::atomic::AtomicU64>,
    ) -> Result<model::Attachment, BackendError> {
        let Some(path) = &file.waiting else {
            // Already uploaded: nothing to send.
            return Ok(file.clone());
        };
        let path = std::path::Path::new(path);
        Ok(self.gmail.upload_to_drive(path, &file.title, &file.mime_type, sent).await?)
    }

    async fn share_file(&self, file_id: &str, email: &str) -> Result<(), BackendError> {
        Ok(self.gmail.share_file(file_id, email).await?)
    }

    async fn move_event(
        &self,
        event: &model::Event,
        destination: &str,
        notify: model::Notify,
    ) -> Result<model::Event, BackendError> {
        paced(self.gmail.move_event(event, destination, notify)).await.map_err(calendar_write_error)
    }

    async fn answer_event(
        &self,
        calendar: &str,
        id: &str,
        me: &str,
        answer: Answer,
        note: Option<&str>,
    ) -> Result<model::Event, BackendError> {
        paced(self.gmail.answer_event(calendar, id, me, answer, note)).await.map_err(calendar_write_error)
    }

    async fn edit_list(
        &self,
        calendar: &str,
        edit: &model::list::ListEdit,
    ) -> Result<Option<model::Calendar>, BackendError> {
        paced(self.gmail.edit_calendar_list(calendar, edit)).await.map_err(calendar_write_error)
    }
}

/// Maps a calendar write's Gmail error to a neutral kind, so the copy
/// never names a Gmail error: a
/// version conflict, Google's 412 or the 409 a duplicate create answers,
/// becomes `Changed`; any other 4xx becomes `Refused` in Google's own
/// words, since nothing else can say what a client-side refusal meant.
fn calendar_write_error(err: GmailError) -> BackendError {
    match &err {
        GmailError::Changed => BackendError::Changed,
        // The client reads every 410 as an expired sync token, which only
        // the change feed can mean. A write gets 410 Gone for an event
        // already deleted.
        GmailError::ExpiredSyncToken => BackendError::NotFound,
        GmailError::Http { status: 409, .. } => BackendError::Changed,
        GmailError::Http { status, .. } if (400..500).contains(status) => {
            BackendError::Refused(err.to_string())
        }
        _ => err.into(),
    }
}

impl<G: GmailApi> ContactsService for Google<G> {
    async fn connections(
        &self,
        page_token: Option<&str>,
        sync_token: Option<&str>,
    ) -> Result<ConnectionsPage, BackendError> {
        Ok(paced(self.gmail.connections(page_token, sync_token)).await?)
    }

    async fn contact_photo(&self, url: &str) -> Result<Vec<u8>, BackendError> {
        Ok(paced(self.gmail.contact_photo(url)).await?)
    }

    async fn create_contact(&self, fields: &ContactFields) -> Result<Person, BackendError> {
        Ok(paced(self.gmail.create_contact(fields)).await?)
    }

    async fn update_contact(
        &self,
        resource: &str,
        fields: &ContactFields,
    ) -> Result<Person, BackendError> {
        Ok(paced(self.gmail.update_contact(resource, fields)).await?)
    }
}

impl<G: GmailApi> RulesService for Google<G> {
    async fn filters(&self) -> Result<Vec<Filter>, BackendError> {
        Ok(paced(self.gmail.filters()).await?)
    }

    async fn create_filter(&self, filter: &Filter) -> Result<Filter, BackendError> {
        Ok(paced(self.gmail.create_filter(filter)).await?)
    }

    async fn delete_filter(&self, id: &str) -> Result<(), BackendError> {
        Ok(paced(self.gmail.delete_filter(id)).await?)
    }
}

impl<G: GmailApi> AutoReplyService for Google<G> {
    async fn vacation(&self) -> Result<Vacation, BackendError> {
        Ok(paced(self.gmail.vacation()).await?)
    }

    async fn set_vacation(&self, vacation: &Vacation) -> Result<(), BackendError> {
        Ok(paced(self.gmail.set_vacation(vacation)).await?)
    }

    fn keeps_subject(&self) -> bool {
        true
    }

    fn limits_to_contacts(&self) -> bool {
        true
    }
}

impl<G: GmailApi> IdentityService for Google<G> {
    /// Gmail's send-as list, one `sendAs.list` call. An alias still waiting
    /// on its owner to confirm it is left out, since Gmail would refuse to
    /// send from it. Signatures come as HTML and leave as text.
    async fn identities(&self) -> Result<Vec<SendAsAddress>, BackendError> {
        Ok(paced(self.gmail.send_as())
            .await?
            .into_iter()
            .filter(SendAs::is_verified)
            .map(|identity| SendAsAddress {
                name: Some(identity.display_name).filter(|n| !n.trim().is_empty()),
                email: identity.send_as_email,
                signature: html_to_text(&identity.signature),
                default: identity.is_default,
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use mailrs_gmail::{
        AccountQuota, CALENDAR_LIST_SCOPE, CALENDAR_LIST_WRITE_SCOPE, CALENDAR_SCOPE, CALENDARS_SCOPE, CONTACTS_SCOPE,
        CONTACTS_WRITE_SCOPE, DELETE_SCOPE, DRIVE_FILE_SCOPE, GMAIL_SCOPE, GmailError, Granted, SETTINGS_SCOPE, SIGN_IN_SCOPES, SendAs,
        limiter,
    };

    use super::{Google, paced, withheld};
    use crate::BackendError;
    use crate::fake::FakeGmail;
    use crate::services::{
        AutoReplyService, IdentityService, MailBackend, MailCapabilities, SendAsAddress, Withheld,
        background,
    };

    fn google() -> (Arc<FakeGmail>, Google<FakeGmail>) {
        let gmail = Arc::new(FakeGmail::new());
        (Arc::clone(&gmail), Google::new(gmail))
    }

    #[tokio::test]
    async fn a_missing_message_is_only_missing() {
        let (_, google) = google();
        let found = google
            .fetch(vec![crate::Want::message("gone")])
            .await
            .unwrap();
        assert_eq!(found.gone, ["gone"]);
    }

    #[tokio::test]
    async fn a_missing_permission_is_its_own_kind() {
        let (gmail, google) = google();
        gmail.fail_next(GmailError::MissingScope);
        assert!(matches!(
            google.vacation().await,
            Err(BackendError::NeedsPermission)
        ));
    }

    #[tokio::test]
    async fn identities_are_the_confirmed_addresses_with_their_signatures_as_text() {
        let (gmail, google) = google();
        gmail.with(|s| {
            s.signature = Some("Me\nExample Co".into());
            s.send_as.push(SendAs {
                send_as_email: "old@example.com".into(),
                display_name: "Old".into(),
                is_default: false,
                is_primary: false,
                signature: String::new(),
                verification_status: Some("pending".into()),
            });
        });
        assert_eq!(
            google.identities().await.unwrap(),
            vec![SendAsAddress {
                email: "me@example.com".into(),
                name: Some("Me".into()),
                signature: "Me\nExample Co".into(),
                default: true,
            }]
        );
    }

    #[test]
    fn gmail_has_labels_threads_categories_and_files_its_sent_mail() {
        let (_, google) = google();
        assert_eq!(
            google.capabilities(),
            MailCapabilities {
                labels: true,
                server_threads: true,
                files_sent_mail: true,
                categories: true,
                delete_forever: true,
                keywords: &["$seen", "$flagged", "$muted"],
                native_search: true,
                batch_limit: 1000,
                renames: false,
                restates: false,
                tags: false,
                focus: false,
                local_mailboxes: false,
                files_muted_replies: true,
            }
        );
    }

    #[tokio::test(start_paused = true)]
    async fn standing_by_counts_as_the_person_waiting() {
        let gmail = FakeGmail::new().under_quota(Arc::new(AccountQuota::standalone()));
        let google = Google::new(Arc::new(gmail));
        assert!(!google.person_waiting());
        let ((), seen) = tokio::join!(google.stand_by(Duration::from_secs(2)), async {
            tokio::task::yield_now().await;
            google.person_waiting()
        });
        assert!(seen, "backfill sees the person waiting for the whole wait");
        assert!(!google.person_waiting());
    }

    #[tokio::test]
    async fn background_work_reaches_gmail_as_background() {
        let asked = || paced(async { limiter::priority() });
        assert_eq!(asked().await, mailrs_gmail::Priority::Foreground);
        assert_eq!(
            background(asked()).await,
            mailrs_gmail::Priority::Background
        );
    }

    #[test]
    fn withheld_follows_the_granted_scopes() {
        assert_eq!(withheld(None), Withheld::NONE, "an unknown grant withholds nothing");
        let everything = Granted::parse(&SIGN_IN_SCOPES.join(" "));
        assert_eq!(withheld(Some(&everything)), Withheld::NONE);
        let no_calendar = Granted::parse(&format!(
            "{GMAIL_SCOPE} {SETTINGS_SCOPE} {DELETE_SCOPE} {CALENDAR_LIST_SCOPE}"
        ));
        assert_eq!(
            withheld(Some(&no_calendar)),
            Withheld {
                contacts: true,
                change_contacts: true,
                calendar: true,
                calendars: true,
                change_calendar_list: true,
                drive: true,
                ..Withheld::NONE
            }
        );
        let delete_only = Granted::parse(DELETE_SCOPE);
        assert!(
            !withheld(Some(&delete_only)).delete,
            "mail.google.com covers gmail.modify and is delete's own scope"
        );
        assert!(withheld(Some(&delete_only)).settings, "settings is asked for on its own");
    }

    /// An account that signed in before the five-scope trim may still
    /// carry `gmail.modify` and `contacts.readonly` on their own, never
    /// re-asked. Reading mail and reading contacts stay withheld=false
    /// for such a grant; writing contacts and deleting mail, which those
    /// narrower scopes never covered, stay withheld=true.
    #[test]
    fn withheld_reads_an_old_grant_that_predates_the_five_scope_trim() {
        let old = Granted::parse(&format!(
            "{GMAIL_SCOPE} {SETTINGS_SCOPE} {CONTACTS_SCOPE} {CALENDAR_SCOPE} {CALENDAR_LIST_SCOPE}"
        ));
        assert!(old.reads_mail(), "gmail.modify alone still reads mail");
        assert_eq!(
            withheld(Some(&old)),
            Withheld {
                delete: true,
                change_contacts: true,
                calendars: true,
                change_calendar_list: true,
                drive: true,
                ..Withheld::NONE
            },
            "contacts.readonly reads contacts but neither narrower scope writes or deletes"
        );
    }

    /// A person who signs in and unticks the calendar list withholds
    /// reading it and changing it, and nothing else.
    #[test]
    fn withheld_reads_a_scope_the_person_unticked_from_the_seven() {
        let unticked_calendar_list = Granted::parse(&format!(
            "{DELETE_SCOPE} {SETTINGS_SCOPE} {CONTACTS_WRITE_SCOPE} {CALENDAR_SCOPE} {CALENDARS_SCOPE} {DRIVE_FILE_SCOPE}"
        ));
        assert_eq!(
            withheld(Some(&unticked_calendar_list)),
            Withheld { calendar_list: true, change_calendar_list: true, ..Withheld::NONE }
        );
    }

    /// An account that signed in before the owner approved the calendar
    /// management scopes holds the read-only list: it still reads every
    /// calendar, and withholds changing the list and the calendars.
    #[test]
    fn withheld_reads_the_five_scope_grant_before_calendar_management() {
        let five = Granted::parse(&format!(
            "{DELETE_SCOPE} {SETTINGS_SCOPE} {CONTACTS_WRITE_SCOPE} {CALENDAR_SCOPE} {CALENDAR_LIST_SCOPE}"
        ));
        assert_eq!(
            withheld(Some(&five)),
            Withheld { calendars: true, change_calendar_list: true, drive: true, ..Withheld::NONE }
        );
    }

    /// An account that signed in before attachments used Drive, or
    /// unticked it, withholds putting files in Drive and nothing else:
    /// reading an event's attachments needs only the calendar.
    #[test]
    fn withheld_reads_a_grant_without_drive() {
        let no_drive = Granted::parse(&format!(
            "{DELETE_SCOPE} {SETTINGS_SCOPE} {CONTACTS_WRITE_SCOPE} {CALENDAR_SCOPE} {CALENDAR_LIST_WRITE_SCOPE} \
             {CALENDARS_SCOPE}"
        ));
        assert_eq!(withheld(Some(&no_drive)), Withheld { drive: true, ..Withheld::NONE });
    }
}
