//! Google's OAuth 2.0 installed-app flow: PKCE, the token endpoint, and a
//! loopback listener that receives the browser redirect.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use url::Url;

use crate::GmailError;

pub const GMAIL_SCOPE: &str = "https://www.googleapis.com/auth/gmail.modify";
pub const GOOGLE_AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
pub const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

#[derive(Debug, Clone)]
pub struct AccessToken {
    pub token: String,
    pub expires_at: Instant,
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
        Pkce { verifier, challenge }
    }
}

/// A URL-safe string built from `bytes` random bytes.
pub fn random_token(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::fill(&mut buf[..]);
    URL_SAFE_NO_PAD.encode(buf)
}

#[derive(Debug, Clone)]
pub struct OAuthClient {
    http: reqwest::Client,
    client_id: String,
    client_secret: String,
    auth_url: String,
    token_url: String,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
    refresh_token: Option<String>,
}

impl TokenResponse {
    fn access(&self) -> AccessToken {
        AccessToken {
            token: self.access_token.clone(),
            expires_at: Instant::now() + Duration::from_secs(self.expires_in),
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
        }
    }

    /// Points the client at other endpoints. Tests use this.
    pub fn with_endpoints(mut self, auth_url: impl Into<String>, token_url: impl Into<String>) -> Self {
        self.auth_url = auth_url.into();
        self.token_url = token_url.into();
        self
    }

    pub(crate) fn http(&self) -> &reqwest::Client {
        &self.http
    }

    pub fn authorize_url(&self, redirect_uri: &str, pkce: &Pkce, state: &str) -> Result<String, GmailError> {
        let mut url = Url::parse(&self.auth_url)
            .map_err(|e| GmailError::OAuth(format!("bad authorization URL {}: {e}", self.auth_url)))?;
        url.query_pairs_mut()
            .append_pair("client_id", &self.client_id)
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("response_type", "code")
            .append_pair("scope", GMAIL_SCOPE)
            .append_pair("code_challenge", &pkce.challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", state)
            .append_pair("access_type", "offline")
            .append_pair("prompt", "consent");
        Ok(url.into())
    }

    pub async fn exchange_code(&self, code: &str, redirect_uri: &str, pkce: &Pkce) -> Result<Tokens, GmailError> {
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
        Ok(Tokens { access: response.access(), refresh_token })
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
            return response.json().await.map_err(|e| GmailError::Decode(e.to_string()));
        }
        let body = response.text().await.unwrap_or_default();
        Err(match status {
            400 | 401 if body.contains("invalid_grant") => GmailError::NeedsReauth,
            429 => GmailError::RateLimited { retry_after: None },
            _ => GmailError::Http { status, body },
        })
    }
}

/// Receives Google's redirect on 127.0.0.1 during consent.
pub struct LoopbackListener {
    listener: TcpListener,
    pub redirect_uri: String,
}

impl LoopbackListener {
    /// Listens on an ephemeral port.
    pub async fn bind() -> Result<Self, GmailError> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.map_err(io_error)?;
        let port = listener.local_addr().map_err(io_error)?.port();
        Ok(LoopbackListener { listener, redirect_uri: format!("http://127.0.0.1:{port}") })
    }

    /// Answers requests until one carries the authorization code or an error.
    pub async fn wait_for_code(self, expected_state: &str) -> Result<String, GmailError> {
        loop {
            let (mut stream, _) = self.listener.accept().await.map_err(io_error)?;
            let mut buf = vec![0u8; 8192];
            let n = stream.read(&mut buf).await.map_err(io_error)?;
            let outcome = parse_redirect(&String::from_utf8_lossy(&buf[..n]), expected_state);
            let (status, message) = match &outcome {
                Some(Ok(_)) => ("200 OK", "mailrs is authorized. You can close this tab."),
                Some(Err(_)) => ("400 Bad Request", "Authorization failed. The terminal has the details."),
                None => ("404 Not Found", "Not found."),
            };
            let body = format!("<!doctype html><meta charset=utf-8><title>mailrs</title><p>{message}</p>");
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
        return Some(Err(GmailError::OAuth("the redirect's state did not match; try again".into())));
    }
    Some(Ok(code.clone()))
}
