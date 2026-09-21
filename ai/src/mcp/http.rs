//! The Streamable HTTP transport: each message is a POST to one URL, and the
//! reply is a JSON body or an SSE stream ending in the response.

use std::sync::Mutex;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use base64::Engine;
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::Failure;
use crate::sse::SseReader;

const SESSION: &str = "mcp-session-id";
const VERSION: &str = "mcp-protocol-version";

/// What a POST mirrors into headers besides the version.
#[derive(Default)]
pub(super) struct Mirrored {
    /// The tool, for `Mcp-Name`.
    pub name: Option<String>,
    /// `Mcp-Param-{name}` headers from the tool's `x-mcp-header` marks.
    pub params: Vec<(String, String)>,
}

pub(super) struct Http {
    client: reqwest::Client,
    url: String,
    token: Option<String>,
    /// The version header. A handshake-era `initialize` goes without one.
    version: Mutex<Option<String>>,
    /// Only servers of the handshake era hand one out.
    session: Mutex<Option<String>>,
    next_id: AtomicI64,
    notifications: mpsc::UnboundedSender<Value>,
}

impl Http {
    pub(super) fn new(
        url: &str,
        token: Option<String>,
        notifications: mpsc::UnboundedSender<Value>,
    ) -> Result<Http, Failure> {
        let parsed = reqwest::Url::parse(url.trim())
            .map_err(|e| Failure::Other(format!("{url} is not a URL: {e}")))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(Failure::Other(format!("{url} is not an http or https URL")));
        }
        Ok(Http {
            client: reqwest::Client::new(),
            url: parsed.to_string(),
            token: token.filter(|t| !t.trim().is_empty()),
            version: Mutex::new(None),
            session: Mutex::new(None),
            next_id: AtomicI64::new(1),
            notifications,
        })
    }

    pub(super) fn set_version(&self, version: Option<&str>) {
        *self
            .version
            .lock()
            .expect("the version lock is never poisoned") = version.map(String::from);
    }

    pub(super) fn session(&self) -> Option<String> {
        self.session
            .lock()
            .expect("the session lock is never poisoned")
            .clone()
    }

    fn headers(&self, method: Option<&str>, mirrored: &Mirrored) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/json, text/event-stream"),
        );
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        if let Some(token) = &self.token
            && let Ok(value) = HeaderValue::from_str(&format!("Bearer {}", token.trim()))
        {
            headers.insert(AUTHORIZATION, value);
        }
        let version = self
            .version
            .lock()
            .expect("the version lock is never poisoned")
            .clone();
        let mut put = |name: &str, value: &str| {
            if let (Ok(name), Ok(value)) = (
                reqwest::header::HeaderName::from_bytes(name.as_bytes()),
                HeaderValue::from_str(value),
            ) {
                headers.insert(name, value);
            }
        };
        if let Some(version) = &version {
            put(VERSION, version);
        }
        if let Some(session) = self.session() {
            put(SESSION, &session);
        }
        if let Some(method) = method {
            put("mcp-method", method);
        }
        if let Some(name) = &mirrored.name {
            put("mcp-name", &header_value(name));
        }
        for (name, value) in &mirrored.params {
            put(&format!("mcp-param-{name}"), &header_value(value));
        }
        headers
    }

    pub(super) async fn request(
        &self,
        method: &str,
        params: Value,
        mirrored: Mirrored,
        limit: Duration,
    ) -> Result<Value, Failure> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let body = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let exchange = async {
            let response = self.post(Some(method), &body, &mirrored).await?;
            self.read_reply(response, id).await
        };
        tokio::time::timeout(limit, exchange)
            .await
            .map_err(|_| Failure::Timeout)?
    }

    pub(super) async fn notify(&self, method: &str, params: Value) -> Result<(), Failure> {
        let body = json!({"jsonrpc": "2.0", "method": method, "params": params});
        let response = self.post(Some(method), &body, &Mirrored::default()).await?;
        match response.status().as_u16() {
            200..=299 => Ok(()),
            status => Err(Failure::Status {
                status,
                body: response.text().await.unwrap_or_default(),
            }),
        }
    }

    /// Sends one message and checks the status, keeping any session id the
    /// server hands out.
    async fn post(
        &self,
        method: Option<&str>,
        body: &Value,
        mirrored: &Mirrored,
    ) -> Result<reqwest::Response, Failure> {
        let response = self
            .client
            .post(&self.url)
            .headers(self.headers(method, mirrored))
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| Failure::Other(format!("could not reach {}: {e}", self.url)))?;
        if let Some(session) = response
            .headers()
            .get(SESSION)
            .and_then(|v| v.to_str().ok())
        {
            *self
                .session
                .lock()
                .expect("the session lock is never poisoned") = Some(session.to_string());
        }
        let status = response.status().as_u16();
        if (200..300).contains(&status) {
            return Ok(response);
        }
        let text = response.text().await.unwrap_or_default();
        // A modern server explains a refusal in a JSON-RPC error, which
        // is how the client tells it from a server of the handshake era.
        if let Ok(message) = serde_json::from_str::<Value>(&text)
            && let Some(error) = message.get("error")
        {
            return Err(Failure::rpc(error));
        }
        Err(Failure::Status { status, body: text })
    }

    async fn read_reply(&self, response: reqwest::Response, id: i64) -> Result<Value, Failure> {
        let streamed = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("text/event-stream"));
        if !streamed {
            let text = response
                .text()
                .await
                .map_err(|e| Failure::Other(e.to_string()))?;
            let message: Value = serde_json::from_str(&text)
                .map_err(|e| Failure::Other(format!("the server sent something not JSON: {e}")))?;
            // Servers of 2025-03-26 could answer with a batch.
            let messages = match message {
                Value::Array(messages) => messages,
                message => vec![message],
            };
            for message in messages {
                if let Some(found) = self.take(message, id).await {
                    return super::response(found);
                }
            }
            return Err(Failure::Other("the server sent no response".into()));
        }
        let mut events = SseReader::new(response);
        loop {
            match events.next().await {
                Ok(Some(event)) => {
                    let Ok(message) = serde_json::from_str::<Value>(&event.data) else {
                        continue;
                    };
                    if let Some(found) = self.take(message, id).await {
                        return super::response(found);
                    }
                }
                Ok(None) => {
                    return Err(Failure::Closed(
                        "the server closed the stream before answering".into(),
                    ));
                }
                Err(e) => return Err(Failure::Other(e.to_string())),
            }
        }
    }

    /// Hands back the message when it answers `id`; passes notifications
    /// on and answers the server's own requests.
    async fn take(&self, message: Value, id: i64) -> Option<Value> {
        let method = message.get("method").and_then(Value::as_str);
        let its_id = message.get("id").filter(|v| !v.is_null()).cloned();
        match (its_id, method) {
            (Some(its_id), None) => (its_id.as_i64() == Some(id)).then_some(message),
            (Some(its_id), Some(method)) => {
                let reply = super::answer_server_request(its_id, method);
                if let Err(failure) = self.post(None, &reply, &Mirrored::default()).await {
                    tracing::debug!(?failure, "could not answer the server's request");
                }
                None
            }
            (None, Some(_)) => {
                let _ = self.notifications.send(message);
                None
            }
            (None, None) => None,
        }
    }

    /// Opens a stream the server keeps open for change notifications and
    /// reads it until it ends. `listen` holds the modern request that asks
    /// for one; without it, this is the handshake era's GET stream.
    pub(super) async fn follow(&self, listen: Option<Value>) -> Result<(), Failure> {
        let response = match listen {
            Some(params) => {
                let id = self.next_id.fetch_add(1, Ordering::SeqCst);
                let body = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "method": "subscriptions/listen",
                    "params": params,
                });
                self.post(Some("subscriptions/listen"), &body, &Mirrored::default())
                    .await?
            }
            None => {
                let mut headers = self.headers(None, &Mirrored::default());
                headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
                headers.remove(CONTENT_TYPE);
                let response = self
                    .client
                    .get(&self.url)
                    .headers(headers)
                    .send()
                    .await
                    .map_err(|e| Failure::Other(e.to_string()))?;
                if !response.status().is_success() {
                    return Err(Failure::Status {
                        status: response.status().as_u16(),
                        body: String::new(),
                    });
                }
                response
            }
        };
        let mut events = SseReader::new(response);
        while let Ok(Some(event)) = events.next().await {
            if let Ok(message) = serde_json::from_str::<Value>(&event.data)
                && message.get("method").is_some()
                && message.get("id").is_none_or(Value::is_null)
            {
                let _ = self.notifications.send(message);
            }
        }
        Ok(())
    }

    /// Ends a handshake-era session, as that revision asks a client to.
    pub(super) async fn close(&self) {
        let Some(session) = self.session() else {
            return;
        };
        let mut request = self.client.delete(&self.url).header(SESSION, session);
        if let Some(token) = &self.token {
            request = request.bearer_auth(token.trim());
        }
        let _ = tokio::time::timeout(Duration::from_secs(5), request.send()).await;
    }
}

/// A header value as the spec writes one: plain when it is visible ASCII
/// with no space at either end, and Base64 between sentinels otherwise.
pub(crate) fn header_value(value: &str) -> String {
    let plain = value
        .bytes()
        .all(|b| b == b' ' || b == b'\t' || (0x21..=0x7e).contains(&b))
        && value.trim() == value
        && !(value.starts_with("=?base64?") && value.ends_with("?="));
    if plain {
        value.to_string()
    } else {
        format!(
            "=?base64?{}?=",
            base64::engine::general_purpose::STANDARD.encode(value.as_bytes())
        )
    }
}
