//! Google's OAuth 2.0 installed-app flow: PKCE, the token endpoint, and a
//! loopback listener that receives the browser redirect.

use std::collections::{BTreeSet, HashMap};
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use url::Url;

use crate::GmailError;
use crate::calendar::{CALENDAR_LIST_SCOPE, CALENDAR_SCOPE};
use crate::people::{CONTACTS_SCOPE, CONTACTS_WRITE_SCOPE};

/// Read, send, and organize mail. Sign-in no longer asks for this one on
/// its own: [`DELETE_SCOPE`] covers it. [`Granted::has`] still checks it,
/// so an account that signed in before this change and kept only this
/// narrower grant still reads and sends mail.
pub const GMAIL_SCOPE: &str = "https://www.googleapis.com/auth/gmail.modify";
/// Read and change the automatic reply and signatures.
pub const SETTINGS_SCOPE: &str = "https://www.googleapis.com/auth/gmail.settings.basic";
/// Erase mail so that Gmail cannot bring it back. It covers the whole
/// mailbox, which is far more than [`GMAIL_SCOPE`] does, but sign-in asks
/// for it instead of the narrower one: leaving it out would mean asking
/// again the first time somebody deletes mail from the Trash, and the
/// owner decided every scope goes in one consent.
pub const DELETE_SCOPE: &str = "https://mail.google.com/";
pub const GOOGLE_AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
pub const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

/// Every scope sign-in asks Google for, in one consent: [`DELETE_SCOPE`]
/// (mail, and deleting it for good), settings, [`CONTACTS_WRITE_SCOPE`]
/// (contacts, and changing them), and the two calendar scopes. Google's
/// verification team asks for least privilege, so this leaves out
/// [`GMAIL_SCOPE`] and [`crate::people::CONTACTS_SCOPE`]: each is already
/// covered by the wider scope in the list. A person may untick any of
/// these on Google's screen; [`Granted`] says which the token still
/// carries.
pub const SIGN_IN_SCOPES: [&str; 5] = [
    DELETE_SCOPE,
    SETTINGS_SCOPE,
    CONTACTS_WRITE_SCOPE,
    CALENDAR_SCOPE,
    CALENDAR_LIST_SCOPE,
];

#[derive(Debug, Clone)]
pub struct AccessToken {
    pub token: String,
    pub expires_at: Instant,
    /// The scopes Google's token answer said this token carries. `None`
    /// when the answer carried no `scope` field, which an old refresh
    /// token's first use after an upgrade still can.
    pub granted: Option<Granted>,
}

/// The scopes a token carries, parsed from Google's space-separated
/// `scope` field.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Granted(BTreeSet<String>);

impl Granted {
    /// Splits Google's `scope` string into a set.
    pub fn parse(scope: &str) -> Granted {
        Granted(scope.split_whitespace().map(str::to_string).collect())
    }

    /// Whether the token carries `scope`, directly or through a wider one
    /// that covers it: [`DELETE_SCOPE`] covers [`GMAIL_SCOPE`], and
    /// [`CONTACTS_WRITE_SCOPE`] covers [`CONTACTS_SCOPE`].
    pub fn has(&self, scope: &str) -> bool {
        self.0.contains(scope)
            || (scope == GMAIL_SCOPE && self.0.contains(DELETE_SCOPE))
            || (scope == CONTACTS_SCOPE && self.0.contains(CONTACTS_WRITE_SCOPE))
    }

    /// The scopes, sorted and joined with single spaces, as the store
    /// keeps them.
    pub fn to_scope(&self) -> String {
        self.0.iter().cloned().collect::<Vec<_>>().join(" ")
    }

    /// Whether the token can read and change mail.
    pub fn reads_mail(&self) -> bool {
        self.has(GMAIL_SCOPE)
    }
}

#[cfg(test)]
mod granted_tests {
    use super::*;

