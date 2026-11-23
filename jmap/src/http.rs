//! The account's one HTTP client. The session comes from the URL the
//! account was saved with, and every other call goes to a URL that
//! session names. Credentials go nowhere else: an authenticated call
//! follows no redirect, and the session fetch follows one only to the
//! same scheme, host and port. Plain HTTP is taken only on this computer.
//! Every body is read a chunk at a time under a cap.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use reqwest::StatusCode;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderValue, LOCATION, RETRY_AFTER};
use serde_json::Value;
use tokio::sync::{Semaphore, SemaphorePermit};
use url::Url;

use crate::error::problem;
use crate::request::{Request, Response};
use crate::session::{MOST_AT_ONCE, Session};
use crate::{Auth, AuthKind, JmapError};

/// The most one JSON answer may take: a page of a few hundred messages'
/// metadata is far below it.
pub const JSON_LIMIT: usize = 16 << 20;

/// What an error body may take.
const ERROR_LIMIT: usize = 64 << 10;

/// How long one call may take before the network counts as gone. Blob
/// transfers set their own.
const CALL_TIMEOUT: Duration = Duration::from_secs(60);

/// How long a call may wait for the head of its answer while a push
/// stream is open before the stream counts as what holds it up. Cyrus
/// answers no other request of the account while a stream is open.
const STALL_LIMIT: Duration = Duration::from_secs(20);

/// The waits push judges a server by. [`JmapClient::new`] uses the real
/// ones; tests that play a server which never answers pass shorter ones
/// to [`JmapClient::with_push_limits`], rather than wait them out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PushLimits {
    /// See `STALL_LIMIT`.
    pub stall: Duration,
    /// How long the echo sent beside a new stream may take before the
    /// server counts as one that holds calls while a stream is open.
    pub probe: Duration,
}

impl Default for PushLimits {
    fn default() -> Self {
        PushLimits {
            stall: STALL_LIMIT,
            probe: Duration::from_secs(10),
        }
    }
}

/// How long push stays off after an API call stalled behind the stream.
/// The account polls meanwhile, and the next stream is probed again.
const PUSH_PAUSE: Duration = Duration::from_secs(30 * 60);

/// How many same-host redirects the session fetch follows.
const MOST_REDIRECTS: usize = 5;

#[derive(Clone)]
pub struct JmapClient {
    inner: Arc<Inner>,
}

struct Inner {
    http: reqwest::Client,
    auth: Auth,
    session_url: Url,
    held: Mutex<Held>,
    /// Held while the session is read, so callers that find it missing or
    /// stale wait for one read instead of each starting their own.
    reading: tokio::sync::Mutex<()>,
    permits: Semaphore,
    /// Set once the permits shrank to the session's own limit.
    sized: AtomicBool,
    /// The open push stream. It takes no permit: it lives for hours.
    events: tokio::sync::Mutex<Option<crate::push::EventStream>>,
    /// Set while a push stream is open.
    streaming: AtomicBool,
    /// Set from a stream's opening until its probe passes. A stall in
    /// that time means the server holds calls behind any stream.
    probing: AtomicBool,
    /// Set once the probe beside a new stream went unanswered; the
    /// account then polls for the client's life.
    push_off: AtomicBool,
    /// Until when push stays off after a call stalled behind the stream.
    paused_until: Mutex<Option<tokio::time::Instant>>,
    /// The task that holds the open stream's connection.
    reader: Mutex<Option<tokio::task::AbortHandle>>,
    limits: PushLimits,
}

#[derive(Default)]
struct Held {
    session: Option<Arc<Session>>,
    stale: bool,
}

impl std::fmt::Debug for JmapClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JmapClient")
            .field("session_url", &self.inner.session_url.as_str())
            .finish_non_exhaustive()
    }
}

/// Whether `url` names this computer.
fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

fn same_origin(a: &Url, b: &Url) -> bool {
    a.scheme() == b.scheme()
        && a.host_str() == b.host_str()
        && a.port_or_known_default() == b.port_or_known_default()
}

/// reqwest's message names the URL, and a download URL can carry the
/// account's address, so the URL stays out of the error.
pub(crate) fn network(err: reqwest::Error) -> JmapError {
    JmapError::Network(err.without_url().to_string())
}

