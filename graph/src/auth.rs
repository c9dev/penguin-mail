//! Microsoft's sign-in for a desktop app with no secret: the
//! authorization code flow with PKCE, the system browser, and a loopback
//! on `localhost` with a random port, which Microsoft matches against the
//! `http://localhost` redirect the app registers. The token endpoint and
//! the consent page are the only other host a token or a code goes to.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use url::Url;

use mailrs_domain::sign_in_page::{self, Finished};

use crate::error::GraphError;
use crate::http::Graph;

pub const AUTHORIZE_URL: &str = "https://login.microsoftonline.com/common/oauth2/v2.0/authorize";
pub const TOKEN_URL: &str = "https://login.microsoftonline.com/common/oauth2/v2.0/token";

/// Every scope sign-in asks for, in one consent. All are delegated and
/// need no admin consent on a personal account. `openid` brings the id
/// token whose tenant tells Outlook from Microsoft 365; `offline_access`
/// brings the refresh token.
pub const SCOPES: [&str; 8] = [
    "openid",
    "offline_access",
    "User.Read",
    "Mail.ReadWrite",
    "Mail.Send",
    "MailboxSettings.ReadWrite",
    "Calendars.ReadWrite",
    "Contacts.ReadWrite",
];

/// The tenant id every personal Microsoft account signs in under.
pub const PERSONAL_TENANT: &str = "9188040d-6c67-4c5b-b112-36a304b66dad";

/// Graph's prefix on a scope in a token answer, which the app's own
/// names leave off.
const GRAPH_SCOPE_PREFIX: &str = "https://graph.microsoft.com/";

/// Who serves an account: a personal Microsoft account, or an
/// organization's Microsoft 365.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tenant {
    Personal,
    Work,
}

impl Tenant {
    /// The provider name the account keeps. Brands, so not translated.
    pub fn provider_name(self) -> &'static str {
        match self {
            Tenant::Personal => "Outlook",
            Tenant::Work => "Microsoft 365",
        }
    }
}

/// The scopes a token carries, in lower case and without Graph's prefix,
/// since Microsoft reads scope names without regard to case.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Granted(BTreeSet<String>);

impl Granted {
    pub fn parse(scope: &str) -> Granted {
        Granted(
            scope
                .split_whitespace()
                .map(|s| {
                    s.strip_prefix(GRAPH_SCOPE_PREFIX)
                        .unwrap_or(s)
                        .to_ascii_lowercase()
                })
                .collect(),
        )
    }

    pub fn has(&self, scope: &str) -> bool {
        self.0.contains(&scope.to_ascii_lowercase())
    }

    /// Sorted and joined with spaces, as the store keeps them.
    pub fn to_scope(&self) -> String {
        self.0.iter().cloned().collect::<Vec<_>>().join(" ")
    }

    pub fn reads_mail(&self) -> bool {
        self.has("Mail.ReadWrite")
    }
}

#[derive(Debug, Clone)]
pub struct AccessToken {
    pub token: String,
    pub expires_at: Instant,
    pub granted: Option<Granted>,
}

impl AccessToken {
    pub fn valid_for(&self, margin: Duration) -> bool {
        self.expires_at > Instant::now() + margin
    }
}

/// What the id token says: the tenant and the name the person signed in
/// with. Read without checking its signature, which is fine for naming
/// the provider and nothing else: no decision about access rests on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdToken {
    pub tenant: Tenant,
    pub username: Option<String>,
}

impl IdToken {
    pub fn parse(token: &str) -> Option<IdToken> {
        let payload = token.split('.').nth(1)?;
        let bytes = URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
        let claims: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
        let tenant = match claims["tid"].as_str()? {
            PERSONAL_TENANT => Tenant::Personal,
            _ => Tenant::Work,
        };
        Some(IdToken {
            tenant,
            username: claims["preferred_username"].as_str().map(str::to_string),
        })
    }
}

#[derive(Debug, Clone)]
pub struct Tokens {
    pub access: AccessToken,
    /// Microsoft sends a new one with most answers and the old one stops
    /// working some time after.
    pub refresh_token: Option<String>,
    pub id_token: Option<IdToken>,
}

#[derive(Debug, Clone)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl Pkce {
    pub fn generate() -> Pkce {
        let verifier = random_token(32);
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        Pkce {
            verifier,
            challenge,
        }
    }
}

pub fn random_token(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::fill(&mut buf[..]);
    URL_SAFE_NO_PAD.encode(buf)
}

#[derive(Deserialize)]
struct TokenAnswer {
    access_token: String,
    expires_in: u64,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
}

impl TokenAnswer {
    fn tokens(self) -> Tokens {
        Tokens {
            access: AccessToken {
                token: self.access_token,
                expires_at: Instant::now() + Duration::from_secs(self.expires_in),
                granted: self.scope.as_deref().map(Granted::parse),
            },
            refresh_token: self.refresh_token,
            id_token: self.id_token.as_deref().and_then(IdToken::parse),
        }
    }
}

