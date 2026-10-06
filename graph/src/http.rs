//! Every call Penguin Mail makes to Graph. The client talks to one host,
//! the base it was built with, and follows a link Graph hands back only
//! when the link names that same scheme, host and port: a token is worth
//! the whole mailbox, and a link is only text a server sent. Every call
//! asks for immutable ids, so a message keeps its id when it moves. A
//! refused access token is refreshed once; a throttle of five seconds or
//! less is waited out once and a longer one is the caller's to wait.

use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use url::Url;

use crate::auth::{Granted, Session};
use crate::error::{GraphError, classify};

pub const GRAPH_BASE: &str = "https://graph.microsoft.com/v1.0/";

/// The most requests Graph takes in one `$batch`.
pub const BATCH_LIMIT: usize = 20;

/// The longest `Retry-After` the client waits out itself, once.
pub const MAX_INLINE_WAIT: Duration = Duration::from_secs(5);

/// The page size every listing asks for.
pub const PAGE_SIZE: u32 = 50;

/// The most one JSON answer may take. A page of 50 messages' metadata is
/// well under a megabyte; this stops a broken server from filling memory.
const JSON_LIMIT: usize = 8 << 20;

/// What an error body may take; Graph's are a few hundred bytes.
const ERROR_LIMIT: usize = 64 << 10;

const IMMUTABLE_IDS: &str = "IdType=\"ImmutableId\"";

/// The most pages [`Graph::get_all`] reads: a thousand calendars, contact
/// folders or rules at a hundred a page.
const MOST_LIST_PAGES: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Patch,
    Delete,
}

impl Method {
    fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Post => "POST",
            Method::Patch => "PATCH",
            Method::Delete => "DELETE",
        }
    }

    fn reqwest(self) -> reqwest::Method {
        match self {
            Method::Get => reqwest::Method::GET,
            Method::Post => reqwest::Method::POST,
            Method::Patch => reqwest::Method::PATCH,
            Method::Delete => reqwest::Method::DELETE,
        }
    }
}

/// One page of a listing.
#[derive(Debug, Clone, Deserialize)]
pub struct Page<T> {
    #[serde(default = "Vec::new")]
    pub value: Vec<T>,
    #[serde(rename = "@odata.nextLink", default)]
    pub next_link: Option<String>,
}

/// One page of a delta round: a next link while pages follow, and on the
/// last page the delta link the next round starts from.
#[derive(Debug, Clone, Deserialize)]
pub struct DeltaPage<T> {
    #[serde(default = "Vec::new")]
    pub value: Vec<T>,
    #[serde(rename = "@odata.nextLink", default)]
    pub next_link: Option<String>,
    #[serde(rename = "@odata.deltaLink", default)]
    pub delta_link: Option<String>,
}

/// One call inside a `$batch`, by its path under the base.
#[derive(Debug, Clone)]
pub struct BatchRequest {
    pub id: String,
    pub method: Method,
    pub url: String,
    pub body: Option<Value>,
    pub headers: Vec<(String, String)>,
}

impl BatchRequest {
    pub fn new(method: Method, url: String, body: Option<Value>) -> BatchRequest {
        BatchRequest {
            id: String::new(),
            method,
            url,
            body,
            headers: Vec::new(),
        }
    }

    pub fn get(url: String) -> BatchRequest {
        BatchRequest::new(Method::Get, url, None)
    }

    pub fn header(mut self, name: &str, value: &str) -> BatchRequest {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }
}

/// One answer inside a `$batch`, in the order its request was given.
#[derive(Debug, Clone)]
pub struct BatchResponse {
    pub id: String,
    pub status: u16,
    pub body: Value,
    pub retry_after: Option<Duration>,
}

impl BatchResponse {
    /// The body as `T`, `None` for an answer with no body, or the error
    /// this entry's status and code stand for.
    pub fn into_json<T: DeserializeOwned>(self) -> Result<Option<T>, GraphError> {
        if (200..300).contains(&self.status) {
            if self.body.is_null() {
                return Ok(None);
            }
            return serde_json::from_value(self.body)
                .map(Some)
                .map_err(|e| GraphError::Decode(e.to_string()));
        }
        if matches!(self.status, 429 | 503) {
            return Err(GraphError::Throttled {
                retry_after: self.retry_after,
            });
        }
        let code = self.body["error"]["code"].as_str().unwrap_or_default();
        let message = self.body["error"]["message"].as_str().unwrap_or_default();
        Err(classify(self.status, code, message))
    }
}

/// A call as it goes out, kept whole so a retry can send it again.
struct Call {
    method: Method,
    url: Url,
    body: Option<Body>,
    headers: Vec<(String, String)>,
}

