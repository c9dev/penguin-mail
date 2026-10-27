//! A JMAP account signed in on its server's own page. The server must
//! publish OAuth metadata (RFC 8414) beside its session, with open dynamic
//! registration: each sign-in registers a public client for the loopback
//! redirect it listens on, port and all, and the account keeps its id for
//! refreshing. Fastmail and Stalwart both work this way, so no client id is
//! built in.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use mailrs_discover::JmapProvider;
use mailrs_jmap::oauth::{self, Loopback, Metadata, OAuthClient, Pkce, Tokens};
use mailrs_jmap::{Auth, JmapClient, JmapError};
use mailrs_store::jmap::{JmapAuth, JmapSaved, SavedOAuth};
use mailrs_store::{Db, accounts};

use super::{BrowserSignInError, NewJmap, SignedAs, SignedIn, jmap_signed_in};
use crate::passwords::{PasswordStore, Secrets};
use crate::{Connector, JmapGrant, connect_jmap, now_millis};

/// How long the sign-in waits for the browser to come back: RFC 8252's
/// usual bound for a person at a consent page. The loopback listener
/// closes when it runs out.
pub const BROWSER_WAIT: Duration = Duration::from_secs(600);

/// A JMAP session offered for a browser sign-in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JmapBrowser {
    pub session_url: String,
    /// The table's entry for the provider, when it has one.
    pub provider: Option<JmapProvider>,
}

/// Whether the browser sign-in can run for a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserReady {
    /// The server's metadata, which offers dynamic registration.
    Registers(Metadata),
    NotOffered,
}

/// The sign-in server's OAuth calls. The live one goes to the server over
/// https only; tests answer from memory, so nothing here loosens that.
pub(crate) trait SignInServer: Send + Sync {
    fn metadata(
        &self,
        session_url: &str,
    ) -> impl Future<Output = Result<Metadata, JmapError>> + Send;
    fn register(
        &self,
        metadata: &Metadata,
        redirect: &str,
        scopes: &[String],
    ) -> impl Future<Output = Result<String, JmapError>> + Send;
    fn exchange(
        &self,
        client: &OAuthClient,
        code: &str,
        redirect: &str,
        pkce: &Pkce,
    ) -> impl Future<Output = Result<Tokens, JmapError>> + Send;
    /// Revokes a grant the app will not keep, on a task of its own.
    fn revoke(&self, grant: JmapGrant);
}

struct Live;

impl SignInServer for Live {
    async fn metadata(&self, session_url: &str) -> Result<Metadata, JmapError> {
        oauth::metadata(session_url).await
    }

    async fn register(
        &self,
        metadata: &Metadata,
        redirect: &str,
        scopes: &[String],
    ) -> Result<String, JmapError> {
        oauth::register(metadata, redirect, scopes).await
    }

    async fn exchange(
        &self,
        client: &OAuthClient,
        code: &str,
        redirect: &str,
        pkce: &Pkce,
    ) -> Result<Tokens, JmapError> {
        client.exchange(code, redirect, pkce).await
    }

    fn revoke(&self, grant: JmapGrant) {
        tokio::spawn(grant.revoke());
    }
}

/// Whether the browser sign-in can run for `offer`: the server's metadata
/// must read, pass its checks and name a registration endpoint.
pub async fn browser_ready(offer: &JmapBrowser) -> BrowserReady {
    ready_over(&Live, offer).await
}

async fn ready_over<S: SignInServer>(server: &S, offer: &JmapBrowser) -> BrowserReady {
    match server.metadata(&offer.session_url).await {
        Ok(found) if found.registration_endpoint.is_some() => BrowserReady::Registers(found),
        _ => BrowserReady::NotOffered,
    }
}

/// The scopes to ask for: the table's for a provider it lists, the mail
/// scopes the metadata lists otherwise.
fn scopes_for(offer: &JmapBrowser, metadata: &Metadata) -> Vec<String> {
    match &offer.provider {
        Some(provider) if !provider.oauth_scopes.is_empty() => provider.oauth_scopes.clone(),
        _ => oauth::oauth_scopes(metadata),
    }
}

/// The host a session URL names, as the account's provider name. The
/// metadata read already refused a URL with a user name in it.
fn host_of(session_url: &str) -> &str {
    let rest = session_url
        .split_once("://")
        .map_or(session_url, |(_, rest)| rest);
    rest.split(['/', ':', '?', '#']).next().unwrap_or_default()
}

/// The code from the browser's redirect, or why there is none. The wait
/// ends at `wait` or when `canceled` resolves, and the loopback listener
/// closes with it either way.
async fn redirected(
    loopback: Loopback,
    state: &str,
    metadata: &Metadata,
    canceled: impl Future<Output = ()>,
    wait: Duration,
) -> Result<String, BrowserSignInError> {
    tokio::select! {
        code = tokio::time::timeout(wait, loopback.wait_for_code(state, metadata)) => match code {
            Ok(code) => code.map_err(BrowserSignInError::Jmap),
            Err(_) => Err(BrowserSignInError::GaveUp),
        },
        () = canceled => Err(BrowserSignInError::Canceled),
    }
}

