//! Microsoft Graph as the Microsoft adapter uses it. A trait, so the
//! adapter runs over the real client or over `FakeGraph` alike, as the
//! IMAP adapter runs over `ImapApi`. Each method is one Graph call, named
//! and shaped as `mailrs_graph::Graph` names it.

use mailrs_graph::{
    AutomaticReplies, ContactFolder, DeltaPage, Granted, Graph, GraphCalendar, GraphContact,
    GraphError, GraphEvent, Listing, MailFolder, MasterCategory, Me, Message, MessageBody,
    MessageRule, Override, Page, Response, Write,
};
use serde_json::Value;

type Answer<T> = Result<T, GraphError>;

pub trait GraphApi: Send + Sync + 'static {
    /// The scopes the account's token carries, or `None` before any token
    /// said so.
    fn granted(&self) -> Option<Granted>;

    fn me(&self) -> impl Future<Output = Answer<Me>> + Send;
    fn well_known(&self, names: &[&str]) -> impl Future<Output = Answer<Vec<Option<MailFolder>>>> + Send;
    fn folders(&self, parent: Option<&str>, next: Option<&str>) -> impl Future<Output = Answer<Page<MailFolder>>> + Send;
    fn categories(&self) -> impl Future<Output = Answer<Vec<MasterCategory>>> + Send;
    fn message_delta(&self, folder: &str, link: Option<&str>, received_since: &str) -> impl Future<Output = Answer<DeltaPage<Message>>> + Send;
    fn list_messages(&self, listing: &Listing, next: Option<&str>) -> impl Future<Output = Answer<Page<Message>>> + Send;
    fn messages(&self, ids: &[String]) -> impl Future<Output = Answer<Vec<Answer<Message>>>> + Send;
    fn raw(&self, id: &str, limit: usize) -> impl Future<Output = Answer<Vec<u8>>> + Send;
    fn body(&self, id: &str) -> impl Future<Output = Answer<MessageBody>> + Send;
    fn attachment(&self, message: &str, attachment: &str, limit: usize) -> impl Future<Output = Answer<Vec<u8>>> + Send;
    fn apply(&self, writes: &[Write]) -> impl Future<Output = Answer<Vec<Answer<()>>>> + Send;
    fn create_folder(&self, parent: Option<&str>, name: &str) -> impl Future<Output = Answer<MailFolder>> + Send;
    fn rename_folder(&self, id: &str, name: &str) -> impl Future<Output = Answer<MailFolder>> + Send;
    fn delete_folder(&self, id: &str) -> impl Future<Output = Answer<()>> + Send;
    fn set_category_color(&self, id: &str, color: &str) -> impl Future<Output = Answer<MasterCategory>> + Send;
    fn send_mime(&self, raw: &[u8]) -> impl Future<Output = Answer<()>> + Send;
    fn create_draft_mime(&self, raw: &[u8]) -> impl Future<Output = Answer<Message>> + Send;
    fn create_draft(&self, draft: &Value) -> impl Future<Output = Answer<Message>> + Send;
    fn upload_session(&self, message: &str, name: &str, size: u64, is_inline: bool, content_id: Option<&str>) -> impl Future<Output = Answer<String>> + Send;
    fn upload_chunk(&self, url: &str, offset: u64, total: u64, bytes: &[u8]) -> impl Future<Output = Answer<bool>> + Send;
    fn send_draft(&self, id: &str) -> impl Future<Output = Answer<()>> + Send;
    fn delete_message(&self, id: &str) -> impl Future<Output = Answer<()>> + Send;

    fn calendars(&self) -> impl Future<Output = Answer<Vec<GraphCalendar>>> + Send;
    fn calendar_view_delta(&self, calendar: &str, link: Option<&str>, start: &str, end: &str) -> impl Future<Output = Answer<DeltaPage<GraphEvent>>> + Send;
    fn event(&self, id: &str) -> impl Future<Output = Answer<GraphEvent>> + Send;
    fn instances(&self, series: &str, start: &str, end: &str) -> impl Future<Output = Answer<Vec<GraphEvent>>> + Send;
    fn original_starts(&self, ids: &[String]) -> impl Future<Output = Answer<Vec<Answer<GraphEvent>>>> + Send;
    fn events(&self, ids: &[String]) -> impl Future<Output = Answer<Vec<Answer<GraphEvent>>>> + Send;
    fn events_by_uid(&self, calendar: &str, uid: &str) -> impl Future<Output = Answer<Vec<GraphEvent>>> + Send;
    fn create_event(&self, calendar: &str, body: &Value) -> impl Future<Output = Answer<GraphEvent>> + Send;
    fn update_event(&self, id: &str, body: &Value, etag: Option<&str>) -> impl Future<Output = Answer<GraphEvent>> + Send;
    fn delete_event(&self, id: &str, etag: Option<&str>) -> impl Future<Output = Answer<()>> + Send;
    fn respond(&self, id: &str, response: Response, comment: Option<&str>) -> impl Future<Output = Answer<()>> + Send;
    fn calendar_view_of(&self, calendar: &str, start: &str, end: &str, link: Option<&str>) -> impl Future<Output = Answer<Page<GraphEvent>>> + Send;
    fn create_calendar(&self, name: &str, hex: &str) -> impl Future<Output = Answer<GraphCalendar>> + Send;
    fn update_calendar(&self, id: &str, body: &Value) -> impl Future<Output = Answer<GraphCalendar>> + Send;
    fn delete_calendar(&self, id: &str) -> impl Future<Output = Answer<()>> + Send;

    fn contact_folders(&self) -> impl Future<Output = Answer<Vec<ContactFolder>>> + Send;
    fn default_contact_folder(&self) -> impl Future<Output = Answer<Option<String>>> + Send;
    fn contact_delta(&self, folder: &str, link: Option<&str>) -> impl Future<Output = Answer<DeltaPage<GraphContact>>> + Send;
    fn contact_photo(&self, id: &str, limit: usize) -> impl Future<Output = Answer<Option<Vec<u8>>>> + Send;
    fn create_contact(&self, body: &Value) -> impl Future<Output = Answer<GraphContact>> + Send;
    fn contact_name(&self, id: &str) -> impl Future<Output = Answer<GraphContact>> + Send;
    fn update_contact(&self, id: &str, body: &Value) -> impl Future<Output = Answer<GraphContact>> + Send;

    fn rules(&self) -> impl Future<Output = Answer<Vec<MessageRule>>> + Send;
    fn create_rule(&self, rule: &MessageRule) -> impl Future<Output = Answer<MessageRule>> + Send;
    fn delete_rule(&self, id: &str) -> impl Future<Output = Answer<()>> + Send;
    fn automatic_replies(&self) -> impl Future<Output = Answer<AutomaticReplies>> + Send;
    /// Writes the reply and answers what the mailbox kept, which may differ.
    fn set_automatic_replies(&self, replies: &AutomaticReplies) -> impl Future<Output = Answer<AutomaticReplies>> + Send;
    fn overrides(&self) -> impl Future<Output = Answer<Vec<Override>>> + Send;
    fn set_override(&self, address: &str, other: bool) -> impl Future<Output = Answer<Override>> + Send;
    fn delete_override(&self, id: &str) -> impl Future<Output = Answer<()>> + Send;
}