enum Body {
    Json(Value),
    Text(String),
}

/// The Graph client for one signed-in account. Clones share the session.
#[derive(Clone)]
pub struct Graph {
    http: reqwest::Client,
    base: Url,
    session: Arc<Session>,
}

impl std::fmt::Debug for Graph {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Graph")
            .field("base", &self.base.as_str())
            .finish_non_exhaustive()
    }
}

impl Graph {
    pub fn new(session: Arc<Session>) -> Graph {
        // GRAPH_BASE is a constant that parses.
        Graph::with_base(session, GRAPH_BASE).expect("GRAPH_BASE is a URL")
    }

    /// A client for another base, which tests point at wiremock. The base
    /// ends in `/`, so paths join under it.
    pub fn with_base(session: Arc<Session>, base: &str) -> Result<Graph, GraphError> {
        let base =
            Url::parse(base).map_err(|e| GraphError::OAuth(format!("bad base {base}: {e}")))?;
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|e| GraphError::Network(e.to_string()))?;
        Ok(Graph {
            http,
            base,
            session,
        })
    }

    /// The scopes the account's token carries, once a token was issued.
    pub fn granted(&self) -> Option<Granted> {
        self.session.granted()
    }

    fn url(&self, path: &str, query: &[(&str, &str)]) -> Result<Url, GraphError> {
        let mut url = self
            .base
            .join(path)
            .map_err(|e| GraphError::Decode(format!("bad path {path}: {e}")))?;
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query);
        }
        Ok(url)
    }

    /// Refuses a URL on any scheme, host or port but the base's.
    fn check_origin(&self, url: &Url) -> Result<(), GraphError> {
        let same = url.scheme() == self.base.scheme()
            && url.host_str() == self.base.host_str()
            && url.port_or_known_default() == self.base.port_or_known_default();
        match same {
            true => Ok(()),
            false => Err(GraphError::OffHost(
                url.host_str().unwrap_or_default().to_string(),
            )),
        }
    }

    /// Microsoft's upload sessions answer on the Outlook web host, with the
    /// right to write already in the URL. The token never goes there, and
    /// nothing goes anywhere else.
    fn upload_host(&self, url: &Url) -> bool {
        let microsoft = url.scheme() == "https"
            && matches!(
                url.host_str(),
                Some("outlook.office.com" | "outlook.office365.com")
            );
        microsoft || self.check_origin(url).is_ok()
    }

    /// PUTs one piece of an upload session, with no token.
    pub async fn upload(
        &self,
        url: &str,
        offset: u64,
        total: u64,
        bytes: &[u8],
    ) -> Result<bool, GraphError> {
        let parsed = Url::parse(url).map_err(|e| GraphError::Decode(e.to_string()))?;
        if !self.upload_host(&parsed) {
            return Err(GraphError::OffHost(
                parsed.host_str().unwrap_or_default().to_string(),
            ));
        }
        let end = offset + bytes.len() as u64 - 1;
        let response = self
            .http
            .put(parsed)
            .header("Content-Range", format!("bytes {offset}-{end}/{total}"))
            .body(bytes.to_vec())
            .send()
            .await
            .map_err(network)?;
        match response.status().as_u16() {
            200 | 202 => Ok(false),
            201 => Ok(true),
            _ => Err(error_of(response).await),
        }
    }

    /// `link`, a next or delta link, as a path under the base, for a
    /// `$batch` entry. Refused off the base like [`Graph::follow`].
    pub fn relative(&self, link: &str) -> Result<String, GraphError> {
        let url = Url::parse(link).map_err(|e| GraphError::Decode(e.to_string()))?;
        self.check_origin(&url)?;
        let full = url.as_str();
        full.strip_prefix(self.base.as_str())
            .map(str::to_string)
            .ok_or_else(|| GraphError::OffHost(url.host_str().unwrap_or_default().to_string()))
    }

    pub async fn get<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<T, GraphError> {
        self.get_with(path, query, &[]).await
    }

    /// Every page of the listing at `path`, following next links up to
    /// [`MOST_LIST_PAGES`]. A listing longer than that is refused rather
    /// than cut short, since a caller may take an entry it did not see
    /// for one that is gone.
    pub async fn get_all<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<Vec<T>, GraphError> {
        let mut page: Page<T> = self.get(path, query).await?;
        let mut all = std::mem::take(&mut page.value);
        for _ in 1..MOST_LIST_PAGES {
            let Some(next) = page.next_link.take() else {
                return Ok(all);
            };
            page = self.follow(&next, &[]).await?;
            all.append(&mut page.value);
        }
        match page.next_link {
            None => Ok(all),
            Some(_) => Err(GraphError::Decode(format!("{path} ran past {MOST_LIST_PAGES} pages"))),
        }
    }

    /// A GET with more `Prefer` values, such as `odata.maxpagesize=50`.
    pub async fn get_with<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, &str)],
        prefer: &[&str],
    ) -> Result<T, GraphError> {
        let url = self.url(path, query)?;
        let response = self.execute(Call::get(url, prefer)).await?;
        json_of(response).await
    }

    /// The bytes at `path`, refused once they pass `limit`.
    pub async fn get_bytes(&self, path: &str, limit: usize) -> Result<Vec<u8>, GraphError> {
        let url = self.url(path, &[])?;
        let response = self.execute(Call::get(url, &[])).await?;
        read_capped(response, limit).await
    }

    /// A next or delta link Graph handed back, followed only on the base's
    /// own scheme, host and port.
    pub async fn follow<T: DeserializeOwned>(
        &self,
        link: &str,
        prefer: &[&str],
    ) -> Result<T, GraphError> {
        let url = Url::parse(link).map_err(|e| GraphError::Decode(e.to_string()))?;
        self.check_origin(&url)?;
        let response = self.execute(Call::get(url, prefer)).await?;
        json_of(response).await
    }

    /// A call that may change something. `None` for an answer with no
    /// body (202, 204).
    pub async fn send<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
        headers: &[(&str, &str)],
    ) -> Result<Option<T>, GraphError> {
        let url = self.url(path, &[])?;
        let call = Call {
            method,
            url,
            body: body.cloned().map(Body::Json),
            headers: headers
                .iter()
                .map(|(n, v)| (n.to_string(), v.to_string()))
                .collect(),
        };
        let response = self.execute(call).await?;
        if matches!(response.status().as_u16(), 202 | 204) {
            return Ok(None);
        }
        json_of(response).await.map(Some)
    }

    /// Posts a MIME message, base64 in a `text/plain` body, as Graph's
    /// `sendMail` and draft creation take one.
    pub async fn post_mime(&self, path: &str, raw: &[u8]) -> Result<Option<Value>, GraphError> {
        use base64::Engine;
        let url = self.url(path, &[])?;
        let call = Call {
            method: Method::Post,
            url,
            body: Some(Body::Text(
                base64::engine::general_purpose::STANDARD.encode(raw),
            )),
            headers: vec![("Content-Type".into(), "text/plain".into())],
        };
        let response = self.execute(call).await?;
        if matches!(response.status().as_u16(), 202 | 204) {
            return Ok(None);
        }
        json_of(response).await.map(Some)
    }

    /// `requests` in `$batch` calls of at most [`BATCH_LIMIT`], answered in
    /// the order given. Entries Graph throttled go again once when every
    /// wait is short; otherwise each keeps its own `Throttled`.
    pub async fn batch(&self, requests: &[BatchRequest]) -> Result<Vec<BatchResponse>, GraphError> {
        let mut answers = Vec::with_capacity(requests.len());
        for chunk in requests.chunks(BATCH_LIMIT) {
            let mut got = self.batch_once(chunk).await?;
            let throttled: Vec<usize> = got
                .iter()
                .enumerate()
                .filter(|(_, a)| matches!(a.status, 429 | 503))
                .map(|(i, _)| i)
                .collect();
            let wait = throttled
                .iter()
                .filter_map(|&i| got[i].retry_after)
                .max()
                .unwrap_or_default();
            if !throttled.is_empty() && wait <= MAX_INLINE_WAIT {
                tokio::time::sleep(wait).await;
                let again: Vec<BatchRequest> =
                    throttled.iter().map(|&i| chunk[i].clone()).collect();
                for (slot, answer) in throttled.into_iter().zip(self.batch_once(&again).await?) {
                    got[slot] = answer;
                }
            }
            answers.extend(got);
        }
        Ok(answers)
    }

    async fn batch_once(&self, chunk: &[BatchRequest]) -> Result<Vec<BatchResponse>, GraphError> {
        let entries: Vec<Value> = chunk
            .iter()
            .enumerate()
            .map(|(i, r)| {
                // Graph applies none of the outer request's headers to the
                // entries in a batch, so each entry asks for immutable ids
                // itself, ahead of any preference it brings.
                let mut prefer = vec![IMMUTABLE_IDS.to_string()];
                let mut headers = serde_json::Map::new();
                for (name, value) in &r.headers {
                    if name.eq_ignore_ascii_case("Prefer") {
                        prefer.push(value.clone());
                    } else {
                        headers.insert(name.clone(), Value::String(value.clone()));
                    }
                }
                headers.insert("Prefer".into(), Value::String(prefer.join(", ")));
                if r.body.is_some() && !headers.contains_key("Content-Type") {
                    headers.insert(
                        "Content-Type".into(),
                        Value::String("application/json".into()),
                    );
                }
                let mut entry = json!({
                    "id": (i + 1).to_string(),
                    "method": r.method.as_str(),
                    "url": format!("/{}", r.url.trim_start_matches('/')),
                });
                if !headers.is_empty() {
                    entry["headers"] = Value::Object(headers);
                }
                if let Some(body) = &r.body {
                    entry["body"] = body.clone();
                }
                entry
            })
            .collect();
        let body = json!({ "requests": entries });
        let answer: Option<Value> = self.send(Method::Post, "$batch", Some(&body), &[]).await?;
        let mut by_id: Vec<(usize, BatchResponse)> = answer
            .and_then(|a| a.get("responses").cloned())
            .and_then(|r| r.as_array().cloned())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|entry| {
                let id = entry["id"].as_str()?.to_string();
                let index: usize = id.parse().ok()?;
                let retry_after = entry["headers"]["Retry-After"]
                    .as_str()
                    .and_then(|s| s.trim().parse::<u64>().ok())
                    .map(Duration::from_secs);
                Some((
                    index,
                    BatchResponse {
                        id,
                        status: u16::try_from(entry["status"].as_u64()?).ok()?,
                        body: entry.get("body").cloned().unwrap_or(Value::Null),
                        retry_after,
                    },
                ))
            })
            .collect();
        by_id.sort_by_key(|(index, _)| *index);
        if by_id.len() != chunk.len() {
            return Err(GraphError::Decode(format!(
                "a batch of {} came back with {} answers",
                chunk.len(),
                by_id.len()
            )));
        }
        Ok(by_id.into_iter().map(|(_, answer)| answer).collect())
    }

    /// Sends `call`, refreshing a refused token once and waiting out a
    /// short throttle once.
    async fn execute(&self, call: Call) -> Result<reqwest::Response, GraphError> {
        self.check_origin(&call.url)?;
        let (mut refreshed, mut waited) = (false, false);
        loop {
            let token = self.session.bearer(refreshed).await?;
            let mut request = self
                .http
                .request(call.method.reqwest(), call.url.clone())
                .bearer_auth(&token)
                .header("Prefer", IMMUTABLE_IDS);
            for (name, value) in &call.headers {
                request = request.header(name.as_str(), value.as_str());
            }
            request = match &call.body {
                Some(Body::Json(value)) => request.json(value),
                Some(Body::Text(text)) => request.body(text.clone()),
                None => request,
            };
            let response = request.send().await.map_err(network)?;
            let status = response.status().as_u16();
            match status {
                200..=299 => return Ok(response),
                401 if !refreshed => refreshed = true,
                429 | 503 => {
                    let wait = retry_after(&response);
                    if waited || wait.is_none_or(|w| w > MAX_INLINE_WAIT) {
                        return Err(GraphError::Throttled { retry_after: wait });
                    }
                    waited = true;
                    tokio::time::sleep(wait.unwrap_or_default()).await;
                }
                _ => return Err(error_of(response).await),
            }
        }
    }
}

