//! Authenticated calls to the Gmail REST API, and the interactive authorize flow.

use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::{URL_SAFE_NO_PAD, URL_SAFE_NO_PAD_INDIFFERENT};
use mailrs_domain::{Filter, Vacation};
use mailrs_mime::html::html_to_text;
use reqwest::header::RETRY_AFTER;
use reqwest::{RequestBuilder, Response, StatusCode};
use serde::de::DeserializeOwned;
use serde_json::json;
use tokio::sync::Mutex;

use crate::convert::text_to_html;
use crate::convert::{HistoryPage, history_page};
use crate::limiter::{self, AccountQuota};
use crate::model::{
    AttachmentBody, Draft, DraftList, GmailFilter, HistoryList, LabelColor, LabelList, Message,
    MessagePage, Profile, RemoteLabel, SendAs, SendAsList, Thread, VacationSettings,
};
use crate::oauth::{AccessToken, Granted, LoopbackListener, OAuthClient, Pkce, random_token};
use crate::people::{self, ConnectionsPage, ContactFields, Person};
use crate::{GmailError, OneClickError};

/// The largest JSON answer most calls take: label and message lists,
/// metadata, history, contacts.
const JSON_LIMIT: usize = 16 << 20;

/// The largest answer for a call that carries a whole message or file as
/// base64 in JSON. Gmail takes mail up to 50 MB, which is 67 MB as base64.
const MESSAGE_LIMIT: usize = 80 << 20;

/// Contact photos are small files; this is far above any real one.
const PHOTO_LIMIT: usize = 8 << 20;

/// How much of a failed answer is kept for the error and the log.
const ERROR_LIMIT: usize = 64 << 10;

pub const GMAIL_API_BASE: &str = "https://gmail.googleapis.com/gmail/v1/users/me";

/// The headers a `format=metadata` fetch asks for. The two unsubscribe
/// headers ride along with the rest: they cost nothing extra and they let
/// the newsletters list read a sender's way out without fetching bodies.
const METADATA_HEADERS: [&str; 7] = [
    "From",
    "To",
    "Cc",
    "Subject",
    "Message-ID",
    "List-Unsubscribe",
    "List-Unsubscribe-Post",
];

/// Message ids Gmail takes in one `batchModify` or `batchDelete` call.
pub const BATCH_LIMIT: usize = 1000;

/// Quota units per call, from Gmail's usage-limits table. The in-memory
/// Gmail prices its calls from the same table, so a test can add up what an
/// operation would spend against the real API.
pub mod cost {
    pub const PROFILE: u32 = 1;
    pub const LABELS: u32 = 1;
    pub const LIST: u32 = 5;
    pub const GET: u32 = 5;
    pub const THREAD: u32 = 10;
    pub const HISTORY: u32 = 2;
    pub const MODIFY: u32 = 5;
    pub const TRASH: u32 = 5;
    pub const DELETE: u32 = 10;
    /// One call for up to [`super::BATCH_LIMIT`] messages.
    pub const BATCH_MODIFY: u32 = 50;
    pub const BATCH_DELETE: u32 = 50;
    pub const SEND: u32 = 100;
    pub const DRAFT_CREATE: u32 = 10;
    pub const DRAFT_UPDATE: u32 = 15;
    pub const DRAFT_DELETE: u32 = 10;
    pub const DRAFT_LIST: u32 = 5;
    pub const SEND_AS: u32 = 1;
    pub const ATTACHMENT: u32 = 5;
    pub const SETTINGS: u32 = 1;
    /// One page of contacts. The People API keeps a budget of its own, so
    /// this only stops a refresh from crowding out the mail the user is
    /// waiting for.
    pub const CONNECTIONS: u32 = 5;
    /// Adding or changing one contact, on the People API's budget too.
    pub const CONTACT_WRITE: u32 = 5;
}

/// What runs when a refresh reports a different set of scopes than
/// [`GmailClient`] held. See the field's own doc for what it must not do.
type GrantedHook = std::sync::Arc<dyn Fn(&Granted) + Send + Sync>;

/// The HTTP client a [`GmailClient`] built with [`GmailClient::with_source`]
/// sends through: Google's calls wait at most 30 seconds, as the OAuth
/// client's do.
pub fn api_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .expect("the TLS backend initializes")
}

/// Where an access token comes from when the client does not refresh its
/// own, such as the desktop's Online Accounts.
pub trait AccessSource: Send + Sync {
    /// A token to send. `renew` is true only on the one retry after a 401,
    /// when the last token was refused.
    fn access(&self, renew: bool) -> futures::future::BoxFuture<'_, Result<String, GmailError>>;
}

/// How the client gets its tokens.
enum Access {
    /// The client's refresh token, exchanged at Google's token endpoint.
    Refresh {
        oauth: OAuthClient,
        refresh_token: String,
    },
    /// A token from elsewhere, which keeps no refresh token here.
    Source {
        source: std::sync::Arc<dyn AccessSource>,
        http: reqwest::Client,
    },
}