impl GraphApi for Graph {
    fn granted(&self) -> Option<Granted> {
        Graph::granted(self)
    }

    async fn me(&self) -> Answer<Me> {
        Graph::me(self).await
    }

    async fn well_known(&self, names: &[&str]) -> Answer<Vec<Option<MailFolder>>> {
        Graph::well_known(self, names).await
    }

    async fn folders(&self, parent: Option<&str>, next: Option<&str>) -> Answer<Page<MailFolder>> {
        Graph::folders(self, parent, next).await
    }

    async fn categories(&self) -> Answer<Vec<MasterCategory>> {
        Graph::categories(self).await
    }

    async fn message_delta(&self, folder: &str, link: Option<&str>, received_since: &str) -> Answer<DeltaPage<Message>> {
        Graph::message_delta(self, folder, link, received_since).await
    }

    async fn list_messages(&self, listing: &Listing, next: Option<&str>) -> Answer<Page<Message>> {
        Graph::list_messages(self, listing, next).await
    }

    async fn messages(&self, ids: &[String]) -> Answer<Vec<Answer<Message>>> {
        Graph::messages(self, ids).await
    }

    async fn raw(&self, id: &str, limit: usize) -> Answer<Vec<u8>> {
        Graph::raw(self, id, limit).await
    }

    async fn body(&self, id: &str) -> Answer<MessageBody> {
        Graph::body(self, id).await
    }

    async fn attachment(&self, message: &str, attachment: &str, limit: usize) -> Answer<Vec<u8>> {
        Graph::attachment(self, message, attachment, limit).await
    }

    async fn apply(&self, writes: &[Write]) -> Answer<Vec<Answer<()>>> {
        Graph::apply(self, writes).await
    }

    async fn create_folder(&self, parent: Option<&str>, name: &str) -> Answer<MailFolder> {
        Graph::create_folder(self, parent, name).await
    }

    async fn rename_folder(&self, id: &str, name: &str) -> Answer<MailFolder> {
        Graph::rename_folder(self, id, name).await
    }

    async fn delete_folder(&self, id: &str) -> Answer<()> {
        Graph::delete_folder(self, id).await
    }

    async fn set_category_color(&self, id: &str, color: &str) -> Answer<MasterCategory> {
        Graph::set_category_color(self, id, color).await
    }

    async fn send_mime(&self, raw: &[u8]) -> Answer<()> {
        Graph::send_mime(self, raw).await
    }

    async fn create_draft_mime(&self, raw: &[u8]) -> Answer<Message> {
        Graph::create_draft_mime(self, raw).await
    }

    async fn create_draft(&self, draft: &Value) -> Answer<Message> {
        Graph::create_draft(self, draft).await
    }