impl Call {
    fn get(url: Url, prefer: &[&str]) -> Call {
        Call {
            method: Method::Get,
            url,
            body: None,
            headers: prefer
                .iter()
                .map(|p| ("Prefer".to_string(), p.to_string()))
                .collect(),
        }
    }
}

fn network(err: reqwest::Error) -> GraphError {
    GraphError::Network(err.to_string())
}

fn retry_after(response: &reqwest::Response) -> Option<Duration> {
    response
        .headers()
        .get("Retry-After")?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}

async fn error_of(response: reqwest::Response) -> GraphError {
    let status = response.status().as_u16();
    let body = read_capped(response, ERROR_LIMIT).await.unwrap_or_default();
    let parsed: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let code = parsed["error"]["code"].as_str().unwrap_or_default();
    let message = parsed["error"]["message"].as_str().unwrap_or_default();
    classify(status, code, message)
}

async fn json_of<T: DeserializeOwned>(response: reqwest::Response) -> Result<T, GraphError> {
    let bytes = read_capped(response, JSON_LIMIT).await?;
    serde_json::from_slice(&bytes).map_err(|e| GraphError::Decode(e.to_string()))
}

/// The body, read a chunk at a time and refused once it passes `limit`,
/// so a large answer never sits whole in memory before it is refused.
async fn read_capped(mut response: reqwest::Response, limit: usize) -> Result<Vec<u8>, GraphError> {
    if response.content_length().is_some_and(|n| n > limit as u64) {
        return Err(GraphError::TooLarge { limit });
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(network)? {
        if bytes.len() + chunk.len() > limit {
            return Err(GraphError::TooLarge { limit });
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
