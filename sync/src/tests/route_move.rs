//! Moving a Gmail account to a new sign-in: the drain on the old route,
//! the move to an own client, and the one transaction that commits a
//! route.

use std::sync::{Arc, Mutex};

use mailrs_domain::{AccountState, MailRoute, Provider, SignInClient};
use mailrs_gmail::{Authorized, GmailError, Granted, TokenStore};
use mailrs_store::accounts::{self, Consent, GoogleSignIn, RouteCommit};
use mailrs_store::outbox::{self, Queued};
use mailrs_store::{Db, StoreError, drafts};

use super::{Connected, Harness, eventually, harness};
use crate::fake::meta;
use crate::passwords::{PasswordStore, Secrets};
use crate::route_move::{DRAIN_WAIT, MoveError, commit, drain, needs_move, to_own_client};
use crate::sign_in::own_client::OwnClient;
use crate::{Outbox, Posted, TriageAction, now_millis};

const OLD_GRANTED: &str = "openid email gmail.modify";
const OLD_ASKED: &str = "openid email gmail.modify calendar";

/// A Gmail account on the built-in client, bootstrapped with two threads,
/// a draft row, the sync state "777" and the consent of its old sign-in.
async fn built_in_account() -> Harness {
    let h = harness().await;
    let now = now_millis();
    h.fake.seed(meta("a", "t1", now - 1000, &["INBOX"]));
    h.fake.seed(meta("b", "t2", now, &["INBOX"]));
    h.bootstrap_all().await;
    let id = h.account_id;
    h.db.write(move |c| {
        drafts::remember(c, id, "d1", "a")?;
        accounts::set_sync_state(c, id, "777")?;
        accounts::set_granted(c, id, OLD_GRANTED)?;
        accounts::set_asked(c, id, OLD_ASKED)
    })
    .await
    .unwrap();
    h
}

fn own() -> OwnClient {
    OwnClient::new("own-cid", "own-secret")
}

fn signed_in_as(email: &str) -> Authorized {
    Authorized {
        email: email.into(),
        refresh_token: "own-token".into(),
        granted: Some(Granted::parse("openid email gmail.modify calendar")),
    }
}

async fn client(db: &Db, id: i64) -> SignInClient {
    db.read(move |c| accounts::sign_in_client(c, id))
        .await
        .unwrap()
}

async fn route(db: &Db, id: i64) -> MailRoute {
    db.read(move |c| accounts::mail_route(c, id)).await.unwrap()
}

async fn sync_state(db: &Db, id: i64) -> Option<String> {
    db.read(move |c| accounts::sync_cursor(c, id))
        .await
        .unwrap()
        .state
}

async fn consent(db: &Db, id: i64) -> Consent {
    db.read(move |c| accounts::consent(c, id)).await.unwrap()
}

async fn count(db: &Db, table: &'static str) -> i64 {
    db.read(move |c| {
        Ok(
            c.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })?,
        )
    })
    .await
    .unwrap()
}

