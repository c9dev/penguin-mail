//! JMAP as the JMAP adapter uses it. A trait, so the adapter runs over the
//! real client or over `FakeJmap` alike, as the Microsoft adapter runs over
//! `GraphApi`. Each method is one `mailrs_jmap::JmapClient` call, so each
//! is one request on the wire.

use std::sync::Arc;
use std::time::Duration;

use mailrs_jmap::{
    AddressBook, CalendarEvents, Canceled, CardChanges, CardWrite, CardsPage, ChangesResponse,
    Email, EmailQuery, EventWrite, EventsPage, Got, HeldSubmission, Hold, Identity, Imported,
    JmapCalendar, JmapClient, JmapError, Listed, MailChanges, Mailbox, MailboxWrite, MaskedEmail,
    MaskedState, MdnOut, Pushed, Queried, Quota, Saved, Session, SetResponse, SieveScript,
    Submission, SubmissionState, Threads, VacationPatch, VacationResponse,
};
use serde_json::{Map, Value};

pub(crate) type Answer<T> = Result<T, JmapError>;

/// The push stream gives up after twice the ping it asks for.
const PUSH_IDLE: Duration = Duration::from_secs(2 * mailrs_jmap::push::PING_SECONDS as u64);

pub trait JmapApi: Send + Sync + 'static {
    fn session(&self) -> impl Future<Output = Answer<Arc<Session>>> + Send;
    fn calendars(&self) -> impl Future<Output = Answer<Vec<JmapCalendar>>> + Send;
    fn calendar_event_changes(
        &self,
        since: &str,
        max: usize,
    ) -> impl Future<Output = Answer<ChangesResponse>> + Send;
    fn calendar_events(
        &self,
        ids: &[String],
    ) -> impl Future<Output = Answer<CalendarEvents>> + Send;
    fn calendar_events_page(
        &self,
        filter: &Value,
        position: u64,
        limit: usize,
    ) -> impl Future<Output = Answer<EventsPage>> + Send;
    fn set_calendar_event(
        &self,
        write: &EventWrite,
        send: bool,
        state: Option<&str>,
    ) -> impl Future<Output = Answer<Option<Value>>> + Send;
    fn mailboxes(&self) -> impl Future<Output = Answer<Listed<Mailbox>>> + Send;
    fn mail_changes(
        &self,
        email: &str,
        mailbox: &str,
        max: usize,
    ) -> impl Future<Output = Answer<MailChanges>> + Send;
    fn email_state(&self) -> impl Future<Output = Answer<String>> + Send;
    fn query(&self, query: &EmailQuery) -> impl Future<Output = Answer<Queried>> + Send;
    fn query_after(
        &self,
        query: &EmailQuery,
        anchor: &str,
    ) -> impl Future<Output = Answer<Queried>> + Send;
    fn emails(
        &self,
        ids: &[String],
        properties: &[&str],
    ) -> impl Future<Output = Answer<Got<Email>>> + Send;
    fn threads(&self, ids: &[String]) -> impl Future<Output = Answer<Threads>> + Send;
    fn set_emails(
        &self,
        update: &Map<String, Value>,
        destroy: &[String],
        state: Option<&str>,
    ) -> impl Future<Output = Answer<SetResponse>> + Send;
    fn set_mailbox(&self, write: &MailboxWrite) -> impl Future<Output = Answer<String>> + Send;
    fn download(&self, blob_id: &str, limit: usize)
    -> impl Future<Output = Answer<Vec<u8>>> + Send;
    fn upload(&self, bytes: Vec<u8>) -> impl Future<Output = Answer<String>> + Send;
    fn import(
        &self,
        blob_id: &str,
        mailbox_ids: &[String],
        keywords: &[&str],
        destroy: Option<&str>,
    ) -> impl Future<Output = Answer<Saved>> + Send;
    fn submit(&self, submission: &Submission) -> impl Future<Output = Answer<Imported>> + Send;
    fn identities(&self) -> impl Future<Output = Answer<Vec<Identity>>> + Send;
    fn vacation(&self) -> impl Future<Output = Answer<VacationResponse>> + Send;
    fn set_vacation(&self, reply: &VacationPatch) -> impl Future<Output = Answer<()>> + Send;
    fn address_books(&self) -> impl Future<Output = Answer<Vec<AddressBook>>> + Send;
    fn cards_page(
        &self,
        position: u64,
        limit: usize,
    ) -> impl Future<Output = Answer<CardsPage>> + Send;
    fn card_changes(
        &self,
        since: &str,
        max: usize,
    ) -> impl Future<Output = Answer<CardChanges>> + Send;
    fn card(&self, id: &str) -> impl Future<Output = Answer<Option<Value>>> + Send;
    fn set_card(&self, write: &CardWrite) -> impl Future<Output = Answer<Value>> + Send;
    fn sieve_scripts(&self) -> impl Future<Output = Answer<Vec<SieveScript>>> + Send;
    fn sieve_text(&self, blob_id: &str) -> impl Future<Output = Answer<String>> + Send;
    fn put_sieve(
        &self,
        name: &str,
        existing: Option<&str>,
        text: &str,
    ) -> impl Future<Output = Answer<String>> + Send;
    fn activate_sieve(&self, id: &str) -> impl Future<Output = Answer<()>> + Send;
    fn sieve_extensions(&self) -> impl Future<Output = Answer<Vec<String>>> + Send;
    fn masked_emails(&self) -> impl Future<Output = Answer<Vec<MaskedEmail>>> + Send;
    fn create_masked(
        &self,
        for_domain: &str,
        description: &str,
    ) -> impl Future<Output = Answer<MaskedEmail>> + Send;
    fn set_masked_state(
        &self,
        id: &str,
        state: MaskedState,
    ) -> impl Future<Output = Answer<()>> + Send;
    fn hold(&self, hold: &Hold) -> impl Future<Output = Answer<HeldSubmission>> + Send;
    fn submissions(
        &self,
        ids: &[String],
    ) -> impl Future<Output = Answer<Vec<SubmissionState>>> + Send;
    fn answers_submission_get(&self) -> impl Future<Output = Answer<bool>> + Send;
    fn submissions_for(&self, email_id: &str) -> impl Future<Output = Answer<Vec<String>>> + Send;
    fn cancel_submission(&self, id: &str) -> impl Future<Output = Answer<Canceled>> + Send;
    fn file_held(
        &self,
        email_id: &str,
        drafts: &str,
        sent: &str,
    ) -> impl Future<Output = Answer<()>> + Send;
    fn unhold(&self, email_id: &str) -> impl Future<Output = Answer<()>> + Send;
    fn quotas(&self) -> impl Future<Output = Answer<Vec<Quota>>> + Send;
    fn send_mdn(&self, mdn: &MdnOut) -> impl Future<Output = Answer<()>> + Send;
    fn push(&self) -> impl Future<Output = Answer<Pushed>> + Send;
    /// When push comes back, while a stalled call has paused it.
    fn push_paused(&self) -> Option<tokio::time::Instant>;
}