    async fn upload_session(&self, message: &str, name: &str, size: u64, is_inline: bool, content_id: Option<&str>) -> Answer<String> {
        Graph::upload_session(self, message, name, size, is_inline, content_id).await
    }

    async fn upload_chunk(&self, url: &str, offset: u64, total: u64, bytes: &[u8]) -> Answer<bool> {
        Graph::upload_chunk(self, url, offset, total, bytes).await
    }

    async fn send_draft(&self, id: &str) -> Answer<()> {
        Graph::send_draft(self, id).await
    }

    async fn delete_message(&self, id: &str) -> Answer<()> {
        Graph::delete_message(self, id).await
    }


    async fn calendars(&self) -> Answer<Vec<GraphCalendar>> {
        Graph::calendars(self).await
    }

    async fn calendar_view_delta(&self, calendar: &str, link: Option<&str>, start: &str, end: &str) -> Answer<DeltaPage<GraphEvent>> {
        Graph::calendar_view_delta(self, calendar, link, start, end).await
    }

    async fn event(&self, id: &str) -> Answer<GraphEvent> {
        Graph::event(self, id).await
    }

    async fn instances(&self, series: &str, start: &str, end: &str) -> Answer<Vec<GraphEvent>> {
        Graph::instances(self, series, start, end).await
    }

    async fn original_starts(&self, ids: &[String]) -> Answer<Vec<Answer<GraphEvent>>> {
        Graph::original_starts(self, ids).await
    }

    async fn events(&self, ids: &[String]) -> Answer<Vec<Answer<GraphEvent>>> {
        Graph::events(self, ids).await
    }

    async fn events_by_uid(&self, calendar: &str, uid: &str) -> Answer<Vec<GraphEvent>> {
        Graph::events_by_uid(self, calendar, uid).await
    }

    async fn create_event(&self, calendar: &str, body: &Value) -> Answer<GraphEvent> {
        Graph::create_event(self, calendar, body).await
    }

    async fn update_event(&self, id: &str, body: &Value, etag: Option<&str>) -> Answer<GraphEvent> {
        Graph::update_event(self, id, body, etag).await
    }

    async fn delete_event(&self, id: &str, etag: Option<&str>) -> Answer<()> {
        Graph::delete_event(self, id, etag).await
    }

    async fn respond(&self, id: &str, response: Response, comment: Option<&str>) -> Answer<()> {
        Graph::respond(self, id, response, comment).await
    }

    async fn calendar_view_of(&self, calendar: &str, start: &str, end: &str, link: Option<&str>) -> Answer<Page<GraphEvent>> {
        Graph::calendar_view_of(self, calendar, start, end, link).await
    }

    async fn create_calendar(&self, name: &str, hex: &str) -> Answer<GraphCalendar> {
        Graph::create_calendar(self, name, hex).await
    }

    async fn update_calendar(&self, id: &str, body: &Value) -> Answer<GraphCalendar> {
        Graph::update_calendar(self, id, body).await
    }

    async fn delete_calendar(&self, id: &str) -> Answer<()> {
        Graph::delete_calendar(self, id).await
    }


    async fn contact_folders(&self) -> Answer<Vec<ContactFolder>> {
        Graph::contact_folders(self).await
    }

    async fn default_contact_folder(&self) -> Answer<Option<String>> {
        Graph::default_contact_folder(self).await
    }

    async fn contact_delta(&self, folder: &str, link: Option<&str>) -> Answer<DeltaPage<GraphContact>> {
        Graph::contact_delta(self, folder, link).await
    }

    async fn contact_photo(&self, id: &str, limit: usize) -> Answer<Option<Vec<u8>>> {
        Graph::contact_photo(self, id, limit).await
    }

    async fn create_contact(&self, body: &Value) -> Answer<GraphContact> {
        Graph::create_contact(self, body).await
    }

    async fn contact_name(&self, id: &str) -> Answer<GraphContact> {
        Graph::contact_name(self, id).await
    }

    async fn update_contact(&self, id: &str, body: &Value) -> Answer<GraphContact> {
        Graph::update_contact(self, id, body).await
    }


    async fn rules(&self) -> Answer<Vec<MessageRule>> {
        Graph::rules(self).await
    }

    async fn create_rule(&self, rule: &MessageRule) -> Answer<MessageRule> {
        Graph::create_rule(self, rule).await
    }

    async fn delete_rule(&self, id: &str) -> Answer<()> {
        Graph::delete_rule(self, id).await
    }

    async fn automatic_replies(&self) -> Answer<AutomaticReplies> {
        Graph::automatic_replies(self).await
    }

    async fn set_automatic_replies(&self, replies: &AutomaticReplies) -> Answer<AutomaticReplies> {
        Graph::set_automatic_replies(self, replies).await
    }

    async fn overrides(&self) -> Answer<Vec<Override>> {
        Graph::overrides(self).await
    }

    async fn set_override(&self, address: &str, other: bool) -> Answer<Override> {
        Graph::set_override(self, address, other).await
    }

    async fn delete_override(&self, id: &str) -> Answer<()> {
        Graph::delete_override(self, id).await
    }
}