    #[test]
    fn granted_scopes_parse_and_cover_the_narrower_ones() {
        let granted = Granted::parse(DELETE_SCOPE);
        assert!(granted.has(DELETE_SCOPE));
        assert!(granted.has(GMAIL_SCOPE), "mail.google.com covers gmail.modify");
        assert!(!granted.has(SETTINGS_SCOPE));

        let contacts = Granted::parse(CONTACTS_WRITE_SCOPE);
        assert!(contacts.has(CONTACTS_SCOPE), "contacts covers contacts.readonly");
    }

    #[test]
    fn granted_scopes_write_back_sorted() {
        let granted = Granted::parse(&format!("{SETTINGS_SCOPE} {GMAIL_SCOPE}"));
        assert_eq!(granted.to_scope(), format!("{GMAIL_SCOPE} {SETTINGS_SCOPE}"));
    }
}

impl AccessToken {
    /// True when the token has at least `margin` of life left.
    pub fn valid_for(&self, margin: Duration) -> bool {
        self.expires_at > Instant::now() + margin
    }
}

#[derive(Debug, Clone)]
pub struct Tokens {
    pub access: AccessToken,
    pub refresh_token: String,
}

/// A PKCE verifier and its S256 challenge.
#[derive(Debug, Clone)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl Pkce {
    pub fn generate() -> Self {
        let verifier = random_token(32);
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        Pkce {
            verifier,
            challenge,
        }
    }
}

/// A URL-safe string built from `bytes` random bytes.
pub fn random_token(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::fill(&mut buf[..]);
    URL_SAFE_NO_PAD.encode(buf)
}

#[derive(Clone)]
pub struct OAuthClient {
    http: reqwest::Client,
    client_id: String,
    client_secret: String,
    auth_url: String,
    token_url: String,
    /// Google counts quota against the OAuth client, so every account
    /// signed in through this one draws on the same pool. Clones share it.
    quota: std::sync::Arc<crate::limiter::QuotaPool>,
}

// Written out so the secret never reaches a log line through `{:?}`. The
// build's client comes from the release environment, and its values stay
// out of logs and error messages.
impl std::fmt::Debug for OAuthClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthClient")
            .field("auth_url", &self.auth_url)
            .field("token_url", &self.token_url)
            .finish_non_exhaustive()
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
    refresh_token: Option<String>,
    #[serde(default)]
    scope: Option<String>,
}

impl TokenResponse {
    fn access(&self) -> AccessToken {
        AccessToken {
            token: self.access_token.clone(),
            expires_at: Instant::now() + Duration::from_secs(self.expires_in),
            granted: self.scope.as_deref().map(Granted::parse),
        }
    }
}

impl OAuthClient {
    /// A client for Google's endpoints. Google issues installed-app secrets to
    /// identify the app; they are not confidential.
    pub fn new(client_id: impl Into<String>, client_secret: impl Into<String>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("the TLS backend initializes");
        OAuthClient {
            http,
            client_id: client_id.into(),
            client_secret: client_secret.into(),
            auth_url: GOOGLE_AUTH_URL.into(),
            token_url: GOOGLE_TOKEN_URL.into(),
            quota: std::sync::Arc::new(crate::limiter::QuotaPool::new()),
        }
    }

    /// The client ID, which Google shows on its consent screen and which
    /// is no secret. Tests use it to tell two clients apart.
    pub fn id(&self) -> &str {
        &self.client_id
    }

    /// `email`'s bucket in this OAuth client's quota pool.
    pub fn account_quota(&self, email: &str) -> std::sync::Arc<crate::limiter::AccountQuota> {
        self.quota.account(email)
    }

    /// Points the client at other endpoints. Tests use this.
    pub fn with_endpoints(
        mut self,
        auth_url: impl Into<String>,
        token_url: impl Into<String>,
    ) -> Self {
        self.auth_url = auth_url.into();
        self.token_url = token_url.into();
        self
    }