/// A Gmail client for one account.
pub struct GmailClient {
    access_from: Access,
    base_url: String,
    /// The People API, which lives at a host of its own.
    people_url: String,
    /// Answering an invitation goes to the Calendar API, which is another
    /// server behind the same access token. See `crate::calendar`.
    pub(crate) calendar_base_url: String,
    /// Drive's upload endpoint, for files an event links to. See
    /// `crate::drive`.
    pub(crate) drive_upload_base_url: String,
    /// Drive's own API, for sharing a file the app uploaded.
    pub(crate) drive_base_url: String,
    access: Mutex<Option<AccessToken>>,
    quota: std::sync::Arc<AccountQuota>,
    /// The scopes this account is believed to have granted: seeded from
    /// the store and updated whenever a refresh reports a different set.
    granted: std::sync::Mutex<Option<Granted>>,
    /// Runs when [`GmailClient::bearer`] finds a refresh reporting a
    /// different set of scopes than `granted` held, with the refresh lock
    /// still taken. It must only hand the value on, never await anything
    /// that itself waits on this client.
    on_granted: Option<GrantedHook>,
}

impl GmailClient {
    /// A client with a quota of its own. The consent flow uses this; an
    /// account the app syncs wants [`GmailClient::for_account`], so two
    /// clients for one address cannot each spend that address's budget.
    pub fn new(oauth: OAuthClient, refresh_token: String) -> Self {
        let quota = std::sync::Arc::new(AccountQuota::standalone());
        GmailClient::with_access(
            Access::Refresh {
                oauth,
                refresh_token,
            },
            quota,
        )
    }

    /// A client that takes its tokens from `source` and sends them over
    /// `http`. The `on_granted` hook never runs for it: no refresh reports
    /// scopes.
    pub fn with_source(
        source: std::sync::Arc<dyn AccessSource>,
        http: reqwest::Client,
        quota: std::sync::Arc<AccountQuota>,
    ) -> Self {
        GmailClient::with_access(Access::Source { source, http }, quota)
    }

    fn with_access(access_from: Access, quota: std::sync::Arc<AccountQuota>) -> Self {
        GmailClient {
            access_from,
            base_url: GMAIL_API_BASE.to_string(),
            people_url: people::PEOPLE_API_BASE.to_string(),
            calendar_base_url: crate::calendar::CALENDAR_API_BASE.to_string(),
            drive_upload_base_url: crate::drive::DRIVE_UPLOAD_BASE.to_string(),
            drive_base_url: crate::drive::DRIVE_API_BASE.to_string(),
            access: Mutex::new(None),
            quota,
            granted: std::sync::Mutex::new(None),
            on_granted: None,
        }
    }

    /// A client that spends `email`'s share of the OAuth client's quota.
    /// Every client built this way for one address waits on one bucket, and
    /// all of them wait on the project's bucket as well.
    pub fn for_account(oauth: OAuthClient, refresh_token: String, email: &str) -> Self {
        let quota = oauth.account_quota(email);
        GmailClient {
            quota,
            ..GmailClient::new(oauth, refresh_token)
        }
    }

    /// Points the client at another server. Tests use this.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// Points the contacts calls at another server. Tests use this.
    pub fn with_people_url(mut self, people_url: impl Into<String>) -> Self {
        self.people_url = people_url.into();
        self
    }

    /// Seeds the access token cache so the first call skips a refresh.
    pub fn with_access_token(self, token: AccessToken) -> Self {
        GmailClient {
            access: Mutex::new(Some(token)),
            ..self
        }
    }

    /// Seeds the scopes this account is believed to have granted, from
    /// what the store kept. `None` when they are not known yet: an
    /// account that has never refreshed since the upgrade that added
    /// this, or one just added, for which [`crate::authorize`] already
    /// checked mail was granted.
    pub fn with_granted(self, granted: Option<Granted>) -> Self {
        GmailClient {
            granted: std::sync::Mutex::new(granted),
            ..self
        }
    }

    /// Runs `f` whenever a refresh reports a set of scopes different from
    /// the one this client held. See the field's own doc for what `f`
    /// must not do.
    pub fn on_granted(self, f: impl Fn(&Granted) + Send + Sync + 'static) -> Self {
        GmailClient {
            on_granted: Some(std::sync::Arc::new(f)),
            ..self
        }
    }

    /// The scopes this client currently believes the account has
    /// granted, or `None` while that is not known.
    pub fn granted(&self) -> Option<Granted> {
        self.granted.lock().expect("client poisoned").clone()
    }

    pub async fn profile(&self) -> Result<Profile, GmailError> {
        self.call(cost::PROFILE, || self.http().get(self.url("profile")))
            .await
    }

    pub async fn labels(&self) -> Result<Vec<RemoteLabel>, GmailError> {
        let list: LabelList = self
            .call(cost::LABELS, || self.http().get(self.url("labels")))
            .await?;
        Ok(list.labels)
    }

    pub async fn list_messages(
        &self,
        query: &str,
        page_token: Option<&str>,
        max_results: u32,
    ) -> Result<MessagePage, GmailError> {
        self.list(query, None, page_token, max_results).await
    }

