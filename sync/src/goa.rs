//! Talks to the desktop's Online Accounts for the Google accounts it holds
//! and the access tokens it hands out. Everything goes through
//! [`OnlineAccounts`], so tests and macOS use a stand-in.

use std::fmt;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use futures::stream::BoxStream;
use tokio::sync::Mutex;
use tokio::time::Instant;

use crate::connect::STEP_WAIT;

#[cfg(target_os = "linux")]
mod dbus;
#[cfg(any(test, feature = "fake"))]
pub mod fake;

#[cfg(target_os = "linux")]
pub use dbus::DbusAccounts;

/// A Google account Online Accounts holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoaAccount {
    /// The last segment of the account's object path, such as `account_1_0`.
    pub id: String,
    /// The address, as Online Accounts keeps it.
    pub identity: String,
    pub presentation: String,
    /// The person turned Mail off for this account in Online Accounts.
    pub mail_disabled: bool,
    /// Online Accounts needs the person to sign in again.
    pub attention_needed: bool,
}

/// An access token. `Debug` hides it so a log line cannot leak it.
#[derive(Clone)]
pub struct GoaToken {
    pub token: String,
    pub expires_in: Duration,
}

impl fmt::Debug for GoaToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GoaToken")
            .field("token", &"<hidden>")
            .field("expires_in", &self.expires_in)
            .finish()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum GoaError {
    /// No Online Accounts to ask: no service, no permission, no answer.
    #[error("Online Accounts is unreachable: {0}")]
    Unreachable(String),
    /// The person has to sign in again in Online Accounts.
    #[error("Online Accounts needs a new sign-in")]
    NotAuthorized,
    /// The account is not (or no longer) in Online Accounts.
    #[error("the account is not in Online Accounts")]
    NoAccount,
    #[error("Online Accounts failed: {0}")]
    Other(String),
}

/// What changed in Online Accounts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoaChange {
    Added(GoaAccount),
    Removed { id: String },
    Changed(GoaAccount),
}

pub trait OnlineAccounts: Send + Sync {
    fn google_accounts(&self) -> BoxFuture<'_, Result<Vec<GoaAccount>, GoaError>>;
    fn access_token<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<GoaToken, GoaError>>;
    fn ensure_credentials<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<(), GoaError>>;
    fn changes(&self) -> BoxFuture<'_, Result<BoxStream<'static, GoaChange>, GoaError>>;
}

/// The stand-in where there is no Online Accounts: it answers
/// `Unreachable` to everything.
pub struct Absent;

impl Absent {
    fn none() -> GoaError {
        GoaError::Unreachable("there is no Online Accounts here".into())
    }
}

impl OnlineAccounts for Absent {
    fn google_accounts(&self) -> BoxFuture<'_, Result<Vec<GoaAccount>, GoaError>> {
        Box::pin(async { Err(Self::none()) })
    }
    fn access_token<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Result<GoaToken, GoaError>> {
        Box::pin(async { Err(Self::none()) })
    }
    fn ensure_credentials<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Result<(), GoaError>> {
        Box::pin(async { Err(Self::none()) })
    }
    fn changes(&self) -> BoxFuture<'_, Result<BoxStream<'static, GoaChange>, GoaError>> {
        Box::pin(async { Err(Self::none()) })
    }
}

/// The process's Online Accounts: one D-Bus client on Linux, `Absent`
/// elsewhere.
pub fn session() -> Arc<dyn OnlineAccounts> {
    #[cfg(target_os = "linux")]
    {
        static SESSION: std::sync::OnceLock<Arc<dyn OnlineAccounts>> = std::sync::OnceLock::new();
        Arc::clone(SESSION.get_or_init(|| Arc::new(DbusAccounts::session())))
    }
    #[cfg(not(target_os = "linux"))]
    {
        Arc::new(Absent)
    }
}

/// The account for `email`: the saved id first, else the address, trimmed
/// and without regard to case. The saved id counts only while its account
/// still signs in as `email`, so an id given to another Google account
/// never reads this one's mail.
pub fn find<'a>(
    list: &'a [GoaAccount],
    saved_id: Option<&str>,
    email: &str,
) -> Option<&'a GoaAccount> {
    let wanted = email.trim();
    let same = |a: &GoaAccount| a.identity.trim().eq_ignore_ascii_case(wanted);
    if let Some(id) = saved_id
        && let Some(found) = list.iter().find(|a| a.id == id && same(a))
    {
        return Some(found);
    }
    list.iter().find(|a| same(a))
}

/// `work`, given up after [`STEP_WAIT`]: a daemon that hangs must not hang
/// a sync.
async fn bounded<T>(work: impl Future<Output = Result<T, GoaError>>) -> Result<T, GoaError> {
    match tokio::time::timeout(STEP_WAIT, work).await {
        Ok(result) => result,
        Err(_) => Err(GoaError::Unreachable(
            "Online Accounts did not answer".into(),
        )),
    }
}

/// How long before its end a token stops being reused.
const TOKEN_MARGIN: Duration = Duration::from_secs(60);