/// The app's registration with Microsoft: a public client, so no secret.
#[derive(Clone)]
pub struct MicrosoftClient {
    http: reqwest::Client,
    client_id: String,
    authorize_url: String,
    token_url: String,
}

// By hand, so the id never reaches a log line.
impl std::fmt::Debug for MicrosoftClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MicrosoftClient")
            .field("token_url", &self.token_url)
            .finish_non_exhaustive()
    }
}

impl MicrosoftClient {
    pub fn new(client_id: impl Into<String>) -> MicrosoftClient {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("the TLS backend initializes");
        MicrosoftClient {
            http,
            client_id: client_id.into(),
            authorize_url: AUTHORIZE_URL.into(),
            token_url: TOKEN_URL.into(),
        }
    }

    /// Points the client at other endpoints. Tests use this.
    pub fn with_endpoints(
        mut self,
        authorize_url: impl Into<String>,
        token_url: impl Into<String>,
    ) -> Self {
        self.authorize_url = authorize_url.into();
        self.token_url = token_url.into();
        self
    }

    pub fn id(&self) -> &str {
        &self.client_id
    }

    /// Microsoft's consent page for every [`SCOPES`] entry. `login_hint`
    /// fills in the address the person typed in Add Account.
    pub fn authorize_url(
        &self,
        redirect_uri: &str,
        pkce: &Pkce,
        state: &str,
        login_hint: Option<&str>,
    ) -> Result<String, GraphError> {
        let mut url = Url::parse(&self.authorize_url)
            .map_err(|e| GraphError::OAuth(format!("bad authorization URL: {e}")))?;
        {
            let mut pairs = url.query_pairs_mut();
            pairs
                .append_pair("client_id", &self.client_id)
                .append_pair("response_type", "code")
                .append_pair("redirect_uri", redirect_uri)
                .append_pair("response_mode", "query")
                .append_pair("scope", &SCOPES.join(" "))
                .append_pair("state", state)
                .append_pair("code_challenge", &pkce.challenge)
                .append_pair("code_challenge_method", "S256")
                .append_pair("prompt", "select_account");
            if let Some(hint) = login_hint {
                pairs.append_pair("login_hint", hint);
            }
        }
        Ok(url.into())
    }

    pub async fn exchange_code(
        &self,
        code: &str,
        redirect_uri: &str,
        pkce: &Pkce,
    ) -> Result<Tokens, GraphError> {
        self.post_token(&[
            ("grant_type", "authorization_code"),
            ("client_id", &self.client_id),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("code_verifier", &pkce.verifier),
            ("scope", &SCOPES.join(" ")),
        ])
        .await
    }

    pub async fn refresh(&self, refresh_token: &str) -> Result<Tokens, GraphError> {
        self.post_token(&[
            ("grant_type", "refresh_token"),
            ("client_id", &self.client_id),
            ("refresh_token", refresh_token),
            ("scope", &SCOPES.join(" ")),
        ])
        .await
    }

    async fn post_token(&self, form: &[(&str, &str)]) -> Result<Tokens, GraphError> {
        let response = self
            .http
            .post(&self.token_url)
            .form(form)
            .send()
            .await
            .map_err(|e| GraphError::Network(e.to_string()))?;
        let status = response.status().as_u16();
        let body = response
            .bytes()
            .await
            .map_err(|e| GraphError::Network(e.to_string()))?;
        if (200..300).contains(&status) {
            let answer: TokenAnswer =
                serde_json::from_slice(&body).map_err(|e| GraphError::Decode(e.to_string()))?;
            return Ok(answer.tokens());
        }
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
        let error = parsed["error"].as_str().unwrap_or_default();
        Err(match (status, error) {
            (_, "invalid_grant" | "interaction_required" | "consent_required") => {
                GraphError::NeedsReauth
            }
            (429 | 503, _) => GraphError::Throttled { retry_after: None },
            _ => GraphError::OAuth(format!(
                "{status} {error}: {}",
                parsed["error_description"].as_str().unwrap_or_default()
            )),
        })
    }
}

/// The client a build signs in with, from one value; blank means none, as
/// GitHub passes a secret it does not hold.
pub fn client_from(id: Option<&str>) -> Option<MicrosoftClient> {
    let id = id.map(str::trim).filter(|v| !v.is_empty())?;
    Some(MicrosoftClient::new(id))
}

/// The project's client, compiled in from `PENGUIN_MAIL_MICROSOFT_CLIENT_ID`,
/// which the release workflow fills from the `MICROSOFT_CLIENT_ID` secret.
pub fn built_in_client() -> Option<MicrosoftClient> {
    client_from(option_env!("PENGUIN_MAIL_MICROSOFT_CLIENT_ID"))
}

