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

use mailrs_domain::sign_in_page::Finished;

use crate::GmailError;

/// The most a token endpoint's answer may take; a real one is a few hundred bytes.
const TOKEN_LIMIT: usize = 64 << 10;
use crate::calendar::{
    CALENDAR_LIST_SCOPE, CALENDAR_LIST_WRITE_SCOPE, CALENDAR_SCOPE, CALENDARS_SCOPE,
};
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

/// Files the app itself puts in the account's Google Drive, and nothing
/// else there. Sign-in asks for it now so that sending large attachments
/// through Drive needs no second consent later; nothing uses it yet.
pub const DRIVE_FILE_SCOPE: &str = "https://www.googleapis.com/auth/drive.file";

/// Every scope an own Google client asks for, in one consent:
/// [`DELETE_SCOPE`] (mail, and deleting it for good), settings,
/// [`CONTACTS_WRITE_SCOPE`] (contacts, and changing them), events, the
/// calendar list and the calendars themselves, and [`DRIVE_FILE_SCOPE`].
/// The list leaves out [`GMAIL_SCOPE`], [`crate::people::CONTACTS_SCOPE`]
/// and [`CALENDAR_LIST_SCOPE`]: each is already covered by the wider scope
/// in the list. A person may untick any of these on Google's screen;
/// [`Granted`] says which the token still carries.
pub const FULL_SCOPES: [&str; 7] = [
    DELETE_SCOPE,
    SETTINGS_SCOPE,
    CONTACTS_WRITE_SCOPE,
    CALENDAR_SCOPE,
    CALENDAR_LIST_WRITE_SCOPE,
    CALENDARS_SCOPE,
    DRIVE_FILE_SCOPE,
];

/// What the built-in client asks for: no mail scope, so no scope Google
/// restricts. `openid` and `email` come first so the token answer carries
/// an ID token that names the address.
pub const BUILT_IN_SCOPES: [&str; 7] = [
    "openid",
    "email",
    CONTACTS_WRITE_SCOPE,
    CALENDAR_SCOPE,
    CALENDAR_LIST_WRITE_SCOPE,
    CALENDARS_SCOPE,
    DRIVE_FILE_SCOPE,
];

/// Which list of scopes a client asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeList {
    /// [`FULL_SCOPES`]: an own client, mail included.
    Full,
    /// [`BUILT_IN_SCOPES`]: the project's client, no mail.
    BuiltIn,
}

impl ScopeList {
    pub fn scopes(self) -> &'static [&'static str] {
        match self {
            ScopeList::Full => &FULL_SCOPES,
            ScopeList::BuiltIn => &BUILT_IN_SCOPES,
        }
    }

    /// Whether the list asks for the mail scope.
    pub fn asks_mail(self) -> bool {
        self == ScopeList::Full
    }
}

#[derive(Clone)]
pub struct AccessToken {
    pub token: String,
    pub expires_at: Instant,
    /// The scopes Google's token answer said this token carries. `None`
    /// when the answer carried no `scope` field, which an old refresh
    /// token's first use after an upgrade still can.
    pub granted: Option<Granted>,
}

// Tokens and the PKCE verifier sign in as the person, so their Debug
// output, which can end up in a log or a panic message, hides them.
impl std::fmt::Debug for AccessToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccessToken")
            .field("token", &"<hidden>")
            .field("expires_at", &self.expires_at)
            .field("granted", &self.granted)
            .finish()
    }
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
    /// that covers it: [`DELETE_SCOPE`] covers [`GMAIL_SCOPE`],
    /// [`CONTACTS_WRITE_SCOPE`] covers [`CONTACTS_SCOPE`], and
    /// [`CALENDAR_LIST_WRITE_SCOPE`] covers [`CALENDAR_LIST_SCOPE`].
    pub fn has(&self, scope: &str) -> bool {
        self.0.contains(scope)
            || (scope == GMAIL_SCOPE && self.0.contains(DELETE_SCOPE))
            || (scope == CONTACTS_SCOPE && self.0.contains(CONTACTS_WRITE_SCOPE))
            || (scope == CALENDAR_LIST_SCOPE && self.0.contains(CALENDAR_LIST_WRITE_SCOPE))
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
        assert!(
            granted.has(GMAIL_SCOPE),
            "mail.google.com covers gmail.modify"
        );
        assert!(!granted.has(SETTINGS_SCOPE));

        let contacts = Granted::parse(CONTACTS_WRITE_SCOPE);
        assert!(
            contacts.has(CONTACTS_SCOPE),
            "contacts covers contacts.readonly"
        );
    }

    #[test]
    fn granted_scopes_write_back_sorted() {
        let granted = Granted::parse(&format!("{SETTINGS_SCOPE} {GMAIL_SCOPE}"));
        assert_eq!(
            granted.to_scope(),
            format!("{GMAIL_SCOPE} {SETTINGS_SCOPE}")
        );
    }
}

impl AccessToken {
    /// True when the token has at least `margin` of life left.
    pub fn valid_for(&self, margin: Duration) -> bool {
        self.expires_at > Instant::now() + margin
    }
}

#[derive(Clone)]
pub struct Tokens {
    pub access: AccessToken,
    pub refresh_token: String,
    /// Set when the consent asked for `openid` and `email`.
    pub id_token: Option<IdToken>,
}

/// The claims of Google's ID token that name the signed-in address.
#[derive(Clone)]
pub struct IdToken {
    pub email: String,
    pub email_verified: bool,
}

