//! What the sync engine needs from Gmail. A trait, so tests can use a fake.

use mailrs_domain::{AccountId, Filter, MessageBody, MessageMeta, Vacation};
use mailrs_gmail::body::extract_body;
use mailrs_gmail::convert::message_meta;
use mailrs_gmail::{
    GmailClient, GmailError, HistoryPage, LabelColor, MessagePage, Profile, RemoteLabel,
    html_to_text,
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

    /// The signature of the default send-as identity, as plain text.
    fn signature(&self) -> impl Future<Output = Result<Option<String>, GmailError>> + Send;

    fn vacation(&self) -> impl Future<Output = Result<Vacation, GmailError>> + Send;

    fn set_vacation(
        &self,
        vacation: &Vacation,
    ) -> impl Future<Output = Result<(), GmailError>> + Send;
}

/// Gmail for one account: the real client, or the in-memory fake behind
/// `--demo`. The trait's methods return `impl Future`, so no `dyn` object
/// can hold both and a caller that picks at run time forwards by hand.
#[cfg(any(test, feature = "fake"))]
pub enum AnyGmail {
    Real(Box<AccountClient>),
    Fake(std::sync::Arc<crate::fake::FakeGmail>),
}

#[cfg(any(test, feature = "fake"))]
macro_rules! forward {
    ($self:ident, $method:ident($($arg:expr),*)) => {
        match $self {
            AnyGmail::Real(api) => api.$method($($arg),*).await,
            AnyGmail::Fake(api) => api.$method($($arg),*).await,
        }
    };
}

#[cfg(any(test, feature = "fake"))]
impl GmailApi for AnyGmail {
    async fn profile(&self) -> Result<Profile, GmailError> {
        forward!(self, profile())
    }
    async fn labels(&self) -> Result<Vec<RemoteLabel>, GmailError> {
        forward!(self, labels())
    }
    async fn list_messages(
        &self,
        query: &str,
        page_token: Option<&str>,
    ) -> Result<MessagePage, GmailError> {
        forward!(self, list_messages(query, page_token))
    }
    async fn message_metadata(&self, id: &str) -> Result<MessageMeta, GmailError> {
        forward!(self, message_metadata(id))
    }
    async fn thread_metadata(&self, thread_id: &str) -> Result<Vec<MessageMeta>, GmailError> {
        forward!(self, thread_metadata(thread_id))
    }
    async fn message_body(&self, id: &str) -> Result<MessageBody, GmailError> {
        forward!(self, message_body(id))
    }
    async fn history(
        &self,
        start: u64,
        page_token: Option<&str>,
    ) -> Result<HistoryPage, GmailError> {
        forward!(self, history(start, page_token))
    }
    async fn modify_labels(
        &self,
        id: &str,
        add: &[String],
        remove: &[String],
    ) -> Result<(), GmailError> {
        forward!(self, modify_labels(id, add, remove))
    }
    async fn trash(&self, id: &str) -> Result<(), GmailError> {
        forward!(self, trash(id))
    }
    async fn untrash(&self, id: &str) -> Result<(), GmailError> {
        forward!(self, untrash(id))
    }
    async fn send(&self, raw: &[u8], thread_id: Option<&str>) -> Result<String, GmailError> {
        forward!(self, send(raw, thread_id))
    }
    async fn save_draft(
        &self,
        draft_id: Option<&str>,
        raw: &[u8],
        thread_id: Option<&str>,
    ) -> Result<SavedDraft, GmailError> {
        forward!(self, save_draft(draft_id, raw, thread_id))
    }
    async fn send_draft(&self, draft_id: &str) -> Result<String, GmailError> {
        forward!(self, send_draft(draft_id))
    }
    async fn delete_draft(&self, draft_id: &str) -> Result<(), GmailError> {
        forward!(self, delete_draft(draft_id))
    }
    async fn draft_for_message(&self, message_id: &str) -> Result<Option<String>, GmailError> {
        forward!(self, draft_for_message(message_id))
    }
    async fn display_name(&self) -> Result<Option<String>, GmailError> {
        forward!(self, display_name())
    }
    async fn attachment(
        &self,
        message_id: &str,
        attachment_id: &str,
    ) -> Result<Vec<u8>, GmailError> {
        forward!(self, attachment(message_id, attachment_id))
    }
    async fn raw_message(&self, id: &str) -> Result<Vec<u8>, GmailError> {
        forward!(self, raw_message(id))
    }
    async fn filters(&self) -> Result<Vec<Filter>, GmailError> {
        forward!(self, filters())
    }
    async fn create_filter(&self, filter: &Filter) -> Result<Filter, GmailError> {
        forward!(self, create_filter(filter))
    }
    async fn delete_filter(&self, id: &str) -> Result<(), GmailError> {
        forward!(self, delete_filter(id))
    }
    async fn create_label(&self, name: &str) -> Result<RemoteLabel, GmailError> {
        forward!(self, create_label(name))
    }
    async fn rename_label(&self, id: &str, name: &str) -> Result<RemoteLabel, GmailError> {
        forward!(self, rename_label(id, name))
    }
    async fn delete_label(&self, id: &str) -> Result<(), GmailError> {
        forward!(self, delete_label(id))
    }
    async fn set_label_color(
        &self,
        id: &str,
        color: &LabelColor,
    ) -> Result<RemoteLabel, GmailError> {
        forward!(self, set_label_color(id, color))
    }
    async fn signature(&self) -> Result<Option<String>, GmailError> {
        forward!(self, signature())
    }
    async fn vacation(&self) -> Result<Vacation, GmailError> {
        forward!(self, vacation())
    }
    async fn set_vacation(&self, vacation: &Vacation) -> Result<(), GmailError> {
        forward!(self, set_vacation(vacation))
    }
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
}