    /// One page of the messages `query` matches that carry `label_id`,
    /// named by id as `labelIds` takes it, so a label whose name a search
    /// could not spell is still listed.
    pub async fn list_labelled(
        &self,
        label_id: &str,
        query: &str,
        page_token: Option<&str>,
        max_results: u32,
    ) -> Result<MessagePage, GmailError> {
        self.list(query, Some(label_id), page_token, max_results)
            .await
    }

    async fn list(
        &self,
        query: &str,
        label_id: Option<&str>,
        page_token: Option<&str>,
        max_results: u32,
    ) -> Result<MessagePage, GmailError> {
        self.call(cost::LIST, || {
            let mut request = self
                .http()
                .get(self.url("messages"))
                .query(&[("q", query), ("maxResults", &max_results.to_string())]);
            if let Some(label_id) = label_id {
                request = request.query(&[("labelIds", label_id)]);
                if label_id == crate::labels::TRASH {
                    request = request.query(&[("includeSpamTrash", "true")]);
                }
            }
            if let Some(token) = page_token {
                request = request.query(&[("pageToken", token)]);
            }
            request
        })
        .await
    }

    pub async fn message_metadata(&self, id: &str) -> Result<Message, GmailError> {
        self.call(cost::GET, || {
            self.http()
                .get(self.url(&format!("messages/{id}")))
                .query(&metadata_query())
        })
        .await
    }

    pub async fn message_full(&self, id: &str) -> Result<Message, GmailError> {
        self.call_message(cost::GET, || {
            self.http()
                .get(self.url(&format!("messages/{id}")))
                .query(&[("format", "full")])
        })
        .await
    }

    pub async fn thread_metadata(&self, id: &str) -> Result<Thread, GmailError> {
        self.call(cost::THREAD, || {
            self.http()
                .get(self.url(&format!("threads/{id}")))
                .query(&metadata_query())
        })
        .await
    }

    pub async fn history(
        &self,
        start_history_id: u64,
        page_token: Option<&str>,
    ) -> Result<HistoryPage, GmailError> {
        let list: HistoryList = self
            .call(cost::HISTORY, || {
                let mut request = self
                    .http()
                    .get(self.url("history"))
                    .query(&[("startHistoryId", start_history_id.to_string())]);
                if let Some(token) = page_token {
                    request = request.query(&[("pageToken", token)]);
                }
                request
            })
            .await?;
        Ok(history_page(list))
    }

    pub async fn modify(
        &self,
        id: &str,
        add: &[String],
        remove: &[String],
    ) -> Result<(), GmailError> {
        let _: Message = self
            .call(cost::MODIFY, || {
                self.http()
                    .post(self.url(&format!("messages/{id}/modify")))
                    .json(&json!({"addLabelIds": add, "removeLabelIds": remove}))
            })
            .await?;
        Ok(())
    }

    /// One label change over up to [`BATCH_LIMIT`] messages. Gmail charges
    /// 50 units for the call however many ids it carries, against 5 units
    /// for each `modify`, so it is worth it from the eleventh message on.
    pub async fn batch_modify(
        &self,
        ids: &[String],
        add: &[String],
        remove: &[String],
    ) -> Result<(), GmailError> {
        assert!(
            ids.len() <= BATCH_LIMIT,
            "batch of {} exceeds Gmail's limit of {BATCH_LIMIT}",
            ids.len()
        );
        self.call_empty(cost::BATCH_MODIFY, || {
            self.http()
                .post(self.url("messages/batchModify"))
                .json(&json!({
                    "ids": ids,
                    "addLabelIds": add,
                    "removeLabelIds": remove,
                }))
        })
        .await
    }

    /// Erases up to [`BATCH_LIMIT`] messages. Gmail does not put them in the
    /// Trash and nothing brings them back. Gmail refuses the call with a 403
    /// until the account grants [`DELETE_SCOPE`], which arrives here as
    /// [`GmailError::MissingScope`].
    ///
    /// [`DELETE_SCOPE`]: crate::DELETE_SCOPE
    pub async fn batch_delete(&self, ids: &[String]) -> Result<(), GmailError> {
        if ids.is_empty() {
            return Ok(());
        }
        assert!(
            ids.len() <= BATCH_LIMIT,
            "batch of {} exceeds Gmail's limit of {BATCH_LIMIT}",
            ids.len()
        );
        self.call_empty(cost::BATCH_DELETE, || {
            self.http()
                .post(self.url("messages/batchDelete"))
                .json(&json!({"ids": ids}))
        })
        .await
    }

    pub async fn trash(&self, id: &str) -> Result<(), GmailError> {
        let _: Message = self
            .call(cost::TRASH, || {
                self.http()
                    .post(self.url(&format!("messages/{id}/trash")))
                    .json(&json!({}))
            })
            .await?;
        Ok(())
    }

    pub async fn untrash(&self, id: &str) -> Result<(), GmailError> {
        let _: Message = self
            .call(cost::TRASH, || {
                self.http()
                    .post(self.url(&format!("messages/{id}/untrash")))
                    .json(&json!({}))
            })
            .await?;
        Ok(())
    }