async fn account(h: &Harness) -> mailrs_domain::Account {
    let id = h.account_id;
    h.db.read(move |c| accounts::account(c, id))
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn moving_to_an_own_client_keeps_the_account_whole() {
    let h = built_in_account().await;
    let (messages, kept_drafts) = (count(&h.db, "messages").await, count(&h.db, "drafts").await);
    let calls = h.fake.with(|s| s.usage.calls);
    let secrets = Secrets::memory();

    let moved = to_own_client(
        &h.db,
        &secrets,
        &account(&h).await,
        &own(),
        signed_in_as("ME@example.com"),
    )
    .await
    .unwrap();

    assert_eq!(moved.id, h.account_id);
    assert_eq!(client(&h.db, h.account_id).await, SignInClient::Own);
    assert_eq!(route(&h.db, h.account_id).await, MailRoute::GmailApi);
    assert_eq!(
        sync_state(&h.db, h.account_id).await.as_deref(),
        Some("777")
    );
    assert_eq!(count(&h.db, "messages").await, messages);
    assert_eq!(count(&h.db, "drafts").await, kept_drafts);
    assert_eq!(
        h.fake.with(|s| s.usage.calls),
        calls,
        "nothing fetched again"
    );
    assert_eq!(
        secrets.google.load("me@example.com").unwrap().as_deref(),
        Some("own-token")
    );
    assert_eq!(
        consent(&h.db, h.account_id).await.asked,
        Some(own().oauth().scopes().join(" "))
    );
}

#[tokio::test]
async fn a_move_signed_in_as_another_address_changes_nothing() {
    let h = built_in_account().await;
    let secrets = Secrets::memory();

    let refused = to_own_client(
        &h.db,
        &secrets,
        &account(&h).await,
        &own(),
        signed_in_as("someone@example.com"),
    )
    .await;

    assert!(matches!(
        refused,
        Err(MoveError::OtherAddress { ref signed_in, ref wanted })
            if signed_in == "someone@example.com" && wanted == "me@example.com"
    ));
    assert_eq!(client(&h.db, h.account_id).await, SignInClient::BuiltIn);
    assert_eq!(secrets.google.load("me@example.com").unwrap(), None);
}

/// Hands out the token it holds and refuses to keep another.
struct RefusingTokens(Mutex<Option<String>>);

impl TokenStore for RefusingTokens {
    fn load(&self, _: &str) -> Result<Option<String>, GmailError> {
        Ok(self.0.lock().unwrap().clone())
    }
    fn save(&self, _: &str, _: &str) -> Result<(), GmailError> {
        Err(GmailError::Keyring("locked".into()))
    }
    fn delete(&self, _: &str) -> Result<(), GmailError> {
        Ok(())
    }
}

#[tokio::test]
async fn a_move_whose_keyring_refuses_keeps_the_built_in_client() {
    let h = built_in_account().await;
    let secrets = Secrets {
        google: Arc::new(RefusingTokens(Mutex::new(Some("built-in-token".into())))),
        ..Secrets::memory()
    };

    let refused = to_own_client(
        &h.db,
        &secrets,
        &account(&h).await,
        &own(),
        signed_in_as("me@example.com"),
    )
    .await;

    assert!(matches!(refused, Err(MoveError::SignIn(_))));
    assert_eq!(client(&h.db, h.account_id).await, SignInClient::BuiltIn);
    assert_eq!(
        secrets.google.load("me@example.com").unwrap().as_deref(),
        Some("built-in-token")
    );
    assert_eq!(
        consent(&h.db, h.account_id).await,
        Consent {
            granted: Some(OLD_GRANTED.into()),
            asked: Some(OLD_ASKED.into()),
        }
    );
    assert_eq!(
        secrets.google_clients.load(h.account_id).unwrap(),
        None,
        "no own client is left beside the built-in token"
    );
}

#[tokio::test]
async fn a_failed_rewrite_leaves_route_client_and_sync_state_as_they_were() {
    let h = built_in_account().await;
    let err = commit(
        &h.db,
        h.account_id,
        RouteCommit {
            route: MailRoute::Imap,
            client: SignInClient::BuiltIn,
            restart_sync: true,
        },
        |_| {
            Err(StoreError::Corrupt {
                column: "test",
                value: String::new(),
            })
        },
    )
    .await;

    assert!(err.is_err());
    assert_eq!(route(&h.db, h.account_id).await, MailRoute::GmailApi);
    assert_eq!(client(&h.db, h.account_id).await, SignInClient::BuiltIn);
    assert_eq!(
        sync_state(&h.db, h.account_id).await.as_deref(),
        Some("777")
    );
}

#[tokio::test]
async fn a_commit_writes_the_route_and_its_rewrite_together() {
    let h = built_in_account().await;
    let id = h.account_id;
    commit(
        &h.db,
        id,
        RouteCommit {
            route: MailRoute::Imap,
            client: SignInClient::BuiltIn,
            restart_sync: true,
        },
        move |c| drafts::forget(c, id, "d1"),
    )
    .await
    .unwrap();

    assert_eq!(route(&h.db, id).await, MailRoute::Imap);
    assert_eq!(sync_state(&h.db, id).await, None);
    assert_eq!(count(&h.db, "drafts").await, 0);
}

#[tokio::test]
async fn queued_work_lands_before_the_move() {
    let h = harness().await;
    let now = now_millis();
    for (message, thread) in [("a", "t1"), ("b", "t2"), ("c", "t3")] {
        h.fake.seed(meta(message, thread, now, &["INBOX"]));
    }
    h.bootstrap_all().await;
    queue_due(&h, "Report").await;

    // The first write waits at Gmail; the other two queue behind it.
    let mut first = h.fake.hold("users.messages.modify");
    let writes = [
        ("t1", TriageAction::Archive),
        ("t2", TriageAction::AddLabel("IMPORTANT".into())),
        ("t3", TriageAction::Trash),
    ]
    .map(|(thread, action)| {
        let sync = Arc::clone(&h.sync);
        tokio::spawn(async move { sync.triage_thread(thread, &action).await })
    });
    first.entered().await;
    assert!(eventually(|| h.sync.mail().moves_waiting() == 2).await);

    let outbox = outbox_of(&h);
    // Gmail lets the first write go only once the drain waits behind the
    // other two, so a drain that stopped after the send would end first.
    let (drained, ()) = tokio::join!(drain(&h.sync, &outbox, now_millis(), DRAIN_WAIT), async {
        assert!(eventually(|| h.sync.mail().moves_waiting() == 3).await);
        first.release()
    });

    assert!(drained);
    let modifies = h.fake.with(|s| {
        s.usage.calls_to("users.messages.modify") + s.usage.calls_to("users.messages.batchModify")
    });
    assert_eq!(
        modifies, 3,
        "every write reached Gmail before the drain ended"
    );
    assert_eq!(h.fake.with(|s| s.sent.len()), 1);
    for write in writes {
        write.await.unwrap().unwrap();
    }
}

/// A message to `subject`'s reader, due now and in no row yet.
fn due(h: &Harness, subject: &str) -> Queued {
    Queued {
        account_id: h.account_id,
        subject: subject.into(),
        recipients: "ann@example.com".into(),
        send_at: now_millis(),
        raw: Some(format!("Subject: {subject}\r\n\r\nhello").into_bytes()),
        composer: "{}".into(),
        ..Queued::default()
    }
}

/// Puts a message due now in the outbox and answers its row id.
async fn queue_due(h: &Harness, subject: &str) -> i64 {
    let due = due(h, subject);
    h.db.write(move |c| outbox::put(c, &due)).await.unwrap()
}

fn outbox_of(h: &Harness) -> Outbox<Connected> {
    Outbox::new(
        Arc::new(Connected(
            [(h.account_id, Arc::clone(&h.sync))].into_iter().collect(),
        )),
        h.db.clone(),
    )
}

async fn row(h: &Harness, id: i64) -> Queued {
    h.db.read(move |c| outbox::find(c, id))
        .await
        .unwrap()
        .expect("the message still waits")
}

/// Whether the row waits as it did before anything tried it.
fn untried(row: &Queued) -> bool {
    row.attempts == 0 && row.problem.is_none()
}

async fn pause(h: &Harness) {
    let id = h.account_id;
    h.db.write(move |c| accounts::set_state(c, id, AccountState::MailPaused))
        .await
        .unwrap();
}

/// What the move leaves: the same account, on an own client.
async fn moved(h: &Harness) {
    let id = h.account_id;
    h.db.write(move |c| accounts::set_sign_in_client(c, id, SignInClient::Own))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_paused_drain_keeps_the_send_for_the_new_route() {
    let h = harness().await;
    pause(&h).await;
    let id = queue_due(&h, "Report").await;

    assert!(drain(&h.sync, &outbox_of(&h), now_millis(), DRAIN_WAIT).await);
    assert_eq!(h.fake.with(|s| s.usage.calls_to("users.messages.send")), 0);
    assert!(untried(&row(&h, id).await));

    moved(&h).await;
    outbox_of(&h).send_due(now_millis()).await.unwrap();
    assert_eq!(h.fake.with(|s| s.sent.len()), 1);
}

#[tokio::test]
async fn a_drain_refused_by_the_old_route_keeps_the_sends_for_the_new_one() {
    let h = harness().await;
    let first = queue_due(&h, "Report").await;
    let second = queue_due(&h, "Minutes").await;
    h.fake.fail_next(GmailError::NeedsReauth);

    assert!(drain(&h.sync, &outbox_of(&h), now_millis(), DRAIN_WAIT).await);
    assert_eq!(
        h.fake.with(|s| s.sent.len()),
        0,
        "the drain ended at the refusal"
    );
    assert!(untried(&row(&h, first).await));
    assert!(untried(&row(&h, second).await));

    moved(&h).await;
    outbox_of(&h).send_due(now_millis()).await.unwrap();
    assert_eq!(h.fake.with(|s| s.sent.len()), 2);
}

#[tokio::test]
async fn the_outbox_leaves_a_paused_accounts_mail_until_it_moves() {
    let h = harness().await;
    pause(&h).await;
    let id = queue_due(&h, "Report").await;
    let outbox = outbox_of(&h);

    outbox.send_due(now_millis()).await.unwrap();
    let posted = outbox.send_one(id).await.unwrap();
    assert_eq!(posted, Posted::Waiting(id));
    assert_eq!(h.fake.with(|s| s.usage.calls_to("users.messages.send")), 0);
    assert!(untried(&row(&h, id).await));

    moved(&h).await;
    outbox.send_due(now_millis()).await.unwrap();
    assert_eq!(h.fake.with(|s| s.sent.len()), 1);
}

#[tokio::test]
async fn a_send_the_old_route_refuses_waits_for_the_new_one() {
    let h = harness().await;
    let outbox = outbox_of(&h);
    h.fake.fail_next(GmailError::MissingScope);

    let Posted::Waiting(id) = outbox.post(due(&h, "Report")).await.unwrap() else {
        panic!("the message should wait for the new route");
    };
    assert!(untried(&row(&h, id).await));

    moved(&h).await;
    outbox.send_due(now_millis()).await.unwrap();
    assert_eq!(h.fake.with(|s| s.sent.len()), 1);
}

#[test]
fn only_built_in_api_gmail_accounts_need_a_move() {
    let sign_in = |client, route| GoogleSignIn { client, route };
    let gmail = |client, route| needs_move(Provider::Gmail, sign_in(client, route));
    assert!(gmail(SignInClient::BuiltIn, MailRoute::GmailApi));
    assert!(!gmail(SignInClient::BuiltIn, MailRoute::Imap));
    assert!(!gmail(SignInClient::Own, MailRoute::GmailApi));
    assert!(!gmail(SignInClient::Own, MailRoute::Imap));
    assert!(!needs_move(
        Provider::Imap,
        sign_in(SignInClient::BuiltIn, MailRoute::GmailApi)
    ));
}
