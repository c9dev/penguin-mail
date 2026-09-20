use std::collections::HashMap;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use mailrs_gmail::{
    CALENDAR_SCOPE, DELETE_SCOPE, GMAIL_SCOPE, GmailError, LoopbackListener, OAuthClient, Pkce,
    SETTINGS_SCOPE, parse_redirect,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn client_for(server: &MockServer) -> OAuthClient {
    OAuthClient::new("cid", "secret").with_endpoints(
        format!("{}/auth", server.uri()),
        format!("{}/token", server.uri()),
    )
}

#[test]
fn pkce_challenge_is_the_sha256_of_the_verifier() {
    let pkce = Pkce::generate();
    assert_eq!(pkce.verifier.len(), 43);
    assert_eq!(
        pkce.challenge,
        URL_SAFE_NO_PAD.encode(Sha256::digest(pkce.verifier.as_bytes()))
    );
    assert_ne!(Pkce::generate().verifier, pkce.verifier);
}

#[test]
fn authorize_url_carries_pkce_and_offline_access() {
    let client = OAuthClient::new("cid", "secret");
    let pkce = Pkce::generate();
    let url = url::Url::parse(
        &client
            .authorize_url("http://127.0.0.1:5000", &pkce, "st", &[])
            .unwrap(),
    )
    .unwrap();
    let q: HashMap<String, String> = url.query_pairs().into_owned().collect();
    assert_eq!(url.host_str(), Some("accounts.google.com"));
    assert_eq!(q["client_id"], "cid");
    assert_eq!(q["redirect_uri"], "http://127.0.0.1:5000");
    assert_eq!(q["response_type"], "code");
    assert_eq!(q["scope"], format!("{GMAIL_SCOPE} {SETTINGS_SCOPE}"));
    assert_eq!(q["include_granted_scopes"], "true");
    assert_eq!(q["code_challenge"], pkce.challenge);
    assert_eq!(q["code_challenge_method"], "S256");
    assert_eq!(q["state"], "st");
    assert_eq!(q["access_type"], "offline");
    assert_eq!(q["prompt"], "consent");
}

#[test]
fn only_an_asked_for_extra_scope_joins_the_consent_url() {
    let client = OAuthClient::new("cid", "secret");
    let pkce = Pkce::generate();
    let scope = |extra: &[&str]| -> String {
        let url = url::Url::parse(
            &client
                .authorize_url("http://127.0.0.1:5000", &pkce, "st", extra)
                .unwrap(),
        )
        .unwrap();
        url.query_pairs()
            .into_owned()
            .collect::<HashMap<String, String>>()["scope"]
            .clone()
    };
    assert!(!scope(&[]).contains(DELETE_SCOPE), "sign-in leaves it out");
    assert_eq!(
        scope(&[DELETE_SCOPE]),
        format!("{GMAIL_SCOPE} {SETTINGS_SCOPE} {DELETE_SCOPE}")
    );
    assert!(
        !scope(&[]).contains(CALENDAR_SCOPE),
        "answering an invitation is asked for when somebody answers one"
    );
    assert_eq!(
        scope(&[CALENDAR_SCOPE]),
        format!("{GMAIL_SCOPE} {SETTINGS_SCOPE} {CALENDAR_SCOPE}")
    );
    // Asking twice for a scope sign-in already covers changes nothing.
    assert_eq!(scope(&[SETTINGS_SCOPE]), scope(&[]));
}

#[test]
fn redirect_parsing() {
    assert_eq!(
        parse_redirect("GET /?code=abc&state=st HTTP/1.1\r\n", "st")
            .unwrap()
            .unwrap(),
        "abc"
    );
    assert!(matches!(
        parse_redirect("GET /?code=abc&state=zz HTTP/1.1\r\n", "st"),
        Some(Err(GmailError::OAuth(_)))
    ));
    assert!(matches!(
        parse_redirect("GET /?error=access_denied&state=st HTTP/1.1\r\n", "st"),
        Some(Err(GmailError::OAuth(_)))
    ));
    assert!(parse_redirect("GET /favicon.ico HTTP/1.1\r\n", "st").is_none());
    assert!(parse_redirect("", "st").is_none());
}

#[tokio::test]
async fn loopback_listener_returns_the_code_after_ignoring_other_requests() {
    let listener = LoopbackListener::bind().await.unwrap();
    let addr = listener
        .redirect_uri
        .trim_start_matches("http://")
        .to_string();
    let browser = tokio::spawn(async move {
        let mut statuses = Vec::new();
        for target in ["/favicon.ico", "/?code=the-code&state=st"] {
            let mut stream = tokio::net::TcpStream::connect(&addr).await.unwrap();
            stream
                .write_all(format!("GET {target} HTTP/1.1\r\nHost: {addr}\r\n\r\n").as_bytes())
                .await
                .unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).await.unwrap();
            statuses.push(response.lines().next().unwrap_or_default().to_string());
        }
        statuses
    });
    assert_eq!(listener.wait_for_code("st").await.unwrap(), "the-code");
    let statuses = browser.await.unwrap();
    assert_eq!(statuses, vec!["HTTP/1.1 404 Not Found", "HTTP/1.1 200 OK"]);
}

#[tokio::test]
async fn exchange_code_returns_both_tokens() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=authorization_code"))
        .and(body_string_contains("code_verifier="))
        .and(body_string_contains("code=c1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "at", "expires_in": 3599, "refresh_token": "rt", "token_type": "Bearer"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let tokens = client_for(&server)
        .exchange_code("c1", "http://127.0.0.1:1", &Pkce::generate())
        .await
        .unwrap();
    assert_eq!(tokens.access.token, "at");
    assert_eq!(tokens.refresh_token, "rt");
    assert!(
        tokens
            .access
            .valid_for(std::time::Duration::from_secs(3000))
    );
}

#[tokio::test]
async fn exchange_without_a_refresh_token_is_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"access_token": "at", "expires_in": 3599})),
        )
        .mount(&server)
        .await;
    let result = client_for(&server)
        .exchange_code("c1", "http://127.0.0.1:1", &Pkce::generate())
        .await;
    assert!(matches!(result, Err(GmailError::OAuth(_))));
}

#[tokio::test]
async fn refresh_returns_a_new_access_token() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains("refresh_token=rt"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"access_token": "at-2", "expires_in": 3599})),
        )
        .expect(1)
        .mount(&server)
        .await;
    assert_eq!(
        client_for(&server).refresh("rt").await.unwrap().token,
        "at-2"
    );
}

#[tokio::test]
async fn invalid_grant_means_the_account_needs_reauthorizing() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({"error": "invalid_grant"})))
        .mount(&server)
        .await;
    assert!(matches!(
        client_for(&server).refresh("rt").await,
        Err(GmailError::NeedsReauth)
    ));
}
