use std::collections::HashMap;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use mailrs_gmail::{
    BUILT_IN_SCOPES, CALENDAR_LIST_SCOPE, CALENDAR_LIST_WRITE_SCOPE, CALENDAR_SCOPE,
    CALENDARS_SCOPE, CONTACTS_SCOPE, CONTACTS_WRITE_SCOPE, DELETE_SCOPE, DRIVE_FILE_SCOPE,
    FULL_SCOPES, GMAIL_SCOPE, GmailError, Granted, IdToken, LoopbackListener, OAuthClient, Pkce,
    SETTINGS_SCOPE, ScopeList, authorize, parse_redirect,
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
            .authorize_url("http://127.0.0.1:5000", &pkce, "st")
            .unwrap(),
    )
    .unwrap();
    let q: HashMap<String, String> = url.query_pairs().into_owned().collect();
    assert_eq!(url.host_str(), Some("accounts.google.com"));
    assert_eq!(q["client_id"], "cid");
    assert_eq!(q["redirect_uri"], "http://127.0.0.1:5000");
    assert_eq!(q["response_type"], "code");
    assert_eq!(q["scope"], FULL_SCOPES.join(" "));
    assert_eq!(q["include_granted_scopes"], "true");
    assert_eq!(q["code_challenge"], pkce.challenge);
    assert_eq!(q["code_challenge_method"], "S256");
    assert_eq!(q["state"], "st");
    assert_eq!(q["access_type"], "offline");
    assert_eq!(q["prompt"], "consent");
}

/// The owner decided every scope goes in one consent: sign-in no longer
/// asks for settings alone and leaves the rest for later, one at a time.
#[test]
fn sign_in_asks_for_every_scope_at_once() {
    let client = OAuthClient::new("cid", "secret");
    let pkce = Pkce::generate();
    let url = url::Url::parse(
        &client
            .authorize_url("http://127.0.0.1:5000", &pkce, "st")
            .unwrap(),
    )
    .unwrap();
    let scope = url
        .query_pairs()
        .into_owned()
        .collect::<HashMap<String, String>>()["scope"]
        .clone();
    assert_eq!(scope, FULL_SCOPES.join(" "));
    for asked in [
        DELETE_SCOPE,
        SETTINGS_SCOPE,
        CONTACTS_WRITE_SCOPE,
        CALENDAR_SCOPE,
    ] {
        assert!(scope.contains(asked), "{asked} missing from {scope}");
    }
}

/// Google's verification team asks for least privilege: `mail.google.com`
/// already covers `gmail.modify`, `contacts` already covers
/// `contacts.readonly`, and `calendar.calendarlist` already covers
/// `calendar.calendarlist.readonly`, so sign-in leaves the narrower
/// scopes out.
#[test]
fn an_own_client_asks_for_seven_scopes() {
    assert_eq!(FULL_SCOPES.len(), 7);
    let scope = FULL_SCOPES.join(" ");
    for asked in [
        DELETE_SCOPE,
        SETTINGS_SCOPE,
        CONTACTS_WRITE_SCOPE,
        CALENDAR_SCOPE,
        CALENDAR_LIST_WRITE_SCOPE,
        CALENDARS_SCOPE,
        DRIVE_FILE_SCOPE,
    ] {
        assert!(FULL_SCOPES.contains(&asked), "{asked} missing from {scope}");
    }
    for covered in [GMAIL_SCOPE, CONTACTS_SCOPE, CALENDAR_LIST_SCOPE] {
        assert!(
            !FULL_SCOPES.contains(&covered),
            "{covered} should not be asked"
        );
    }
}

/// An account that granted the old read-only calendar list still reads
/// the list, and one that granted the new list scope reads it too.
#[test]
fn the_calendar_list_scope_covers_the_read_only_one() {
    let old = Granted::parse(CALENDAR_LIST_SCOPE);
    assert!(old.has(CALENDAR_LIST_SCOPE));
    assert!(!old.has(CALENDAR_LIST_WRITE_SCOPE));
    let new = Granted::parse(CALENDAR_LIST_WRITE_SCOPE);
    assert!(
        new.has(CALENDAR_LIST_SCOPE),
        "calendar.calendarlist covers the read-only list"
    );
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
async fn an_exchange_keeps_the_scopes_google_granted() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=authorization_code"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "at", "expires_in": 3599, "refresh_token": "rt",
            "scope": format!("{GMAIL_SCOPE} {SETTINGS_SCOPE}"),
        })))
        .mount(&server)
        .await;
    let tokens = client_for(&server)
        .exchange_code("c1", "http://127.0.0.1:1", &Pkce::generate())
        .await
        .unwrap();
    assert_eq!(
        tokens.access.granted,
        Some(Granted::parse(&format!("{GMAIL_SCOPE} {SETTINGS_SCOPE}")))
    );
}