    /// Sends a complete RFC 822 message. `thread_id` files a reply in its thread.
    pub async fn send(&self, raw: &[u8], thread_id: Option<&str>) -> Result<Message, GmailError> {
        let body = raw_message(raw, thread_id);
        self.call(cost::SEND, || {
            self.http().post(self.url("messages/send")).json(&body)
        })
        .await
    }

    pub async fn create_draft(
        &self,
        raw: &[u8],
        thread_id: Option<&str>,
    ) -> Result<Draft, GmailError> {
        let body = json!({"message": raw_message(raw, thread_id)});
        self.call(cost::DRAFT_CREATE, || {
            self.http().post(self.url("drafts")).json(&body)
        })
        .await
    }

    pub async fn update_draft(
        &self,
        id: &str,
        raw: &[u8],
        thread_id: Option<&str>,
    ) -> Result<Draft, GmailError> {
        let body = json!({"id": id, "message": raw_message(raw, thread_id)});
        self.call(cost::DRAFT_UPDATE, || {
            self.http()
                .put(self.url(&format!("drafts/{id}")))
                .json(&body)
        })
        .await
    }

    /// Sends an existing draft as it stands in Gmail.
    pub async fn send_draft(&self, id: &str) -> Result<Message, GmailError> {
        let body = json!({"id": id});
        self.call(cost::SEND, || {
            self.http().post(self.url("drafts/send")).json(&body)
        })
        .await
    }

    pub async fn delete_draft(&self, id: &str) -> Result<(), GmailError> {
        self.call_empty(cost::DRAFT_DELETE, || {
            self.http().delete(self.url(&format!("drafts/{id}")))
        })
        .await
    }

    /// Every draft in the account, following page tokens.
    pub async fn list_drafts(&self) -> Result<Vec<Draft>, GmailError> {
        let mut drafts = Vec::new();
        let mut page_token: Option<String> = None;
        loop {
            let page: DraftList = self
                .call(cost::DRAFT_LIST, || {
                    let request = self.http().get(self.url("drafts"));
                    match &page_token {
                        Some(token) => request.query(&[("pageToken", token)]),
                        None => request,
                    }
                })
                .await?;
            drafts.extend(page.drafts);
            match page.next_page_token {
                Some(token) => page_token = Some(token),
                None => return Ok(drafts),
            }
        }
    }

    /// Every filter in the account. Needs the settings scope.
    pub async fn filters(&self) -> Result<Vec<Filter>, GmailError> {
        #[derive(serde::Deserialize)]
        struct FilterList {
            #[serde(default)]
            filter: Vec<GmailFilter>,
        }
        let list: FilterList = self
            .call(cost::SETTINGS, || {
                self.http().get(self.url("settings/filters"))
            })
            .await?;
        Ok(list
            .filter
            .into_iter()
            .map(GmailFilter::into_filter)
            .collect())
    }

    pub async fn create_filter(&self, filter: &Filter) -> Result<Filter, GmailError> {
        let body = GmailFilter {
            id: None,
            ..GmailFilter::from(filter)
        };
        self.call(cost::SETTINGS, || {
            self.http().post(self.url("settings/filters")).json(&body)
        })
        .await
        .map(GmailFilter::into_filter)
    }

    pub async fn delete_filter(&self, id: &str) -> Result<(), GmailError> {
        self.call_empty(cost::SETTINGS, || {
            self.http()
                .delete(self.url(&format!("settings/filters/{id}")))
        })
        .await
    }

    /// Creates a user label shown in Gmail's label list.
    pub async fn create_label(&self, name: &str) -> Result<RemoteLabel, GmailError> {
        let body = json!({
            "name": name,
            "labelListVisibility": "labelShow",
            "messageListVisibility": "show",
        });
        self.call(cost::LABELS, || {
            self.http().post(self.url("labels")).json(&body)
        })
        .await
    }

    pub async fn rename_label(&self, id: &str, name: &str) -> Result<RemoteLabel, GmailError> {
        let body = json!({"name": name});
        self.call(cost::LABELS, || {
            self.http()
                .patch(self.url(&format!("labels/{id}")))
                .json(&body)
        })
        .await
    }

    pub async fn set_label_color(
        &self,
        id: &str,
        color: &LabelColor,
    ) -> Result<RemoteLabel, GmailError> {
        let body = json!({"color": color});
        self.call(cost::LABELS, || {
            self.http()
                .patch(self.url(&format!("labels/{id}")))
                .json(&body)
        })
        .await
    }

    /// Deletes a label. Gmail removes it from every message; the mail stays.
    pub async fn delete_label(&self, id: &str) -> Result<(), GmailError> {
        self.call_empty(cost::LABELS, || {
            self.http().delete(self.url(&format!("labels/{id}")))
        })
        .await
    }

    /// How many conversations carry a label, across the whole mailbox.
    pub async fn label_threads(&self, id: &str) -> Result<u64, GmailError> {
        let totals: LabelTotals = self
            .call(cost::LABELS, || {
                self.http().get(self.url(&format!("labels/{id}")))
            })
            .await?;
        Ok(totals.threads_total)
    }

