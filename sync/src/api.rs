//! Gmail's REST API as the Google adapter uses it. A trait, so tests and the
//! demo can hand the adapter a fake.

use mailrs_domain::calendar;
use mailrs_domain::invitation::Answer;
use mailrs_domain::{AccountId, EpochMillis, Filter, MessageMeta, Vacation};
use mailrs_gmail::convert::message_meta;
use mailrs_gmail::model::Message;
use mailrs_gmail::{
    AccountQuota, Answered, Busy, ConnectionsPage, ContactFields, Event, EventFields, GmailClient,
    GmailError, Granted, HistoryPage, LabelColor, MessagePage, Person, Profile, RemoteLabel,
    SendAs, Series,
};

/// Gmail operations for one account.
pub trait GmailApi: Send + Sync + 'static {
    /// The budget this account's calls come out of, where there is one.
    /// The sync loops read it to see whether the user is waiting on Gmail,
    /// and a mail action waiting out a 429 marks itself on it. A fake
    /// nobody paces answers `None`, and then nothing waits for anything.
    fn quota(&self) -> Option<&AccountQuota> {
        None
    }

    /// The scopes this account is believed to have granted, or `None`
    /// while that is not known yet. A caller that does not track consent
    /// answers `None`, which reads as "nothing withheld".
    fn granted(&self) -> Option<Granted> {
        None
    }

    fn profile(&self) -> impl Future<Output = Result<Profile, GmailError>> + Send;

    fn labels(&self) -> impl Future<Output = Result<Vec<RemoteLabel>, GmailError>> + Send;

    /// One page of a search, at most `page_size` ids long. Gmail caps a
    /// page at 500 and charges 5 units whatever its length.
    fn list_messages(
        &self,
        query: &str,
        page_token: Option<&str>,
        page_size: u32,
    ) -> impl Future<Output = Result<MessagePage, GmailError>> + Send;

    /// One page of the messages `query` matches that carry `label_id`,
    /// the label named by id. Compares label membership with the store
    /// without fetching metadata, at 5 units a page.
    fn list_labelled(
        &self,
        label_id: &str,
        query: &str,
        page_token: Option<&str>,
        page_size: u32,
    ) -> impl Future<Output = Result<MessagePage, GmailError>> + Send;

    fn message_metadata(
        &self,
        id: &str,
    ) -> impl Future<Output = Result<MessageMeta, GmailError>> + Send;

    /// Every message in the thread, oldest first.
    fn thread_metadata(
        &self,
        thread_id: &str,
    ) -> impl Future<Output = Result<Vec<MessageMeta>, GmailError>> + Send;

    /// The message's part tree, `format=full`: inline bytes for small
    /// parts and an attachment handle for the rest.
    fn message_structure(
        &self,
        id: &str,
    ) -> impl Future<Output = Result<Message, GmailError>> + Send;

    fn history(
        &self,
        start_history_id: u64,
        page_token: Option<&str>,
    ) -> impl Future<Output = Result<HistoryPage, GmailError>> + Send;

    fn modify_labels(
        &self,
        id: &str,
        add: &[String],
        remove: &[String],
    ) -> impl Future<Output = Result<(), GmailError>> + Send;

    /// One label change over many messages in a single call. Gmail charges
    /// 50 units for it whatever the count, against 5 for each
    /// `modify_labels`, so bulk work goes through here. At most
    /// [`mailrs_gmail::BATCH_LIMIT`] ids; the caller splits longer lists.
    fn batch_modify(
        &self,
        ids: &[String],
        add: &[String],
        remove: &[String],
    ) -> impl Future<Output = Result<(), GmailError>> + Send;

    /// Erases messages for good. Gmail cannot bring them back, and it
    /// answers `GmailError::MissingScope` until the account grants the
    /// delete permission.
    fn delete_messages(
        &self,
        ids: &[String],
    ) -> impl Future<Output = Result<(), GmailError>> + Send;

    /// Sends raw RFC 822 bytes. Returns the new message id.
    fn send(
        &self,
        raw: &[u8],
        thread_id: Option<&str>,
    ) -> impl Future<Output = Result<String, GmailError>> + Send;

    /// Creates a draft, or replaces draft `draft_id`.
    fn save_draft(
        &self,
        draft_id: Option<&str>,
        raw: &[u8],
        thread_id: Option<&str>,
    ) -> impl Future<Output = Result<SavedDraft, GmailError>> + Send;

    /// Sends a draft as Gmail holds it. Returns the sent message's id.
    fn send_draft(&self, draft_id: &str)
    -> impl Future<Output = Result<String, GmailError>> + Send;

    fn delete_draft(&self, draft_id: &str) -> impl Future<Output = Result<(), GmailError>> + Send;

    /// Every draft in the account, each with the message inside it.
    /// Nothing else in Gmail maps the two, and the call charges 5 units a
    /// page of the whole account, so a caller stores what comes back
    /// rather than asking again for the next draft.
    fn list_drafts(&self) -> impl Future<Output = Result<Vec<DraftRef>, GmailError>> + Send;

    /// Every address the account may send from, as Gmail lists them.
    fn send_as(&self) -> impl Future<Output = Result<Vec<SendAs>, GmailError>> + Send;

    fn attachment(
        &self,
        message_id: &str,
        attachment_id: &str,
    ) -> impl Future<Output = Result<Vec<u8>, GmailError>> + Send;

    /// The message as it arrived, in RFC 822 form.
    fn raw_message(&self, id: &str) -> impl Future<Output = Result<Vec<u8>, GmailError>> + Send;

    fn filters(&self) -> impl Future<Output = Result<Vec<Filter>, GmailError>> + Send;

    fn create_filter(
        &self,
        filter: &Filter,
    ) -> impl Future<Output = Result<Filter, GmailError>> + Send;

    fn delete_filter(&self, id: &str) -> impl Future<Output = Result<(), GmailError>> + Send;

    fn create_label(
        &self,
        name: &str,
    ) -> impl Future<Output = Result<RemoteLabel, GmailError>> + Send;

    fn rename_label(
        &self,
        id: &str,
        name: &str,
    ) -> impl Future<Output = Result<RemoteLabel, GmailError>> + Send;

    fn delete_label(&self, id: &str) -> impl Future<Output = Result<(), GmailError>> + Send;

    fn set_label_color(
        &self,
        id: &str,
        color: &LabelColor,
    ) -> impl Future<Output = Result<RemoteLabel, GmailError>> + Send;

    /// How many conversations carry the label in the whole mailbox, not
    /// only in the part this computer keeps.
    fn label_threads(&self, id: &str) -> impl Future<Output = Result<u64, GmailError>> + Send;

    fn vacation(&self) -> impl Future<Output = Result<Vacation, GmailError>> + Send;

    fn set_vacation(
        &self,
        vacation: &Vacation,
    ) -> impl Future<Output = Result<(), GmailError>> + Send;

    /// One page of the account's Google contacts. `sync_token` from the
    /// last refresh asks for changes alone. Google answers
    /// `GmailError::MissingScope` until the account grants the contacts
    /// permission, and `GmailError::ExpiredSyncToken` once a token is too
    /// old to answer from.
    fn connections(
        &self,
        page_token: Option<&str>,
        sync_token: Option<&str>,
    ) -> impl Future<Output = Result<ConnectionsPage, GmailError>> + Send;

    /// The bytes of one contact photo, at the size its URL asks for.
    fn contact_photo(&self, url: &str) -> impl Future<Output = Result<Vec<u8>, GmailError>> + Send;

    /// Adds a contact to the account's Google contacts. Google answers
    /// `GmailError::MissingScope` until the account grants the permission
    /// to change contacts.
    fn create_contact(
        &self,
        fields: &ContactFields,
    ) -> impl Future<Output = Result<Person, GmailError>> + Send;

    /// Changes the fields `fields` names on the contact `resource`.
    fn update_contact(
        &self,
        resource: &str,
        fields: &ContactFields,
    ) -> impl Future<Output = Result<Person, GmailError>> + Send;
    /// Answers the event `ical_uid` names as `me`, through Google
    /// Calendar, and lets Google tell the organizer. Answers
    /// `GmailError::MissingScope` until the account grants the calendar
    /// permission, so a caller offers to ask for it rather than showing an
    /// error.
    /// `occurrence` is the start of the one occurrence to answer, for an
    /// invitation to a single occurrence of a repeating event; `None`
    /// answers the series.
    fn answer_invitation(
        &self,
        ical_uid: &str,
        me: &str,
        answer: Answer,
        occurrence: Option<EpochMillis>,
    ) -> impl Future<Output = Result<Answered, GmailError>> + Send;

    /// What the account's calendar already holds between `from` and `to`.
    /// Answers `GmailError::MissingScope` until the account grants the
    /// calendar permission, the same as answering does.
    fn busy_between(
        &self,
        from: EpochMillis,
        to: EpochMillis,
    ) -> impl Future<Output = Result<Vec<Busy>, GmailError>> + Send;

    /// How the repeating event `ical_uid` names repeats, with the
    /// occurrences still to come from `from` counted when its rule stops
    /// after a number of them. `None` when the calendar holds no such
    /// event or it does not repeat. Answers `GmailError::MissingScope`
    /// until the account grants the calendar permission.
    fn series(
        &self,
        ical_uid: &str,
        from: EpochMillis,
    ) -> impl Future<Output = Result<Option<Series>, GmailError>> + Send;

    /// Every event on the account's primary calendar that overlaps `from`
    /// to `to`, in the order they start. Answers `GmailError::MissingScope`
    /// until the account grants the calendar permission, as the other
    /// calendar calls do.
    fn events_between(
        &self,
        from: EpochMillis,
        to: EpochMillis,
    ) -> impl Future<Output = Result<Vec<Event>, GmailError>> + Send;

    /// Puts a new event on the primary calendar and invites its guests.
    fn create_event(
        &self,
        fields: &EventFields,
    ) -> impl Future<Output = Result<Event, GmailError>> + Send;

    /// Changes the fields `fields` sets on event `id` and tells its guests.
    fn update_event(
        &self,
        id: &str,
        fields: &EventFields,
    ) -> impl Future<Output = Result<Event, GmailError>> + Send;

    /// Takes event `id` off the primary calendar and tells its guests.
    fn delete_event(&self, id: &str) -> impl Future<Output = Result<(), GmailError>> + Send;

    /// Every calendar on the account. Answers `GmailError::MissingScope`
    /// until the account grants the calendar list permission.
    fn calendars(&self) -> impl Future<Output = Result<Vec<calendar::Calendar>, GmailError>> + Send;

    /// One page of changes to `calendar` since `token`, or of the whole
    /// calendar from `from` without one.
    fn event_changes(
        &self,
        calendar: &str,
        token: Option<&str>,
        page: Option<&str>,
        from: EpochMillis,
    ) -> impl Future<Output = Result<calendar::EventPage, GmailError>> + Send;

    /// Creates `event` under its own id when `create`, or changes it to
    /// match, and tells its guests. `etag` makes the server refuse the
    /// change with `GmailError::Changed` when the event moved on since.
    fn put_event(
        &self,
        event: &calendar::Event,
        etag: Option<&str>,
        create: bool,
    ) -> impl Future<Output = Result<calendar::Event, GmailError>> + Send;

    /// Deletes an event and tells its guests.
    fn remove_event(
        &self,
        calendar: &str,
        id: &str,
        etag: Option<&str>,
    ) -> impl Future<Output = Result<(), GmailError>> + Send;
}