#[tokio::test]
async fn a_refresh_keeps_the_scopes_google_granted() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "at-2", "expires_in": 3599,
            "scope": format!("{GMAIL_SCOPE} {SETTINGS_SCOPE} {CALENDAR_SCOPE}"),
        })))
        .mount(&server)
        .await;
    let token = client_for(&server).refresh("rt").await.unwrap();
    assert_eq!(
        token.granted,
        Some(Granted::parse(&format!(
            "{GMAIL_SCOPE} {SETTINGS_SCOPE} {CALENDAR_SCOPE}"
        )))
    );
}

#[tokio::test]
async fn a_token_answer_without_a_scope_leaves_it_unknown() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"access_token": "at-2", "expires_in": 3599})),
        )
        .mount(&server)
        .await;
    let token = client_for(&server).refresh("rt").await.unwrap();
    assert_eq!(token.granted, None);
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

#[tokio::test]
async fn a_token_answer_past_the_cap_is_refused() {
    let server = MockServer::start().await;
    let padding = "a".repeat(200 << 10);
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"access_token": "at", "expires_in": 3599, "junk": padding})),
        )
        .mount(&server)
        .await;
    let result = client_for(&server).refresh("rt").await;
    assert!(
        matches!(result, Err(GmailError::TooLarge { .. })),
        "{result:?}"
    );
}

#[tokio::test]
async fn a_refusal_with_an_enormous_body_is_not_read_whole() {
    let server = MockServer::start().await;
    let padding = format!("invalid_grant {}", "a".repeat(200 << 10));
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(400).set_body_string(padding))
        .mount(&server)
        .await;
    let result = client_for(&server).refresh("rt").await;
    assert!(
        matches!(result, Err(GmailError::Http { status: 400, .. })),
        "{result:?}"
    );
}

const RESTRICTED: [&str; 3] = [DELETE_SCOPE, GMAIL_SCOPE, SETTINGS_SCOPE];

fn jwt(claims: serde_json::Value) -> String {
    format!("e30.{}.sig", URL_SAFE_NO_PAD.encode(claims.to_string()))
}

#[test]
fn the_built_in_list_holds_no_restricted_scope() {
    for scope in RESTRICTED {
        assert!(!BUILT_IN_SCOPES.contains(&scope), "{scope} is restricted");
    }
    assert_eq!(&BUILT_IN_SCOPES[..2], &["openid", "email"]);
}

#[test]
fn the_built_in_client_asks_for_its_own_list() {
    let client = OAuthClient::new("cid", "secret").asking(ScopeList::BuiltIn);
    let url = url::Url::parse(
        &client
            .authorize_url("http://127.0.0.1:5000", &Pkce::generate(), "st")
            .unwrap(),
    )
    .unwrap();
    let q: HashMap<String, String> = url.query_pairs().into_owned().collect();
    assert_eq!(q["scope"], BUILT_IN_SCOPES.join(" "));
}

#[test]
fn a_new_client_asks_for_the_full_list() {
    let client = OAuthClient::new("cid", "secret");
    assert_eq!(client.scopes(), &FULL_SCOPES);
    assert_eq!(client.scope_list(), ScopeList::Full);
    assert!(ScopeList::Full.asks_mail());
    assert!(!ScopeList::BuiltIn.asks_mail());
}

#[test]
fn an_id_token_with_a_string_verified_claim_counts() {
    let token = jwt(json!({"email": "Dana@Gmail.com", "email_verified": "true"}));
    let parsed = IdToken::parse(&token).unwrap();
    assert!(parsed.email_verified);
    assert_eq!(parsed.email, "Dana@Gmail.com");
}

#[test]
fn an_id_token_with_a_bool_verified_claim_counts() {
    let token = jwt(json!({"email": "dana@gmail.com", "email_verified": true}));
    assert!(IdToken::parse(&token).unwrap().email_verified);
}

#[test]
fn an_id_token_without_the_verified_claim_is_refused() {
    let token = jwt(json!({"email": "dana@gmail.com"}));
    assert!(!IdToken::parse(&token).unwrap().email_verified);
}