/// What one trip to the consent page needs.
struct Trip<'a, S, O> {
    server: &'a S,
    metadata: &'a Metadata,
    scopes: &'a [String],
    session_url: &'a str,
    address: &'a str,
    open: &'a O,
    wait: Duration,
}

impl<S: SignInServer, O: Fn(&str)> Trip<'_, S, O> {
    /// One consent: the loopback, a client registered for its redirect,
    /// the page through `open`, the redirect and the code exchange straight
    /// after it. Answers the client's id beside the tokens.
    ///
    /// The client names the loopback's exact redirect, port included.
    /// Fastmail takes any port only for a redirect registered with one (it
    /// then stores `http://localhost/`); one registered as
    /// `http://127.0.0.1/` must match exactly, and its page refuses the
    /// request. An exact redirect suits every server, so no client from an
    /// earlier sign-in, which listened on another port, is used again.
    async fn consent(
        &self,
        canceled: impl Future<Output = ()>,
    ) -> Result<(String, Tokens), BrowserSignInError> {
        let loopback = Loopback::bind().await.map_err(BrowserSignInError::Jmap)?;
        let redirect = loopback.redirect_uri.clone();
        let client_id = self
            .server
            .register(self.metadata, &redirect, self.scopes)
            .await
            .map_err(BrowserSignInError::Jmap)?;
        let (pkce, state) = (Pkce::generate(), oauth::random_token(32));
        let url = oauth::authorize_url(
            self.metadata,
            &client_id,
            &redirect,
            self.scopes,
            &pkce,
            &state,
            self.session_url,
            Some(self.address),
        )
        .map_err(BrowserSignInError::Jmap)?;
        (self.open)(&url);
        let code = redirected(loopback, &state, self.metadata, canceled, self.wait).await?;
        let client = OAuthClient {
            client_id,
            token_endpoint: self.metadata.token_endpoint.clone(),
            resource: self.session_url.to_string(),
        };
        let tokens = self
            .server
            .exchange(&client, &code, &redirect, &pkce)
            .await
            .map_err(BrowserSignInError::Jmap)?;
        Ok((client.client_id, tokens))
    }
}

/// The whole sign-in: a client registered for this sign-in's redirect,
/// the consent page through `open`, the code exchange, the session check,
/// then the account kept with its refresh token and its services built.
/// `address`, as the person typed it, fills the server's address field and
/// names the account. The wait for the browser ends after
/// [`BROWSER_WAIT`], or when `canceled` resolves.
/// `dav_password`, when the person gave one, is kept as the app password
/// for a calendar or contacts server that refused the browser sign-in.
pub async fn jmap_in_browser(
    connector: &Connector,
    offer: JmapBrowser,
    address: &str,
    dav_password: Option<String>,
    open: impl Fn(&str),
    canceled: impl Future<Output = ()>,
) -> Result<SignedIn, BrowserSignInError> {
    signing_in(
        &Live,
        connector,
        offer,
        address,
        dav_password,
        open,
        canceled,
        BROWSER_WAIT,
    )
    .await
}