/// `url` when it carries no user name or password. Credentials in a URL
/// would go out as a second `Authorization` header and show in the log,
/// so the refusal names the URL without them.
fn without_userinfo(mut url: Url) -> Result<Url, JmapError> {
    if url.username().is_empty() && url.password().is_none() {
        return Ok(url);
    }
    // Both setters fail only on a URL that cannot have a host, and a URL
    // with a user name has one.
    let _ = url.set_username("");
    let _ = url.set_password(None);
    Err(JmapError::BadUrl(url.to_string()))
}

impl JmapClient {
    /// A client for the session at `session_url`, which must be HTTPS, or
    /// HTTP on this computer. Nothing goes out until the first call.
    pub fn new(session_url: &str, auth: Auth) -> Result<JmapClient, JmapError> {
        JmapClient::with_push_limits(session_url, auth, PushLimits::default())
    }

    /// As [`JmapClient::new`], judging push by `limits`.
    pub fn with_push_limits(
        session_url: &str,
        auth: Auth,
        limits: PushLimits,
    ) -> Result<JmapClient, JmapError> {
        // A URL that does not parse could still hold a password or an
        // address, so the error names which URL it was and none of it.
        let url =
            Url::parse(session_url).map_err(|_| JmapError::BadUrl("the session URL".into()))?;
        let url = without_userinfo(url)?;
        let usable = url.scheme() == "https" || (url.scheme() == "http" && is_loopback(&url));
        if !usable || url.host().is_none() {
            return Err(JmapError::BadUrl(session_url.to_string()));
        }
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .referer(false)
            .user_agent(concat!("Penguin Mail/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(20))
            .build()
            .map_err(network)?;
        Ok(JmapClient {
            inner: Arc::new(Inner {
                http,
                auth,
                session_url: url,
                held: Mutex::new(Held::default()),
                reading: tokio::sync::Mutex::new(()),
                permits: Semaphore::new(MOST_AT_ONCE),
                sized: AtomicBool::new(false),
                events: tokio::sync::Mutex::new(None),
                streaming: AtomicBool::new(false),
                probing: AtomicBool::new(false),
                push_off: AtomicBool::new(false),
                paused_until: Mutex::new(None),
                reader: Mutex::new(None),
                limits,
            }),
        })
    }

    pub(crate) fn push_limits(&self) -> PushLimits {
        self.inner.limits
    }

    pub fn session_url(&self) -> &str {
        self.inner.session_url.as_str()
    }

    pub fn auth_kind(&self) -> AuthKind {
        self.inner.auth.kind()
    }

    fn held(&self) -> std::sync::MutexGuard<'_, Held> {
        self.inner
            .held
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn cached(&self) -> Option<Arc<Session>> {
        let held = self.held();
        held.session
            .as_ref()
            .filter(|_| !held.stale)
            .map(Arc::clone)
    }

    /// The session, fetched the first time and again after a call said it
    /// moved on. Callers that arrive during a read wait for its result.
    pub async fn session(&self) -> Result<Arc<Session>, JmapError> {
        if let Some(session) = self.cached() {
            return Ok(session);
        }
        let _reading = self.inner.reading.lock().await;
        if let Some(session) = self.cached() {
            return Ok(session);
        }
        self.read_and_keep().await
    }

    /// The session, fetched now.
    pub async fn refresh_session(&self) -> Result<Arc<Session>, JmapError> {
        let _reading = self.inner.reading.lock().await;
        self.read_and_keep().await
    }

    /// Reads the session under a permit and keeps it. The caller holds
    /// `reading`.
    async fn read_and_keep(&self) -> Result<Arc<Session>, JmapError> {
        let session = {
            let _permit = self.permit().await;
            Arc::new(self.read_session().await?)
        };
        // The permit is back by now, and no call holds one before the
        // first session exists, so every permit is free to forget.
        self.size_permits(&session);
        let mut held = self.held();
        held.session = Some(Arc::clone(&session));
        held.stale = false;
        Ok(session)
    }

    /// The session document, following redirects on the same origin, with
    /// every URL it names checked.
    async fn read_session(&self) -> Result<Session, JmapError> {
        let mut url = self.inner.session_url.clone();
        for _ in 0..=MOST_REDIRECTS {
            let response = self
                .with_credentials(|header| {
                    let request = self
                        .inner
                        .http
                        .get(url.clone())
                        .header(AUTHORIZATION, header)
                        .timeout(CALL_TIMEOUT);
                    async move { request.send().await.map_err(network) }
                })
                .await?;
            match response.status().as_u16() {
                200..=299 => {
                    let bytes = read_capped(response, JSON_LIMIT).await?;
                    let session = Session::parse(&bytes)?;
                    for named in [&session.api_url, &session.upload_url, &session.download_url] {
                        self.endpoint(named)?;
                    }
                    if let Some(events) = &session.event_source_url {
                        self.endpoint(events)?;
                    }
                    return Ok(session);
                }
                301 | 302 | 303 | 307 | 308 => {
                    let next = response
                        .headers()
                        .get(LOCATION)
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| url.join(v).ok())
                        .ok_or_else(|| JmapError::Decode("a redirect without a location".into()))?;
                    if !same_origin(&next, &url) {
                        return Err(JmapError::OffHost(
                            next.host_str().unwrap_or_default().to_string(),
                        ));
                    }
                    url = without_userinfo(next)?;
                }
                _ => return Err(failure(response).await),
            }
        }
        Err(JmapError::Problem {
            status: 310,
            kind: "redirects".into(),
            detail: "too many redirects".into(),
        })
    }

    /// Shrinks the permits to the session's `maxConcurrentRequests` the
    /// first time a session says it, when that is under the client's own.
    fn size_permits(&self, session: &Session) {
        if self.inner.sized.swap(true, Ordering::Relaxed) {
            return;
        }
        let allowed = session.limits().max_concurrent_requests.max(1);
        if allowed < MOST_AT_ONCE {
            self.inner.permits.forget_permits(MOST_AT_ONCE - allowed);
        }
    }

    /// `url`, a URL the session named, when the credentials may go there:
    /// HTTPS, or HTTP when the session itself is on this computer.
    pub(crate) fn endpoint(&self, url: &str) -> Result<Url, JmapError> {
        let parsed =
            Url::parse(url).map_err(|_| JmapError::BadUrl("a URL the session names".into()))?;
        let parsed = without_userinfo(parsed)?;
        let session_is_local = self.inner.session_url.scheme() == "http";
        let fine = match parsed.scheme() {
            "https" => true,
            "http" => session_is_local && is_loopback(&parsed),
            _ => false,
        };
        match fine && parsed.host().is_some() {
            true => Ok(parsed),
            false => Err(JmapError::BadUrl(url.to_string())),
        }
    }

    /// A request with the account's credentials, and the header it
    /// carries. With `refused`, the header a server just answered 401 to,
    /// an OAuth account's token is refreshed first.
    pub(crate) async fn authorized(
        &self,
        method: reqwest::Method,
        url: Url,
        refused: Option<&HeaderValue>,
    ) -> Result<(reqwest::RequestBuilder, HeaderValue), JmapError> {
        let header = self.inner.auth.header(refused).await?;
        Ok((
            self.inner
                .http
                .request(method, url)
                .header(AUTHORIZATION, header.clone()),
            header,
        ))
    }

    /// The HTTP client every call shares.
    pub(crate) fn http(&self) -> &reqwest::Client {
        &self.inner.http
    }

    /// Whether a 401 is worth one more try with a refreshed token.
    pub(crate) fn refreshes(&self) -> bool {
        self.inner.auth.kind() == AuthKind::OAuth
    }

    /// What `send` answers when it goes out with the account's credentials,
    /// sent once more with another token when an OAuth server answered
    /// 401. A 401 means the server acted on nothing, so even a write goes
    /// again safely; any other answer, or none, comes back as it is, and
    /// a second 401 too.
    pub(crate) async fn with_credentials<F, Fut>(
        &self,
        send: F,
    ) -> Result<reqwest::Response, JmapError>
    where
        F: Fn(HeaderValue) -> Fut,
        Fut: Future<Output = Result<reqwest::Response, JmapError>>,
    {
        let header = self.inner.auth.header(None).await?;
        let response = send(header.clone()).await?;
        if response.status() != StatusCode::UNAUTHORIZED
            || self.inner.auth.kind() != AuthKind::OAuth
        {
            return Ok(response);
        }
        drop(response);
        let fresh = self.inner.auth.header(Some(&header)).await?;
        send(fresh).await
    }

    pub(crate) fn events(&self) -> &tokio::sync::Mutex<Option<crate::push::EventStream>> {
        &self.inner.events
    }

    pub(crate) fn set_streaming(&self, open: bool) {
        self.inner.streaming.store(open, Ordering::SeqCst);
    }

    pub(crate) fn set_probing(&self, probing: bool) {
        self.inner.probing.store(probing, Ordering::SeqCst);
    }

    pub(crate) fn push_off(&self) -> bool {
        self.inner.push_off.load(Ordering::SeqCst)
    }

    /// When push comes back, while it is paused after a stall.
    pub fn push_paused(&self) -> Option<tokio::time::Instant> {
        let mut until = self
            .inner
            .paused_until
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if until.is_some_and(|at| at <= tokio::time::Instant::now()) {
            *until = None;
        }
        *until
    }

    /// Records the task that holds a newly opened stream.
    pub(crate) fn keep_reader(&self, reader: tokio::task::AbortHandle) {
        *self
            .inner
            .reader
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(reader);
    }

    /// Turns push off for the client's life and closes the stream.
    pub(crate) fn stop_pushing(&self) {
        self.inner.push_off.store(true, Ordering::SeqCst);
        self.close_stream();
    }

    /// Turns push off for a while and closes the stream.
    fn pause_pushing(&self) {
        *self
            .inner
            .paused_until
            .lock()
            .unwrap_or_else(PoisonError::into_inner) =
            Some(tokio::time::Instant::now() + PUSH_PAUSE);
        self.close_stream();
    }

    /// Aborts the task that holds the stream, which drops the connection
    /// whether or not anyone polls the watch that waits on it. The watch
    /// finds the stream closed when it next runs.
    fn close_stream(&self) {
        if let Some(reader) = self
            .inner
            .reader
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            reader.abort();
        }
        self.set_streaming(false);
        if let Ok(mut held) = self.inner.events.try_lock() {
            *held = None;
        }
    }