#[test]
fn an_id_token_without_an_email_does_not_parse() {
    assert!(IdToken::parse(&jwt(json!({"sub": "1"}))).is_none());
}

#[test]
fn an_id_token_hides_the_address_in_debug_output() {
    let token = IdToken::parse(&jwt(
        json!({"email": "dana@gmail.com", "email_verified": true}),
    ))
    .unwrap();
    assert!(!format!("{token:?}").contains("dana@gmail.com"));
}

/// Drives `authorize` against a mock token endpoint that answers with the
/// five sensitive scopes and `id_token`. Returns the result and the
/// number of profile calls the mock saw.
async fn built_in_authorize(
    id_token: Option<serde_json::Value>,
    list: ScopeList,
    scope: &str,
) -> Result<mailrs_gmail::Authorized, GmailError> {
    let server = MockServer::start().await;
    let mut body = json!({
        "access_token": "at-1", "expires_in": 3600, "refresh_token": "rt-new",
        "scope": scope,
    });
    if let Some(claims) = id_token {
        body["id_token"] = json!(jwt(claims));
    }
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;
    let profile = Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/profile"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"emailAddress": "profile@example.com", "historyId": "5"})),
        );
    if list == ScopeList::BuiltIn {
        profile.expect(0).mount(&server).await;
    } else {
        profile.mount(&server).await;
    }
    let api = format!("{}/gmail/v1/users/me", server.uri());
    authorize(&client_for(&server).asking(list), &api, |consent_url| {
        let url = url::Url::parse(consent_url).unwrap();
        let q: HashMap<String, String> = url.query_pairs().into_owned().collect();
        let redirect = q["redirect_uri"].trim_start_matches("http://").to_string();
        let state = q["state"].clone();
        tokio::spawn(async move {
            let mut stream = tokio::net::TcpStream::connect(&redirect).await.unwrap();
            let request =
                format!("GET /?code=c&state={state} HTTP/1.1\r\nHost: {redirect}\r\n\r\n");
            stream.write_all(request.as_bytes()).await.unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).await.unwrap();
        });
    })
    .await
}

fn sensitive() -> String {
    BUILT_IN_SCOPES[2..].join(" ")
}

#[tokio::test]
async fn built_in_authorize_takes_the_address_from_the_id_token() {
    let claims = json!({"email": "dana@gmail.com", "email_verified": true});
    let authorized = built_in_authorize(Some(claims), ScopeList::BuiltIn, &sensitive())
        .await
        .unwrap();
    assert_eq!(authorized.email, "dana@gmail.com");
    assert_eq!(authorized.refresh_token, "rt-new");
}

#[tokio::test]
async fn an_id_token_without_email_is_refused() {
    let result =
        built_in_authorize(Some(json!({"sub": "1"})), ScopeList::BuiltIn, &sensitive()).await;
    assert!(matches!(result, Err(GmailError::NoAddress)), "{result:?}");
}

#[tokio::test]
async fn a_consent_with_no_id_token_is_refused() {
    let result = built_in_authorize(None, ScopeList::BuiltIn, &sensitive()).await;
    assert!(matches!(result, Err(GmailError::NoAddress)), "{result:?}");
}

#[tokio::test]
async fn an_unverified_address_is_refused() {
    let claims = json!({"email": "dana@gmail.com", "email_verified": false});
    let result = built_in_authorize(Some(claims), ScopeList::BuiltIn, &sensitive()).await;
    assert!(matches!(result, Err(GmailError::NoAddress)), "{result:?}");
}

#[tokio::test]
async fn the_full_list_still_refuses_a_consent_without_mail() {
    let result = built_in_authorize(None, ScopeList::Full, &sensitive()).await;
    assert!(
        matches!(result, Err(GmailError::MailNotGranted)),
        "{result:?}"
    );
}

#[test]
fn a_hinted_authorize_url_names_the_address_and_an_unhinted_one_does_not() {
    let client = OAuthClient::new("cid", "secret");
    let pkce = Pkce::generate();
    let query = |hint: Option<&str>| -> HashMap<String, String> {
        let url = client
            .authorize_url_hinted("http://127.0.0.1:5000", &pkce, "st", hint)
            .unwrap();
        url::Url::parse(&url)
            .unwrap()
            .query_pairs()
            .into_owned()
            .collect()
    };
    assert_eq!(
        query(Some("dana@example.com"))["login_hint"],
        "dana@example.com"
    );
    assert!(!query(None).contains_key("login_hint"));
}