#[expect(
    clippy::too_many_arguments,
    reason = "jmap_in_browser's arguments, plus the server and the wait tests set"
)]
pub(crate) async fn signing_in<S: SignInServer>(
    server: &S,
    connector: &Connector,
    offer: JmapBrowser,
    address: &str,
    dav_password: Option<String>,
    open: impl Fn(&str),
    canceled: impl Future<Output = ()>,
    wait: Duration,
) -> Result<SignedIn, BrowserSignInError> {
    let BrowserReady::Registers(metadata) = ready_over(server, &offer).await else {
        return Err(BrowserSignInError::NotOffered);
    };
    let scopes = scopes_for(&offer, &metadata);
    let trip = Trip {
        server,
        metadata: &metadata,
        scopes: &scopes,
        session_url: &offer.session_url,
        address,
        open: &open,
        wait,
    };
    let (client_id, tokens) = trip.consent(canceled).await?;
    let refresh = tokens
        .refresh_token
        .ok_or(BrowserSignInError::NoRefreshToken)?;
    // The check sends the access token as it came. A session here could
    // refresh and rotate the refresh token before the keyring holds it,
    // and Fastmail revokes a grant whose old refresh token comes back.
    let api = JmapClient::new(&offer.session_url, Auth::Bearer(tokens.access_token))
        .map_err(BrowserSignInError::Jmap)?;
    let checked = mailrs_jmap::check(&api)
        .await
        .map_err(BrowserSignInError::Jmap)?;
    let session = &checked.session;
    let provider_name = match &offer.provider {
        Some(provider) => provider.name.clone(),
        None => host_of(&offer.session_url).to_string(),
    };
    // The account keeps the address the person typed, as a password
    // sign-in does: an alias, or a token account signing in again, then
    // finds its own account rather than making one under the session's.
    let address = match address.trim() {
        "" => checked.address.clone().unwrap_or_default(),
        typed => typed.to_string(),
    };
    // What revoking this grant takes, should the session turn out to be
    // someone else's.
    let refused = JmapGrant {
        session_url: offer.session_url.clone(),
        client_id: client_id.clone(),
        refresh_token: refresh.clone(),
    };
    let new = NewJmap {
        address,
        provider_name,
        saved: JmapSaved {
            labels: session.mailboxes_per_email().is_none_or(|most| most > 1),
            capabilities: session
                .capabilities
                .keys()
                .filter(|c| session.offers(c))
                .cloned()
                .collect(),
            session_url: offer.session_url,
            auth: JmapAuth::OAuth,
            user_name: None,
            oauth: Some(SavedOAuth {
                client_id,
                token_endpoint: metadata.token_endpoint,
            }),
            owner: None,
        },
        secret: refresh,
        dav_password: dav_password.filter(|p| !p.is_empty()),
        signed_as: SignedAs::of(&checked),
    };
    let Connector {
        db,
        config,
        secrets,
        ..
    } = connector;
    // The keyring takes the refresh token as the account is kept; when it
    // refuses, a new account goes again and the sign-in fails. The new
    // refresh token, and maybe a new client, need a new session:
    // `jmap_signed_in` retires the old one once it keeps the token.
    let address = new.address.clone();
    let account = match jmap_signed_in(db, secrets, new, now_millis()).await {
        Ok(account) => account,
        Err(err) => {
            // The grant belongs to another account, or a keyring or store
            // failure left it unkept: a live refresh token nothing holds is
            // revoked. One an account took before a later step failed is
            // the token it signs in with, and stays.
            if !held_by(db, secrets, &address, &refused.refresh_token).await {
                server.revoke(refused);
            }
            return Err(err.into());
        }
    };
    // A new grant may carry what a calendar server refused before.
    crate::connect::try_the_token_again(db, account.id).await?;
    let window_days = config.engine_config().window_days;
    let services = connect_jmap(db, secrets, &account, window_days).await?;
    Ok(SignedIn {
        account,
        name: None,
        services: crate::Connected::Ready(services),
    })
}