    /// Sends an API call and waits for the head of its answer. While a
    /// push stream is open, a head that takes past the stall limit drops
    /// the stream and pauses push, or turns it off for good when the
    /// stream's probe has not passed yet. The wait goes on: the call may
    /// be a write, and giving up on it could let it run later unseen. Only
    /// the head is timed, so a slow body is never taken for a stall.
    async fn sent(&self, request: reqwest::Request) -> Result<reqwest::Response, JmapError> {
        let answer = self.inner.http.execute(request);
        tokio::pin!(answer);
        if self.inner.streaming.load(Ordering::SeqCst) && !self.push_off() {
            tokio::select! {
                got = &mut answer => return got.map_err(network),
                _ = tokio::time::sleep(self.inner.limits.stall) => {
                    match self.inner.probing.load(Ordering::SeqCst) {
                        true => {
                            tracing::info!("a call stalled before the push stream's probe passed; polling instead");
                            self.stop_pushing();
                        }
                        false => {
                            tracing::warn!("a call stalled while the push stream was open; pausing push");
                            self.pause_pushing();
                        }
                    }
                }
            }
        }
        answer.await.map_err(network)
    }

    /// One of the account's slots for a request in flight.
    pub(crate) async fn permit(&self) -> SemaphorePermit<'_> {
        // The semaphore is never closed, so acquiring cannot fail.
        self.inner
            .permits
            .acquire()
            .await
            .expect("the permits stay open")
    }

    /// Sends `request` to the session's `apiUrl`. A capability the session
    /// does not offer, a call count or a size past its limits, is refused
    /// before anything goes out.
    pub async fn send(&self, request: &Request) -> Result<Response, JmapError> {
        let session = self.session().await?;
        if let Some(missing) = request
            .using()
            .find(|c| !session.capabilities.contains_key(*c))
        {
            return Err(JmapError::UnknownCapability(missing.to_string()));
        }
        let limits = session.limits();
        if request.len() > limits.max_calls_in_request {
            return Err(JmapError::RequestTooLarge);
        }
        let body =
            serde_json::to_vec(&request.body()).map_err(|e| JmapError::Decode(e.to_string()))?;
        if body.len() as u64 > limits.max_size_request {
            return Err(JmapError::RequestTooLarge);
        }
        let url = self.endpoint(&session.api_url)?;
        let _permit = self.permit().await;
        let request = self
            .inner
            .http
            .post(url)
            .header(CONTENT_TYPE, "application/json")
            .timeout(CALL_TIMEOUT)
            .body(body)
            .build()
            .map_err(network)?;
        let response = self
            .with_credentials(|header| {
                // The body sits in memory, so it always clones, and the
                // copy shares its buffer.
                let copy = request
                    .try_clone()
                    .ok_or_else(|| JmapError::Network("the request cannot be sent again".into()));
                async move {
                    let mut copy = copy?;
                    copy.headers_mut().insert(AUTHORIZATION, header);
                    self.sent(copy).await
                }
            })
            .await?;
        if !response.status().is_success() {
            return Err(failure(response).await);
        }
        let bytes = read_capped(response, JSON_LIMIT).await?;
        let answer = Response::parse(&bytes)?;
        if !answer.session_state.is_empty() && answer.session_state != session.state {
            self.held().stale = true;
        }
        Ok(answer)
    }
}