/// The access tokens of one Online Accounts account, kept until a minute
/// before they end. A mutex keeps two callers from asking at once.
pub struct GoaTokens {
    accounts: Arc<dyn OnlineAccounts>,
    id: String,
    cached: Mutex<Option<(String, Instant)>>,
}

impl GoaTokens {
    pub fn new(accounts: Arc<dyn OnlineAccounts>, id: String) -> Self {
        Self {
            accounts,
            id,
            cached: Mutex::new(None),
        }
    }

    pub async fn token(&self) -> Result<String, GoaError> {
        let mut cached = self.cached.lock().await;
        if let Some((token, ends)) = cached.as_ref()
            && Instant::now() < *ends
        {
            return Ok(token.clone());
        }
        self.fetch(&mut cached).await
    }

    /// A new token after the old one was refused: drops the cache, asks
    /// Online Accounts to check the account's credentials once, then asks
    /// for a token.
    pub async fn renew(&self) -> Result<String, GoaError> {
        let mut cached = self.cached.lock().await;
        *cached = None;
        self.accounts.ensure_credentials(&self.id).await?;
        self.fetch(&mut cached).await
    }

    async fn fetch(&self, cached: &mut Option<(String, Instant)>) -> Result<String, GoaError> {
        let got = self.accounts.access_token(&self.id).await?;
        let ends = Instant::now() + got.expires_in.saturating_sub(TOKEN_MARGIN);
        *cached = Some((got.token.clone(), ends));
        Ok(got.token)
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeAccounts;
    use super::*;

    fn account(id: &str, identity: &str) -> GoaAccount {
        GoaAccount {
            id: id.into(),
            identity: identity.into(),
            presentation: identity.into(),
            mail_disabled: false,
            attention_needed: false,
        }
    }

    #[test]
    fn a_saved_id_counts_only_while_it_signs_in_as_the_address() {
        let other = GoaAccount {
            id: "account_1_0".into(),
            identity: "someone@example.com".into(),
            presentation: "someone@example.com".into(),
            mail_disabled: false,
            attention_needed: false,
        };
        let mine = GoaAccount {
            id: "account_2_0".into(),
            identity: "Dana@Example.com".into(),
            ..other.clone()
        };
        let list = [other, mine];
        let found = super::find(&list, Some("account_1_0"), "dana@example.com");
        assert_eq!(found.map(|a| a.id.as_str()), Some("account_2_0"));
        assert!(super::find(&list[..1], Some("account_1_0"), "dana@example.com").is_none());
    }

    #[test]
    fn find_matches_the_identity_whatever_its_case_and_spaces() {
        let list = [account("account_1_0", "Dana@Example.com")];
        assert_eq!(
            find(&list, None, " dana@example.com ").map(|a| a.id.as_str()),
            Some("account_1_0")
        );
    }

    #[test]
    fn find_prefers_the_saved_id_and_falls_back_to_the_identity() {
        let list = [
            account("account_2_0", "dana@example.com"),
            account("account_1_0", "dana@example.com"),
        ];
        assert_eq!(
            find(&list, Some("account_1_0"), "dana@example.com").map(|a| a.id.as_str()),
            Some("account_1_0")
        );
        assert_eq!(
            find(&list, Some("account_9_0"), "dana@example.com").map(|a| a.id.as_str()),
            Some("account_2_0")
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_cached_token_is_reused_until_a_minute_before_it_ends() {
        let fake = FakeAccounts::new();
        fake.add(account("account_1_0", "dana@example.com"));
        let tokens = GoaTokens::new(fake.clone(), "account_1_0".into());
        assert_eq!(tokens.token().await.unwrap(), "t1");
        assert_eq!(tokens.token().await.unwrap(), "t1");
        assert_eq!(fake.token_calls(), 1);
        tokio::time::advance(Duration::from_secs(3540)).await;
        assert_eq!(tokens.token().await.unwrap(), "t2");
        assert_eq!(fake.token_calls(), 2);
    }

    #[tokio::test]
    async fn renew_ensures_credentials_once_and_asks_again() {
        let fake = FakeAccounts::new();
        fake.add(account("account_1_0", "dana@example.com"));
        let tokens = GoaTokens::new(fake.clone(), "account_1_0".into());
        tokens.token().await.unwrap();
        assert_eq!(tokens.renew().await.unwrap(), "t2");
        assert_eq!(fake.ensure_calls(), 1);
        assert_eq!(fake.token_calls(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn a_hung_daemon_gives_up_after_the_step_wait() {
        let fake = FakeAccounts::new();
        fake.hang(true);
        let started = Instant::now();
        let got = fake.google_accounts().await;
        assert!(matches!(got, Err(GoaError::Unreachable(_))));
        assert_eq!(started.elapsed(), STEP_WAIT);
    }

    #[test]
    fn the_token_never_reaches_debug() {
        let token = GoaToken {
            token: "secret-token".into(),
            expires_in: Duration::from_secs(1),
        };
        assert!(!format!("{token:?}").contains("secret-token"));
    }

    #[tokio::test]
    async fn the_absent_stand_in_is_unreachable() {
        assert!(matches!(
            Absent.google_accounts().await,
            Err(GoaError::Unreachable(_))
        ));
    }
}