// The address names the person, so Debug output hides it.
impl std::fmt::Debug for IdToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IdToken")
            .field("email", &"<hidden>")
            .field("email_verified", &self.email_verified)
            .finish()
    }
}

impl IdToken {
    /// Reads the payload without checking the signature: the token comes
    /// straight from Google's token endpoint over TLS. `None` when the
    /// payload does not decode or holds no address. A missing
    /// `email_verified` reads as unverified.
    pub fn parse(token: &str) -> Option<IdToken> {
        let payload = token.split('.').nth(1)?;
        let bytes = URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
        let claims: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
        // Google has sent the claim as a bool and as the string "true".
        let email_verified = match &claims["email_verified"] {
            serde_json::Value::Bool(verified) => *verified,
            serde_json::Value::String(verified) => verified == "true",
            _ => false,
        };
        Some(IdToken {
            email: claims["email"].as_str()?.to_string(),
            email_verified,
        })
    }
}

impl std::fmt::Debug for Tokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tokens")
            .field("access", &self.access)
            .field("refresh_token", &"<hidden>")
            .field("id_token", &self.id_token)
            .finish()
    }
}

/// A PKCE verifier and its S256 challenge.
#[derive(Clone)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl std::fmt::Debug for Pkce {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pkce")
            .field("verifier", &"<hidden>")
            .field("challenge", &self.challenge)
            .finish()
    }
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
    list: ScopeList,
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
    #[serde(default)]
    id_token: Option<String>,
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
            list: ScopeList::Full,
        }
    }

    /// The same client asking for `list` instead.
    pub fn asking(mut self, list: ScopeList) -> Self {
        self.list = list;
        self
    }

    pub fn scope_list(&self) -> ScopeList {
        self.list
    }

    /// The scopes this client asks for.
    pub fn scopes(&self) -> &'static [&'static str] {
        self.list.scopes()
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

    /// Google's consent URL, asking for every [`Self::scopes`] entry in
    /// one visit. Google still lets the person untick any of them; what
    /// the token ends up carrying comes back as [`Granted`].
    pub fn authorize_url(
        &self,
        redirect_uri: &str,
        pkce: &Pkce,
        state: &str,
    ) -> Result<String, GmailError> {
        self.authorize_url_hinted(redirect_uri, pkce, state, None)
    }

    /// [`Self::authorize_url`] naming the account to sign in, so Google
    /// does not offer another.
    pub fn authorize_url_hinted(
        &self,
        redirect_uri: &str,
        pkce: &Pkce,
        state: &str,
        login_hint: Option<&str>,
    ) -> Result<String, GmailError> {
        let mut url = Url::parse(&self.auth_url).map_err(|e| {
            GmailError::OAuth(format!("bad authorization URL {}: {e}", self.auth_url))
        })?;
        url.query_pairs_mut()
            .append_pair("client_id", &self.client_id)
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("response_type", "code")
            .append_pair("scope", &self.scopes().join(" "))
            .append_pair("include_granted_scopes", "true")
            .append_pair("code_challenge", &pkce.challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", state)
            .append_pair("access_type", "offline")
            .append_pair("prompt", "consent");
        if let Some(hint) = login_hint {
            url.query_pairs_mut().append_pair("login_hint", hint);
        }
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
            id_token: response.id_token.as_deref().and_then(IdToken::parse),
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
            let bytes = crate::client::read_capped(response, TOKEN_LIMIT).await?;
            return serde_json::from_slice(&bytes).map_err(|e| GmailError::Decode(e.to_string()));
        }
        let body = crate::client::read_capped(response, TOKEN_LIMIT)
            .await
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .unwrap_or_default();
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
    .map(|client| client.asking(ScopeList::BuiltIn))
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
            let (status, body) = mailrs_domain::sign_in_page::page(finished(&outcome));
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

/// Which of the shared sign-in pages an outcome ends on.
fn finished(outcome: &Option<Result<String, GmailError>>) -> Finished {
    match outcome {
        Some(Ok(_)) => Finished::Signed,
        Some(Err(GmailError::OAuth(reason))) if reason.ends_with("access_denied") => {
            Finished::Denied
        }
        Some(Err(_)) => Finished::Failed,
        None => Finished::Nothing,
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

    #[test]
    fn debug_output_leaves_out_tokens_and_the_verifier() {
        let access = AccessToken {
            token: "ya29.access".into(),
            expires_at: Instant::now(),
            granted: None,
        };
        let tokens = Tokens {
            access: access.clone(),
            refresh_token: "1//refresh".into(),
            id_token: None,
        };
        let authorized = crate::Authorized {
            email: "dana@gmail.com".into(),
            refresh_token: "1//refresh".into(),
            granted: None,
        };
        let pkce = Pkce::generate();
        let shown = format!("{access:?} {tokens:?} {authorized:?} {pkce:?}");
        assert!(!shown.contains("ya29.access"), "{shown}");
        assert!(!shown.contains("1//refresh"), "{shown}");
        assert!(!shown.contains(&pkce.verifier), "{shown}");
    }
}

#[cfg(test)]
mod finished_tests {
    use super::*;

    #[test]
    fn an_outcome_picks_the_shared_page() {
        assert_eq!(finished(&Some(Ok("code".into()))), Finished::Signed);
        let denied = Some(Err(GmailError::OAuth(
            "Google returned access_denied".into(),
        )));
        assert_eq!(finished(&denied), Finished::Denied);
        let odd = Some(Err(GmailError::OAuth(
            "Google returned <script>x</script>".into(),
        )));
        assert_eq!(finished(&odd), Finished::Failed);
        assert_eq!(finished(&None), Finished::Nothing);
    }
}