fn retry_after(response: &reqwest::Response) -> Option<Duration> {
    wait_of(
        response.headers().get(RETRY_AFTER)?.to_str().ok()?,
        chrono::Utc::now(),
    )
}

/// The longest wait a `Retry-After` gets: a server's clock or a typo should
/// not stop an account for a day.
const MOST_RETRY_AFTER: Duration = Duration::from_secs(30 * 60);

/// What a `Retry-After` value asks for at `now`: seconds, or an HTTP date
/// (RFC 9110 section 10.2.3), capped at `MOST_RETRY_AFTER`. A date already
/// past asks for no wait.
fn wait_of(value: &str, now: chrono::DateTime<chrono::Utc>) -> Option<Duration> {
    let value = value.trim();
    let wait = match value.parse::<u64>() {
        Ok(seconds) => Duration::from_secs(seconds),
        Err(_) => {
            let at = chrono::DateTime::parse_from_rfc2822(value).ok()?;
            (at.with_timezone(&chrono::Utc) - now)
                .to_std()
                .unwrap_or_default()
        }
    };
    Some(wait.min(MOST_RETRY_AFTER))
}

/// The error a failed answer stands for: a rate limit with its wait, or
/// the problem document's type and detail.
pub(crate) async fn failure(response: reqwest::Response) -> JmapError {
    let status = response.status().as_u16();
    if matches!(status, 429 | 503) {
        return JmapError::RateLimited {
            retry_after: retry_after(&response),
        };
    }
    let body = read_capped(response, ERROR_LIMIT).await.unwrap_or_default();
    let parsed: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let kind = parsed
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let detail = parsed
        .get("detail")
        .and_then(Value::as_str)
        .unwrap_or_default();
    problem(status, kind, detail)
}

