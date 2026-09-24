use std::sync::Arc;

use mailrs_domain::{Account, AccountState};
use mailrs_gmail::{MemoryTokenStore, OAuthClient, TokenStore};

use crate::{SyncError, connect_account};

#[tokio::test]
async fn connecting_needs_a_stored_refresh_token() {
    let tokens: Arc<dyn TokenStore> = Arc::new(MemoryTokenStore::default());
    let account = Account {
        id: 1,
        email: "me@example.com".into(),
        state: AccountState::Ok,
        provider: mailrs_domain::Provider::Gmail,
        provider_name: None,
    };
    let oauth = OAuthClient::new("cid", "secret");
    assert!(matches!(
        connect_account(oauth.clone(), Arc::clone(&tokens), &account).await,
        Err(SyncError::Backend(crate::BackendError::NeedsReauth))
    ));
    tokens.save("me@example.com", "rt").unwrap();
    let client = connect_account(oauth, tokens, &account).await.unwrap();
    assert_eq!(client.account_id, 1);
}