impl JmapApi for JmapClient {
    async fn calendars(&self) -> Answer<Vec<JmapCalendar>> {
        JmapClient::calendars(self).await
    }
    async fn calendar_event_changes(&self, since: &str, max: usize) -> Answer<ChangesResponse> {
        JmapClient::calendar_event_changes(self, since, max).await
    }
    async fn calendar_events(&self, ids: &[String]) -> Answer<CalendarEvents> {
        JmapClient::calendar_events(self, ids).await
    }
    async fn calendar_events_page(
        &self,
        filter: &Value,
        position: u64,
        limit: usize,
    ) -> Answer<EventsPage> {
        JmapClient::calendar_events_page(self, filter, position, limit).await
    }
    async fn set_calendar_event(
        &self,
        write: &EventWrite,
        send: bool,
        state: Option<&str>,
    ) -> Answer<Option<Value>> {
        JmapClient::set_calendar_event(self, write, send, state).await
    }
    async fn session(&self) -> Answer<Arc<Session>> {
        JmapClient::session(self).await
    }
    async fn mailboxes(&self) -> Answer<Listed<Mailbox>> {
        JmapClient::mailboxes(self).await
    }
    async fn mail_changes(&self, email: &str, mailbox: &str, max: usize) -> Answer<MailChanges> {
        JmapClient::mail_changes(self, email, mailbox, max).await
    }
    async fn email_state(&self) -> Answer<String> {
        JmapClient::email_state(self).await
    }
    async fn query(&self, query: &EmailQuery) -> Answer<Queried> {
        JmapClient::query(self, query).await
    }
    async fn query_after(&self, query: &EmailQuery, anchor: &str) -> Answer<Queried> {
        JmapClient::query_after(self, query, anchor).await
    }
    async fn emails(&self, ids: &[String], properties: &[&str]) -> Answer<Got<Email>> {
        JmapClient::emails(self, ids, properties).await
    }
    async fn threads(&self, ids: &[String]) -> Answer<Threads> {
        JmapClient::threads(self, ids).await
    }
    async fn set_emails(
        &self,
        update: &Map<String, Value>,
        destroy: &[String],
        state: Option<&str>,
    ) -> Answer<SetResponse> {
        JmapClient::set_emails(self, update, destroy, state).await
    }
    async fn set_mailbox(&self, write: &MailboxWrite) -> Answer<String> {
        JmapClient::set_mailbox(self, write).await
    }
    async fn download(&self, blob_id: &str, limit: usize) -> Answer<Vec<u8>> {
        let account = self.mail_account().await?;
        JmapClient::download(
            self,
            &account,
            blob_id,
            "message.eml",
            "application/octet-stream",
            limit,
        )
        .await
    }
    async fn upload(&self, bytes: Vec<u8>) -> Answer<String> {
        let account = self.mail_account().await?;
        Ok(JmapClient::upload(self, &account, "message/rfc822", bytes)
            .await?
            .blob_id)
    }
    async fn import(
        &self,
        blob_id: &str,
        mailbox_ids: &[String],
        keywords: &[&str],
        destroy: Option<&str>,
    ) -> Answer<Saved> {
        JmapClient::import(self, blob_id, mailbox_ids, keywords, destroy).await
    }
    async fn submit(&self, submission: &Submission) -> Answer<Imported> {
        JmapClient::submit(self, submission).await
    }
    async fn identities(&self) -> Answer<Vec<Identity>> {
        mailrs_jmap::identity::identities(self).await
    }
    async fn vacation(&self) -> Answer<VacationResponse> {
        JmapClient::vacation(self).await
    }
    async fn set_vacation(&self, reply: &VacationPatch) -> Answer<()> {
        JmapClient::set_vacation(self, reply).await
    }
    async fn address_books(&self) -> Answer<Vec<AddressBook>> {
        JmapClient::address_books(self).await
    }
    async fn cards_page(&self, position: u64, limit: usize) -> Answer<CardsPage> {
        JmapClient::cards_page(self, position, limit).await
    }
    async fn card_changes(&self, since: &str, max: usize) -> Answer<CardChanges> {
        JmapClient::card_changes(self, since, max).await
    }
    async fn card(&self, id: &str) -> Answer<Option<Value>> {
        JmapClient::card(self, id).await
    }
    async fn set_card(&self, write: &CardWrite) -> Answer<Value> {
        JmapClient::set_card(self, write).await
    }
    async fn sieve_scripts(&self) -> Answer<Vec<SieveScript>> {
        JmapClient::sieve_scripts(self).await
    }
    async fn sieve_text(&self, blob_id: &str) -> Answer<String> {
        JmapClient::sieve_text(self, blob_id).await
    }
    async fn put_sieve(&self, name: &str, existing: Option<&str>, text: &str) -> Answer<String> {
        JmapClient::put_sieve(self, name, existing, text).await
    }
    async fn activate_sieve(&self, id: &str) -> Answer<()> {
        JmapClient::activate_sieve(self, id).await
    }
    async fn sieve_extensions(&self) -> Answer<Vec<String>> {
        JmapClient::sieve_extensions(self).await
    }
    async fn masked_emails(&self) -> Answer<Vec<MaskedEmail>> {
        JmapClient::masked_emails(self).await
    }
    async fn create_masked(&self, for_domain: &str, description: &str) -> Answer<MaskedEmail> {
        JmapClient::create_masked(self, for_domain, description).await
    }
    async fn set_masked_state(&self, id: &str, state: MaskedState) -> Answer<()> {
        JmapClient::set_masked_state(self, id, state).await
    }
    async fn hold(&self, hold: &Hold) -> Answer<HeldSubmission> {
        JmapClient::hold(self, hold).await
    }
    async fn submissions(&self, ids: &[String]) -> Answer<Vec<SubmissionState>> {
        JmapClient::submissions(self, ids).await
    }
    async fn answers_submission_get(&self) -> Answer<bool> {
        JmapClient::answers_submission_get(self).await
    }
    async fn submissions_for(&self, email_id: &str) -> Answer<Vec<String>> {
        JmapClient::submissions_for(self, email_id).await
    }
    async fn cancel_submission(&self, id: &str) -> Answer<Canceled> {
        JmapClient::cancel_submission(self, id).await
    }
    async fn file_held(&self, email_id: &str, drafts: &str, sent: &str) -> Answer<()> {
        JmapClient::file_held(self, email_id, drafts, sent).await
    }
    async fn unhold(&self, email_id: &str) -> Answer<()> {
        JmapClient::unhold(self, email_id).await
    }
    async fn quotas(&self) -> Answer<Vec<Quota>> {
        JmapClient::quotas(self).await
    }
    async fn send_mdn(&self, mdn: &MdnOut) -> Answer<()> {
        JmapClient::send_mdn(self, mdn).await
    }
    async fn push(&self) -> Answer<Pushed> {
        JmapClient::push(self, PUSH_IDLE).await
    }
    fn push_paused(&self) -> Option<tokio::time::Instant> {
        JmapClient::push_paused(self)
    }
}
