use mailrs_graph::{
    Granted, GraphError, Loopback, Me, MicrosoftClient, PERSONAL_TENANT, Pkce, SCOPES, Tenant,
    client_from, parse_redirect,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use url::Url;

const CLIENT: &str = "00000000-0000-0000-0000-000000000000";

#[test]
fn a_build_without_a_client_id_has_no_client() {
    assert!(client_from(None).is_none());
    assert!(client_from(Some("  ")).is_none());
    assert_eq!(client_from(Some(CLIENT)).unwrap().id(), CLIENT);
}

#[test]
fn the_consent_url_asks_for_every_scope_with_pkce() {
    let client = MicrosoftClient::new(CLIENT);
    let pkce = Pkce::generate();
    let url = client
        .authorize_url(
            "http://localhost:4000",
            &pkce,
            "state-1",
            Some("dana@outlook.com"),
        )
        .unwrap();
    let url = Url::parse(&url).unwrap();
    assert_eq!(url.host_str(), Some("login.microsoftonline.com"));
    assert_eq!(url.path(), "/common/oauth2/v2.0/authorize");
    let pairs: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(pairs["client_id"], CLIENT);
    assert_eq!(pairs["redirect_uri"], "http://localhost:4000");
    assert_eq!(pairs["code_challenge"], pkce.challenge);
    assert_eq!(pairs["code_challenge_method"], "S256");
    assert_eq!(pairs["login_hint"], "dana@outlook.com");
    assert_eq!(pairs["prompt"], "select_account");
    assert_eq!(pairs["scope"], SCOPES.join(" "));
    assert!(SCOPES.contains(&"offline_access") && SCOPES.contains(&"openid"));
}

#[test]
fn a_redirect_carries_the_code_only_with_the_right_state() {
    let ok = "GET /?code=abc&state=s1 HTTP/1.1\r\nHost: localhost\r\n\r\n";
    assert_eq!(parse_redirect(ok, "s1").unwrap().unwrap(), "abc");
    let wrong = "GET /?code=abc&state=s2 HTTP/1.1\r\n\r\n";
    assert!(matches!(
        parse_redirect(wrong, "s1"),
        Some(Err(GraphError::OAuth(_)))
    ));
    assert!(parse_redirect("GET /favicon.ico HTTP/1.1\r\n\r\n", "s1").is_none());
}

#[test]
fn an_organization_that_blocks_consent_says_so() {
    let admin = "GET /?error=access_denied&error_description=AADSTS65001%3A+admin+consent&state=s1 HTTP/1.1\r\n\r\n";
    assert!(matches!(
        parse_redirect(admin, "s1"),
        Some(Err(GraphError::AdminApproval))
    ));
    let needs = "GET /?error=consent_required&state=s1 HTTP/1.1\r\n\r\n";
    assert!(matches!(
        parse_redirect(needs, "s1"),
        Some(Err(GraphError::AdminApproval))
    ));
    let no = "GET /?error=access_denied&error_description=AADSTS65004%3A+declined&state=s1 HTTP/1.1\r\n\r\n";
    assert!(matches!(
        parse_redirect(no, "s1"),
        Some(Err(GraphError::Declined))
    ));
}

#[test]
fn the_id_token_tenant_tells_outlook_from_microsoft_365() {
    use base64::Engine;
    let token = |tid: &str| {
        let claims = serde_json::json!({"tid": tid, "preferred_username": "dana@outlook.com"});
        let body = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(claims.to_string());
        format!("e30.{body}.sig")
    };
    let personal = mailrs_graph::IdToken::parse(&token(PERSONAL_TENANT)).unwrap();
    assert_eq!(personal.tenant, Tenant::Personal);
    assert_eq!(personal.tenant.provider_name(), "Outlook");
    let work =
        mailrs_graph::IdToken::parse(&token("72f988bf-86f1-41af-91ab-2d7cd011db47")).unwrap();
    assert_eq!(work.tenant, Tenant::Work);
    assert_eq!(work.tenant.provider_name(), "Microsoft 365");
}

#[test]
fn granted_scopes_read_with_or_without_graphs_prefix() {
    let granted =
        Granted::parse("https://graph.microsoft.com/Mail.ReadWrite Calendars.ReadWrite openid");
    assert!(granted.has("Mail.ReadWrite") && granted.has("mail.readwrite"));
    assert!(granted.has("Calendars.ReadWrite"));
    assert!(!granted.has("Contacts.ReadWrite"));
    assert!(granted.reads_mail());
}

#[test]
fn an_address_comes_from_mail_or_the_sign_in_name() {
    let personal = Me {
        display_name: None,
        mail: None,
        user_principal_name: Some("Dana@Outlook.com".into()),
    };
    assert_eq!(personal.address().as_deref(), Some("dana@outlook.com"));
    let work = Me {
        display_name: None,
        mail: Some("d.reyes@contoso.com".into()),
        user_principal_name: Some("dreyes@contoso.onmicrosoft.com".into()),
    };
    assert_eq!(work.address().as_deref(), Some("d.reyes@contoso.com"));
}

#[tokio::test]
async fn the_loopback_answers_on_localhost() {
    let loopback = Loopback::bind().await.unwrap();
    assert!(loopback.redirect_uri.starts_with("http://localhost:"));
    let port: u16 = loopback
        .redirect_uri
        .rsplit(':')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    let waiting = tokio::spawn(loopback.wait_for_code("s1"));
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    stream
        .write_all(b"GET /?code=c1&state=s1 HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();
    let mut reply = String::new();
    stream.read_to_string(&mut reply).await.unwrap();
    assert!(reply.starts_with("HTTP/1.1 200"));
    // The shared, branded page Gmail ends on too, not a bare line.
    assert!(
        reply.contains("Signed in to Penguin Mail") && reply.contains("<svg"),
        "{reply}"
    );
    assert_eq!(waiting.await.unwrap().unwrap(), "c1");
}

#[tokio::test]
async fn a_refused_refresh_token_needs_a_new_sign_in() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "error": "invalid_grant", "error_description": "AADSTS70008: expired"
        })))
        .mount(&server)
        .await;
    let client = MicrosoftClient::new(CLIENT).with_endpoints(
        format!("{}/authorize", server.uri()),
        format!("{}/token", server.uri()),
    );
    assert!(matches!(
        client.refresh("old").await,
        Err(GraphError::NeedsReauth)
    ));
}