/// An instant as the Calendar API writes one. `None` for a time no
/// calendar could mean.
fn rfc3339(at: EpochMillis) -> Option<String> {
    chrono::DateTime::from_timestamp_millis(at).map(|at| at.to_rfc3339())
}

/// One draft as `drafts.list` reports it: Gmail's two names for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftRef {
    pub draft_id: String,
    /// The message the draft holds now. Every edit replaces it.
    pub message_id: String,
}

/// Where a saved draft lives in Gmail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedDraft {
    pub draft_id: String,
    /// The draft's current message. Each save replaces it.
    pub message_id: String,
    pub thread_id: String,
}

/// The real Gmail client, bound to a local account id.
pub struct AccountClient {
    pub account_id: AccountId,
    pub client: GmailClient,
}

impl GmailApi for AccountClient {
    fn quota(&self) -> Option<&AccountQuota> {
        Some(self.client.quota())
    }

    fn granted(&self) -> Option<Granted> {
        self.client.granted()
    }

    async fn profile(&self) -> Result<Profile, GmailError> {
        self.client.profile().await
    }

    async fn labels(&self) -> Result<Vec<RemoteLabel>, GmailError> {
        self.client.labels().await
    }

    async fn list_messages(
        &self,
        query: &str,
        page_token: Option<&str>,
        page_size: u32,
    ) -> Result<MessagePage, GmailError> {
        self.client
            .list_messages(query, page_token, page_size.clamp(1, crate::ID_PAGE_SIZE))
            .await
    }