    pub(crate) fn http(&self) -> &reqwest::Client {
        &self.http
    }

    /// Google's consent URL, asking for every [`SIGN_IN_SCOPES`] entry in
    /// one visit. Google still lets the person untick any of them; what
    /// the token ends up carrying comes back as [`Granted`].
    pub fn authorize_url(
        &self,
        redirect_uri: &str,
        pkce: &Pkce,
        state: &str,
    ) -> Result<String, GmailError> {
        let mut url = Url::parse(&self.auth_url).map_err(|e| {
            GmailError::OAuth(format!("bad authorization URL {}: {e}", self.auth_url))
        })?;
        url.query_pairs_mut()
            .append_pair("client_id", &self.client_id)
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("response_type", "code")
            .append_pair("scope", &SIGN_IN_SCOPES.join(" "))
            .append_pair("include_granted_scopes", "true")
            .append_pair("code_challenge", &pkce.challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", state)
            .append_pair("access_type", "offline")
            .append_pair("prompt", "consent");
        Ok(url.into())
    }

    pub async fn exchange_code(
        &self,
        code: &str,
        redirect_uri: &str,
        pkce: &Pkce,
    ) -> Result<Tokens, GmailError> {
        let response = self
            .post_token(&[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", redirect_uri),
                ("client_id", &self.client_id),
                ("client_secret", &self.client_secret),
                ("code_verifier", &pkce.verifier),
            ])
            .await?;
        let refresh_token = response
            .refresh_token
            .clone()
            .ok_or_else(|| GmailError::OAuth("Google returned no refresh token".into()))?;
        Ok(Tokens {
            access: response.access(),
            refresh_token,
        })
    }

    pub async fn refresh(&self, refresh_token: &str) -> Result<AccessToken, GmailError> {
        let response = self
            .post_token(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token),
                ("client_id", &self.client_id),
                ("client_secret", &self.client_secret),
            ])
            .await?;
        Ok(response.access())
    }

    async fn post_token(&self, form: &[(&str, &str)]) -> Result<TokenResponse, GmailError> {
        let response = self.http.post(&self.token_url).form(form).send().await?;
        let status = response.status().as_u16();
        if response.status().is_success() {
            return response
                .json()
                .await
                .map_err(|e| GmailError::Decode(e.to_string()));
        }
        let body = response.text().await.unwrap_or_default();
        Err(match status {
            400 | 401 if body.contains("invalid_grant") => GmailError::NeedsReauth,
            429 => GmailError::RateLimited { retry_after: None },
            _ => GmailError::Http { status, body },
        })
    }
}

/// The Google client a build signs in with, from two values. Either one
/// missing or blank means the build has none: GitHub passes a secret it
/// does not hold as an empty string, and a pull request from a fork gets
/// no secrets at all.
pub fn client_from(id: Option<&str>, secret: Option<&str>) -> Option<OAuthClient> {
    let id = id.map(str::trim).filter(|v| !v.is_empty())?;
    let secret = secret.map(str::trim).filter(|v| !v.is_empty())?;
    Some(OAuthClient::new(id, secret))
}

/// The project's own Google client, compiled in from
/// `PENGUIN_MAIL_GOOGLE_CLIENT_ID` and `PENGUIN_MAIL_GOOGLE_CLIENT_SECRET`.
/// Cargo rebuilds this crate when either changes. Google treats a desktop
/// client's secret as public, but the values still stay out of logs.
pub fn built_in_client() -> Option<OAuthClient> {
    client_from(
        option_env!("PENGUIN_MAIL_GOOGLE_CLIENT_ID"),
        option_env!("PENGUIN_MAIL_GOOGLE_CLIENT_SECRET"),
    )
}

