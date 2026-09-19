//! Authenticated calls to the Gmail REST API, and the interactive authorize flow.

use std::time::Duration;

use reqwest::header::RETRY_AFTER;
use reqwest::{RequestBuilder, Response, StatusCode};
use serde::de::DeserializeOwned;
use serde_json::json;
use tokio::sync::Mutex;

use crate::convert::{HistoryPage, history_page};
use crate::model::{HistoryList, LabelList, Message, MessagePage, Profile, RemoteLabel, Thread};
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
        GmailClient { access: Mutex::new(Some(token)), ..self }
    }

    pub async fn profile(&self) -> Result<Profile, GmailError> {
        self.call(cost::PROFILE, || self.http().get(self.url("profile"))).await
    }

    pub async fn labels(&self) -> Result<Vec<RemoteLabel>, GmailError> {
        let list: LabelList = self.call(cost::LABELS, || self.http().get(self.url("labels"))).await?;
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
        self.call(cost::GET, || self.http().get(self.url(&format!("messages/{id}"))).query(&metadata_query()))
            .await
    }

    pub async fn message_full(&self, id: &str) -> Result<Message, GmailError> {
        self.call(cost::GET, || self.http().get(self.url(&format!("messages/{id}"))).query(&[("format", "full")]))
            .await
    }

    pub async fn thread_metadata(&self, id: &str) -> Result<Thread, GmailError> {
        self.call(cost::THREAD, || self.http().get(self.url(&format!("threads/{id}"))).query(&metadata_query()))
            .await
    }

    pub async fn history(&self, start_history_id: u64, page_token: Option<&str>) -> Result<HistoryPage, GmailError> {
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

    pub async fn modify(&self, id: &str, add: &[String], remove: &[String]) -> Result<(), GmailError> {
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
            .call(cost::TRASH, || self.http().post(self.url(&format!("messages/{id}/trash"))).json(&json!({})))
            .await?;
        Ok(())
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
        if let Some(token) = cached.as_ref().filter(|t| t.valid_for(Duration::from_secs(60))) {
            return Ok(token.token.clone());
        }
        let fresh = self.oauth.refresh(&self.refresh_token).await?;
        let token = fresh.token.clone();
        *cached = Some(fresh);
        Ok(token)
    }

    /// Sends the request `build` makes, refreshing the token once on a 401.
    async fn call<T: DeserializeOwned>(&self, units: u32, build: impl Fn() -> RequestBuilder) -> Result<T, GmailError> {
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
                return response.json::<T>().await.map_err(|e| GmailError::Decode(e.to_string()));
            }
            return Err(error_from_response(response).await);
        }
    }
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
        403 if body.contains("rateLimitExceeded") || body.contains("userRateLimitExceeded") => {
            GmailError::RateLimited { retry_after }
        }
        _ => GmailError::Http { status, body },
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
    Ok(Authorized { email: profile.email_address, refresh_token: tokens.refresh_token })
}
