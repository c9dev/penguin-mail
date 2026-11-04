//! Shared Microsoft mailboxes keep their own mail and use an owner's sign-in.

use std::sync::Arc;
use std::time::Duration;

use mailrs_domain::translate::gettext;
use mailrs_domain::{Account, AccountId, EpochMillis, Provider};
use mailrs_graph::MicrosoftClient;
use mailrs_store::{Db, accounts};

use crate::passwords::PasswordStore;
use crate::{AccountServices, BackendError, MicrosoftSettings, SyncError};

pub async fn add<P: PasswordStore + 'static>(
    db: &Db,
    tokens: Arc<P>,
    client: MicrosoftClient,
    owner: AccountId,
    address: String,
    now: EpochMillis,
    window_days: i64,
) -> Result<(Account, AccountServices), SyncError> {
    add_at(
        db,
        tokens,
        client,
        owner,
        address,
        now,
        window_days,
        mailrs_graph::GRAPH_BASE,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn add_at<P: PasswordStore + 'static>(
    db: &Db,
    tokens: Arc<P>,
    client: MicrosoftClient,
    owner: AccountId,
    address: String,
    now: EpochMillis,
    window_days: i64,
    base: &str,
) -> Result<(Account, AccountServices), SyncError> {
    let parent = db
        .read(move |c| {
            if accounts::microsoft_owner(c, owner)?.is_some() {
                return Ok(None);
            }
            Ok(accounts::account(c, owner)?.filter(|a| {
                a.provider == Provider::Microsoft && a.provider_name() == "Microsoft 365"
            }))
        })
        .await?
        .ok_or_else(|| {
            BackendError::Refused(gettext(
                "Choose a Microsoft 365 account that has access to this shared mailbox.",
            ))
        })?;
    let graph = crate::connect::microsoft_graph_within(
        db,
        tokens,
        client,
        &parent,
        base,
        Duration::from_secs(60),
    )
    .await?
    .shared_mailbox(&address)
    .map_err(|_| BackendError::Refused(gettext("Enter a valid shared mailbox address.")))?;
    // Probe the target before creating an account. Access to the owner's
    // profile alone says nothing about access to this mailbox.
    graph.folders(None, None).await.map_err(|err| match err {
        mailrs_graph::GraphError::AccessDenied { .. } | mailrs_graph::GraphError::NotFound => BackendError::Refused(gettext("Microsoft could not open this shared mailbox. Check its address and ask your administrator for Full Access permission.")),
        err => crate::services::microsoft::backend(err),
    })?;
    let email = address.clone();
    let owner_address = parent.email;
    // Removal takes the same turn before checking attached mailboxes.
    let _turn = crate::sign_in::secrets_turn().await;
    let account = db
        .write(move |c| {
            // The network wait may outlive removal of the owner. SQLite
            // can reuse its id for a different account added meanwhile.
            if !accounts::account(c, owner)?
                .is_some_and(|a| a.email.eq_ignore_ascii_case(&owner_address))
            {
                return Ok(None);
            }
            let Some(id) = accounts::insert_shared_microsoft(c, owner, &email, now)? else {
                return Ok(None);
            };
            accounts::account(c, id)
        })
        .await?
        .ok_or_else(|| {
            BackendError::Refused(gettext(
                "This mailbox is already added, or its sign-in account was removed.",
            ))
        })?;
    let services = AccountServices::microsoft(
        graph,
        MicrosoftSettings {
            shared: true,
            address,
            provider_name: "Microsoft 365".into(),
            window_days,
        },
    );
    Ok((account, services))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::passwords::MemoryPasswords;
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn removing_the_owner_during_the_access_check_cannot_attach_to_a_reused_id() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("mail.db")).unwrap();
        let owner = db
            .write(|c| {
                accounts::insert_microsoft_account(c, "owner@contoso.com", "Microsoft 365", 0)
            })
            .await
            .unwrap()
            .unwrap();
        let tokens = Arc::new(MemoryPasswords::default());
        tokens.save(owner, "refresh").unwrap();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"access_token":"a", "expires_in":3600})),
            )
            .mount(&server)
            .await;
        let checking = Arc::new(tokio::sync::Notify::new());
        let reached = checking.clone();
        let (release, resumed) = std::sync::mpsc::channel();
        let resumed = std::sync::Mutex::new(resumed);
        Mock::given(method("GET"))
            .and(path("/v1.0/users/sales@contoso.com/mailFolders"))
            .respond_with(move |_: &wiremock::Request| {
                reached.notify_one();
                resumed
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(10))
                    .unwrap();
                ResponseTemplate::new(200).set_body_json(json!({"value":[]}))
            })
            .expect(1)
            .mount(&server)
            .await;
        let client = MicrosoftClient::new("test").with_endpoints(
            format!("{}/authorize", server.uri()),
            format!("{}/token", server.uri()),
        );
        let store = db.clone();
        let base = format!("{}/v1.0/", server.uri());
        let adding = tokio::spawn(async move {
            add_at(
                &store,
                tokens,
                client,
                owner,
                "sales@contoso.com".into(),
                0,
                30,
                &base,
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(10), checking.notified())
            .await
            .unwrap();
        let replacement = db
            .write(move |c| {
                accounts::delete_account(c, owner)?;
                accounts::insert_microsoft_account(c, "other@contoso.com", "Microsoft 365", 0)
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            replacement, owner,
            "the regression needs SQLite to reuse the owner's id"
        );
        release.send(()).unwrap();
        assert!(adding.await.unwrap().is_err());
        assert_eq!(db.read(accounts::list_accounts).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn shared_mail_checks_access_and_keeps_no_separate_token_or_organizer_services() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("mail.db")).unwrap();
        let owner = db
            .write(|c| {
                accounts::insert_microsoft_account(c, "owner@contoso.com", "Microsoft 365", 0)
            })
            .await
            .unwrap()
            .unwrap();
        let tokens = Arc::new(MemoryPasswords::default());
        tokens.save(owner, "refresh").unwrap();
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"access_token":"a", "expires_in":3600,"refresh_token":"rotated","scope":"Mail.ReadWrite.Shared Mail.Send.Shared"})))
            .mount(&server).await;
        Mock::given(method("GET"))
            .and(path("/v1.0/users/sales@contoso.com/mailFolders"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value":[]})))
            .expect(2)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1.0/users/denied@contoso.com/mailFolders"))
            .respond_with(ResponseTemplate::new(403).set_body_json(
                json!({"error":{"code":"ErrorAccessDenied","message":"Access denied"}}),
            ))
            .expect(1)
            .mount(&server)
            .await;
        let client = MicrosoftClient::new("test").with_endpoints(
            format!("{}/authorize", server.uri()),
            format!("{}/token", server.uri()),
        );
        let (account, services) = add_at(
            &db,
            tokens.clone(),
            client.clone(),
            owner,
            "sales@contoso.com".into(),
            0,
            30,
            &format!("{}/v1.0/", server.uri()),
        )
        .await
        .unwrap();
        assert!(tokens.load(account.id).unwrap().is_none());
        assert!(tokens.load(owner).unwrap().is_some());
        let independent_sign_in = crate::sign_in::microsoft_signed_in(
            &db,
            tokens.clone(),
            crate::sign_in::NewMicrosoft {
                address: "SALES@contoso.com".into(),
                provider_name: "Microsoft 365".into(),
                refresh_token: "must-not-be-saved".into(),
                granted: None,
            },
            0,
            "",
        )
        .await;
        assert!(independent_sign_in.is_err());
        assert!(tokens.load(account.id).unwrap().is_none());
        let reconnected = crate::connect::microsoft_graph_within(
            &db,
            tokens.clone(),
            client.clone(),
            &account,
            &format!("{}/v1.0/", server.uri()),
            Duration::from_secs(10),
        )
        .await
        .unwrap();
        assert!(reconnected.is_shared_mailbox());
        reconnected.folders(None, None).await.unwrap();
        for _ in 0..50 {
            if tokens.load(owner).unwrap().as_deref() == Some("rotated") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(tokens.load(owner).unwrap().as_deref(), Some("rotated"));
        assert!(tokens.load(account.id).unwrap().is_none());
        let offers = services.offers();
        assert!(
            !offers.calendar
                && !offers.contacts
                && !offers.rules
                && !offers.auto_reply
                && !offers.tags
                && !offers.focused
        );
        let denied = add_at(
            &db,
            tokens,
            client,
            owner,
            "denied@contoso.com".into(),
            0,
            30,
            &format!("{}/v1.0/", server.uri()),
        )
        .await;
        assert!(denied.is_err());
        assert_eq!(db.read(accounts::list_accounts).await.unwrap().len(), 2);
    }
}