    async fn list_labelled(
        &self,
        label_id: &str,
        query: &str,
        page_token: Option<&str>,
        page_size: u32,
    ) -> Result<MessagePage, GmailError> {
        self.client
            .list_labelled(
                label_id,
                query,
                page_token,
                page_size.clamp(1, crate::ID_PAGE_SIZE),
            )
            .await
    }

    async fn message_metadata(&self, id: &str) -> Result<MessageMeta, GmailError> {
        Ok(message_meta(
            &self.client.message_metadata(id).await?,
            self.account_id,
        ))
    }

    async fn thread_metadata(&self, thread_id: &str) -> Result<Vec<MessageMeta>, GmailError> {
        let thread = self.client.thread_metadata(thread_id).await?;
        let mut metas: Vec<MessageMeta> = thread
            .messages
            .iter()
            .map(|m| message_meta(m, self.account_id))
            .collect();
        metas.sort_by_key(|m| m.date);
        Ok(metas)
    }

    async fn message_structure(&self, id: &str) -> Result<Message, GmailError> {
        self.client.message_full(id).await
    }

    async fn history(
        &self,
        start_history_id: u64,
        page_token: Option<&str>,
    ) -> Result<HistoryPage, GmailError> {
        self.client.history(start_history_id, page_token).await
    }