    /// Addresses the account can send from, including its display names.
    pub async fn send_as(&self) -> Result<Vec<SendAs>, GmailError> {
        let list: SendAsList = self
            .call(cost::SEND_AS, || {
                self.http().get(self.url("settings/sendAs"))
            })
            .await?;
        Ok(list.send_as)
    }

    /// The automatic reply. Needs the settings scope.
    pub async fn vacation(&self) -> Result<Vacation, GmailError> {
        let settings: VacationSettings = self
            .call(cost::SETTINGS, || {
                self.http().get(self.url("settings/vacation"))
            })
            .await?;
        let body = match settings
            .response_body_plain_text
            .filter(|t| !t.trim().is_empty())
        {
            Some(text) => text,
            None => html_to_text(settings.response_body_html.as_deref().unwrap_or_default()),
        };
        let time = |t: Option<String>| t.and_then(|t| t.parse().ok()).filter(|t: &i64| *t > 0);
        Ok(Vacation {
            enabled: settings.enable_auto_reply,
            subject: settings.response_subject,
            body,
            contacts_only: settings.restrict_to_contacts,
            domain_only: settings.restrict_to_domain,
            start: time(settings.start_time),
            end: time(settings.end_time),
        })
    }

    /// Replaces the automatic reply. Needs the settings scope.
    pub async fn set_vacation(&self, vacation: &Vacation) -> Result<(), GmailError> {
        let settings = VacationSettings {
            enable_auto_reply: vacation.enabled,
            response_subject: vacation.subject.clone(),
            response_body_plain_text: Some(vacation.body.clone()),
            response_body_html: Some(text_to_html(&vacation.body)),
            restrict_to_contacts: vacation.contacts_only,
            restrict_to_domain: vacation.domain_only,
            start_time: vacation.start.map(|t| t.to_string()),
            end_time: vacation.end.map(|t| t.to_string()),
        };
        self.call_empty(cost::SETTINGS, || {
            self.http()
                .put(self.url("settings/vacation"))
                .json(&settings)
        })
        .await
    }

    /// The message exactly as it arrived: RFC 822 bytes.
    pub async fn raw_message(&self, id: &str) -> Result<Vec<u8>, GmailError> {
        #[derive(serde::Deserialize)]
        struct Raw {
            #[serde(default)]
            raw: String,
        }
        let message: Raw = self
            .call_message(cost::GET, || {
                self.http()
                    .get(self.url(&format!("messages/{id}")))
                    .query(&[("format", "raw")])
            })
            .await?;
        URL_SAFE_NO_PAD_INDIFFERENT
            .decode(message.raw.trim())
            .map_err(|e| GmailError::Decode(e.to_string()))
    }

    /// The decoded content of one attachment.
    pub async fn attachment(
        &self,
        message_id: &str,
        attachment_id: &str,
    ) -> Result<Vec<u8>, GmailError> {
        let body: AttachmentBody = self
            .call_message(cost::ATTACHMENT, || {
                self.http().get(self.url(&format!(
                    "messages/{message_id}/attachments/{attachment_id}"
                )))
            })
            .await?;
        let data = body.data.unwrap_or_default();
        URL_SAFE_NO_PAD_INDIFFERENT
            .decode(data.trim())
            .map_err(|e| GmailError::Decode(e.to_string()))
    }

    /// One page of the account's Google contacts. Pass `page_token` to
    /// walk a long address book, or `sync_token` from the last refresh to
    /// ask only for what changed. Gmail answers
    /// [`GmailError::MissingScope`] until the account grants
    /// [`CONTACTS_SCOPE`], and [`GmailError::ExpiredSyncToken`] when the
    /// token is too old to answer from.
    ///
    /// [`CONTACTS_SCOPE`]: crate::people::CONTACTS_SCOPE
    pub async fn connections(
        &self,
        page_token: Option<&str>,
        sync_token: Option<&str>,
    ) -> Result<ConnectionsPage, GmailError> {
        let url = format!("{}/people/me/connections", self.people_url);
        let response = self
            .send_request(cost::CONNECTIONS, || {
                let mut request = self.http().get(&url).query(&[
                    ("personFields", people::PERSON_FIELDS),
                    ("pageSize", &people::PAGE_SIZE.to_string()),
                    ("requestSyncToken", "true"),
                ]);
                if let Some(token) = page_token {
                    request = request.query(&[("pageToken", token)]);
                }
                if let Some(token) = sync_token {
                    request = request.query(&[("syncToken", token)]);
                }
                request
            })
            .await?;
        let body = text_of(response, JSON_LIMIT).await?;
        people::parse_connections(&body)
    }

    /// Adds a contact to the account's Google contacts. Gmail answers
    /// [`GmailError::MissingScope`] until the account grants
    /// [`CONTACTS_WRITE_SCOPE`].
    ///
    /// [`CONTACTS_WRITE_SCOPE`]: crate::people::CONTACTS_WRITE_SCOPE
    pub async fn create_contact(&self, fields: &ContactFields) -> Result<Person, GmailError> {
        let url = format!("{}/people:createContact", self.people_url);
        let body = fields.body();
        let reply = self
            .send_request(cost::CONTACT_WRITE, || {
                self.http()
                    .post(&url)
                    .query(&[("personFields", people::PERSON_FIELDS)])
                    .json(&body)
            })
            .await?;
        let reply = text_of(reply, JSON_LIMIT).await?;
        Ok(people::parse_person(&reply)?.0)
    }

