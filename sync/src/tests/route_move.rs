//! Moving a Gmail account to a new sign-in: the drain on the old route,
//! the move to an own client, and the one transaction that commits a
//! route.

use std::sync::{Arc, Mutex};

use mailrs_domain::{AccountState, MailRoute, MailboxKind, Provider, RemoteMailbox, SignInClient};
use mailrs_gmail::{Authorized, GmailError, Granted, TokenStore};
use mailrs_store::accounts::{self, Consent, GoogleSignIn, RouteCommit};
use mailrs_store::messages::{self, Change};
use mailrs_store::outbox::{self, Queued};
use mailrs_store::{Db, StoreError, drafts, mailboxes, servers};

use super::{Connected, Harness, eventually, harness};
use crate::fake::meta;
use crate::passwords::{MemoryPasswords, PasswordStore, Passwords, Secrets};
use crate::route_move::{
    DRAIN_WAIT, MoveEngine, MoveError, Mover, RewriteMail, commit, drain, empty_for_imap,
    needs_move, to_own_client,
};
use crate::sign_in::app_password::NewGmailImap;
use crate::sign_in::goa_link::Linked;
use crate::sign_in::own_client::OwnClient;
use crate::{BackendError, Outbox, Posted, SyncError, TriageAction, now_millis};

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

/// Hands out the token it holds and refuses to keep another. A delete
/// takes the token away, so a move that deletes it shows.
struct RefusingTokens(Mutex<Option<String>>);

