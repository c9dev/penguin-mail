//! Which Google client an account signs in with.

use mailrs_domain::{Account, AccountState, EpochMillis, SignInClient};
use mailrs_gmail::OAuthClient;
use mailrs_store::{Db, StoreError, accounts};

use crate::config::Config;

/// The client `account` signs in with, from what the store recorded for
/// it. An account with no client left, such as an own account whose
/// `config.toml` is gone, or a built-in account in a build without the
/// client, is marked as needing a new sign-in and gets `None`. The engine
/// never starts such an account, so nothing else would mark it.
pub async fn account_client(
    db: &Db,
    config: &Config,
    built_in: Option<OAuthClient>,
    account: &Account,
) -> Result<Option<OAuthClient>, StoreError> {
    let id = account.id;
    let kind = db.read(move |c| accounts::sign_in_client(c, id)).await?;
    let client = client_for(kind, config, built_in);
    if client.is_none() {
        db.write(move |c| accounts::set_state(c, id, AccountState::NeedsReauth))
            .await?;
    }
    Ok(client)
}

/// Adds the account `email` signed in as, or finds it when it is already
/// here, and records that it signed in with the built-in client. Every
/// sign-in uses that client, so an own account that signs in again moves
/// over to it.
pub async fn signed_in(db: &Db, email: &str, now: EpochMillis) -> Result<Account, StoreError> {
    let email = email.to_string();
    db.write(move |c| {
        let id = accounts::insert_account(c, &email, now)?;
        accounts::set_sign_in_client(c, id, SignInClient::BuiltIn)?;
        // The row was written a line above in the same transaction.
        accounts::account_by_email(c, &email)?
            .ok_or(StoreError::Sqlite(rusqlite::Error::QueryReturnedNoRows))
    })
    .await
}

/// The client for an account that signed in with `client`. A built-in
/// account takes `built_in`, the build's own; an own account takes the one
/// in `config.toml`. `None` when that client is missing, which the caller
/// treats as needing a new sign-in, and a new sign-in uses the built-in one.
pub fn client_for(
    client: SignInClient,
    config: &Config,
    built_in: Option<OAuthClient>,
) -> Option<OAuthClient> {
    match client {
        SignInClient::BuiltIn => built_in,
        SignInClient::Own => config
            .oauth
            .as_ref()
            .map(|own| OAuthClient::new(&own.client_id, &own.client_secret)),
    }
}

#[cfg(test)]
mod tests {
    use mailrs_domain::{Account, AccountState, SignInClient};
    use mailrs_gmail::client_from;
    use mailrs_store::{Db, accounts};

    use super::*;
    use crate::config::{Config, OAuthConfig};

    fn built_in() -> Option<mailrs_gmail::OAuthClient> {
        client_from(Some("built.apps.googleusercontent.com"), Some("GOCSPX-built"))
    }

    fn with_own() -> Config {
        Config {
            oauth: Some(OAuthConfig {
                client_id: "own.apps.googleusercontent.com".into(),
                client_secret: "GOCSPX-own".into(),
            }),
            ..Config::default()
        }
    }

    #[test]
    fn a_built_in_account_takes_the_builds_client() {
        let client = client_for(SignInClient::BuiltIn, &with_own(), built_in()).unwrap();
        assert_eq!(client.id(), "built.apps.googleusercontent.com");
    }

    #[test]
    fn an_own_account_takes_the_client_in_the_config() {
        let client = client_for(SignInClient::Own, &with_own(), built_in()).unwrap();
        assert_eq!(client.id(), "own.apps.googleusercontent.com");
    }

    #[test]
    fn an_own_account_with_no_client_in_the_config_has_none() {
        assert!(client_for(SignInClient::Own, &Config::default(), built_in()).is_none());
    }

    #[test]
    fn a_built_in_account_in_a_build_without_one_has_none() {
        assert!(client_for(SignInClient::BuiltIn, &with_own(), None).is_none());
    }

    fn store() -> (tempfile::TempDir, Db) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("mail.db")).unwrap();
        (dir, db)
    }

    async fn account(db: &Db, email: &str, client: SignInClient) -> Account {
        let email = email.to_string();
        db.write(move |c| {
            let id = accounts::insert_account(c, &email, 0)?;
            accounts::set_sign_in_client(c, id, client)?;
            Ok(accounts::account_by_email(c, &email)?.unwrap())
        })
        .await
        .unwrap()
    }

    async fn state(db: &Db, email: &str) -> AccountState {
        let email = email.to_string();
        db.read(move |c| accounts::account_by_email(c, &email))
            .await
            .unwrap()
            .unwrap()
            .state
    }

    #[tokio::test]
    async fn an_account_with_its_client_starts_as_it_was() {
        let (_dir, db) = store();
        let own = account(&db, "own@example.com", SignInClient::Own).await;
        let client = account_client(&db, &with_own(), built_in(), &own)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(client.id(), "own.apps.googleusercontent.com");
        assert_ne!(
            state(&db, "own@example.com").await,
            AccountState::NeedsReauth
        );
    }

    #[tokio::test]
    async fn an_own_account_whose_config_went_needs_a_new_sign_in() {
        let (_dir, db) = store();
        let own = account(&db, "own@example.com", SignInClient::Own).await;
        let client = account_client(&db, &Config::default(), built_in(), &own)
            .await
            .unwrap();
        assert!(client.is_none());
        assert_eq!(
            state(&db, "own@example.com").await,
            AccountState::NeedsReauth
        );
    }

    #[tokio::test]
    async fn a_built_in_account_in_a_build_without_the_client_needs_a_new_sign_in() {
        let (_dir, db) = store();
        let built = account(&db, "built@example.com", SignInClient::BuiltIn).await;
        let client = account_client(&db, &with_own(), None, &built).await.unwrap();
        assert!(client.is_none());
        assert_eq!(
            state(&db, "built@example.com").await,
            AccountState::NeedsReauth
        );
    }

    #[tokio::test]
    async fn a_new_sign_in_records_the_built_in_client() {
        let (_dir, db) = store();
        let added = signed_in(&db, "new@example.com", 0).await.unwrap();
        let id = added.id;
        let kind = db
            .read(move |c| accounts::sign_in_client(c, id))
            .await
            .unwrap();
        assert_eq!(kind, SignInClient::BuiltIn);
    }

    #[tokio::test]
    async fn signing_an_own_account_in_again_moves_it_to_the_built_in_client() {
        let (_dir, db) = store();
        let own = account(&db, "own@example.com", SignInClient::Own).await;
        let again = signed_in(&db, "own@example.com", 0).await.unwrap();
        assert_eq!(again.id, own.id);
        let id = own.id;
        let kind = db
            .read(move |c| accounts::sign_in_client(c, id))
            .await
            .unwrap();
        assert_eq!(kind, SignInClient::BuiltIn);
    }
}
