//! Authenticated calls to the Gmail REST API, and the interactive authorize flow.

use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::{URL_SAFE_NO_PAD, URL_SAFE_NO_PAD_INDIFFERENT};
use mailrs_domain::{Filter, Vacation};
use reqwest::header::RETRY_AFTER;
use reqwest::{RequestBuilder, Response, StatusCode};
use serde::de::DeserializeOwned;
use serde_json::json;
use tokio::sync::Mutex;

use crate::convert::{HistoryPage, history_page};
use crate::convert::{html_to_text, text_to_html};
use crate::model::{
    AttachmentBody, Draft, DraftList, HistoryList, LabelList, Message, MessagePage, Profile,
    RemoteLabel, SendAs, SendAsList, Thread, VacationSettings,
};
use crate::oauth::{AccessToken, LoopbackListener, OAuthClient, Pkce, random_token};
use crate::{GmailError, QuotaLimiter};

pub const GMAIL_API_BASE: &str = "https://gmail.googleapis.com/gmail/v1/users/me";

const METADATA_HEADERS: [&str; 5] = ["From", "To", "Cc", "Subject", "Message-ID"];

/// Quota units per call, from Gmail's usage-limits table.
mod cost {
    pub const PROFILE: u32 = 1;
    pub const LABELS: u32 = 1;
    pub const LIST: u32 = 5;
    pub const GET: u32 = 5;
    pub const THREAD: u32 = 10;
    pub const HISTORY: u32 = 2;
    pub const MODIFY: u32 = 5;
    pub const TRASH: u32 = 5;
    pub const SEND: u32 = 100;
    pub const DRAFT_CREATE: u32 = 10;
    pub const DRAFT_UPDATE: u32 = 15;
    pub const DRAFT_DELETE: u32 = 10;
    pub const DRAFT_LIST: u32 = 5;
    pub const SEND_AS: u32 = 1;
    pub const ATTACHMENT: u32 = 5;
    pub const SETTINGS: u32 = 1;
}

/// A Gmail client for one account.
pub struct GmailClient {
    oauth: OAuthClient,
    refresh_token: String,
    base_url: String,
    access: Mutex<Option<AccessToken>>,
    limiter: QuotaLimiter,
}

impl GmailClient {
    pub fn new(oauth: OAuthClient, refresh_token: String) -> Self {
        GmailClient {
            oauth,
            refresh_token,
            base_url: GMAIL_API_BASE.to_string(),
            access: Mutex::new(None),
            limiter: QuotaLimiter::gmail(),
        }
    }

    /// Points the client at another server. Tests use this.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// Seeds the access token cache so the first call skips a refresh.
    pub fn with_access_token(self, token: AccessToken) -> Self {
        GmailClient {
            access: Mutex::new(Some(token)),
            ..self
        }
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
        self.call(cost::LIST, || {
            let mut request = self
                .http()
                .get(self.url("messages"))
                .query(&[("q", query), ("maxResults", &max_results.to_string())]);
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
        self.call(cost::GET, || {
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
            filter: Vec<Filter>,
        }
        let list: FilterList = self
            .call(cost::SETTINGS, || {
                self.http().get(self.url("settings/filters"))
            })
            .await?;
        Ok(list.filter)
    }

    pub async fn create_filter(&self, filter: &Filter) -> Result<Filter, GmailError> {
        let body = Filter {
            id: None,
            ..filter.clone()
        };
        self.call(cost::SETTINGS, || {
            self.http().post(self.url("settings/filters")).json(&body)
        })
        .await
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

    /// Deletes a label. Gmail removes it from every message; the mail stays.
    pub async fn delete_label(&self, id: &str) -> Result<(), GmailError> {
        self.call_empty(cost::LABELS, || {
            self.http().delete(self.url(&format!("labels/{id}")))
        })
        .await
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
            .call(cost::GET, || {
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
            .call(cost::ATTACHMENT, || {
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

    fn http(&self) -> &reqwest::Client {
        self.oauth.http()
    }

    fn url(&self, path: &str) -> String {
        format!("{}/{path}", self.base_url)
    }

    /// A cached access token with a minute to spare, or a freshly refreshed one.
    async fn bearer(&self) -> Result<String, GmailError> {
        let mut cached = self.access.lock().await;
        if let Some(token) = cached
            .as_ref()
            .filter(|t| t.valid_for(Duration::from_secs(60)))
        {
            return Ok(token.token.clone());
        }
        let fresh = self.oauth.refresh(&self.refresh_token).await?;
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
        let response = self.send_request(units, build).await?;
        response
            .json::<T>()
            .await
            .map_err(|e| GmailError::Decode(e.to_string()))
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
        self.limiter.acquire(units).await;
        let mut retried = false;
        loop {
            let token = self.bearer().await?;
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
            return Err(error_from_response(response).await);
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

async fn error_from_response(response: Response) -> GmailError {
    let status = response.status().as_u16();
    let retry_after = response
        .headers()
        .get(RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_secs);
    let body = response.text().await.unwrap_or_default();
    match status {
        401 => GmailError::NeedsReauth,
        404 => GmailError::NotFound,
        429 => GmailError::RateLimited { retry_after },
        403 if body.contains("ACCESS_TOKEN_SCOPE_INSUFFICIENT")
            || body.contains("insufficientPermissions") =>
        {
            GmailError::MissingScope
        }
        403 if body.contains("rateLimitExceeded") || body.contains("userRateLimitExceeded") => {
            GmailError::RateLimited { retry_after }
        }
        _ => GmailError::Http { status, body },
    }
}

/// Leaves a mailing list the RFC 8058 way: one POST to the list's https
/// unsubscribe link. It is not a Gmail call, so it carries no token.
pub async fn one_click_unsubscribe(url: &str) -> Result<(), GmailError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| GmailError::Network(e.to_string()))?;
    let response = client
        .post(url)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body("List-Unsubscribe=One-Click")
        .send()
        .await?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(GmailError::Http {
            status: response.status().as_u16(),
            body: String::new(),
        })
    }
}

/// The result of a completed consent flow.
#[derive(Debug, Clone)]
pub struct Authorized {
    pub email: String,
    pub refresh_token: String,
}

/// Runs the consent flow for one account. `open_browser` receives Google's
/// consent URL; the flow finishes when the browser redirects back.
pub async fn authorize(
    oauth: &OAuthClient,
    api_base: &str,
    open_browser: impl FnOnce(&str),
) -> Result<Authorized, GmailError> {
    let listener = LoopbackListener::bind().await?;
    let redirect_uri = listener.redirect_uri.clone();
    let pkce = Pkce::generate();
    let state = random_token(16);
    open_browser(&oauth.authorize_url(&redirect_uri, &pkce, &state)?);
    let code = listener.wait_for_code(&state).await?;
    let tokens = oauth.exchange_code(&code, &redirect_uri, &pkce).await?;
    let client = GmailClient::new(oauth.clone(), tokens.refresh_token.clone())
        .with_base_url(api_base)
        .with_access_token(tokens.access);
    let profile = client.profile().await?;
    Ok(Authorized {
        email: profile.email_address,
        refresh_token: tokens.refresh_token,
    })
}
