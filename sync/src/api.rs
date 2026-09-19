//! What the sync engine needs from Gmail. A trait, so tests can use a fake.

use mailrs_domain::{AccountId, MessageBody, MessageMeta};
use mailrs_gmail::body::extract_body;
use mailrs_gmail::convert::message_meta;
use mailrs_gmail::{GmailClient, GmailError, HistoryPage, MessagePage, Profile, RemoteLabel};

/// Page size for window listings.
pub const LIST_PAGE_SIZE: u32 = 100;

/// Gmail operations for one account.
pub trait GmailApi: Send + Sync + 'static {
    fn profile(&self) -> impl Future<Output = Result<Profile, GmailError>> + Send;

    fn labels(&self) -> impl Future<Output = Result<Vec<RemoteLabel>, GmailError>> + Send;

    fn list_messages(
        &self,
        query: &str,
        page_token: Option<&str>,
    ) -> impl Future<Output = Result<MessagePage, GmailError>> + Send;

    fn message_metadata(&self, id: &str) -> impl Future<Output = Result<MessageMeta, GmailError>> + Send;

    /// Every message in the thread, oldest first.
    fn thread_metadata(&self, thread_id: &str) -> impl Future<Output = Result<Vec<MessageMeta>, GmailError>> + Send;

    fn message_body(&self, id: &str) -> impl Future<Output = Result<MessageBody, GmailError>> + Send;

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

    fn trash(&self, id: &str) -> impl Future<Output = Result<(), GmailError>> + Send;
}

/// The real Gmail client, bound to a local account id.
pub struct AccountClient {
    pub account_id: AccountId,
    pub client: GmailClient,
}

impl GmailApi for AccountClient {
    async fn profile(&self) -> Result<Profile, GmailError> {
        self.client.profile().await
    }

    async fn labels(&self) -> Result<Vec<RemoteLabel>, GmailError> {
        self.client.labels().await
    }

    async fn list_messages(&self, query: &str, page_token: Option<&str>) -> Result<MessagePage, GmailError> {
        self.client.list_messages(query, page_token, LIST_PAGE_SIZE).await
    }

    async fn message_metadata(&self, id: &str) -> Result<MessageMeta, GmailError> {
        Ok(message_meta(&self.client.message_metadata(id).await?, self.account_id))
    }

    async fn thread_metadata(&self, thread_id: &str) -> Result<Vec<MessageMeta>, GmailError> {
        let thread = self.client.thread_metadata(thread_id).await?;
        let mut metas: Vec<MessageMeta> = thread.messages.iter().map(|m| message_meta(m, self.account_id)).collect();
        metas.sort_by_key(|m| m.date);
        Ok(metas)
    }

    async fn message_body(&self, id: &str) -> Result<MessageBody, GmailError> {
        let message = self.client.message_full(id).await?;
        Ok(message.payload.as_ref().map(extract_body).unwrap_or_default())
    }

    async fn history(&self, start_history_id: u64, page_token: Option<&str>) -> Result<HistoryPage, GmailError> {
        self.client.history(start_history_id, page_token).await
    }

    async fn modify_labels(&self, id: &str, add: &[String], remove: &[String]) -> Result<(), GmailError> {
        self.client.modify(id, add, remove).await
    }

    async fn trash(&self, id: &str) -> Result<(), GmailError> {
        self.client.trash(id).await
    }
}