    /// Changes the fields `fields` names on the contact `resource`, such
    /// as `people/c17`. Google refuses a change without the contact's
    /// current etag, so this reads the contact first.
    pub async fn update_contact(
        &self,
        resource: &str,
        fields: &ContactFields,
    ) -> Result<Person, GmailError> {
        if !resource.starts_with("people/") || resource.contains("..") {
            return Err(GmailError::NotFound);
        }
        let url = format!("{}/{resource}", self.people_url);
        let current = self
            .send_request(cost::CONNECTIONS, || {
                self.http().get(&url).query(&[("personFields", "metadata")])
            })
            .await?;
        let current = text_of(current, JSON_LIMIT).await?;
        let (_, etag) = people::parse_person(&current)?;
        let mut body = fields.body();
        body["etag"] = json!(etag);
        let mask = fields.mask();
        let url = format!("{url}:updateContact");
        let reply = self
            .send_request(cost::CONTACT_WRITE, || {
                self.http()
                    .patch(&url)
                    .query(&[
                        ("updatePersonFields", mask.as_str()),
                        ("personFields", people::PERSON_FIELDS),
                    ])
                    .json(&body)
            })
            .await?;
        let reply = text_of(reply, JSON_LIMIT).await?;
        Ok(people::parse_person(&reply)?.0)
    }

    /// The bytes of one contact photo. Google serves these from a plain
    /// file host that takes no token and counts against no quota.
    pub async fn contact_photo(&self, url: &str) -> Result<Vec<u8>, GmailError> {
        let response = self.http().get(url).send().await?;
        if !response.status().is_success() {
            return Err(error_from_response(response).await);
        }
        read_capped(response, PHOTO_LIMIT).await
    }

    /// The bucket this client spends from, so a caller can see whether the
    /// user is waiting on it.
    pub fn quota(&self) -> &AccountQuota {
        &self.quota
    }

    pub(crate) fn http(&self) -> &reqwest::Client {
        match &self.access_from {
            Access::Refresh { oauth, .. } => oauth.http(),
            Access::Source { http, .. } => http,
        }
    }

    /// The account's access token, refreshed when it is about to run out.
    pub(crate) async fn token(&self) -> Result<String, GmailError> {
        self.bearer(false).await
    }

    /// Sends the request `build` makes with the account's token and hands
    /// back the answer whatever its status, for a caller that reads more
    /// than success and failure, such as Drive's 308 during a resumable
    /// upload. A 401 refreshes the token and sends once more.
    pub(crate) async fn send_any(
        &self,
        build: impl Fn() -> RequestBuilder,
    ) -> Result<Response, GmailError> {
        let mut retried = false;
        loop {
            let token = self.bearer(retried).await?;
            let response = build().bearer_auth(&token).send().await?;
            if response.status() == StatusCode::UNAUTHORIZED && !retried {
                retried = true;
                *self.access.lock().await = None;
                continue;
            }
            return Ok(response);
        }
    }

    /// Sends a request to a full URL and decodes the JSON reply. Calls
    /// outside Gmail go through here, so they ask the account's Gmail
    /// budget for nothing: the Calendar API counts against a budget of its
    /// own, and charging this one would slow mail down for no reason.
    pub(crate) async fn call_at<T: DeserializeOwned>(
        &self,
        url: &str,
        build: impl Fn(&str) -> RequestBuilder,
    ) -> Result<T, GmailError> {
        let response = self.send_request(0, || build(url)).await?;
        json_of(response).await
    }

    /// As [`GmailClient::call_at`], for a call whose answer has no body,
    /// such as deleting an event.
    pub(crate) async fn call_at_empty(
        &self,
        url: &str,
        build: impl Fn(&str) -> RequestBuilder,
    ) -> Result<(), GmailError> {
        self.send_request(0, || build(url)).await.map(|_| ())
    }

    fn url(&self, path: &str) -> String {
        format!("{}/{path}", self.base_url)
    }

    /// A cached access token with a minute to spare, or a freshly refreshed one.
    /// `renew` is true on the retry after a 401; a source then hands out a
    /// new token, where a refresh token's cache is already dropped.
    async fn bearer(&self, renew: bool) -> Result<String, GmailError> {
        let (oauth, refresh_token) = match &self.access_from {
            Access::Refresh {
                oauth,
                refresh_token,
            } => (oauth, refresh_token),
            Access::Source { source, .. } => return source.access(renew).await,
        };
        let mut cached = self.access.lock().await;
        if let Some(token) = cached
            .as_ref()
            .filter(|t| t.valid_for(Duration::from_secs(60)))
        {
            return Ok(token.token.clone());
        }
        let fresh = oauth.refresh(refresh_token).await?;
        if let Some(granted) = &fresh.granted {
            let changed = {
                let mut held = self.granted.lock().expect("client poisoned");
                let changed = held.as_ref() != Some(granted);
                if changed {
                    *held = Some(granted.clone());
                }
                changed
            };
            if changed && let Some(hook) = &self.on_granted {
                hook(granted);
            }
        }
        let token = fresh.token.clone();
        *cached = Some(fresh);
        Ok(token)
    }