/// The body, read a chunk at a time and refused once it passes `limit`,
/// so a large answer never sits whole in memory before it is refused.
pub(crate) async fn read_capped(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<Vec<u8>, JmapError> {
    if response.content_length().is_some_and(|n| n > limit as u64) {
        return Err(JmapError::AnswerTooLarge { limit });
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(network)? {
        if bytes.len() + chunk.len() > limit {
            return Err(JmapError::AnswerTooLarge { limit });
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    #[test]
    fn a_retry_after_reads_seconds_or_an_http_date_and_is_capped() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-12T09:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert_eq!(wait_of(" 7 ", now), Some(Duration::from_secs(7)));
        assert_eq!(
            wait_of("Mon, 12 Oct 2026 09:02:00 GMT", now),
            Some(Duration::from_secs(120))
        );
        assert_eq!(
            wait_of("Mon, 12 Oct 2026 08:00:00 GMT", now),
            Some(Duration::ZERO),
            "a date past asks for no wait"
        );
        assert_eq!(
            wait_of("Tue, 13 Oct 2026 09:00:00 GMT", now),
            Some(MOST_RETRY_AFTER)
        );
        assert_eq!(wait_of("86400", now), Some(MOST_RETRY_AFTER));
        assert_eq!(wait_of("soon", now), None);
    }

    /// A client whose session allows `most` requests at once, with the
    /// server that answers it. The caller keeps the server: once dropped,
    /// its port can go to another test's server, which answers the session
    /// read with a 404.
    async fn client_with_concurrency(most: u64) -> (MockServer, JmapClient) {
        let server = MockServer::start().await;
        let base = server.uri();
        let session = json!({
            "capabilities": { "urn:ietf:params:jmap:core": { "maxConcurrentRequests": most } },
            "apiUrl": format!("{base}/api/"),
            "downloadUrl": format!("{base}/download/"),
            "uploadUrl": format!("{base}/upload/"),
            "state": "s1"
        });
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(session))
            .mount(&server)
            .await;
        let client =
            JmapClient::new(&format!("{base}/jmap/session"), Auth::Bearer("t".into())).unwrap();
        client.session().await.unwrap();
        (server, client)
    }

    #[test]
    fn an_https_session_refuses_a_loopback_http_url() {
        let client = JmapClient::new(
            "https://mail.example.org/jmap/session",
            Auth::Bearer("t".into()),
        )
        .unwrap();
        assert!(matches!(
            client.endpoint("http://127.0.0.1/api/"),
            Err(JmapError::BadUrl(_))
        ));
        assert!(client.endpoint("https://mail.example.org/api/").is_ok());
    }

    #[tokio::test]
    async fn the_permits_shrink_to_the_sessions_limit() {
        assert_eq!(
            client_with_concurrency(2)
                .await
                .1
                .inner
                .permits
                .available_permits(),
            2
        );
    }

    #[tokio::test]
    async fn a_server_allowing_one_request_leaves_one_permit() {
        assert_eq!(
            client_with_concurrency(1)
                .await
                .1
                .inner
                .permits
                .available_permits(),
            1
        );
    }

    #[tokio::test]
    async fn the_session_read_waits_for_a_free_permit() {
        let (_server, client) = client_with_concurrency(50).await;
        let held: Vec<_> = every_permit(&client).await;
        let waited =
            tokio::time::timeout(Duration::from_millis(100), client.refresh_session()).await;
        assert!(
            waited.is_err(),
            "the session read went out with every permit taken"
        );
        drop(held);
        client.refresh_session().await.unwrap();
    }

    async fn every_permit(client: &JmapClient) -> Vec<SemaphorePermit<'_>> {
        let mut held = Vec::new();
        for _ in 0..MOST_AT_ONCE {
            held.push(client.permit().await);
        }
        held
    }

    #[tokio::test]
    async fn a_network_error_does_not_repeat_the_url() {
        let url = "http://127.0.0.1:1/download/me@example.org/b1/x";
        let err = reqwest::Client::new().get(url).send().await.unwrap_err();
        let said = network(err).to_string();
        assert!(!said.contains("me@example.org"), "{said}");
    }

    #[tokio::test(start_paused = true)]
    async fn push_comes_back_after_a_stall_has_paused_it() {
        let client = JmapClient::new(
            "https://mail.example.org/jmap/session",
            Auth::Bearer("t".into()),
        )
        .unwrap();
        client.pause_pushing();
        assert!(client.push_paused().is_some());
        tokio::time::advance(PUSH_PAUSE).await;
        assert_eq!(client.push_paused(), None);
        assert!(!client.push_off());
    }

    #[tokio::test]
    async fn a_refused_fixed_token_goes_out_once() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(401))
            .expect(1)
            .mount(&server)
            .await;
        let client = JmapClient::new(
            &format!("{}/jmap/session", server.uri()),
            Auth::Bearer("t".into()),
        )
        .unwrap();
        assert_eq!(client.session().await.unwrap_err(), JmapError::Unauthorized);
    }

    #[tokio::test]
    async fn a_server_allowing_more_still_gets_the_clients_own_most() {
        assert_eq!(
            client_with_concurrency(50)
                .await
                .1
                .inner
                .permits
                .available_permits(),
            MOST_AT_ONCE
        );
    }
}