impl TokenStore for RefusingTokens {
    fn load(&self, _: &str) -> Result<Option<String>, GmailError> {
        Ok(self.0.lock().unwrap().clone())
    }
    fn save(&self, _: &str, _: &str) -> Result<(), GmailError> {
        Err(GmailError::Keyring("locked".into()))
    }
    fn delete(&self, _: &str) -> Result<(), GmailError> {
        *self.0.lock().unwrap() = None;
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

/// The engine a move drives, writing down what the move asked of it and
/// which client the store named when the account started again.
struct Steps {
    sync: Arc<crate::AccountSync>,
    db: Db,
    done: Mutex<Vec<String>>,
}

impl Steps {
    fn new(h: &Harness) -> Steps {
        Steps {
            sync: Arc::clone(&h.sync),
            db: h.db.clone(),
            done: Mutex::default(),
        }
    }

    fn note(&self, step: impl Into<String>) {
        self.done.lock().unwrap().push(step.into());
    }

    fn done(&self) -> Vec<String> {
        self.done.lock().unwrap().clone()
    }
}

impl MoveEngine for Steps {
    fn running(&self, account: mailrs_domain::AccountId) -> Option<Arc<crate::AccountSync>> {
        (account == self.sync.account_id()).then(|| Arc::clone(&self.sync))
    }

    fn stop(&self, _: mailrs_domain::AccountId) {
        self.note("stop");
    }

    async fn start_again(&self, account: &mailrs_domain::Account) {
        let started = client(&self.db, account.id).await;
        self.note(format!("start on {started:?}"));
    }
}

#[tokio::test]
async fn a_move_as_another_address_ends_before_the_account_stops() {
    let h = built_in_account().await;
    let secrets = Secrets::memory();
    let steps = Steps::new(&h);
    let outbox = outbox_of(&h);
    let mover = Mover {
        engine: &steps,
        db: &h.db,
        secrets: &secrets,
        outbox: &outbox,
    };

    let refused = mover
        .to_own_client(
            &account(&h).await,
            &own(),
            signed_in_as("someone@example.com"),
            || steps.note("committed"),
        )
        .await;

    assert!(matches!(refused, Err(MoveError::OtherAddress { .. })));
    assert_eq!(steps.done(), Vec::<String>::new());
    assert_eq!(client(&h.db, h.account_id).await, SignInClient::BuiltIn);
    assert_eq!(secrets.google.load("me@example.com").unwrap(), None);
}

#[tokio::test]
async fn a_move_drains_the_old_route_then_starts_on_the_own_client() {
    let h = built_in_account().await;
    queue_due(&h, "Report").await;
    let secrets = Secrets::memory();
    let steps = Steps::new(&h);
    let outbox = outbox_of(&h);
    let mover = Mover {
        engine: &steps,
        db: &h.db,
        secrets: &secrets,
        outbox: &outbox,
    };

    let moved = mover
        .to_own_client(
            &account(&h).await,
            &own(),
            signed_in_as("Me@Example.com"),
            || steps.note("committed"),
        )
        .await
        .unwrap();

    assert_eq!(moved.id, h.account_id);
    assert_eq!(steps.done(), ["committed", "stop", "start on Own"]);
    assert_eq!(
        h.fake.with(|s| s.sent.len()),
        1,
        "the old route sent what was due"
    );
    assert_eq!(
        secrets.google.load("me@example.com").unwrap().as_deref(),
        Some("own-token")
    );
}

fn gmail_imap(address: &str) -> NewGmailImap {
    let saved = |host: &str, port| mailrs_store::servers::Saved {
        host: host.into(),
        port,
        security: mailrs_store::servers::Security::Tls,
        user_name: address.into(),
    };
    NewGmailImap {
        address: address.into(),
        servers: mailrs_store::servers::Servers {
            imap: saved("imap.gmail.com", 993),
            smtp: saved("smtp.gmail.com", 465),
        },
        password: "abcdefghijklmnop".into(),
    }
}

/// Files message "a" under a user label the API named `Label_3`.
async fn label_a_message(h: &Harness) {
    let id = h.account_id;
    h.db.write(move |c| {
        mailboxes::upsert(
            c,
            id,
            &RemoteMailbox {
                id: "Label_3".into(),
                name: "Work/Clients".into(),
                kind: MailboxKind::Label,
                role: None,
                color: Some("#ff0000".into()),
                hidden: false,
            },
        )?;
        let (membership, _) = mailrs_gmail::labels::membership_of("Label_3");
        messages::apply(c, id, &[Change::of("a", membership, true)])?;
        Ok(())
    })
    .await
    .unwrap();
}

async fn labels_of_a(h: &Harness) -> Vec<String> {
    let id = h.account_id;
    h.db.read(move |c| {
        let held = messages::memberships_of(c, id, &["a".to_string()])?;
        Ok(held
            .get("a")
            .map(mailrs_gmail::labels::labels)
            .unwrap_or_default())
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn the_imap_move_empties_the_mail_and_keeps_the_outbox_and_the_rules() {
    let h = built_in_account().await;
    label_a_message(&h).await;
    let id = h.account_id;
    let rule = mailrs_domain::Filter {
        id: Some("local-1".into()),
        action: mailrs_domain::FilterAction {
            add: vec![mailrs_domain::MailSet::Mailbox("Label_3".into())],
            ..Default::default()
        },
        ..mailrs_domain::Filter::default()
    };
    let waiting = Queued {
        draft_id: Some("d1".into()),
        message_id: Some("a".into()),
        send_at: now_millis() + 3_600_000,
        raw: None,
        ..due(&h, "later")
    };
    let row_id = h
        .db
        .write(move |c| {
            mailrs_store::local_rules::add(c, id, &rule)?;
            outbox::put(c, &waiting)
        })
        .await
        .unwrap();

    h.db.write(move |c| empty_for_imap(c, id)).await.unwrap();

    for table in ["messages", "threads", "mailboxes", "drafts", "remote_refs"] {
        assert_eq!(count(&h.db, table).await, 0, "{table}");
    }
    assert_eq!(row(&h, row_id).await.draft_id.as_deref(), Some("a"));
    let rules = h
        .db
        .read(move |c| mailrs_store::local_rules::list(c, id))
        .await
        .unwrap();
    assert_eq!(
        rules[0].action.add,
        [mailrs_domain::MailSet::Mailbox("Work/Clients".into())]
    );
}

#[tokio::test]
async fn a_move_to_imap_takes_send_later_bytes_out_of_the_draft() {
    let h = built_in_account().await;
    let outbox = outbox_of(&h);
    let later = Queued {
        send_at: now_millis() + 3_600_000,
        ..due(&h, "later")
    };
    let Posted::Waiting(row_id) = outbox.schedule(later).await.unwrap() else {
        panic!("the message should wait for its hour");
    };
    let scheduled = row(&h, row_id).await;
    assert!(scheduled.raw.is_none(), "Gmail holds the bytes");
    let secrets = Secrets::memory();
    let steps = Steps::new(&h);
    let mover = Mover {
        engine: &steps,
        db: &h.db,
        secrets: &secrets,
        outbox: &outbox,
    };

    mover
        .to_imap(&account(&h).await, gmail_imap("me@example.com"), || {})
        .await
        .unwrap();

    let kept = row(&h, row_id).await;
    assert!(
        kept.raw.as_deref().is_some_and(|raw| raw.ends_with(b"hello")),
        "the bytes came home"
    );
    assert_eq!(kept.draft_id, scheduled.message_id);
}

#[tokio::test]
async fn a_move_to_imap_restarts_sync_and_empties_the_mail() {
    let h = built_in_account().await;
    assert!(count(&h.db, "messages").await > 0);
    let secrets = Secrets::memory();
    let steps = Steps::new(&h);
    let outbox = outbox_of(&h);
    let mover = Mover {
        engine: &steps,
        db: &h.db,
        secrets: &secrets,
        outbox: &outbox,
    };

    let moved = mover
        .to_imap(&account(&h).await, gmail_imap("Me@Example.com"), || {
            steps.note("committed")
        })
        .await
        .unwrap();

    assert_eq!(moved.id, h.account_id);
    assert_eq!(route(&h.db, h.account_id).await, MailRoute::Imap);
    assert_eq!(sync_state(&h.db, h.account_id).await, None);
    assert_eq!(count(&h.db, "messages").await, 0);
    assert_eq!(count(&h.db, "drafts").await, 0);
    assert_eq!(steps.done(), ["committed", "stop", "start on BuiltIn"]);
    assert_eq!(
        secrets.passwords.load(h.account_id).unwrap().as_deref(),
        Some("abcdefghijklmnop")
    );
    let id = h.account_id;
    let servers = h.db.read(move |c| servers::load(c, id)).await.unwrap();
    assert_eq!(servers.unwrap().imap.host, "imap.gmail.com");
}

#[tokio::test]
async fn a_move_to_imap_as_another_address_changes_nothing() {
    let h = built_in_account().await;
    let secrets = Secrets::memory();
    let steps = Steps::new(&h);
    let outbox = outbox_of(&h);
    let mover = Mover {
        engine: &steps,
        db: &h.db,
        secrets: &secrets,
        outbox: &outbox,
    };

    let refused = mover
        .to_imap(
            &account(&h).await,
            gmail_imap("someone@example.com"),
            || steps.note("committed"),
        )
        .await;

    assert!(matches!(refused, Err(MoveError::OtherAddress { .. })));
    assert_eq!(steps.done(), Vec::<String>::new());
    assert_eq!(route(&h.db, h.account_id).await, MailRoute::GmailApi);
}

#[tokio::test]
async fn a_move_to_imap_whose_keyring_refuses_keeps_the_api_route() {
    let h = built_in_account().await;
    let kept_drafts = count(&h.db, "drafts").await;
    let memory = Arc::new(MemoryPasswords::default());
    memory.refuse_saves();
    let mut secrets = Secrets::memory();
    secrets.passwords = Arc::new(Passwords::Memory(Arc::try_unwrap(memory).ok().unwrap()));
    let steps = Steps::new(&h);
    let outbox = outbox_of(&h);
    let mover = Mover {
        engine: &steps,
        db: &h.db,
        secrets: &secrets,
        outbox: &outbox,
    };

    let refused = mover
        .to_imap(&account(&h).await, gmail_imap("me@example.com"), || {})
        .await;

    assert!(matches!(
        refused,
        Err(MoveError::Sync(SyncError::Password(_)))
    ));
    assert_eq!(route(&h.db, h.account_id).await, MailRoute::GmailApi);
    assert_eq!(count(&h.db, "drafts").await, kept_drafts);
    assert_eq!(steps.done(), ["stop", "start on BuiltIn"]);
}

fn goa_dana(identity: &str) -> crate::goa::GoaAccount {
    crate::goa::GoaAccount {
        id: "account_1_0".into(),
        identity: identity.into(),
        presentation: identity.into(),
        mail_disabled: false,
        attention_needed: false,
    }
}

async fn goa_id(db: &Db, id: i64) -> Option<String> {
    db.read(move |c| accounts::goa_id(c, id)).await.unwrap()
}

/// Rewrites the account's stored mail for its new route, as the move
/// from the API to IMAP does: here it only drops the draft rows.
fn drop_drafts(id: i64) -> Option<RewriteMail> {
    Some(Box::new(move |c| drafts::forget(c, id, "d1")))
}

#[tokio::test]
async fn a_built_in_api_account_moves_to_goa_imap_with_the_rewrite_in_one_step() {
    let h = built_in_account().await;
    let secrets = Secrets::memory();
    let steps = Steps::new(&h);
    let outbox = outbox_of(&h);
    let mover = Mover {
        engine: &steps,
        db: &h.db,
        secrets: &secrets,
        outbox: &outbox,
    };

    let moved = mover
        .to_goa(
            &account(&h).await,
            &goa_dana("me@example.com"),
            Linked::Imap,
            drop_drafts(h.account_id),
            || steps.note("committed"),
        )
        .await
        .unwrap();

    assert_eq!(moved.id, h.account_id);
    assert_eq!(steps.done(), ["committed", "stop", "start on Goa"]);
    assert_eq!(client(&h.db, h.account_id).await, SignInClient::Goa);
    assert_eq!(route(&h.db, h.account_id).await, MailRoute::Imap);
    assert_eq!(sync_state(&h.db, h.account_id).await, None);
    assert_eq!(count(&h.db, "drafts").await, 0, "the rewrite ran");
    assert_eq!(
        goa_id(&h.db, h.account_id).await.as_deref(),
        Some("account_1_0")
    );
}

#[tokio::test]
async fn a_move_to_goa_without_a_rewrite_for_another_route_stops_nothing() {
    let h = built_in_account().await;
    let secrets = Secrets::memory();
    let steps = Steps::new(&h);
    let outbox = outbox_of(&h);
    let mover = Mover {
        engine: &steps,
        db: &h.db,
        secrets: &secrets,
        outbox: &outbox,
    };

    let refused = mover
        .to_goa(
            &account(&h).await,
            &goa_dana("me@example.com"),
            Linked::Imap,
            None,
            || steps.note("committed"),
        )
        .await;

    assert!(
        matches!(
            refused,
            Err(MoveError::Sync(SyncError::Backend(
                BackendError::Unsupported
            )))
        ),
        "{:?}",
        refused.map(|_| ())
    );
    assert_eq!(steps.done(), Vec::<String>::new());
    assert_eq!(client(&h.db, h.account_id).await, SignInClient::BuiltIn);
    assert_eq!(route(&h.db, h.account_id).await, MailRoute::GmailApi);
}

#[tokio::test]
async fn an_app_password_account_moves_to_goa_imap_without_a_resync() {
    let h = built_in_account().await;
    let id = h.account_id;
    h.db.write(move |c| accounts::set_mail_route(c, id, MailRoute::Imap))
        .await
        .unwrap();
    let secrets = Secrets::memory();
    let steps = Steps::new(&h);
    let outbox = outbox_of(&h);
    let mover = Mover {
        engine: &steps,
        db: &h.db,
        secrets: &secrets,
        outbox: &outbox,
    };

    mover
        .to_goa(
            &account(&h).await,
            &goa_dana("me@example.com"),
            Linked::Imap,
            None,
            || (),
        )
        .await
        .unwrap();

    assert_eq!(client(&h.db, id).await, SignInClient::Goa);
    assert_eq!(sync_state(&h.db, id).await.as_deref(), Some("777"));
    assert_eq!(goa_id(&h.db, id).await.as_deref(), Some("account_1_0"));
}

#[tokio::test]
async fn a_move_to_another_goa_address_ends_before_the_account_stops() {
    let h = built_in_account().await;
    let secrets = Secrets::memory();
    let steps = Steps::new(&h);
    let outbox = outbox_of(&h);
    let mover = Mover {
        engine: &steps,
        db: &h.db,
        secrets: &secrets,
        outbox: &outbox,
    };

    let refused = mover
        .to_goa(
            &account(&h).await,
            &goa_dana("other@example.com"),
            Linked::Imap,
            drop_drafts(h.account_id),
            || steps.note("committed"),
        )
        .await;

    assert!(matches!(
        refused,
        Err(MoveError::OtherAddress { ref signed_in, ref wanted })
            if signed_in == "other@example.com" && wanted == "me@example.com"
    ));
    assert_eq!(steps.done(), Vec::<String>::new());
    assert_eq!(client(&h.db, h.account_id).await, SignInClient::BuiltIn);
}

/// Puts the account on Online Accounts' IMAP, as linking it does.
async fn on_goa_imap(h: &Harness) {
    let id = h.account_id;
    h.db.write(move |c| {
        accounts::set_mail_route(c, id, MailRoute::Imap)?;
        accounts::set_sign_in_client(c, id, SignInClient::Goa)?;
        accounts::set_goa_id(c, id, Some("account_1_0"))
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn an_online_accounts_account_moves_to_an_app_password_and_leaves_online_accounts() {
    let h = built_in_account().await;
    on_goa_imap(&h).await;
    let messages_before = count(&h.db, "messages").await;
    let secrets = Secrets::memory();
    let steps = Steps::new(&h);
    let outbox = outbox_of(&h);
    let mover = Mover {
        engine: &steps,
        db: &h.db,
        secrets: &secrets,
        outbox: &outbox,
    };

    mover
        .to_imap(&account(&h).await, gmail_imap("me@example.com"), || {})
        .await
        .unwrap();

    let id = h.account_id;
    assert_eq!(client(&h.db, id).await, SignInClient::BuiltIn);
    assert_eq!(goa_id(&h.db, id).await, None);
    assert_eq!(route(&h.db, id).await, MailRoute::Imap);
    assert_eq!(count(&h.db, "messages").await, messages_before);
    assert_eq!(sync_state(&h.db, id).await.as_deref(), Some("777"));
}

#[tokio::test]
async fn an_online_accounts_account_with_a_kept_project_moves_to_its_own_client() {
    let h = built_in_account().await;
    on_goa_imap(&h).await;
    let secrets = Secrets::memory();
    secrets
        .google_clients
        .save(h.account_id, &own().encode())
        .unwrap();
    let steps = Steps::new(&h);
    let outbox = outbox_of(&h);
    let mover = Mover {
        engine: &steps,
        db: &h.db,
        secrets: &secrets,
        outbox: &outbox,
    };

    mover
        .to_imap(&account(&h).await, gmail_imap("me@example.com"), || {})
        .await
        .unwrap();

    assert_eq!(client(&h.db, h.account_id).await, SignInClient::Own);
    assert_eq!(goa_id(&h.db, h.account_id).await, None);
}

#[tokio::test]
async fn an_imap_account_moving_to_goa_stays_on_imap_whatever_the_probe_found() {
    let h = built_in_account().await;
    let id = h.account_id;
    h.db.write(move |c| accounts::set_mail_route(c, id, MailRoute::Imap))
        .await
        .unwrap();
    let secrets = Secrets::memory();
    let steps = Steps::new(&h);
    let outbox = outbox_of(&h);
    let mover = Mover {
        engine: &steps,
        db: &h.db,
        secrets: &secrets,
        outbox: &outbox,
    };

    mover
        .to_goa(
            &account(&h).await,
            &goa_dana("me@example.com"),
            Linked::Api,
            None,
            || (),
        )
        .await
        .unwrap();

    assert_eq!(route(&h.db, id).await, MailRoute::Imap);
    assert_eq!(client(&h.db, id).await, SignInClient::Goa);
}

#[tokio::test]
async fn an_app_password_account_moving_to_goa_drops_its_password_and_servers() {
    let h = built_in_account().await;
    let id = h.account_id;
    let saved = gmail_imap("me@example.com").servers;
    h.db.write(move |c| {
        accounts::set_mail_route(c, id, MailRoute::Imap)?;
        servers::save(c, id, &saved)
    })
    .await
    .unwrap();
    let secrets = Secrets::memory();
    secrets.passwords.save(id, "abcdefghijklmnop").unwrap();
    let steps = Steps::new(&h);
    let outbox = outbox_of(&h);
    let mover = Mover {
        engine: &steps,
        db: &h.db,
        secrets: &secrets,
        outbox: &outbox,
    };

    mover
        .to_goa(
            &account(&h).await,
            &goa_dana("me@example.com"),
            Linked::Imap,
            None,
            || (),
        )
        .await
        .unwrap();

    assert_eq!(
        h.db.read(move |c| servers::load(c, id)).await.unwrap(),
        None
    );
    assert_eq!(secrets.passwords.load(id).unwrap(), None);
}

#[tokio::test]
async fn a_send_later_message_whose_draft_is_gone_stays_and_asks_to_check_sent() {
    let h = built_in_account().await;
    let gone = Queued {
        draft_id: Some("r-gone".into()),
        message_id: Some("gone".into()),
        raw: None,
        ..due(&h, "lost")
    };
    let row_id = h.db.write(move |c| outbox::put(c, &gone)).await.unwrap();

    let drained = outbox_of(&h).send_due(now_millis()).await.unwrap();

    assert!(drained.sent.is_empty(), "a gone draft never counts as sent");
    let kept = row(&h, row_id).await;
    assert!(
        kept.problem.as_deref().is_some_and(|p| p.contains("Check Sent")),
        "{:?}",
        kept.problem
    );
}