    /// Sends the request `build` makes and decodes the JSON reply.
    async fn call<T: DeserializeOwned>(
        &self,
        units: u32,
        build: impl Fn() -> RequestBuilder,
    ) -> Result<T, GmailError> {
        self.call_up_to(units, JSON_LIMIT, build).await
    }

    /// As [`GmailClient::call`], for an answer that may hold a whole
    /// message or file, which passes the usual cap.
    async fn call_message<T: DeserializeOwned>(
        &self,
        units: u32,
        build: impl Fn() -> RequestBuilder,
    ) -> Result<T, GmailError> {
        self.call_up_to(units, MESSAGE_LIMIT, build).await
    }

    async fn call_up_to<T: DeserializeOwned>(
        &self,
        units: u32,
        limit: usize,
        build: impl Fn() -> RequestBuilder,
    ) -> Result<T, GmailError> {
        let response = self.send_request(units, build).await?;
        let body = text_of(response, limit).await?;
        // Gmail answers some lists that have nothing in them, such as the
        // filters of an account with none, with no body at all, not `{}`.
        let body = if body.trim().is_empty() { "{}" } else { &body };
        serde_json::from_str(body).map_err(|e| GmailError::Decode(e.to_string()))
    }

    /// Sends the request `build` makes and ignores the reply body.
    async fn call_empty(
        &self,
        units: u32,
        build: impl Fn() -> RequestBuilder,
    ) -> Result<(), GmailError> {
        self.send_request(units, build).await.map(|_| ())
    }

    /// Sends the request `build` makes, refreshing the token once on a 401.
    /// Returns the response when the status is a success.
    async fn send_request(
        &self,
        units: u32,
        build: impl Fn() -> RequestBuilder,
    ) -> Result<Response, GmailError> {
        self.quota.acquire(units, limiter::priority()).await;
        let mut retried = false;
        loop {
            let token = self.bearer(retried).await?;
            let response = build().bearer_auth(&token).send().await?;
            let status = response.status();
            if status == StatusCode::UNAUTHORIZED && !retried {
                retried = true;
                *self.access.lock().await = None;
                continue;
            }
            if status.is_success() {
                return Ok(response);
            }
            let error = error_from_response(response).await;
            if matches!(error, GmailError::RateLimited { .. }) {
                // Gmail's limit is a moving average, so this account will
                // not take the pace we are keeping. Drop it and climb back.
                self.quota.slow_down();
                tracing::debug!(
                    rate = self.quota.rate(),
                    "Gmail refused; slowing this account"
                );
            }
            return Err(error);
        }
    }
}

/// The `message` object Gmail expects for sends and drafts.
fn raw_message(raw: &[u8], thread_id: Option<&str>) -> serde_json::Value {
    let mut message = json!({"raw": URL_SAFE_NO_PAD.encode(raw)});
    if let Some(thread_id) = thread_id {
        message["threadId"] = json!(thread_id);
    }
    message
}

fn metadata_query() -> Vec<(&'static str, &'static str)> {
    let mut query = vec![("format", "metadata")];
    query.extend(METADATA_HEADERS.iter().map(|h| ("metadataHeaders", *h)));
    query
}