/// Receives Microsoft's redirect on `localhost` during consent. It
/// listens on 127.0.0.1 and, where the system has IPv6, on ::1 at the
/// same port, since a browser may try either for `localhost`.
pub struct Loopback {
    v4: TcpListener,
    v6: Option<TcpListener>,
    pub redirect_uri: String,
}

impl Loopback {
    pub async fn bind() -> Result<Loopback, GraphError> {
        let v4 = TcpListener::bind(("127.0.0.1", 0)).await.map_err(io)?;
        let port = v4.local_addr().map_err(io)?.port();
        let v6 = TcpListener::bind(("::1", port)).await.ok();
        Ok(Loopback {
            v4,
            v6,
            redirect_uri: format!("http://localhost:{port}"),
        })
    }

    /// Answers requests until one carries the code or an error.
    pub async fn wait_for_code(self, state: &str) -> Result<String, GraphError> {
        loop {
            let accepted = match &self.v6 {
                Some(v6) => tokio::select! {
                    a = self.v4.accept() => a,
                    b = v6.accept() => b,
                },
                None => self.v4.accept().await,
            };
            let (mut stream, _) = accepted.map_err(io)?;
            let mut buf = vec![0u8; 8192];
            let n = stream.read(&mut buf).await.map_err(io)?;
            let outcome = parse_redirect(&String::from_utf8_lossy(&buf[..n]), state);
            let finished = match &outcome {
                Some(Ok(_)) => Finished::Signed,
                Some(Err(GraphError::AdminApproval | GraphError::Declined)) => Finished::Denied,
                Some(Err(_)) => Finished::Failed,
                None => Finished::Nothing,
            };
            let (status, body) = sign_in_page::page(finished);
            let reply = format!(
                "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(reply.as_bytes()).await;
            if let Some(result) = outcome {
                return result;
            }
        }
    }
}

fn io(err: std::io::Error) -> GraphError {
    GraphError::OAuth(format!("loopback listener: {err}"))
}

/// The code out of a raw HTTP request, `None` for a request that is not
/// the redirect. AADSTS65001 and `consent_required` mean the organization
/// wants its administrator to approve the app; AADSTS65004 means the
/// person said no.
pub fn parse_redirect(request: &str, state: &str) -> Option<Result<String, GraphError>> {
    let target = request.lines().next()?.split_whitespace().nth(1)?;
    let url = Url::parse(&format!("http://localhost{target}")).ok()?;
    let params: HashMap<String, String> = url.query_pairs().into_owned().collect();
    if let Some(error) = params.get("error") {
        let described = params
            .get("error_description")
            .map(String::as_str)
            .unwrap_or_default();
        return Some(Err(match error.as_str() {
            "consent_required" => GraphError::AdminApproval,
            _ if described.contains("AADSTS65001") || described.contains("AADSTS90094") => {
                GraphError::AdminApproval
            }
            _ if described.contains("AADSTS65004") => GraphError::Declined,
            other => GraphError::OAuth(format!("Microsoft returned {other}")),
        }));
    }
    let code = params.get("code")?;
    if params.get("state").map(String::as_str) != Some(state) {
        return Some(Err(GraphError::OAuth(
            "the redirect's state did not match; try again".into(),
        )));
    }
    Some(Ok(code.clone()))
}

type Rotated = Box<dyn Fn(String) + Send + Sync>;
type GrantedHook = Box<dyn Fn(&Granted) + Send + Sync>;

struct Held {
    refresh_token: String,
    access: Option<AccessToken>,
}

/// One signed-in account's tokens: the refresh token, the access token of
/// the moment, and what to do when Microsoft rotates the one or changes
/// the scopes of the other. One refresh runs at a time.
pub struct Session {
    client: MicrosoftClient,
    held: tokio::sync::Mutex<Held>,
    granted: Mutex<Option<Granted>>,
    on_rotated: Option<Rotated>,
    on_granted: Option<GrantedHook>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session").finish_non_exhaustive()
    }
}

impl Session {
    pub fn new(client: MicrosoftClient, refresh_token: impl Into<String>) -> Session {
        Session {
            client,
            held: tokio::sync::Mutex::new(Held {
                refresh_token: refresh_token.into(),
                access: None,
            }),
            granted: Mutex::new(None),
            on_rotated: None,
            on_granted: None,
        }
    }

    /// A session that starts from a code exchange's tokens, so the first
    /// call needs no refresh.
    pub fn signed_in(client: MicrosoftClient, tokens: Tokens) -> Session {
        let granted = tokens.access.granted.clone();
        let session = Session::new(client, tokens.refresh_token.unwrap_or_default());
        *session
            .granted
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = granted;
        session
            .held
            .try_lock()
            .expect("a new session is not shared")
            .access = Some(tokens.access);
        session
    }

