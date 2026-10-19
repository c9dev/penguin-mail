//! A Gmail client whose token comes from somewhere else, such as the
//! desktop's Online Accounts: sent as given, renewed once after a 401.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use futures::future::BoxFuture;
use mailrs_gmail::{AccessSource, AccountQuota, GmailClient, GmailError};
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const API: &str = "/gmail/v1/users/me";

/// Answers "old" until asked to renew, then "new".
#[derive(Default)]
struct Source {
    renewals: AtomicUsize,
}

impl AccessSource for Source {
    fn access(&self, renew: bool) -> BoxFuture<'_, Result<String, GmailError>> {
        if renew {
            self.renewals.fetch_add(1, Ordering::SeqCst);
        }
        let token = if self.renewals.load(Ordering::SeqCst) > 0 {
            "new"
        } else {
            "old"
        };
        Box::pin(async move { Ok(token.to_string()) })
    }
}

fn client(server: &MockServer, source: Arc<Source>) -> GmailClient {
    GmailClient::with_source(
        source,
        reqwest::Client::new(),
        Arc::new(AccountQuota::standalone()),
    )
    .with_base_url(format!("{}{API}", server.uri()))
}

#[tokio::test]
async fn a_source_token_is_sent_and_renewed_once_after_a_401() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/profile")))
        .and(header("authorization", "Bearer old"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/profile")))
        .and(header("authorization", "Bearer new"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"emailAddress": "dana@example.com", "historyId": "7"})),
        )
        .mount(&server)
        .await;
    let source = Arc::new(Source::default());
    let profile = client(&server, source.clone()).profile().await.unwrap();
    assert_eq!(profile.email_address, "dana@example.com");
    assert_eq!(source.renewals.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_source_that_keeps_getting_401_gives_up_after_one_renewal() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    let source = Arc::new(Source::default());
    let err = client(&server, source.clone()).profile().await.unwrap_err();
    assert!(matches!(err, GmailError::NeedsReauth), "{err:?}");
    assert_eq!(source.renewals.load(Ordering::SeqCst), 1);
}