    async fn modify_labels(
        &self,
        id: &str,
        add: &[String],
        remove: &[String],
    ) -> Result<(), GmailError> {
        self.client.modify(id, add, remove).await
    }

    async fn batch_modify(
        &self,
        ids: &[String],
        add: &[String],
        remove: &[String],
    ) -> Result<(), GmailError> {
        self.client.batch_modify(ids, add, remove).await
    }

    async fn delete_messages(&self, ids: &[String]) -> Result<(), GmailError> {
        self.client.batch_delete(ids).await
    }

    async fn send(&self, raw: &[u8], thread_id: Option<&str>) -> Result<String, GmailError> {
        Ok(self.client.send(raw, thread_id).await?.id)
    }

    async fn save_draft(
        &self,
        draft_id: Option<&str>,
        raw: &[u8],
        thread_id: Option<&str>,
    ) -> Result<SavedDraft, GmailError> {
        let draft = match draft_id {
            Some(id) => self.client.update_draft(id, raw, thread_id).await?,
            None => self.client.create_draft(raw, thread_id).await?,
        };
        Ok(SavedDraft {
            draft_id: draft.id,
            message_id: draft.message.id,
            thread_id: draft.message.thread_id,
        })
    }

    async fn send_draft(&self, draft_id: &str) -> Result<String, GmailError> {
        Ok(self.client.send_draft(draft_id).await?.id)
    }

    async fn delete_draft(&self, draft_id: &str) -> Result<(), GmailError> {
        self.client.delete_draft(draft_id).await
    }

    async fn list_drafts(&self) -> Result<Vec<DraftRef>, GmailError> {
        Ok(self
            .client
            .list_drafts()
            .await?
            .into_iter()
            .map(|d| DraftRef {
                draft_id: d.id,
                message_id: d.message.id,
            })
            .collect())
    }

    async fn send_as(&self) -> Result<Vec<SendAs>, GmailError> {
        self.client.send_as().await
    }

    async fn attachment(
        &self,
        message_id: &str,
        attachment_id: &str,
    ) -> Result<Vec<u8>, GmailError> {
        self.client.attachment(message_id, attachment_id).await
    }

    async fn vacation(&self) -> Result<Vacation, GmailError> {
        self.client.vacation().await
    }

    async fn set_vacation(&self, vacation: &Vacation) -> Result<(), GmailError> {
        self.client.set_vacation(vacation).await
    }

    async fn answer_invitation(
        &self,
        ical_uid: &str,
        me: &str,
        answer: Answer,
        occurrence: Option<EpochMillis>,
    ) -> Result<Answered, GmailError> {
        let occurrence = occurrence.and_then(rfc3339);
        self.client
            .answer_invitation(ical_uid, me, answer, occurrence.as_deref())
            .await
    }