    /// Seeds what the store last recorded, until a refresh says otherwise.
    pub fn with_granted(self, granted: Option<Granted>) -> Self {
        *self.granted.lock().unwrap_or_else(PoisonError::into_inner) = granted;
        self
    }

    /// Runs `f` with each new refresh token Microsoft hands back. It must
    /// not block: the caller saves the token off the runtime.
    pub fn on_rotated(mut self, f: impl Fn(String) + Send + Sync + 'static) -> Self {
        self.on_rotated = Some(Box::new(f));
        self
    }

    /// Runs `f` when a refresh reports scopes other than the ones held.
    pub fn on_granted(mut self, f: impl Fn(&Granted) + Send + Sync + 'static) -> Self {
        self.on_granted = Some(Box::new(f));
        self
    }

    pub fn granted(&self) -> Option<Granted> {
        self.granted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// An access token with a minute or more to live, refreshed first when
    /// `force` or when the held one is about to expire.
    pub async fn bearer(&self, force: bool) -> Result<String, GraphError> {
        let mut held = self.held.lock().await;
        if !force
            && let Some(access) = &held.access
            && access.valid_for(Duration::from_secs(60))
        {
            return Ok(access.token.clone());
        }
        let tokens = self.client.refresh(&held.refresh_token).await?;
        if let Some(new) = tokens
            .refresh_token
            .clone()
            .filter(|t| *t != held.refresh_token)
        {
            held.refresh_token = new.clone();
            if let Some(rotated) = &self.on_rotated {
                rotated(new);
            }
        }
        if let Some(granted) = tokens.access.granted.clone() {
            let changed = {
                let mut kept = self.granted.lock().unwrap_or_else(PoisonError::into_inner);
                let changed = kept.as_ref() != Some(&granted);
                *kept = Some(granted.clone());
                changed
            };
            if changed && let Some(hook) = &self.on_granted {
                hook(&granted);
            }
        }
        let token = tokens.access.token.clone();
        held.access = Some(tokens.access);
        Ok(token)
    }
}

/// The signed-in person, as `/me` answers.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct Me {
    pub display_name: Option<String>,
    pub mail: Option<String>,
    pub user_principal_name: Option<String>,
}

impl Me {
    /// The mail address, in lower case: `mail` where the tenant sets it,
    /// else the sign-in name, which is the address for a personal account.
    pub fn address(&self) -> Option<String> {
        self.mail
            .as_deref()
            .or(self.user_principal_name.as_deref())
            .filter(|a| a.contains('@'))
            .map(str::to_ascii_lowercase)
    }
}

/// What a finished sign-in hands the app.
pub struct Authorized {
    pub email: String,
    pub name: Option<String>,
    pub tenant: Tenant,
    pub refresh_token: String,
    pub granted: Option<Granted>,
}

/// The whole sign-in: the loopback, the consent page through `open`, the
/// code exchange, the address from `/me`, and one call to the Inbox that
/// an on-premises mailbox refuses with `MailboxOnPremises`, so such an
/// account is never added.
pub async fn authorize(
    client: &MicrosoftClient,
    base: &str,
    login_hint: Option<&str>,
    open: impl FnOnce(&str),
) -> Result<Authorized, GraphError> {
    let loopback = Loopback::bind().await?;
    let pkce = Pkce::generate();
    let state = random_token(16);
    let url = client.authorize_url(&loopback.redirect_uri, &pkce, &state, login_hint)?;
    let redirect = loopback.redirect_uri.clone();
    open(&url);
    let code = loopback.wait_for_code(&state).await?;
    let tokens = client.exchange_code(&code, &redirect, &pkce).await?;
    let refresh_token = tokens
        .refresh_token
        .clone()
        .ok_or_else(|| GraphError::OAuth("Microsoft returned no refresh token".into()))?;
    let granted = tokens.access.granted.clone();
    if granted.as_ref().is_some_and(|g| !g.reads_mail()) {
        return Err(GraphError::MailNotGranted);
    }
    let tenant = tokens.id_token.as_ref().map_or(Tenant::Work, |t| t.tenant);
    let graph = Graph::with_base(Arc::new(Session::signed_in(client.clone(), tokens)), base)?;
    let me: Me = graph
        .get("me", &[("$select", "displayName,mail,userPrincipalName")])
        .await?;
    let _: serde_json::Value = graph
        .get("me/mailFolders/inbox", &[("$select", "id")])
        .await?;
    let email = me.address().ok_or_else(|| {
        GraphError::OAuth("Microsoft named no mail address for this account".into())
    })?;
    Ok(Authorized {
        email,
        name: me.display_name,
        tenant,
        refresh_token,
        granted,
    })
}