/// The project's Microsoft client ID, from
/// `PENGUIN_MAIL_MICROSOFT_CLIENT_ID`. A desktop sign-in with Microsoft
/// needs no secret. Nothing reads it until Microsoft accounts arrive.
pub fn built_in_microsoft_client_id() -> Option<&'static str> {
    option_env!("PENGUIN_MAIL_MICROSOFT_CLIENT_ID")
        .map(str::trim)
        .filter(|v| !v.is_empty())
}

/// Receives Google's redirect on 127.0.0.1 during consent.
pub struct LoopbackListener {
    listener: TcpListener,
    pub redirect_uri: String,
}

impl LoopbackListener {
    /// Listens on an ephemeral port.
    pub async fn bind() -> Result<Self, GmailError> {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .map_err(io_error)?;
        let port = listener.local_addr().map_err(io_error)?.port();
        Ok(LoopbackListener {
            listener,
            redirect_uri: format!("http://127.0.0.1:{port}"),
        })
    }

    /// Answers requests until one carries the authorization code or an error.
    pub async fn wait_for_code(self, expected_state: &str) -> Result<String, GmailError> {
        loop {
            let (mut stream, _) = self.listener.accept().await.map_err(io_error)?;
            let mut buf = vec![0u8; 8192];
            let n = stream.read(&mut buf).await.map_err(io_error)?;
            let outcome = parse_redirect(&String::from_utf8_lossy(&buf[..n]), expected_state);
            let (status, message) = match &outcome {
                Some(Ok(_)) => (
                    "200 OK",
                    "Penguin Mail is authorized. You can close this tab.",
                ),
                Some(Err(_)) => (
                    "400 Bad Request",
                    "Authorization failed. The terminal has the details.",
                ),
                None => ("404 Not Found", "Not found."),
            };
            let body = format!(
                "<!doctype html><meta charset=utf-8><title>Penguin Mail</title><p>{message}</p>"
            );
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes()).await;
            if let Some(result) = outcome {
                return result;
            }
        }
    }
}

fn io_error(err: std::io::Error) -> GmailError {
    GmailError::OAuth(format!("loopback listener: {err}"))
}

/// Reads the OAuth redirect out of a raw HTTP request. `None` means the
/// request was something else, such as a favicon fetch.
pub fn parse_redirect(request: &str, expected_state: &str) -> Option<Result<String, GmailError>> {
    let target = request.lines().next()?.split_whitespace().nth(1)?;
    let url = Url::parse(&format!("http://127.0.0.1{target}")).ok()?;
    let params: HashMap<String, String> = url.query_pairs().into_owned().collect();
    if let Some(error) = params.get("error") {
        return Some(Err(GmailError::OAuth(format!("Google returned {error}"))));
    }
    let code = params.get("code")?;
    if params.get("state").map(String::as_str) != Some(expected_state) {
        return Some(Err(GmailError::OAuth(
            "the redirect's state did not match; try again".into(),
        )));
    }
    Some(Ok(code.clone()))
}

#[cfg(test)]
mod build_client_tests {
    use super::*;

    #[test]
    fn a_build_with_no_values_has_no_client() {
        assert!(client_from(None, None).is_none());
    }

    #[test]
    fn empty_values_count_as_none() {
        assert!(client_from(Some(""), Some("")).is_none());
    }

    #[test]
    fn an_id_without_a_secret_is_no_client() {
        assert!(client_from(Some("id.apps.googleusercontent.com"), Some(" ")).is_none());
    }

    #[test]
    fn both_values_make_a_client() {
        let client = client_from(Some("id.apps.googleusercontent.com"), Some("GOCSPX-x"));
        assert_eq!(
            client.map(|c| c.client_id),
            Some("id.apps.googleusercontent.com".to_string())
        );
    }

    #[test]
    fn debug_output_leaves_out_the_values() {
        let client = OAuthClient::new("id.apps.googleusercontent.com", "GOCSPX-x");
        let shown = format!("{client:?}");
        assert!(!shown.contains("GOCSPX-x"));
        assert!(!shown.contains("id.apps.googleusercontent.com"));
    }
}
