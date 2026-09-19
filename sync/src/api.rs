//! What the sync engine needs from Gmail. A trait, so tests can use a fake.

use mailrs_domain::{AccountId, MessageBody, MessageMeta, Vacation};
use mailrs_gmail::body::extract_body;
use mailrs_gmail::convert::message_meta;
use mailrs_gmail::{
    GmailClient, GmailError, HistoryPage, MessagePage, Profile, RemoteLabel, html_to_text,
};

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

    fn message_metadata(
        &self,
        id: &str,
    ) -> impl Future<Output = Result<MessageMeta, GmailError>> + Send;

    /// Every message in the thread, oldest first.
    fn thread_metadata(
        &self,
        thread_id: &str,
    ) -> impl Future<Output = Result<Vec<MessageMeta>, GmailError>> + Send;

    fn message_body(
        &self,
        id: &str,
    ) -> impl Future<Output = Result<MessageBody, GmailError>> + Send;

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

    fn untrash(&self, id: &str) -> impl Future<Output = Result<(), GmailError>> + Send;

    /// Sends raw RFC 822 bytes. Returns the new message id.
    fn send(
        &self,
        raw: &[u8],
        thread_id: Option<&str>,
    ) -> impl Future<Output = Result<String, GmailError>> + Send;

    /// Creates a draft, or replaces draft `draft_id`. Returns the draft id.
    fn save_draft(
        &self,
        draft_id: Option<&str>,
        raw: &[u8],
        thread_id: Option<&str>,
    ) -> impl Future<Output = Result<String, GmailError>> + Send;

    fn delete_draft(&self, draft_id: &str) -> impl Future<Output = Result<(), GmailError>> + Send;

    /// The draft whose current message is `message_id`, if any.
    fn draft_for_message(
        &self,
        message_id: &str,
    ) -> impl Future<Output = Result<Option<String>, GmailError>> + Send;

    /// The display name of the account's default send-as identity.
    fn display_name(&self) -> impl Future<Output = Result<Option<String>, GmailError>> + Send;

    fn attachment(
        &self,
        message_id: &str,
        attachment_id: &str,
    ) -> impl Future<Output = Result<Vec<u8>, GmailError>> + Send;

    /// The signature of the default send-as identity, as plain text.
    fn signature(&self) -> impl Future<Output = Result<Option<String>, GmailError>> + Send;

    fn vacation(&self) -> impl Future<Output = Result<Vacation, GmailError>> + Send;

    fn set_vacation(
        &self,
        vacation: &Vacation,
    ) -> impl Future<Output = Result<(), GmailError>> + Send;
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

    async fn list_messages(
        &self,
        query: &str,
        page_token: Option<&str>,
    ) -> Result<MessagePage, GmailError> {
        self.client
            .list_messages(query, page_token, LIST_PAGE_SIZE)
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

    async fn message_body(&self, id: &str) -> Result<MessageBody, GmailError> {
        let message = self.client.message_full(id).await?;
        Ok(message
            .payload
            .as_ref()
            .map(extract_body)
            .unwrap_or_default())
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

    async fn trash(&self, id: &str) -> Result<(), GmailError> {
        self.client.trash(id).await
    }

    async fn untrash(&self, id: &str) -> Result<(), GmailError> {
        self.client.untrash(id).await
    }

    async fn send(&self, raw: &[u8], thread_id: Option<&str>) -> Result<String, GmailError> {
        Ok(self.client.send(raw, thread_id).await?.id)
    }

    async fn save_draft(
        &self,
        draft_id: Option<&str>,
        raw: &[u8],
        thread_id: Option<&str>,
    ) -> Result<String, GmailError> {
        let draft = match draft_id {
            Some(id) => self.client.update_draft(id, raw, thread_id).await?,
            None => self.client.create_draft(raw, thread_id).await?,
        };
        Ok(draft.id)
    }

    async fn delete_draft(&self, draft_id: &str) -> Result<(), GmailError> {
        self.client.delete_draft(draft_id).await
    }

    async fn draft_for_message(&self, message_id: &str) -> Result<Option<String>, GmailError> {
        let drafts = self.client.list_drafts().await?;
        Ok(drafts
            .into_iter()
            .find(|d| d.message.id == message_id)
            .map(|d| d.id))
    }

    async fn display_name(&self) -> Result<Option<String>, GmailError> {
        let identities = self.client.send_as().await?;
        Ok(identities
            .into_iter()
            .find(|s| s.is_default)
            .map(|s| s.display_name)
            .filter(|name| !name.trim().is_empty()))
    }

    async fn attachment(
        &self,
        message_id: &str,
        attachment_id: &str,
    ) -> Result<Vec<u8>, GmailError> {
        self.client.attachment(message_id, attachment_id).await
    }

    async fn signature(&self) -> Result<Option<String>, GmailError> {
        let identities = self.client.send_as().await?;
        Ok(identities
            .into_iter()
            .find(|s| s.is_default)
            .map(|s| html_to_text(&s.signature))
            .filter(|text| !text.is_empty()))
    }

    async fn vacation(&self) -> Result<Vacation, GmailError> {
        self.client.vacation().await
    }

    async fn set_vacation(&self, vacation: &Vacation) -> Result<(), GmailError> {
        self.client.set_vacation(vacation).await
    }
}