/// Whether the account at `address` keeps `refresh_token` as its secret.
/// A store or keyring that cannot answer says no.
async fn held_by(db: &Db, secrets: &Secrets, address: &str, refresh_token: &str) -> bool {
    let email = address.to_string();
    let Ok(Some(account)) = db
        .read(move |c| accounts::account_by_email(c, &email))
        .await
    else {
        return false;
    };
    let jmap = Arc::clone(&secrets.jmap);
    let kept = tokio::task::spawn_blocking(move || jmap.load(account.id)).await;
    matches!(kept, Ok(Ok(Some(token))) if token == refresh_token)
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use mailrs_jmap::oauth::Tokens;
    use mailrs_store::Db;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::passwords::{PasswordStore, Secrets};

    /// A sign-in server that answers from memory: no request leaves the
    /// test, and the https endpoints in its metadata are never reached.
    struct FakeServer {
        /// What the clients it registers are called: `<names>-1` first.
        names: &'static str,
        registered: AtomicUsize,
        /// The redirect each client was registered with, in order.
        registered_for: Mutex<Vec<String>>,
        /// The redirect each code exchange named, in order.
        exchanged_for: Mutex<Vec<String>>,
        exchanges: Mutex<VecDeque<Result<Tokens, JmapError>>>,
        /// The client and refresh token of each grant it was asked to revoke.
        revoked: Mutex<Vec<(String, String)>>,
    }

    impl FakeServer {
        fn answering(names: &'static str, exchanges: Vec<Result<Tokens, JmapError>>) -> FakeServer {
            FakeServer {
                names,
                registered: AtomicUsize::new(0),
                registered_for: Mutex::new(Vec::new()),
                exchanged_for: Mutex::new(Vec::new()),
                exchanges: Mutex::new(exchanges.into()),
                revoked: Mutex::new(Vec::new()),
            }
        }

        fn registrations(&self) -> usize {
            self.registered.load(Ordering::SeqCst)
        }

        fn revoked(&self) -> Vec<(String, String)> {
            self.revoked.lock().unwrap().clone()
        }
    }

    impl SignInServer for FakeServer {
        async fn metadata(&self, _session_url: &str) -> Result<Metadata, JmapError> {
            Ok(metadata())
        }

        async fn register(
            &self,
            _metadata: &Metadata,
            redirect: &str,
            _scopes: &[String],
        ) -> Result<String, JmapError> {
            self.registered_for
                .lock()
                .unwrap()
                .push(redirect.to_string());
            let n = self.registered.fetch_add(1, Ordering::SeqCst) + 1;
            Ok(format!("{}-{n}", self.names))
        }

        async fn exchange(
            &self,
            client: &OAuthClient,
            code: &str,
            redirect: &str,
            _pkce: &Pkce,
        ) -> Result<Tokens, JmapError> {
            self.exchanged_for
                .lock()
                .unwrap()
                .push(redirect.to_string());
            assert_eq!(
                (code, client.token_endpoint.as_str()),
                ("c1", "https://api.fastmail.com/oauth/refresh")
            );
            self.exchanges
                .lock()
                .unwrap()
                .pop_front()
                .expect("an exchange the test did not expect")
        }

        fn revoke(&self, grant: JmapGrant) {
            self.revoked
                .lock()
                .unwrap()
                .push((grant.client_id, grant.refresh_token));
        }
    }

    fn tokens(refresh: &str) -> Result<Tokens, JmapError> {
        Ok(Tokens {
            access_token: "a1".into(),
            expires_in: 3600,
            refresh_token: Some(refresh.into()),
        })
    }

    /// A JMAP server whose session answers only the access token `a1`,
    /// for me@fastmail.com in mail account A1, who also sends as
    /// alias@fastmail.com.
    async fn jmap_server() -> MockServer {
        let server = MockServer::start().await;
        mount_session(
            &server,
            "a1",
            "me@fastmail.com",
            "A1",
            &["me@fastmail.com", "alias@fastmail.com"],
        )
        .await;
        server
    }

    /// Answers the access token `bearer` with a session that belongs to
    /// `username`, with mail in `account`, whose identities send as
    /// `sends_as`.
    async fn mount_session(
        server: &MockServer,
        bearer: &str,
        username: &str,
        account: &str,
        sends_as: &[&str],
    ) {
        let (mail, submission) = (
            "urn:ietf:params:jmap:mail",
            "urn:ietf:params:jmap:submission",
        );
        let session = serde_json::json!({
            "capabilities": {"urn:ietf:params:jmap:core": {}, mail: {}, submission: {}},
            "accounts": {account: {"name": username, "isPersonal": true, "isReadOnly": false,
                "accountCapabilities": {mail: {}, submission: {}}}},
            "primaryAccounts": {mail: account, submission: account},
            "username": username,
            "apiUrl": format!("{}/api/", server.uri()),
            "downloadUrl": format!("{}/download/{{accountId}}/{{blobId}}/{{name}}?accept={{type}}", server.uri()),
            "uploadUrl": format!("{}/upload/{{accountId}}/", server.uri()),
            "eventSourceUrl": format!("{}/events/?types={{types}}&closeafter={{closeafter}}&ping={{ping}}", server.uri()),
            "state": "s0"
        });
        let bearer = format!("Bearer {bearer}");
        Mock::given(method("GET"))
            .and(path("/jmap/session"))
            .and(header("authorization", bearer.as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(session))
            .mount(server)
            .await;
        let list: Vec<_> = sends_as
            .iter()
            .enumerate()
            .map(|(n, email)| serde_json::json!({"id": format!("i{n}"), "email": email}))
            .collect();
        let identities = serde_json::json!({
            "methodResponses": [["Identity/get", {"accountId": account, "state": "i0", "list": list, "notFound": []}, "c0"]],
            "sessionState": "s0"
        });
        Mock::given(method("POST"))
            .and(path("/api/"))
            .and(header("authorization", bearer.as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(identities))
            .mount(server)
            .await;
    }

    fn connector(db: &Db) -> Connector {
        Connector {
            db: db.clone(),
            config: crate::config::Config::default(),
            clients: crate::Clients::default(),
            secrets: Secrets::memory(),
            goa: crate::goa::session(),
        }
    }

    /// The value of `name` in the authorize URL's query, with the escapes
    /// a loopback address and a token can hold undone.
    fn query_value(url: &str, name: &str) -> String {
        let start = url.find(&format!("{name}=")).unwrap() + name.len() + 1;
        let value = url[start..].split('&').next().unwrap();
        value.replace("%3A", ":").replace("%2F", "/")
    }

    /// Plays the browser: records each consent page and comes back to the
    /// loopback with the code `c1` and this page's state.
    fn browser(pages: Arc<Mutex<Vec<String>>>) -> impl Fn(&str) {
        move |url: &str| {
            pages.lock().unwrap().push(url.to_string());
            let (state, redirect) = (query_value(url, "state"), query_value(url, "redirect_uri"));
            let address = redirect
                .trim_start_matches("http://")
                .trim_end_matches('/')
                .to_string();
            tokio::spawn(async move {
                let mut stream = tokio::net::TcpStream::connect(&address).await.unwrap();
                let request = format!(
                    "GET /?code=c1&state={state}&iss=https%3A%2F%2Fapi.fastmail.com HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n"
                );
                stream.write_all(request.as_bytes()).await.unwrap();
                let mut page = Vec::new();
                let _ = stream.read_to_end(&mut page).await;
            });
        }
    }

    async fn sign_in(
        server: &FakeServer,
        reach: &Connector,
        session_url: &str,
        pages: &Arc<Mutex<Vec<String>>>,
    ) -> Result<SignedIn, BrowserSignInError> {
        sign_in_as(server, reach, session_url, pages, "me@fastmail.com").await
    }

    /// A sign-in where the person typed `address`. The session names
    /// me@fastmail.com whatever the person typed.
    async fn sign_in_as(
        server: &FakeServer,
        reach: &Connector,
        session_url: &str,
        pages: &Arc<Mutex<Vec<String>>>,
        address: &str,
    ) -> Result<SignedIn, BrowserSignInError> {
        let offer = JmapBrowser {
            session_url: session_url.into(),
            provider: None,
        };
        signing_in(
            server,
            reach,
            offer,
            address,
            None,
            browser(Arc::clone(pages)),
            std::future::pending(),
            Duration::from_secs(5),
        )
        .await
    }

    #[tokio::test]
    async fn an_app_password_given_with_the_browser_sign_in_is_kept() {
        let jmap = jmap_server().await;
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("mail.db")).unwrap();
        let reach = connector(&db);
        let server = FakeServer::answering("client", vec![tokens("r1")]);
        let pages = Arc::new(Mutex::new(Vec::new()));
        let offer = JmapBrowser {
            session_url: format!("{}/jmap/session", jmap.uri()),
            provider: None,
        };
        let signed = signing_in(
            &server,
            &reach,
            offer,
            "me@fastmail.com",
            Some("app-pw".into()),
            browser(Arc::clone(&pages)),
            std::future::pending(),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        assert_eq!(
            reach
                .secrets
                .passwords
                .load(signed.account.id)
                .unwrap()
                .as_deref(),
            Some("app-pw")
        );
    }

    #[tokio::test]
    async fn a_new_grant_lets_a_refused_calendar_try_the_token_once_more() {
        use mailrs_store::services::{FoundService, Miss, ServiceKind};

        let jmap = jmap_server().await;
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("mail.db")).unwrap();
        let reach = connector(&db);
        let session_url = format!("{}/jmap/session", jmap.uri());
        let server = FakeServer::answering("client", vec![tokens("r1"), tokens("r2")]);
        let pages = Arc::new(Mutex::new(Vec::new()));
        let id = sign_in(&server, &reach, &session_url, &pages)
            .await
            .unwrap()
            .account
            .id;
        db.write(move |c| {
            let row = FoundService {
                kind: ServiceKind::CalDav,
                url: "https://127.0.0.1/dav/".into(),
                user_name: "me@fastmail.com".into(),
                confirmed: true,
                source: "table".into(),
            };
            mailrs_store::services::save(c, id, &row)?;
            mailrs_store::services::save_miss(c, id, ServiceKind::CalDav, Some(Miss::Refused))
        })
        .await
        .unwrap();
        let again = sign_in(&server, &reach, &session_url, &pages)
            .await
            .unwrap();
        assert!(
            db.read(mailrs_store::services::all_misses)
                .await
                .unwrap()
                .is_empty(),
            "the new grant clears the mark"
        );
        assert!(
            matches!(&again.services, crate::Connected::Ready(s) if matches!(s.calendar, Some(crate::AnyCalendar::Dav(_)))),
            "the calendar tries the token again"
        );
    }

    #[tokio::test]
    async fn the_account_keeps_the_address_the_person_typed() {
        let jmap = jmap_server().await;
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("mail.db")).unwrap();
        let reach = connector(&db);
        let server = FakeServer::answering("client", vec![tokens("r1")]);
        let pages = Arc::new(Mutex::new(Vec::new()));
        let session_url = format!("{}/jmap/session", jmap.uri());
        let signed = sign_in_as(&server, &reach, &session_url, &pages, "alias@fastmail.com")
            .await
            .unwrap();
        assert_eq!(signed.account.email, "alias@fastmail.com");
    }

    #[tokio::test]
    async fn a_token_account_signing_in_through_the_browser_stays_one_account() {
        let jmap = jmap_server().await;
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("mail.db")).unwrap();
        let reach = connector(&db);
        let session_url = format!("{}/jmap/session", jmap.uri());
        let token = NewJmap {
            address: "alias@fastmail.com".into(),
            provider_name: "Fastmail".into(),
            saved: JmapSaved {
                labels: true,
                capabilities: Vec::new(),
                session_url: session_url.clone(),
                auth: JmapAuth::Bearer,
                user_name: None,
                oauth: None,
                owner: None,
            },
            secret: "fmu1-token".into(),
            dav_password: None,
            signed_as: SignedAs::address("alias@fastmail.com"),
        };
        let held = jmap_signed_in(&db, &reach.secrets, token, 1).await.unwrap();
        let server = FakeServer::answering("client", vec![tokens("r1")]);
        let pages = Arc::new(Mutex::new(Vec::new()));
        let signed = sign_in_as(&server, &reach, &session_url, &pages, "alias@fastmail.com")
            .await
            .unwrap();
        assert_eq!(signed.account.id, held.id);
        let accounts = db.read(accounts::list_accounts).await.unwrap();
        assert_eq!(accounts.len(), 1, "{accounts:?}");
        let saved = db
            .read(move |c| mailrs_store::jmap::load(c, held.id))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(saved.auth, JmapAuth::OAuth);
    }

    /// A keyring that will not take the new refresh token leaves nothing
    /// holding the grant, so it is revoked.
    #[tokio::test]
    async fn a_grant_the_keyring_refuses_is_revoked() {
        let jmap = jmap_server().await;
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("mail.db")).unwrap();
        let reach = connector(&db);
        let crate::passwords::Passwords::Memory(kept) = &*reach.secrets.jmap else {
            unreachable!("tests keep secrets in memory")
        };
        kept.refuse_saves();
        let server = FakeServer::answering("client", vec![tokens("r1")]);
        let session_url = format!("{}/jmap/session", jmap.uri());
        let refused = sign_in(
            &server,
            &reach,
            &session_url,
            &Arc::new(Mutex::new(Vec::new())),
        )
        .await;
        assert!(
            matches!(
                refused,
                Err(BrowserSignInError::KeptJmap(
                    crate::sign_in::JmapSignInError::Password(_)
                ))
            ),
            "{:?}",
            refused.err()
        );
        assert_eq!(
            server.revoked(),
            [("client-1".to_string(), "r1".to_string())]
        );
    }

    /// The account took the new refresh token before a later step failed:
    /// the grant is the one it signs in with, so it stays.
    #[tokio::test]
    async fn a_grant_the_account_already_holds_is_not_revoked() {
        let jmap = jmap_server().await;
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("mail.db")).unwrap();
        let reach = connector(&db);
        let session_url = format!("{}/jmap/session", jmap.uri());
        let server = FakeServer::answering("client", vec![tokens("r1"), tokens("r2")]);
        let pages = Arc::new(Mutex::new(Vec::new()));
        let id = sign_in(&server, &reach, &session_url, &pages)
            .await
            .unwrap()
            .account
            .id;
        let crate::passwords::Passwords::Memory(passwords) = &*reach.secrets.passwords else {
            unreachable!("tests keep secrets in memory")
        };
        passwords.refuse_saves();
        let offer = JmapBrowser {
            session_url: session_url.clone(),
            provider: None,
        };
        let failed = signing_in(
            &server,
            &reach,
            offer,
            "me@fastmail.com",
            Some("app-pw".into()),
            browser(Arc::clone(&pages)),
            std::future::pending(),
            Duration::from_secs(5),
        )
        .await;
        assert!(failed.is_err());
        assert_eq!(reach.secrets.jmap.load(id).unwrap().as_deref(), Some("r2"));
        assert!(server.revoked().is_empty(), "{:?}", server.revoked());
    }

    async fn kept_client_id(db: &Db, id: i64) -> Option<String> {
        db.read(move |c| mailrs_store::jmap::load(c, id))
            .await
            .unwrap()
            .and_then(|saved| saved.oauth)
            .map(|o| o.client_id)
    }

    /// The app runs the sign-in on its runtime's threads.
    #[expect(dead_code, reason = "only compiled, to check the future is Send")]
    fn the_sign_in_can_move_between_threads(connector: &Connector, offer: JmapBrowser) {
        fn sends<T: Send>(_: T) {}
        sends(jmap_in_browser(
            connector,
            offer,
            "",
            None,
            |_: &str| {},
            std::future::pending::<()>(),
        ));
    }

    #[tokio::test]
    async fn a_new_account_registers_a_client_and_signs_in() {
        let jmap = jmap_server().await;
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("mail.db")).unwrap();
        let reach = connector(&db);
        let server = FakeServer::answering("client", vec![tokens("r1")]);
        let pages = Arc::new(Mutex::new(Vec::new()));
        let signed = sign_in(
            &server,
            &reach,
            &format!("{}/jmap/session", jmap.uri()),
            &pages,
        )
        .await
        .unwrap();
        assert_eq!(server.registrations(), 1);
        assert!(pages.lock().unwrap()[0].contains("client_id=client-1"));
        assert_eq!(signed.account.email, "me@fastmail.com");
        assert_eq!(
            kept_client_id(&db, signed.account.id).await.as_deref(),
            Some("client-1")
        );
        assert_eq!(
            reach
                .secrets
                .jmap
                .load(signed.account.id)
                .unwrap()
                .as_deref(),
            Some("r1")
        );
        assert!(matches!(
            &signed.services,
            crate::Connected::Ready(s) if matches!(s.mail, crate::AnyMail::Jmap(_))
        ));
    }

    /// Fastmail takes a loopback redirect on any port only when the client
    /// registered it with a port; `http://127.0.0.1/` it matches exactly,
    /// and its page then says the request is invalid. So the client names
    /// the very redirect the browser comes back to.
    #[tokio::test]
    async fn the_client_is_registered_for_the_redirect_the_browser_comes_back_to() {
        let jmap = jmap_server().await;
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("mail.db")).unwrap();
        let reach = connector(&db);
        let server = FakeServer::answering("client", vec![tokens("r1")]);
        let pages = Arc::new(Mutex::new(Vec::new()));
        sign_in(
            &server,
            &reach,
            &format!("{}/jmap/session", jmap.uri()),
            &pages,
        )
        .await
        .unwrap();
        let asked = query_value(&pages.lock().unwrap()[0], "redirect_uri");
        let port = asked
            .strip_prefix("http://127.0.0.1:")
            .and_then(|rest| rest.strip_suffix('/'))
            .and_then(|port| port.parse::<u16>().ok());
        assert!(port.is_some_and(|p| p != 0), "{asked}");
        assert_eq!(
            *server.registered_for.lock().unwrap(),
            std::slice::from_ref(&asked)
        );
        assert_eq!(*server.exchanged_for.lock().unwrap(), [asked]);
    }

    /// Each sign-in listens on a port of its own, so a client registered
    /// for an earlier one would name another redirect: signing in again
    /// registers a client for this one.
    #[tokio::test]
    async fn signing_in_again_registers_a_client_for_its_own_redirect() {
        let jmap = jmap_server().await;
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("mail.db")).unwrap();
        let reach = connector(&db);
        let session_url = format!("{}/jmap/session", jmap.uri());
        let server = FakeServer::answering("client", vec![tokens("r1"), tokens("r2")]);
        let pages = Arc::new(Mutex::new(Vec::new()));
        let first = sign_in(&server, &reach, &session_url, &pages)
            .await
            .unwrap();
        let before = reach.secrets.jmap_sessions.held(first.account.id).unwrap();
        let again = sign_in(&server, &reach, &session_url, &pages)
            .await
            .unwrap();
        let after = reach.secrets.jmap_sessions.held(again.account.id).unwrap();
        assert!(
            !Arc::ptr_eq(&before, &after),
            "the new refresh token gets a new session"
        );
        assert_eq!(again.account.id, first.account.id);
        assert_eq!(server.registrations(), 2);
        let pages = pages.lock().unwrap().clone();
        assert!(pages[1].contains("client_id=client-2"), "{}", pages[1]);
        let asked: Vec<_> = pages
            .iter()
            .map(|page| query_value(page, "redirect_uri"))
            .collect();
        assert_eq!(*server.registered_for.lock().unwrap(), asked);
        assert_eq!(
            kept_client_id(&db, again.account.id).await.as_deref(),
            Some("client-2")
        );
        assert_eq!(
            reach
                .secrets
                .jmap
                .load(again.account.id)
                .unwrap()
                .as_deref(),
            Some("r2")
        );
    }

    fn metadata() -> Metadata {
        Metadata {
            issuer: "https://api.fastmail.com".into(),
            authorization_endpoint: "https://api.fastmail.com/oauth/authorize".into(),
            token_endpoint: "https://api.fastmail.com/oauth/refresh".into(),
            registration_endpoint: Some("https://api.fastmail.com/oauth/register".into()),
            scopes_supported: vec![
                "urn:ietf:params:oauth:scope:mail".into(),
                "offline_access".into(),
                "openid".into(),
            ],
            code_challenge_methods_supported: vec!["S256".into()],
            authorization_response_iss_parameter_supported: true,
            revocation_endpoint: None,
        }
    }

    /// The loopback's address, as the browser would reach it.
    fn address_of(loopback: &Loopback) -> String {
        loopback
            .redirect_uri
            .trim_start_matches("http://")
            .trim_end_matches('/')
            .to_string()
    }

    async fn closed(address: &str) -> bool {
        tokio::net::TcpStream::connect(address).await.is_err()
    }

    #[tokio::test]
    async fn the_browser_s_redirect_hands_back_its_code() {
        let loopback = Loopback::bind().await.unwrap();
        let address = address_of(&loopback);
        let browser = tokio::spawn(async move {
            let mut stream = tokio::net::TcpStream::connect(&address).await.unwrap();
            let request = "GET /?code=c1&state=s1&iss=https%3A%2F%2Fapi.fastmail.com HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n";
            stream.write_all(request.as_bytes()).await.unwrap();
            let mut page = Vec::new();
            let _ = stream.read_to_end(&mut page).await;
        });
        let code = redirected(
            loopback,
            "s1",
            &metadata(),
            std::future::pending(),
            Duration::from_secs(5),
        )
        .await;
        assert_eq!(code.unwrap(), "c1");
        browser.await.unwrap();
    }

    #[tokio::test]
    async fn a_canceled_sign_in_closes_the_listener() {
        let loopback = Loopback::bind().await.unwrap();
        let address = address_of(&loopback);
        let canceled = redirected(
            loopback,
            "s1",
            &metadata(),
            std::future::ready(()),
            Duration::from_secs(600),
        )
        .await;
        assert!(matches!(canceled, Err(BrowserSignInError::Canceled)));
        assert!(closed(&address).await, "the listener outlived the sign-in");
    }

    #[tokio::test]
    async fn a_sign_in_nobody_finishes_gives_up_and_closes_the_listener() {
        let loopback = Loopback::bind().await.unwrap();
        let address = address_of(&loopback);
        let waited = redirected(
            loopback,
            "s1",
            &metadata(),
            std::future::pending(),
            Duration::from_millis(50),
        )
        .await;
        assert!(matches!(waited, Err(BrowserSignInError::GaveUp)));
        assert!(closed(&address).await, "the listener outlived the sign-in");
    }

    #[test]
    fn the_sign_in_waits_ten_minutes_for_the_browser() {
        assert_eq!(BROWSER_WAIT, Duration::from_secs(600));
    }

    #[test]
    fn a_server_outside_the_table_goes_by_its_host() {
        assert_eq!(
            host_of("https://mail.example.org:8443/jmap/session"),
            "mail.example.org"
        );
        assert_eq!(
            host_of("https://mail.example.org/jmap/session"),
            "mail.example.org"
        );
    }

    #[test]
    fn the_table_s_scopes_win_over_the_metadata_s() {
        let offer = JmapBrowser {
            session_url: "https://api.fastmail.com/jmap/session".into(),
            provider: mailrs_discover::jmap_named("Fastmail"),
        };
        let scopes = scopes_for(&offer, &metadata());
        assert_eq!(scopes, offer.provider.unwrap().oauth_scopes);
    }

    #[test]
    fn a_server_outside_the_table_asks_for_the_mail_scopes_its_metadata_lists() {
        let offer = JmapBrowser {
            session_url: "https://mail.example.org/jmap/session".into(),
            provider: None,
        };
        assert_eq!(
            scopes_for(&offer, &metadata()),
            ["urn:ietf:params:oauth:scope:mail", "offline_access"]
        );
    }

    /// Reads what the store and the keyring hold for `id`: the session URL,
    /// the kept client and the refresh token.
    async fn kept(db: &Db, reach: &Connector, id: i64) -> (String, Option<String>, Option<String>) {
        let saved = db
            .read(move |c| mailrs_store::jmap::load(c, id))
            .await
            .unwrap()
            .unwrap();
        (
            saved.session_url,
            saved.oauth.map(|o| o.client_id),
            reach.secrets.jmap.load(id).unwrap(),
        )
    }

    #[tokio::test]
    async fn a_sign_in_again_that_comes_back_as_another_account_changes_nothing() {
        let jmap = jmap_server().await;
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("mail.db")).unwrap();
        let reach = connector(&db);
        let session_url = format!("{}/jmap/session", jmap.uri());
        let first = FakeServer::answering("client", vec![tokens("r1")]);
        let id = sign_in(
            &first,
            &reach,
            &session_url,
            &Arc::new(Mutex::new(Vec::new())),
        )
        .await
        .unwrap()
        .account
        .id;
        let before = kept(&db, &reach, id).await;
        // The browser is signed in to someone else's account on the same
        // server: its token reaches another user's session.
        mount_session(
            &jmap,
            "a2",
            "other@fastmail.com",
            "B2",
            &["other@fastmail.com"],
        )
        .await;
        let theirs = FakeServer::answering(
            "client",
            vec![Ok(Tokens {
                access_token: "a2".into(),
                expires_in: 3600,
                refresh_token: Some("r2".into()),
            })],
        );
        let refused = sign_in(
            &theirs,
            &reach,
            &session_url,
            &Arc::new(Mutex::new(Vec::new())),
        )
        .await;
        let Err(err) = refused else {
            panic!("another account's grant was kept")
        };
        assert_eq!(
            err.to_string(),
            "You signed in to a different account than me@fastmail.com, so nothing changed."
        );
        assert_eq!(
            kept(&db, &reach, id).await,
            before,
            "the account keeps its server, client and token"
        );
        assert_eq!(
            theirs.revoked(),
            [("client-1".to_string(), "r2".to_string())],
            "the other account's grant is revoked"
        );
        assert!(first.revoked().is_empty());
    }

    #[tokio::test]
    async fn a_new_account_at_an_address_the_session_does_not_send_as_is_refused() {
        let jmap = jmap_server().await;
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("mail.db")).unwrap();
        let reach = connector(&db);
        let server = FakeServer::answering("client", vec![tokens("r1")]);
        let session_url = format!("{}/jmap/session", jmap.uri());
        let refused = sign_in_as(
            &server,
            &reach,
            &session_url,
            &Arc::new(Mutex::new(Vec::new())),
            "boss@fastmail.com",
        )
        .await;
        assert!(matches!(
            refused,
            Err(BrowserSignInError::KeptJmap(
                crate::sign_in::JmapSignInError::DifferentAccount { .. }
            ))
        ));
        assert_eq!(
            server.revoked(),
            [("client-1".to_string(), "r1".to_string())],
            "the refused grant is revoked"
        );
        assert!(
            db.read(accounts::list_accounts).await.unwrap().is_empty(),
            "no account is added"
        );
    }
}