/// The body, read a chunk at a time and refused once it passes `limit`,
/// so a large answer never sits whole in memory before it is refused.
pub(crate) async fn read_capped(
    mut response: Response,
    limit: usize,
) -> Result<Vec<u8>, GmailError> {
    if response.content_length().is_some_and(|n| n > limit as u64) {
        return Err(GmailError::TooLarge { limit });
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len() + chunk.len() > limit {
            return Err(GmailError::TooLarge { limit });
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

async fn text_of(response: Response, limit: usize) -> Result<String, GmailError> {
    String::from_utf8(read_capped(response, limit).await?)
        .map_err(|e| GmailError::Decode(e.to_string()))
}

/// A JSON answer, read under the usual cap.
pub(crate) async fn json_of<T: DeserializeOwned>(response: Response) -> Result<T, GmailError> {
    let bytes = read_capped(response, JSON_LIMIT).await?;
    serde_json::from_slice(&bytes).map_err(|e| GmailError::Decode(e.to_string()))
}

pub(crate) async fn error_from_response(response: Response) -> GmailError {
    let status = response.status().as_u16();
    let retry_after = response
        .headers()
        .get(RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_secs);
    let body =
        String::from_utf8_lossy(&read_capped(response, ERROR_LIMIT).await.unwrap_or_default())
            .into_owned();
    match status {
        401 => GmailError::NeedsReauth,
        404 => GmailError::NotFound,
        429 => GmailError::RateLimited { retry_after },
        403 if body.contains("SERVICE_DISABLED") => {
            api_disabled(&body).unwrap_or(GmailError::Http { status, body })
        }
        403 if body.contains("ACCESS_TOKEN_SCOPE_INSUFFICIENT")
            || body.contains("insufficientPermissions") =>
        {
            GmailError::MissingScope
        }
        403 if body.contains("rateLimitExceeded") || body.contains("userRateLimitExceeded") => {
            GmailError::RateLimited { retry_after }
        }
        400 if body.contains("EXPIRED_SYNC_TOKEN") => GmailError::ExpiredSyncToken,
        // The Calendar API answers a sync token it no longer holds with
        // 410 rather than Gmail's 400.
        410 => GmailError::ExpiredSyncToken,
        412 => GmailError::Changed,
        _ => GmailError::Http { status, body },
    }
}

/// Reads the API's name and the page that turns it on out of Google's
/// SERVICE_DISABLED answer, which carries both in its ErrorInfo detail.
fn api_disabled(body: &str) -> Option<GmailError> {
    let answer: serde_json::Value = serde_json::from_str(body).ok()?;
    let details = answer.pointer("/error/details")?.as_array()?;
    let info = details
        .iter()
        .find(|d| d.get("reason").and_then(|r| r.as_str()) == Some("SERVICE_DISABLED"))?;
    let field = |name: &str| {
        info.pointer(&format!("/metadata/{name}"))
            .and_then(|v| v.as_str())
            .map(str::to_string)
    };
    Some(GmailError::ApiDisabled {
        service: field("serviceTitle").or_else(|| field("service"))?,
        enable_url: field("activationUrl")?,
    })
}

/// Leaves a mailing list the RFC 8058 way: one POST to the list's https
/// unsubscribe link. It is not a Gmail call, so it carries no token, and
/// a failure names the list's server rather than Gmail.
pub async fn one_click_unsubscribe(url: &str) -> Result<(), OneClickError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| OneClickError::unreachable(url, e))?;
    let response = client
        .post(url)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body("List-Unsubscribe=One-Click")
        .send()
        .await
        .map_err(|e| OneClickError::unreachable(url, e))?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(OneClickError::refused(url, response.status().as_u16()))
    }
}

/// The result of a completed consent flow.
#[derive(Clone)]
pub struct Authorized {
    pub email: String,
    pub refresh_token: String,
    /// The scopes Google's token answer said it granted. `None` when the
    /// answer carried no `scope` field.
    pub granted: Option<Granted>,
}

// The refresh token signs in as the person and the address names them, so
// Debug output, which can end up in a log, hides both.
impl std::fmt::Debug for Authorized {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Authorized")
            .field("email", &"<hidden>")
            .field("refresh_token", &"<hidden>")
            .field("granted", &self.granted)
            .finish()
    }
}

/// Runs the consent flow for one account, asking for every scope of the
/// client's list in one visit. `open_browser` receives Google's consent
/// URL; the flow finishes when the browser redirects back.
///
/// With [`crate::ScopeList::Full`] it fails with
/// [`GmailError::MailNotGranted`] when the person unticked mail itself,
/// before asking Gmail for anything that would only answer 403, and reads
/// the address from the profile. With [`crate::ScopeList::BuiltIn`] the
/// token cannot call the profile, so the address comes from a verified ID
/// token, and [`GmailError::NoAddress`] answers when there is none.
pub async fn authorize(
    oauth: &OAuthClient,
    api_base: &str,
    open_browser: impl FnOnce(&str),
) -> Result<Authorized, GmailError> {
    authorize_hinted(oauth, api_base, None, open_browser).await
}

/// [`authorize`] naming the account to sign in as, with Google's
/// `login_hint`.
pub async fn authorize_hinted(
    oauth: &OAuthClient,
    api_base: &str,
    login_hint: Option<&str>,
    open_browser: impl FnOnce(&str),
) -> Result<Authorized, GmailError> {
    let listener = LoopbackListener::bind().await?;
    let redirect_uri = listener.redirect_uri.clone();
    let pkce = Pkce::generate();
    let state = random_token(16);
    open_browser(&oauth.authorize_url_hinted(&redirect_uri, &pkce, &state, login_hint)?);
    let code = listener.wait_for_code(&state).await?;
    let tokens = oauth.exchange_code(&code, &redirect_uri, &pkce).await?;
    let granted = tokens.access.granted.clone();
    if !oauth.scope_list().asks_mail() {
        let email = tokens
            .id_token
            .filter(|id| id.email_verified)
            .map(|id| id.email)
            .ok_or(GmailError::NoAddress)?;
        return Ok(Authorized {
            email,
            refresh_token: tokens.refresh_token,
            granted,
        });
    }
    if granted.as_ref().is_some_and(|g| !g.reads_mail()) {
        return Err(GmailError::MailNotGranted);
    }
    let client = GmailClient::new(oauth.clone(), tokens.refresh_token.clone())
        .with_base_url(api_base)
        .with_access_token(tokens.access);
    let profile = client.profile().await?;
    Ok(Authorized {
        email: profile.email_address,
        refresh_token: tokens.refresh_token,
        granted,
    })
}

/// The counts `labels.get` gives with a label. Only the thread total is
/// read.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct LabelTotals {
    #[serde(default)]
    threads_total: u64,
}
