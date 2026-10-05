//! The DAV calls Penguin Mail makes, typed, behind [`DavApi`], so tests
//! and the demo hand the adapters [`crate::fake::FakeDav`] in place of a
//! server. [`DavClient`] makes them over HTTPS with reqwest. The password
//! goes only to the context URL's own registrable domain: a redirect or a
//! home set elsewhere gets no credentials, and the request fails as
//! unauthorized. Every answer is read up to [`crate::MOST_BYTES`].

use std::time::Duration;

use mailrs_domain::EpochMillis;
use reqwest::{Method, StatusCode};
use url::Url;

use crate::ids::{canonical_href, path_of, wire_href};
use crate::xml::{self, Multistatus, body};
use crate::{DavError, MOST_BYTES, MOST_RESOURCE_BYTES, MULTIGET_BATCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Calendar,
    AddressBook,
}

#[derive(Clone)]
pub struct Login {
    pub user: String,
    pub password: String,
}

impl Login {
    pub fn new(user: impl Into<String>, password: impl Into<String>) -> Login {
        Login { user: user.into(), password: password.into() }
    }
}

// The password never reaches a log line.
impl std::fmt::Debug for Login {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Login").field("user", &self.user).finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Homes {
    pub principal: String,
    pub calendar: Option<String>,
    pub addressbook: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collection {
    pub href: String,
    pub kind: Kind,
    pub name: String,
    /// Lower-case `#rrggbb`.
    pub color: Option<String>,
    pub components: Vec<String>,
    pub can_write: bool,
    /// A VCALENDAR holding the collection's VTIMEZONE.
    pub timezone: Option<String>,
    /// The collection answers `sync-collection`.
    pub sync: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CollectionState {
    pub sync_token: Option<String>,
    pub ctag: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub href: String,
    pub etag: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fetched {
    pub href: String,
    pub etag: String,
    pub body: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Synced {
    pub changed: Vec<Member>,
    pub removed: Vec<String>,
    pub token: String,
    /// The server cut the answer short (507); ask again with `token`.
    pub more: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Precondition {
    /// `If-Match`: only while the resource still has this etag.
    Match(String),
    /// `If-None-Match: *`: only when nothing is there yet.
    NoneMatch,
}

/// The DAV calls, typed. `href`s may be paths or full URLs; the answer's
/// hrefs are the server's own, as it wrote them.
pub trait DavApi: Send + Sync + 'static {
    fn homes(&self) -> impl Future<Output = Result<Homes, DavError>> + Send;
    fn collections(&self, home: &str, kind: Kind) -> impl Future<Output = Result<Vec<Collection>, DavError>> + Send;
    fn state(&self, collection: &str) -> impl Future<Output = Result<CollectionState, DavError>> + Send;
    /// What changed since `token`; an empty token lists every member.
    fn sync(&self, collection: &str, token: &str) -> impl Future<Output = Result<Synced, DavError>> + Send;
    /// Every member's etag; for a calendar, only the events overlapping
    /// the range (`i64::MAX` as its end leaves the end open).
    fn members(&self, collection: &str, kind: Kind, range: Option<(EpochMillis, EpochMillis)>) -> impl Future<Output = Result<Vec<Member>, DavError>> + Send;
    /// Up to [`MULTIGET_BATCH`] members with their data; one gone is left out.
    fn fetch(&self, collection: &str, kind: Kind, hrefs: &[String]) -> impl Future<Output = Result<Vec<Fetched>, DavError>> + Send;
    fn get(&self, href: &str) -> impl Future<Output = Result<Fetched, DavError>> + Send;
    fn put(&self, href: &str, body: &str, kind: Kind, when: Precondition) -> impl Future<Output = Result<Option<String>, DavError>> + Send;
    fn delete(&self, href: &str, etag: Option<&str>) -> impl Future<Output = Result<(), DavError>> + Send;
    fn find_uid(&self, collection: &str, uid: &str) -> impl Future<Output = Result<Option<Fetched>, DavError>> + Send;
    /// Whether the server's `DAV:` header lists `calendar-auto-schedule`,
    /// which means it mails the organizer's reply itself when a PUT
    /// changes an attendee's PARTSTAT.
    fn auto_schedule(&self) -> impl Future<Output = Result<bool, DavError>> + Send;
    fn host(&self) -> &str;
}

/// Whether `other` shares `host`'s registrable domain, so the account's
/// password may go there: iCloud serves the principal at caldav.icloud.com
/// and the calendars at p12-caldav.icloud.com.
pub fn same_site(host: &str, other: &str) -> bool {
    match (psl::domain_str(host), psl::domain_str(other)) {
        (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
        _ => host.eq_ignore_ascii_case(other),
    }
}

pub struct DavClient {
    base: Url,
    /// Which well-known URL `homes` falls back on (RFC 6764).
    kind: Kind,
    login: Login,
    http: reqwest::Client,
}

struct Answer {
    /// The URL that answered, after any redirect.
    url: Url,
    status: StatusCode,
    etag: Option<String>,
    retry_after: Option<Duration>,
    /// The `DAV` header, which lists what the server supports.
    dav: Option<String>,
    body: String,
}

const REQUEST_LIMIT: Duration = Duration::from_secs(30);

fn http_client(https_only: bool) -> Result<reqwest::Client, DavError> {
    reqwest::Client::builder()
        .https_only(https_only)
        // Redirects are followed by hand, so the password goes only
        // where `same_site` allows.
        .redirect(reqwest::redirect::Policy::none())
        .timeout(REQUEST_LIMIT)
        .user_agent(concat!("Penguin Mail/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|err| DavError::Network(err.to_string()))
}

impl DavClient {
    pub fn new(context: &str, kind: Kind, login: Login) -> Result<DavClient, DavError> {
        let base = Url::parse(context).map_err(|err| DavError::Parse(err.to_string()))?;
        if base.scheme() != "https" {
            return Err(DavError::Forbidden("Penguin Mail reaches calendar and contact servers over HTTPS only".into()));
        }
        Ok(DavClient { base, kind, login, http: http_client(true)? })
    }

    /// A client with no https check, for tests against a local mock
    /// server. It builds its own HTTP client, so it follows redirects by
    /// hand as `new` does and a test exercises the real credential rule.
    #[cfg(any(test, feature = "fake"))]
    pub fn over(base: Url, kind: Kind, login: Login) -> Result<DavClient, DavError> {
        Ok(DavClient { base, kind, login, http: http_client(false)? })
    }

    fn url(&self, href: &str) -> Result<Url, DavError> {
        // The path is encoded here, once, from its canonical form: `join`
        // alone would leave a stray `?` or `#` to start a query.
        self.base.join(&wire_href(href)).map_err(|err| DavError::Parse(err.to_string()))
    }

    async fn send(&self, method: &str, href: &str, depth: Option<&str>, body: Option<String>, headers: &[(&str, String)]) -> Result<Answer, DavError> {
        let method = Method::from_bytes(method.as_bytes()).map_err(|err| DavError::Parse(err.to_string()))?;
        let mut url = self.url(href)?;
        let home = self.base.host_str().unwrap_or_default().to_string();
        for _ in 0..4 {
            let mut request = self.http.request(method.clone(), url.clone());
            if url.host_str().is_some_and(|h| same_site(&home, h)) {
                request = request.basic_auth(&self.login.user, Some(&self.login.password));
            }
            if let Some(depth) = depth {
                request = request.header("Depth", depth);
            }
            for (name, value) in headers {
                request = request.header(*name, value);
            }
            if let Some(body) = &body {
                request = request.header("Content-Type", "application/xml; charset=utf-8").body(body.clone());
            }
            let mut response = request.send().await.map_err(|err| DavError::Network(err.to_string()))?;
            let status = response.status();
            if status.is_redirection() {
                let Some(next) = response.headers().get("Location").and_then(|l| l.to_str().ok()) else {
                    return Err(DavError::Http { status: status.as_u16(), detail: "a redirect with nowhere to go".into() });
                };
                url = url.join(next).map_err(|err| DavError::Parse(err.to_string()))?;
                continue;
            }
            let etag = response.headers().get("ETag").and_then(|e| e.to_str().ok()).map(str::to_string);
            let dav = response.headers().get("DAV").and_then(|d| d.to_str().ok()).map(str::to_string);
            let retry_after = response
                .headers()
                .get("Retry-After")
                .and_then(|r| r.to_str().ok())
                .and_then(|r| r.parse().ok())
                .map(Duration::from_secs);
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|err| DavError::Network(err.to_string()))? {
                bytes.extend_from_slice(&chunk);
                if bytes.len() > MOST_BYTES {
                    return Err(DavError::TooLarge(MOST_BYTES));
                }
            }
            let body = String::from_utf8_lossy(&bytes).into_owned();
            return check(Answer { url, status, etag, retry_after, dav, body });
        }
        Err(DavError::Http { status: 310, detail: "too many redirects".into() })
    }

    async fn multistatus(&self, method: &str, href: &str, depth: &str, request: String) -> Result<Multistatus, DavError> {
        let answer = self.send(method, href, Some(depth), Some(request), &[]).await?;
        xml::parse_multistatus(&answer.body)
    }

    /// `href` from an answer given at `answered`. It stays as the server
    /// wrote it while that is the context URL's own origin, and becomes
    /// absolute when a redirect led to another host of the site.
    fn resolve(&self, answered: &Url, href: &str) -> String {
        match answered.origin() == self.base.origin() {
            true => href.to_string(),
            false => absolute(answered, href),
        }
    }

    /// The principal the server names at `href`.
    async fn principal_at(&self, href: &str) -> Result<String, DavError> {
        let answer = self.send("PROPFIND", href, Some("0"), Some(body::principal().into()), &[]).await?;
        let found = xml::parse_multistatus(&answer.body)?;
        let principal = found
            .responses
            .iter()
            .find_map(|r| r.props.principal.clone())
            .ok_or_else(|| DavError::Parse("the server named no principal".into()))?;
        Ok(self.resolve(&answer.url, &principal))
    }

    /// The principal at the context URL, or at the host's well-known URL
    /// for the client's kind when the context URL holds no DAV principal
    /// (RFC 6764 section 5). Fastmail's hosts and iCloud's contacts host
    /// answer 404 at their root, Zoho's 501 or 502, and GMX's CardDAV root
    /// sends a web page.
    async fn principal(&self) -> Result<String, DavError> {
        let context = path_of(self.base.as_str());
        let well_known = match self.kind {
            Kind::Calendar => "/.well-known/caldav",
            Kind::AddressBook => "/.well-known/carddav",
        };
        let first = match self.principal_at(&context).await {
            Err(err) if not_dav(&err) && !context.starts_with("/.well-known/") => err,
            other => return other,
        };
        match self.principal_at(well_known).await {
            Err(second) if not_dav(&second) => Err(first),
            other => other,
        }
    }
}

/// An answer that says the URL asked holds no DAV principal, as opposed
/// to a refused login, a busy server or a network failure.
fn not_dav(err: &DavError) -> bool {
    match err {
        DavError::NotFound | DavError::Parse(_) => true,
        DavError::Http { status, .. } => matches!(status, 300..=399 | 400 | 405 | 501 | 502),
        _ => false,
    }
}

/// `href` as an absolute URL, read against the URL that answered it.
/// The path is canonical (decoded), so the same resource has one string
/// whichever way the server spelled its escapes.
fn absolute(asked: &Url, href: &str) -> String {
    asked.join(&wire_href(href)).map(|url| canonical_href(url.as_str())).unwrap_or_else(|_| canonical_href(href))
}

fn check(answer: Answer) -> Result<Answer, DavError> {
    let detail = |body: &str| body.chars().take(200).collect::<String>();
    match answer.status.as_u16() {
        200..=299 => Ok(answer),
        401 => Err(DavError::Unauthorized),
        403 | 409 if xml::precondition(&answer.body).as_deref() == Some("valid-sync-token") => Err(DavError::InvalidSyncToken),
        403 => Err(DavError::Forbidden(detail(&answer.body))),
        404 | 410 => Err(DavError::NotFound),
        409 | 412 => Err(DavError::Changed),
        429 | 503 => Err(DavError::Busy(answer.retry_after)),
        status => Err(DavError::Http { status, detail: detail(&answer.body) }),
    }
}

/// `#RRGGBBAA` or `#RRGGBB` as lower-case `#rrggbb`.
fn color(raw: &str) -> Option<String> {
    let hex = raw.trim().strip_prefix('#')?;
    (hex.len() >= 6 && hex[..6].bytes().all(|b| b.is_ascii_hexdigit())).then(|| format!("#{}", hex[..6].to_ascii_lowercase()))
}

impl DavApi for DavClient {
    async fn homes(&self) -> Result<Homes, DavError> {
        let principal = self.principal().await?;
        let answer = self.send("PROPFIND", &principal, Some("0"), Some(body::homes().into()), &[]).await?;
        let homes = xml::parse_multistatus(&answer.body)?;
        let props = homes.responses.first().map(|r| r.props.clone()).unwrap_or_default();
        let resolve = |home: Option<String>| home.map(|h| self.resolve(&answer.url, &h));
        Ok(Homes { principal, calendar: resolve(props.calendar_home), addressbook: resolve(props.addressbook_home) })
    }

    async fn collections(&self, home: &str, kind: Kind) -> Result<Vec<Collection>, DavError> {
        let found = self.multistatus("PROPFIND", home, "1", body::collections().into()).await?;
        let asked = self.url(home)?;
        let home_path = path_of(home);
        Ok(found
            .responses
            .into_iter()
            .filter(|r| path_of(&r.href) != home_path)
            .filter(|r| match kind {
                Kind::Calendar => r.props.is_calendar && (r.props.components.is_empty() || r.props.components.iter().any(|c| c == "VEVENT")),
                Kind::AddressBook => r.props.is_addressbook,
            })
            .map(|r| {
                let name = r.props.displayname.clone().unwrap_or_else(|| {
                    path_of(&r.href).trim_end_matches('/').rsplit('/').next().unwrap_or_default().to_string()
                });
                Collection {
                    href: absolute(&asked, &r.href),
                    kind,
                    name,
                    color: r.props.color.as_deref().and_then(color),
                    components: r.props.components,
                    can_write: r.props.can_write,
                    timezone: r.props.timezone,
                    sync: r.props.reports_sync,
                }
            })
            .collect())
    }

    async fn state(&self, collection: &str) -> Result<CollectionState, DavError> {
        let found = self.multistatus("PROPFIND", collection, "0", body::state().into()).await?;
        let props = found.responses.into_iter().next().map(|r| r.props).unwrap_or_default();
        Ok(CollectionState { sync_token: props.sync_token, ctag: props.getctag })
    }

    async fn sync(&self, collection: &str, token: &str) -> Result<Synced, DavError> {
        let answer = self.send("REPORT", collection, None, Some(body::sync_collection(token)), &[]).await;
        let answer = match answer {
            // A server without the report answers 400, 403 or 501.
            Err(DavError::Http { status: 400 | 501, .. }) | Err(DavError::Forbidden(_)) if token.is_empty() => {
                return Err(DavError::NoSyncCollection);
            }
            other => other?,
        };
        let found = xml::parse_multistatus(&answer.body)?;
        let asked = self.url(collection)?;
        let own = path_of(collection);
        let mut synced = Synced { token: found.sync_token.unwrap_or_default(), ..Synced::default() };
        for response in found.responses {
            if path_of(&response.href) == own {
                synced.more |= response.status == Some(507);
                continue;
            }
            let href = absolute(&asked, &response.href);
            match (response.status, response.props.etag) {
                (Some(404), _) => synced.removed.push(href),
                (_, Some(etag)) => synced.changed.push(Member { href, etag }),
                _ => {}
            }
        }
        Ok(synced)
    }

    async fn members(&self, collection: &str, kind: Kind, range: Option<(EpochMillis, EpochMillis)>) -> Result<Vec<Member>, DavError> {
        let found = match kind {
            Kind::Calendar => self.multistatus("REPORT", collection, "1", body::calendar_query(range)).await?,
            Kind::AddressBook => self.multistatus("PROPFIND", collection, "1", body::members().into()).await?,
        };
        let asked = self.url(collection)?;
        let own = path_of(collection);
        Ok(found
            .responses
            .into_iter()
            .filter(|r| path_of(&r.href) != own)
            .filter_map(|r| Some(Member { etag: r.props.etag?, href: absolute(&asked, &r.href) }))
            .collect())
    }

    async fn fetch(&self, collection: &str, kind: Kind, hrefs: &[String]) -> Result<Vec<Fetched>, DavError> {
        let mut out = Vec::new();
        for batch in hrefs.chunks(MULTIGET_BATCH) {
            let request = match kind {
                Kind::Calendar => body::calendar_multiget(batch),
                Kind::AddressBook => body::addressbook_multiget(batch),
            };
            let found = self.multistatus("REPORT", collection, "1", request).await?;
            let asked = self.url(collection)?;
            out.extend(found.responses.into_iter().filter(|r| r.status != Some(404)).filter_map(|r| {
                let body = r.props.calendar_data.or(r.props.address_data)?;
                // One resource past the limit is left out, not the batch.
                if body.len() > MOST_RESOURCE_BYTES {
                    return None;
                }
                Some(Fetched { etag: r.props.etag.unwrap_or_default(), href: absolute(&asked, &r.href), body })
            }));
        }
        Ok(out)
    }

    async fn get(&self, href: &str) -> Result<Fetched, DavError> {
        let answer = self.send("GET", href, None, None, &[]).await?;
        if answer.body.len() > MOST_RESOURCE_BYTES {
            return Err(DavError::TooLarge(MOST_RESOURCE_BYTES));
        }
        Ok(Fetched { href: canonical_href(self.url(href)?.as_str()), etag: answer.etag.unwrap_or_default(), body: answer.body })
    }

    async fn put(&self, href: &str, body: &str, kind: Kind, when: Precondition) -> Result<Option<String>, DavError> {
        if body.len() > MOST_RESOURCE_BYTES {
            return Err(DavError::TooLarge(MOST_RESOURCE_BYTES));
        }
        let content_type = match kind {
            Kind::Calendar => "text/calendar; charset=utf-8",
            Kind::AddressBook => "text/vcard; charset=utf-8",
        };
        let mut headers = vec![("Content-Type", content_type.to_string())];
        match when {
            Precondition::Match(etag) => headers.push(("If-Match", etag)),
            Precondition::NoneMatch => headers.push(("If-None-Match", "*".to_string())),
        }
        // The body is not XML, so it goes through `send` without the XML
        // content type: a request built here, as `send` builds one.
        let url = self.url(href)?;
        let mut request = self.http.put(url.clone()).body(body.to_string());
        if url.host_str().is_some_and(|h| same_site(self.base.host_str().unwrap_or_default(), h)) {
            request = request.basic_auth(&self.login.user, Some(&self.login.password));
        }
        for (name, value) in headers {
            request = request.header(name, value);
        }
        let response = request.send().await.map_err(|err| DavError::Network(err.to_string()))?;
        let status = response.status();
        let etag = response.headers().get("ETag").and_then(|e| e.to_str().ok()).map(str::to_string);
        let text = response.text().await.unwrap_or_default();
        check(Answer { url, status, etag: etag.clone(), retry_after: None, dav: None, body: text })?;
        Ok(etag)
    }

    async fn delete(&self, href: &str, etag: Option<&str>) -> Result<(), DavError> {
        let headers: Vec<(&str, String)> = etag.map(|e| ("If-Match", e.to_string())).into_iter().collect();
        self.send("DELETE", href, None, None, &headers).await.map(drop)
    }

    async fn find_uid(&self, collection: &str, uid: &str) -> Result<Option<Fetched>, DavError> {
        let found = self.multistatus("REPORT", collection, "1", body::uid_query(uid)).await?;
        let asked = self.url(collection)?;
        Ok(found.responses.into_iter().find_map(|r| {
            Some(Fetched { etag: r.props.etag.unwrap_or_default(), body: r.props.calendar_data?, href: absolute(&asked, &r.href) })
        }))
    }

    async fn auto_schedule(&self) -> Result<bool, DavError> {
        let answer = self.send("OPTIONS", &path_of(self.base.as_str()), None, None, &[]).await?;
        Ok(answer.dav.is_some_and(|header| header.split(',').any(|token| token.trim().eq_ignore_ascii_case("calendar-auto-schedule"))))
    }

    fn host(&self) -> &str {
        self.base.host_str().unwrap_or_default()
    }
}