    /// The Calendar API takes its window as RFC 3339, so the instants turn
    /// into timestamps here rather than in the client, which keeps a clock
    /// out of the Gmail crate.
    async fn busy_between(
        &self,
        from: EpochMillis,
        to: EpochMillis,
    ) -> Result<Vec<Busy>, GmailError> {
        let (Some(from), Some(to)) = (rfc3339(from), rfc3339(to)) else {
            return Ok(Vec::new());
        };
        self.client.busy_between(&from, &to).await
    }

    async fn series(
        &self,
        ical_uid: &str,
        from: EpochMillis,
    ) -> Result<Option<Series>, GmailError> {
        let Some(from) = rfc3339(from) else {
            return Ok(None);
        };
        self.client.series(ical_uid, &from).await
    }

    async fn events_between(
        &self,
        from: EpochMillis,
        to: EpochMillis,
    ) -> Result<Vec<Event>, GmailError> {
        let (Some(from), Some(to)) = (rfc3339(from), rfc3339(to)) else {
            return Ok(Vec::new());
        };
        self.client.events_between(&from, &to).await
    }

    async fn create_event(&self, fields: &EventFields) -> Result<Event, GmailError> {
        self.client.create_event(fields).await
    }

    async fn update_event(&self, id: &str, fields: &EventFields) -> Result<Event, GmailError> {
        self.client.update_event(id, fields).await
    }

    async fn delete_event(&self, id: &str) -> Result<(), GmailError> {
        self.client.delete_event(id).await
    }

    async fn calendars(&self) -> Result<Vec<calendar::Calendar>, GmailError> {
        self.client.calendar_list().await
    }

    /// The Calendar API takes `time_min` as RFC 3339; an instant this
    /// computer's clock could not read becomes the zero time rather than
    /// missing the call, since `from` is required here, unlike
    /// `busy_between`'s window.
    async fn event_changes(
        &self,
        calendar: &str,
        token: Option<&str>,
        page: Option<&str>,
        from: EpochMillis,
    ) -> Result<calendar::EventPage, GmailError> {
        let from = rfc3339(from).unwrap_or_default();
        self.client.event_changes(calendar, token, page, &from).await
    }

    async fn put_event(
        &self,
        event: &calendar::Event,
        etag: Option<&str>,
        create: bool,
    ) -> Result<calendar::Event, GmailError> {
        self.client.put_event(event, etag, create).await
    }

    async fn remove_event(&self, calendar: &str, id: &str, etag: Option<&str>) -> Result<(), GmailError> {
        self.client.remove_event(calendar, id, etag).await
    }

    async fn create_label(&self, name: &str) -> Result<RemoteLabel, GmailError> {
        self.client.create_label(name).await
    }

    async fn rename_label(&self, id: &str, name: &str) -> Result<RemoteLabel, GmailError> {
        self.client.rename_label(id, name).await
    }

    async fn delete_label(&self, id: &str) -> Result<(), GmailError> {
        self.client.delete_label(id).await
    }

    async fn filters(&self) -> Result<Vec<Filter>, GmailError> {
        self.client.filters().await
    }

    async fn create_filter(&self, filter: &Filter) -> Result<Filter, GmailError> {
        self.client.create_filter(filter).await
    }

    async fn delete_filter(&self, id: &str) -> Result<(), GmailError> {
        self.client.delete_filter(id).await
    }

    async fn raw_message(&self, id: &str) -> Result<Vec<u8>, GmailError> {
        self.client.raw_message(id).await
    }

    async fn set_label_color(
        &self,
        id: &str,
        color: &LabelColor,
    ) -> Result<RemoteLabel, GmailError> {
        self.client.set_label_color(id, color).await
    }

    async fn connections(
        &self,
        page_token: Option<&str>,
        sync_token: Option<&str>,
    ) -> Result<ConnectionsPage, GmailError> {
        self.client.connections(page_token, sync_token).await
    }

    async fn contact_photo(&self, url: &str) -> Result<Vec<u8>, GmailError> {
        self.client.contact_photo(url).await
    }

    async fn label_threads(&self, id: &str) -> Result<u64, GmailError> {
        self.client.label_threads(id).await
    }

    async fn create_contact(&self, fields: &ContactFields) -> Result<Person, GmailError> {
        self.client.create_contact(fields).await
    }

    async fn update_contact(
        &self,
        resource: &str,
        fields: &ContactFields,
    ) -> Result<Person, GmailError> {
        self.client.update_contact(resource, fields).await
    }
}
