//! The JMAP adapter over the real client, against Cyrus, Stalwart and
//! Apache James in Docker. The server's own JMAP, through the test's own
//! client in `mailrs_testmail`, plays the other client and the witness.
//! One test, as every Docker file here has.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use mailrs_domain::query::{Query, Term};
use mailrs_domain::{AccountId, ChangeEvent, Target};
use mailrs_jmap::{Auth, JmapClient};
use mailrs_store::{Db, accounts};
use mailrs_sync::{
    AccountServices, AccountSync, AnyAutoReply, AnyCalendar, AnyContacts, AnyJmap, AnyMail,
    AnyRules, AutoReplyService, BackendError, CalendarService, ContactsService,
    DEFAULT_BODY_CACHE_BYTES, IdentityService, JmapSettings, JmapSieve, LocalRules, MailBackend,
    RulesService, SearchQuery, Settled, SieveRules, TriageAction, Unheld,
};
use mailrs_testmail::{JmapKind, JmapServer};
use rusqlite::OptionalExtension;
use serde_json::json;

const WINDOW_DAYS: i64 = 30;

/// How long the push step waits for the server to report a delivery.
const PUSH_WAIT: Duration = Duration::from_secs(10);

/// How long a write may take while the same adapter watches.
const WRITE_WAIT: Duration = Duration::from_secs(15);

/// How often an account looks when push does not work.
const NO_PUSH_POLL: Duration = Duration::from_secs(60);

const MAIL: [&str; 2] = ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail"];

#[test]
fn the_jmap_adapter_syncs_and_writes_against_every_server() {
    if mailrs_testmail::jmap_docker().is_none() {
        return;
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .thread_stack_size(mailrs_sync::WORKER_STACK)
        .enable_all()
        .build()
        .expect("a runtime");
    let body = runtime.spawn(async move {
        let password = mailrs_testmail::password();
        for kind in JmapKind::ALL {
            eprintln!("{kind:?}");
            let Some(server) = JmapServer::start(kind, &password).await else {
                return;
            };
            Run::new(&server, &password).await.all().await;
        }
    });
    runtime
        .block_on(body)
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic.into_panic()));
}

struct Run<'a> {
    server: &'a JmapServer,
    password: &'a str,
    db: Db,
    account_id: AccountId,
    sync: AccountSync,
    _events: async_channel::Receiver<ChangeEvent>,
    _dir: tempfile::TempDir,
}

/// The services the app would attach for this server: what its session
/// offers, as `connect_jmap` reads it from the saved capabilities.
async fn services(
    server: &JmapServer,
    password: &str,
    db: &Db,
    account_id: AccountId,
) -> AccountServices {
    let client = JmapClient::new(
        &server.session_url,
        Auth::Basic {
            user: server.user.clone(),
            password: password.to_string(),
        },
    )
    .expect("a client for the server");
    let session = client.refresh_session().await.expect("the session");
    let capabilities: Vec<String> = session
        .capabilities
        .keys()
        .filter(|c| session.offers(c))
        .cloned()
        .collect();
    let settings = JmapSettings {
        address: server.address.clone(),
        provider_name: format!("{:?}", server.kind),
        window_days: WINDOW_DAYS,
        labels: true,
        capabilities: capabilities.clone(),
        auto_reply_needs_sign_in: false,
    };
    let jmap = Arc::new(AnyJmap::from(client.clone()));
    let mut services = AccountServices::jmap(client, settings);
    let mail = services.mail.clone();
    let offers = |capability: &str| capabilities.iter().any(|c| c == capability);
    if let AnyMail::Jmap(adapter) = &mail {
        if offers(mailrs_jmap::VACATION) && !offers(mailrs_jmap::SIEVE) {
            services = services.with_auto_reply(AnyAutoReply::Jmap(adapter.clone()));
        }
        if offers(mailrs_jmap::CONTACTS) {
            services = services.with_contacts(AnyContacts::Jmap(adapter.clone()));
        }
        if offers(mailrs_jmap::CALENDARS) {
            services = services.with_calendar(AnyCalendar::Jmap(adapter.clone()));
        }
    }
    if offers(mailrs_jmap::SIEVE) {
        let rules = SieveRules::new(
            Arc::new(JmapSieve::new(jmap)),
            mail,
            server.address.clone(),
            format!("{:?}", server.kind),
        )
        .beside_server_reply(
            offers(mailrs_jmap::VACATION).then_some(mailrs_sync::SERVER_REPLY_SCRIPT),
        );
        services
            .with_rules(AnyRules::JmapSieve(rules.clone()))
            .with_auto_reply(AnyAutoReply::JmapSieve(rules))
    } else {
        services.with_rules(AnyRules::Local(LocalRules::new(db.clone(), account_id)))
    }
}

fn incoming(message_id: &str, subject: &str, to: &str) -> Vec<u8> {
    format!(
        "From: Ada Lovelace <ada@example.test>\r\nTo: {to}\r\nSubject: {subject}\r\nMessage-ID: <{message_id}>\r\nDate: {}\r\n\r\n{subject} body\r\n",
        chrono::Utc::now().to_rfc2822()
    )
    .into_bytes()
}

impl<'a> Run<'a> {
    async fn new(server: &'a JmapServer, password: &'a str) -> Run<'a> {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let db = Db::open(&dir.path().join("mail.db")).expect("the store opens");
        let address = server.address.clone();
        let account_id = db
            .write(move |c| {
                Ok(accounts::insert_jmap_account(c, &address, "JMAP", 0)?.expect("a fresh store"))
            })
            .await
            .expect("the account row");
        let (events, receiver) = async_channel::unbounded();
        let sync = AccountSync::new(
            account_id,
            services(server, password, &db, account_id).await,
            db.clone(),
            events,
        )
        .with_limits(WINDOW_DAYS, DEFAULT_BODY_CACHE_BYTES);
        Run {
            server,
            password,
            db,
            account_id,
            sync,
            _events: receiver,
            _dir: dir,
        }
    }

    fn kind(&self) -> JmapKind {
        self.server.kind
    }

    fn id(&self, name: &str) -> String {
        format!("{name}.{:?}@example.test", self.kind())
    }

    fn mail(&self) -> &mailrs_sync::AnyMail {
        &self.sync.services().mail
    }

    /// The store's id and thread for the message with `message_id`.
    async fn stored(&self, message_id: &str) -> Option<(String, String)> {
        let (account, wanted) = (self.account_id, message_id.to_string());
        self.db
            .read(move |c| {
                Ok(c.query_row(
                    "SELECT id, thread_id FROM messages WHERE account_id = ?1 AND rfc822_msgid IN (?2, '<' || ?2 || '>')",
                    rusqlite::params![account, wanted],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?)
            })
            .await
            .expect("the store answers")
    }

    /// The server's word on where `email` sits and what it carries, or
    /// `None` when the server no longer has it.
    async fn on_server(
        &self,
        email: &str,
    ) -> Option<(BTreeMap<String, bool>, BTreeMap<String, bool>)> {
        let account = self.server.mail_account(self.password).await;
        let got = self
            .server
            .call(
                self.password,
                &MAIL,
                json!([["Email/get", {"accountId": account, "ids": [email], "properties": ["mailboxIds", "keywords"]}, "0"]]),
            )
            .await;
        let found = got["methodResponses"][0][1]["list"].get(0)?;
        let map = |v: &serde_json::Value| serde_json::from_value(v.clone()).unwrap_or_default();
        Some((map(&found["mailboxIds"]), map(&found["keywords"])))
    }

    /// The id of the server's mailbox with `role`, if it has one.
    async fn role(&self, role: &str) -> Option<String> {
        let account = self.server.mail_account(self.password).await;
        let got = self
            .server
            .call(self.password, &MAIL, json!([["Mailbox/get", {"accountId": account, "ids": null, "properties": ["role"]}, "0"]]))
            .await;
        got["methodResponses"][0][1]["list"]
            .as_array()
            .and_then(|list| list.iter().find(|m| m["role"] == role))
            .and_then(|m| m["id"].as_str())
            .map(str::to_string)
    }

    async fn sent_mailbox(&self) -> String {
        self.role("sent")
            .await
            .unwrap_or_else(|| panic!("{:?} has no Sent mailbox", self.kind()))
    }

    async fn all(self) {
        let started = std::time::Instant::now();
        self.mail_arrives_and_lands_in_the_store().await;
        self.a_flag_reaches_the_server().await;
        self.archive_keeps_the_message_in_a_mailbox().await;
        self.search_finds_by_subject().await;
        self.a_body_opens().await;
        self.a_send_lands_in_sent().await;
        self.a_draft_saves_replaces_and_sends().await;
        self.identities_name_the_account().await;
        self.the_reply_round_trips_where_offered().await;
        self.a_contact_round_trips_where_offered().await;
        self.a_contact_with_every_field_round_trips_where_offered().await;
        self.a_rule_lands_in_sieve_where_offered().await;
        self.the_reply_and_a_rule_run_together_where_sieve_is_offered()
            .await;
        self.masked_email_is_hidden_where_not_offered().await;
        self.the_calendar_round_trips().await;
        self.send_later_waits_on_the_server().await;
        self.storage_answers().await;
        self.a_receipt_goes_out().await;
        eprintln!("{:?}: the steps took {:?}", self.kind(), started.elapsed());
        self.a_delivery_during_a_watch_reaches_the_store().await;
        self.a_write_during_a_watch_completes().await;
    }

    /// Cyrus and Stalwart hold and call back; James lists a hold it cannot
    /// call back, so the app keeps the message in its own queue.
    async fn send_later_waits_on_the_server(&self) {
        let raw = format!(
            "From: {0}\r\nTo: {0}\r\nSubject: Later\r\nMessage-ID: <{1}>\r\n\r\nLater\r\n",
            self.server.address,
            self.id("later-held")
        );
        let mail = self.mail();
        let at = mailrs_sync::now_millis() + 3_600_000;
        match mail.schedule(raw.as_bytes(), at).await {
            Ok(held) => {
                let (_, waiting) = self
                    .on_server(&held.message_id)
                    .await
                    .expect("the held copy");
                assert_eq!(
                    waiting.get(mailrs_jmap::HELD),
                    Some(&true),
                    "{:?}: the copy is marked held",
                    self.kind()
                );
                let settled = mail
                    .settle(&held.handle, &held.message_id, at)
                    .await
                    .expect("a settle");
                assert!(
                    matches!(settled, Settled::Waiting(_)),
                    "{:?}: {settled:?}",
                    self.kind()
                );
                let unheld = mail
                    .unschedule(&held.handle, &held.message_id, at)
                    .await
                    .expect("a cancel");
                assert_eq!(unheld, Unheld::InDrafts, "{:?}", self.kind());
                let (_, keywords) = self
                    .on_server(&held.message_id)
                    .await
                    .expect("the copy stays as a draft");
                assert!(
                    !keywords.contains_key(mailrs_jmap::HELD),
                    "{:?}: a cancelled hold is a plain draft",
                    self.kind()
                );
                mail.delete_draft(&held.message_id)
                    .await
                    .expect("the draft goes");
                eprintln!(
                    "{:?}: the server held the message and gave it back",
                    self.kind()
                );
            }
            Err(BackendError::Unsupported) => {
                eprintln!(
                    "{:?}: no hold the server can call back; the app keeps the message",
                    self.kind()
                );
            }
            Err(err) => panic!("{:?}: {err}", self.kind()),
        }
    }

    /// James reports a quota once one is set; Cyrus and Stalwart test
    /// users have none, which reads as nothing to show.
    async fn storage_answers(&self) {
        if self.kind() == JmapKind::James {
            let root = format!("#private&{}", self.server.address);
            let status = std::process::Command::new("docker")
                .args([
                    "exec",
                    self.server.id(),
                    "james-cli",
                    "SetMaxStorageQuota",
                    &root,
                    "1G",
                ])
                .status()
                .expect("docker runs");
            assert!(status.success(), "james-cli sets the quota");
        }
        let storage = self.sync.storage().await.expect("a storage answer");
        eprintln!("{:?}: storage {storage:?}", self.kind());
        match self.kind() {
            JmapKind::James => assert_eq!(storage.map(|s| s.limit), Some(1 << 30)),
            _ => assert!(storage.is_none_or(|s| s.limit > 0), "{:?}", self.kind()),
        }
    }

    /// MDN/send where the session offers it; a report sent as mail where
    /// not. Either way `$mdnsent` lands on the server's copy.
    async fn a_receipt_goes_out(&self) {
        let raw = format!(
            "From: {0}\r\nTo: {0}\r\nSubject: Asks\r\nMessage-ID: <{1}>\r\nDisposition-Notification-To: {0}\r\nDate: {2}\r\n\r\nAsks\r\n",
            self.server.address,
            self.id("asks"),
            chrono::Utc::now().to_rfc2822()
        );
        self.server.deliver(self.password, raw.as_bytes()).await;
        self.sync.incremental().await.expect("a look");
        let (id, _) = self.stored(&self.id("asks")).await.expect("stored");
        self.sync.body(&id).await.expect("the body");
        assert!(!self.sync.receipt_answered(&id).await.expect("the record"));
        self.sync
            .answer_receipt(&id, true)
            .await
            .unwrap_or_else(|err| panic!("{:?}: {err}", self.kind()));
        assert!(self.sync.receipt_answered(&id).await.expect("the record"));
        let (_, keywords) = self.on_server(&id).await.expect("still there");
        assert_eq!(keywords.get("$mdnsent"), Some(&true), "{:?}", self.kind());
    }

    async fn mail_arrives_and_lands_in_the_store(&self) {
        self.server
            .deliver(
                self.password,
                &incoming(&self.id("welcome"), "Welcome", &self.server.address),
            )
            .await;
        self.sync.bootstrap().await.expect("the first sync");
        assert!(
            self.stored(&self.id("welcome")).await.is_some(),
            "{:?}: the welcome message",
            self.kind()
        );
        let later = self
            .server
            .deliver(
                self.password,
                &incoming(&self.id("later"), "Later", &self.server.address),
            )
            .await;
        self.sync.incremental().await.expect("a look");
        assert_eq!(
            self.stored(&self.id("later")).await.map(|(id, _)| id),
            Some(later),
            "{:?}",
            self.kind()
        );
    }

    async fn a_flag_reaches_the_server(&self) {
        let (id, thread) = self.stored(&self.id("welcome")).await.expect("stored");
        self.sync
            .triage_all(
                &[Target::thread(self.account_id, thread)],
                &TriageAction::Star,
                None,
            )
            .await
            .expect("a star");
        let (_, keywords) = self.on_server(&id).await.expect("still there");
        assert_eq!(keywords.get("$flagged"), Some(&true), "{:?}", self.kind());
    }

    /// Archive on a label account takes the message out of the Inbox and
    /// into Archive. A server without an Archive mailbox, as a new Stalwart
    /// account is, gets one made by the adapter.
    async fn archive_keeps_the_message_in_a_mailbox(&self) {
        let (id, thread) = self.stored(&self.id("later")).await.expect("stored");
        let had = self.role("archive").await.is_some();
        self.sync
            .triage_all(
                &[Target::thread(self.account_id, thread)],
                &TriageAction::Archive,
                None,
            )
            .await
            .expect("archive");
        let archive = match self.role("archive").await {
            Some(archive) => archive,
            None => self
                .top_level("Archive")
                .await
                .unwrap_or_else(|| panic!("{:?}: no Archive was made", self.kind())),
        };
        if !had {
            eprintln!("{:?}: the adapter made the Archive mailbox", self.kind());
        }
        let (mailboxes, _) = self.on_server(&id).await.expect("still there");
        let mailboxes: Vec<String> = mailboxes.into_keys().collect();
        assert_eq!(
            mailboxes,
            [archive],
            "{:?}: out of the Inbox, into Archive",
            self.kind()
        );
    }

    /// The id of the top-level mailbox called `name`, if there is one.
    async fn top_level(&self, name: &str) -> Option<String> {
        let account = self.server.mail_account(self.password).await;
        let got = self
            .server
            .call(self.password, &MAIL, json!([["Mailbox/get", {"accountId": account, "ids": null, "properties": ["name", "parentId"]}, "0"]]))
            .await;
        got["methodResponses"][0][1]["list"]
            .as_array()
            .and_then(|list| {
                list.iter()
                    .find(|m| m["name"] == name && m["parentId"].is_null())
            })
            .and_then(|m| m["id"].as_str())
            .map(str::to_string)
    }

    async fn search_finds_by_subject(&self) {
        let query = Query::term(Term::Subject("Welcome".into()));
        let found = self
            .mail()
            .search(&SearchQuery::Tree(query), 10)
            .await
            .expect("a search");
        let (id, _) = self.stored(&self.id("welcome")).await.expect("stored");
        assert!(
            found.iter().any(|r| r.id == id),
            "{:?}: {found:?}",
            self.kind()
        );
    }

    async fn a_body_opens(&self) {
        let (id, _) = self.stored(&self.id("welcome")).await.expect("stored");
        let raw = self.mail().fetch_raw(&[id]).await.expect("the raw message");
        assert!(
            String::from_utf8_lossy(&raw[0].bytes).contains("Welcome body"),
            "{:?}",
            self.kind()
        );
    }

    async fn a_send_lands_in_sent(&self) {
        let raw = format!(
            "From: {0}\r\nTo: {0}\r\nSubject: Sent here\r\nMessage-ID: <{1}>\r\n\r\nOut\r\n",
            self.server.address,
            self.id("sent")
        );
        let id = self
            .mail()
            .send(raw.as_bytes(), None)
            .await
            .expect("a send");
        let (mailboxes, keywords) = self.on_server(&id).await.expect("the sent copy");
        let sent = self.sent_mailbox().await;
        assert_eq!(
            mailboxes.into_keys().collect::<Vec<_>>(),
            std::slice::from_ref(&sent),
            "{:?}: the copy sits in Sent",
            self.kind()
        );
        assert!(
            !keywords.contains_key("$draft"),
            "{:?}: {keywords:?}",
            self.kind()
        );
        // A mailbox's share of the window, as the relist after a lost state
        // asks for it.
        let listed = self
            .mail()
            .window_ids(WINDOW_DAYS, Some(&sent))
            .await
            .expect("Sent's window");
        assert!(
            listed.iter().any(|r| r.id == id),
            "{:?}: {listed:?}",
            self.kind()
        );
    }

    /// A draft edited, then saved again unchanged, keeps one copy on the
    /// server, which then sends it and files it in Sent. Cyrus keeps one
    /// copy of the same bytes and answers the unchanged save with the copy
    /// it has; the others make a new one.
    async fn a_draft_saves_replaces_and_sends(&self) {
        let letter = |body: &str| {
            format!(
                "From: {0}\r\nTo: {0}\r\nSubject: Draft\r\n\r\n{body}\r\n",
                self.server.address
            )
            .into_bytes()
        };
        let mail = self.mail();
        let first = mail
            .save_draft(None, &letter("One"), None)
            .await
            .expect("a draft");
        let edited = mail
            .save_draft(Some(&first.draft_id), &letter("Two"), None)
            .await
            .expect("an edit");
        assert!(
            self.on_server(&first.draft_id).await.is_none(),
            "{:?}: the old copy is gone",
            self.kind()
        );
        let again = mail
            .save_draft(Some(&edited.draft_id), &letter("Two"), None)
            .await
            .expect("an unchanged save");
        let listed: Vec<String> = mail
            .list_drafts()
            .await
            .expect("the drafts")
            .into_iter()
            .map(|d| d.draft_id)
            .collect();
        assert_eq!(
            listed,
            std::slice::from_ref(&again.draft_id),
            "{:?}: one draft",
            self.kind()
        );
        if again.draft_id == edited.draft_id {
            eprintln!("{:?}: an unchanged save keeps the draft's id", self.kind());
        }

        let sent = mail
            .send_draft(&again.draft_id)
            .await
            .expect("the draft goes out");
        let (mailboxes, keywords) = self.on_server(&sent).await.expect("the sent draft");
        assert_eq!(
            mailboxes.into_keys().collect::<Vec<_>>(),
            [self.sent_mailbox().await],
            "{:?}",
            self.kind()
        );
        assert!(
            !keywords.contains_key("$draft"),
            "{:?}: {keywords:?}",
            self.kind()
        );
        assert!(
            mail.list_drafts().await.expect("the drafts").is_empty(),
            "{:?}",
            self.kind()
        );

        let third = mail
            .save_draft(None, &letter("Three"), None)
            .await
            .expect("another draft");
        mail.delete_draft(&third.draft_id).await.expect("a delete");
        assert!(
            mail.list_drafts().await.expect("the drafts").is_empty(),
            "{:?}",
            self.kind()
        );
    }

    /// A server with working EventSource push wakes the watch when mail
    /// arrives; one without it, or whose stream fails, leaves the poll to
    /// find the mail. Either way the next look stores it. The log says
    /// which the server gave.
    ///
    /// The watch runs on an adapter of its own, dropped once the wait is
    /// over: Cyrus 3.13 answers no other request, on any connection, while
    /// an EventSource stream is open, so there the delivery waits until the
    /// adapter's probe finds the server holding calls and drops the stream.
    async fn a_delivery_during_a_watch_reaches_the_store(&self) {
        let watcher = services(self.server, self.password, &self.db, self.account_id)
            .await
            .mail;
        let mut watching = tokio::spawn({
            let watcher = watcher.clone();
            async move { watcher.watch().await }
        });
        // Let the stream open before the delivery, so the delivery is what
        // it reports.
        tokio::time::sleep(Duration::from_secs(2)).await;
        let early = watching.is_finished();
        let started = std::time::Instant::now();
        let raw = incoming(&self.id("pushed"), "Pushed", &self.server.address);
        let delivery = self.server.deliver(self.password, &raw);
        let waited = async {
            let woke = tokio::time::timeout(PUSH_WAIT, &mut watching).await.is_ok();
            watching.abort();
            let pushed = watcher.poll_interval() != Some(NO_PUSH_POLL);
            drop(watcher);
            (woke, pushed)
        };
        let (_, (woke, pushed)) = tokio::join!(delivery, waited);
        let said = match (early, woke, pushed) {
            (true, _, _) => "the watch ended before the delivery",
            (false, true, true) => "push woke the watch on the delivery",
            (false, true, false) => "the push stream failed, so the minute poll looks",
            (false, false, _) => "the watch slept through the delivery, so the minute poll looks",
        };
        eprintln!(
            "{:?}: {said}; the delivery took {:?}",
            self.kind(),
            started.elapsed()
        );
        self.sync.incremental().await.expect("a look");
        assert!(
            self.stored(&self.id("pushed")).await.is_some(),
            "{:?}: the pushed message",
            self.kind()
        );
    }

    /// A write on the adapter whose watch holds the stream open goes
    /// through in seconds. On Cyrus, which answers nothing else while a
    /// stream is open, the adapter drops the stream and polls instead.
    async fn a_write_during_a_watch_completes(&self) {
        let mail = self.mail().clone();
        let watching = tokio::spawn(async move { mail.watch().await });
        tokio::time::sleep(Duration::from_secs(2)).await;
        let (_, thread) = self.stored(&self.id("welcome")).await.expect("stored");
        let started = std::time::Instant::now();
        let written = tokio::time::timeout(
            WRITE_WAIT,
            self.sync.triage_all(
                &[Target::thread(self.account_id, thread)],
                &TriageAction::Unstar,
                None,
            ),
        )
        .await;
        watching.abort();
        eprintln!(
            "{:?}: a write during a watch took {:?}",
            self.kind(),
            started.elapsed()
        );
        written
            .unwrap_or_else(|_| panic!("{:?}: the write waited past {WRITE_WAIT:?}", self.kind()))
            .expect("an unstar");
    }

    /// Cyrus answers `user1`'s identity with an empty email; the adapter
    /// fills it with the address the user name stands for.
    async fn identities_name_the_account(&self) {
        let listed = self
            .sync
            .services()
            .identities
            .identities()
            .await
            .expect("identities");
        assert!(
            listed.iter().any(|a| a.email == self.server.address),
            "{:?}: {listed:?}",
            self.kind()
        );
    }

    async fn the_reply_round_trips_where_offered(&self) {
        let Some(reply) = self.sync.services().auto_reply.clone() else {
            eprintln!("{:?} offers no automatic reply", self.kind());
            return;
        };
        let wanted = mailrs_domain::Vacation {
            enabled: true,
            subject: "Away".into(),
            body: "Back soon".into(),
            contacts_only: false,
            domain_only: false,
            start: None,
            end: None,
        };
        reply.set_vacation(&wanted).await.expect("a reply");
        let back = reply.vacation().await.expect("the reply back");
        assert!(
            back.enabled && back.body.contains("Back soon"),
            "{:?}",
            self.kind()
        );
    }

    async fn a_contact_round_trips_where_offered(&self) {
        let Some(contacts) = self.sync.services().contacts.clone() else {
            eprintln!("{:?} offers no contacts over JMAP", self.kind());
            return;
        };
        let fields = mailrs_gmail::ContactFields {
            name: Some("Ada Lovelace".into()),
            emails: Some(vec!["ada@example.test".into()]),
            phones: None,
            organization: None,
            ..Default::default()
        };
        let made = contacts.create_contact(&fields).await.expect("a contact");
        let page = contacts
            .connections(None, None)
            .await
            .expect("the contacts");
        assert!(
            page.people.iter().any(|p| p.resource == made.resource),
            "{:?}",
            self.kind()
        );
        // An edit patches entries by key, which each server must take.
        let more = mailrs_gmail::ContactFields {
            emails: Some(vec![
                "ada@example.test".into(),
                "ada@engines.example".into(),
            ]),
            organization: Some("Analytical Engines".into()),
            ..Default::default()
        };
        let edited = contacts
            .update_contact(&made.resource, &more)
            .await
            .unwrap_or_else(|err| panic!("{:?}: {err}", self.kind()));
        assert_eq!(edited.emails.len(), 2, "{:?}: {edited:?}", self.kind());
        assert_eq!(
            edited.name.as_deref(),
            Some("Ada Lovelace"),
            "{:?}",
            self.kind()
        );
        let renamed = mailrs_gmail::ContactFields {
            organization: Some("Difference Engines".into()),
            ..Default::default()
        };
        let edited = contacts
            .update_contact(&made.resource, &renamed)
            .await
            .unwrap_or_else(|err| panic!("{:?}: {err}", self.kind()));
        assert_eq!(
            edited.organization.as_deref(),
            Some("Difference Engines"),
            "{:?}",
            self.kind()
        );
        assert_eq!(edited.emails.len(), 2, "{:?}: {edited:?}", self.kind());
    }

    /// Create, read, edit and delete a card carrying every field the editor
    /// has, and print what the server answered.
    async fn a_contact_with_every_field_round_trips_where_offered(&self) {
        use mailrs_domain::contact::{Changes, Field};
        let Some(contacts) = self.sync.services().contacts.clone() else {
            return;
        };
        let kind = self.kind();
        let fields = mailrs_gmail::ContactFields {
            name: Some("Priya Raman".into()),
            emails: Some(vec!["priya@fernwood.example".into()]),
            phones: Some(vec!["+351 910 000 000".into()]),
            organization: Some("Fernwood".into()),
            photo: Some(vec![0x89, b'P', b'N', b'G', 13, 10, 26, 10]),
            details: Changes {
                birthday: Some("1990-04-12".into()),
                notes: Some("Met in March".into()),
                addresses: Some(vec![[
                    String::new(),
                    String::new(),
                    "1 Main Street".into(),
                    "Lisbon".into(),
                    "LX".into(),
                    "1000-001".into(),
                    "Portugal".into(),
                ]]),
                urls: Some(vec!["https://example.org".into()]),
                groups: Some(vec!["Friends".into()]),
                dates: Some(vec![Field {
                    label: "Anniversary".into(),
                    value: "2015-06-20".into(),
                }]),
                custom: Some(vec![Field {
                    label: "Badge".into(),
                    value: "42".into(),
                }]),
            },
            ..Default::default()
        };
        // Which field a server refuses, one at a time, for the log.
        let one = |f: &dyn Fn(&mut mailrs_gmail::ContactFields)| {
            let mut only = mailrs_gmail::ContactFields {
                name: Some("Probe".into()),
                ..Default::default()
            };
            f(&mut only);
            only
        };
        let probes: Vec<(&str, mailrs_gmail::ContactFields)> = vec![
            ("birthday", one(&|c| c.details.birthday = fields.details.birthday.clone())),
            ("notes", one(&|c| c.details.notes = fields.details.notes.clone())),
            ("addresses", one(&|c| c.details.addresses = fields.details.addresses.clone())),
            ("urls", one(&|c| c.details.urls = fields.details.urls.clone())),
            ("groups", one(&|c| c.details.groups = fields.details.groups.clone())),
            ("dates", one(&|c| c.details.dates = fields.details.dates.clone())),
            ("custom", one(&|c| c.details.custom = fields.details.custom.clone())),
            ("photo", one(&|c| c.photo = fields.photo.clone())),
        ];
        for (what, probe) in probes {
            match contacts.create_contact(&probe).await {
                Ok(made) => {
                    eprintln!("{kind:?} takes {what}");
                    let _ = contacts.delete_contact(&made.resource, None).await;
                }
                Err(err) => eprintln!("{kind:?} REFUSES {what}: {err}"),
            }
        }
        let made = match contacts.create_contact(&fields).await {
            Ok(made) => made,
            Err(err) => {
                eprintln!("{kind:?} refused a card with every field: {err}");
                return;
            }
        };
        eprintln!("{kind:?} created: {:?}", made.details);
        let page = contacts.connections(None, None).await.expect("the cards");
        let read = page
            .people
            .iter()
            .find(|p| p.resource == made.resource)
            .unwrap_or_else(|| panic!("{kind:?} lists the card"));
        eprintln!("{kind:?} read back: {:?}", read.details);
        let d = &read.details;
        assert_eq!(d.birthday, "1990-04-12", "{kind:?}");
        assert_eq!(d.notes, "Met in March", "{kind:?}");
        assert_eq!(d.urls, ["https://example.org"], "{kind:?}");
        assert_eq!(d.groups, ["Friends"], "{kind:?}");
        assert_eq!(d.addresses[0][3], "Lisbon", "{kind:?}");
        assert_eq!(d.dates[0].value, "2015-06-20", "{kind:?}");
        assert_eq!(d.custom[0].value, "42", "{kind:?}");
        let edited = contacts
            .update_contact(
                &made.resource,
                &mailrs_gmail::ContactFields {
                    details: Changes {
                        notes: Some("Changed".into()),
                        groups: Some(vec!["Friends".into(), "Work".into()]),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .await
            .unwrap_or_else(|err| panic!("{kind:?} refused an edit: {err}"));
        assert_eq!(edited.details.notes, "Changed", "{kind:?}");
        assert_eq!(edited.details.groups.len(), 2, "{kind:?}");
        assert_eq!(edited.details.birthday, "1990-04-12", "{kind:?}");
        contacts
            .delete_contact(&made.resource, None)
            .await
            .unwrap_or_else(|err| panic!("{kind:?} refused a delete: {err}"));
        let after = contacts.connections(None, None).await.expect("the cards");
        assert!(
            after.people.iter().all(|p| p.resource != made.resource),
            "{kind:?}"
        );
    }

    async fn a_rule_lands_in_sieve_where_offered(&self) {
        let Some(rules) = self.sync.services().rules.clone() else {
            return;
        };
        if rules.place() != mailrs_sync::RulesPlace::Server {
            eprintln!("{:?} offers no Sieve over JMAP", self.kind());
            return;
        }
        let made = rules
            .create_filter(&mailrs_domain::Filter::block("pest@example.test"))
            .await
            .expect("a rule");
        assert_eq!(
            rules.filters().await.expect("the rules").len(),
            1,
            "{:?}",
            self.kind()
        );
        rules
            .delete_filter(made.id.as_deref().expect("the rule's id"))
            .await
            .expect("a delete");
        assert!(
            rules.filters().await.expect("the rules").is_empty(),
            "{:?}",
            self.kind()
        );
    }

    /// On a Sieve server the reply lives in the rules script, so adding a
    /// rule leaves it on, and mail that arrives while it is on still
    /// meets the rule. Stalwart once kept the reply in a script of its own,
    /// which the rules script switched off.
    async fn the_reply_and_a_rule_run_together_where_sieve_is_offered(&self) {
        let services = self.sync.services();
        let (Some(rules), Some(reply)) = (services.rules.clone(), services.auto_reply.clone())
        else {
            return;
        };
        if rules.place() != mailrs_sync::RulesPlace::Server {
            return;
        }
        let away = mailrs_domain::Vacation {
            enabled: true,
            subject: "Away".into(),
            body: "Back on Monday".into(),
            ..Default::default()
        };
        reply
            .set_vacation(&away)
            .await
            .unwrap_or_else(|err| panic!("{:?}: {err}", self.kind()));
        let pests = self.mail().create_mailbox("Pests").await.expect("a folder");
        let filter = mailrs_domain::Filter {
            criteria: mailrs_domain::FilterCriteria {
                from: Some("pest@example.test".into()),
                ..Default::default()
            },
            action: mailrs_domain::FilterAction {
                add: vec![mailrs_domain::MailSet::Mailbox(pests.id.clone())],
                ..Default::default()
            },
            ..Default::default()
        };
        let made = rules
            .create_filter(&filter)
            .await
            .unwrap_or_else(|err| panic!("{:?}: {err}", self.kind()));
        let back = reply.vacation().await.expect("the reply back");
        assert!(
            back.enabled && back.body.contains("Back on Monday"),
            "{:?}: the reply is still on: {back:?}",
            self.kind()
        );
        let raw = format!(
            "From: Pest <pest@example.test>\r\nTo: {0}\r\nSubject: Buy now\r\nMessage-ID: <{1}>\r\nDate: {2}\r\n\r\nBuy now\r\n",
            self.server.address,
            self.id("pest"),
            chrono::Utc::now().to_rfc2822()
        );
        self.server.deliver_through_rules(&raw).await;
        let account = self.server.mail_account(self.password).await;
        let mut filed = false;
        for _ in 0..40 {
            let found = self
                .server
                .call(
                    self.password,
                    &MAIL,
                    json!([["Email/query", {"accountId": account, "filter": {"inMailbox": pests.id, "subject": "Buy now"}}, "0"]]),
                )
                .await;
            if found["methodResponses"][0][1]["ids"]
                .as_array()
                .is_some_and(|ids| !ids.is_empty())
            {
                filed = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        assert!(
            filed,
            "{:?}: the rule filed the mail while the reply was on",
            self.kind()
        );
        rules
            .delete_filter(made.id.as_deref().expect("the rule's id"))
            .await
            .expect("a delete");
        reply
            .set_vacation(&mailrs_domain::Vacation::default())
            .await
            .expect("the reply off");
        assert!(
            !reply.vacation().await.expect("the reply back").enabled,
            "{:?}",
            self.kind()
        );
        eprintln!("{:?}: the reply and a rule run in one script", self.kind());
    }

    /// Only Fastmail offers Masked Email; the three test servers do not,
    /// so the adapter must say so and the app hides the feature.
    async fn masked_email_is_hidden_where_not_offered(&self) {
        let AnyMail::Jmap(adapter) = self.mail() else {
            panic!("a JMAP account")
        };
        assert!(
            !adapter.masked_email(),
            "{:?} does not offer Masked Email",
            self.kind()
        );
    }

    /// Cyrus and Stalwart offer JMAP Calendars; James does not. A server
    /// whose shape the gate refuses is reported, so the gate's reading of
    /// a real session is checked as well as its tests.
    /// Whether the server's session offers calendars in the shape the gate
    /// accepts, with the account's calendar attached exactly then.
    async fn the_calendar_is_offered_as_the_gate_reads_it(&self) -> bool {
        let session = self.server.session(self.password).await;
        let offers = mailrs_jmap::Session::parse(&serde_json::to_vec(&session).expect("json"))
            .map(|s| s.offers(mailrs_jmap::CALENDARS))
            .unwrap_or(false);
        let listed = session["capabilities"]
            .get(mailrs_jmap::CALENDARS)
            .is_some();
        eprintln!(
            "{:?}: calendars listed {listed}, offered in draft 32's shape {offers}",
            self.kind()
        );
        let attached = self.sync.services().calendar.is_some();
        assert_eq!(
            attached,
            offers,
            "{:?}: the calendar follows the gate",
            self.kind()
        );
        if !offers {
            assert!(
                !listed || self.kind() == JmapKind::James,
                "{:?} lists calendars in a shape the gate refuses",
                self.kind()
            );
        }
        offers
    }

    async fn the_calendar_round_trips(&self) {
        if !self.the_calendar_is_offered_as_the_gate_reads_it().await {
            return;
        }
        let calendar = self.sync.services().calendar.clone().expect("a calendar");
        let calendars = calendar.calendars().await.expect("the calendars");
        let Some(home) = calendars.iter().find(|c| c.access.can_write()) else {
            eprintln!(
                "{:?}: no calendar to write to for this user; skipping the round trip",
                self.kind()
            );
            return;
        };
        let start = mailrs_sync::now_millis() + 86_400_000;
        let new = mailrs_domain::calendar::Event {
            calendar: home.id.clone(),
            uid: self.id("event"),
            title: "Seed fair".into(),
            start,
            end: start + 3_600_000,
            zone: "Europe/Lisbon".into(),
            busy: true,
            ..Default::default()
        };
        let made = calendar
            .put_event(&new, None, true, mailrs_domain::calendar::Notify::Nobody)
            .await
            .unwrap_or_else(|err| panic!("{:?}: {err}", self.kind()));
        // A create tried again, as after a lost answer, finds the first.
        let again = calendar
            .put_event(&new, None, true, mailrs_domain::calendar::Notify::Nobody)
            .await
            .unwrap_or_else(|err| panic!("{:?}: {err}", self.kind()));
        assert_eq!(
            again.id,
            made.id,
            "{:?}: a create tried again makes no second event",
            self.kind()
        );
        let first = calendar
            .event_changes(&home.id, None, None, start - 86_400_000)
            .await
            .expect("a read");
        assert!(
            first
                .events
                .iter()
                .any(|e| e.id == made.id && e.title == "Seed fair"),
            "{:?}",
            self.kind()
        );
        assert_eq!(
            first.events.iter().filter(|e| e.uid == new.uid).count(),
            1,
            "{:?}",
            self.kind()
        );
        let mut changed = made.clone();
        changed.title = "Seed fair, moved".into();
        calendar
            .put_event(
                &changed,
                Some(&made.etag),
                false,
                mailrs_domain::calendar::Notify::Nobody,
            )
            .await
            .expect("a change");
        let next = calendar
            .event_changes(&home.id, first.next_sync.as_deref(), None, 0)
            .await
            .expect("the changes since");
        assert!(
            next.events
                .iter()
                .any(|e| e.id == made.id && e.title == "Seed fair, moved"),
            "{:?}",
            self.kind()
        );
        calendar
            .remove_event(
                &home.id,
                &made.id,
                None,
                mailrs_domain::calendar::Notify::Nobody,
            )
            .await
            .expect("a delete");
        self.a_moved_series_takes_its_changed_occurrence(&calendar, &home.id)
            .await;
    }

    /// A weekly series with one changed occurrence, moved an hour later
    /// whole: the changed occurrence moves with it, and no occurrence
    /// stays at the old time.
    async fn a_moved_series_takes_its_changed_occurrence(
        &self,
        calendar: &AnyCalendar,
        home: &str,
    ) {
        use mailrs_domain::calendar::{Event, Notify, occurrence_id};
        const HOUR: i64 = 3_600_000;
        const WEEK: i64 = 7 * 86_400_000;
        // Cross Lisbon's fall clock change, away from the repeated hour.
        // The next weekly occurrence is 169 hours later, not 168.
        let start = chrono::DateTime::parse_from_rfc3339("2026-10-18T12:00:00Z")
            .expect("the fixed series date")
            .timestamp_millis();
        let series = Event {
            calendar: home.to_string(),
            uid: self.id("weekly"),
            title: "Watering".into(),
            start,
            end: start + HOUR,
            zone: "Europe/Lisbon".into(),
            busy: true,
            rules: vec!["RRULE:FREQ=WEEKLY;COUNT=4".into()],
            ..Default::default()
        };
        let made = calendar
            .put_event(&series, None, true, Notify::Nobody)
            .await
            .unwrap_or_else(|err| panic!("{:?}: {err}", self.kind()));
        let (original, _) = mailrs_domain::calendar::expand(&made, start, start + 2 * WEEK)
            .into_iter()
            .find(|(at, _)| *at > start)
            .expect("the next weekly occurrence");
        assert_eq!(original - start, WEEK + HOUR);
        let late = Event {
            id: occurrence_id(&made, original),
            series: Some(made.id.clone()),
            original_start: Some(original),
            start: original,
            end: original + HOUR,
            etag: String::new(),
            rules: Vec::new(),
            title: "Watering, late".into(),
            ..made.clone()
        };
        calendar
            .put_event(&late, None, false, Notify::Nobody)
            .await
            .unwrap_or_else(|err| panic!("{:?}: {err}", self.kind()));
        let read = calendar
            .event_changes(home, None, None, start - 86_400_000)
            .await
            .expect("a read");
        let now = read
            .events
            .iter()
            .find(|e| e.id == made.id)
            .expect("the series")
            .clone();
        let moved = Event {
            start: now.start + HOUR,
            end: now.end + HOUR,
            ..now.clone()
        };
        calendar
            .put_event(&moved, Some(&now.etag), false, Notify::Nobody)
            .await
            .unwrap_or_else(|err| panic!("{:?}: {err}", self.kind()));
        let read = calendar
            .event_changes(home, None, None, start - 86_400_000)
            .await
            .expect("a read");
        let changed: Vec<_> = read
            .events
            .iter()
            .filter(|e| e.series.as_deref() == Some(made.id.as_str()))
            .collect();
        assert_eq!(changed.len(), 1, "{:?}: {changed:?}", self.kind());
        assert_eq!(
            (changed[0].title.as_str(), changed[0].original_start),
            ("Watering, late", Some(original + HOUR)),
            "{:?}",
            self.kind()
        );
        assert_eq!(changed[0].start, original + HOUR, "{:?}", self.kind());
        let series = read
            .events
            .iter()
            .find(|e| e.id == made.id)
            .expect("the series");
        let shown = mailrs_domain::calendar::expand(series, original - HOUR, original + 2 * HOUR);
        assert_eq!(
            shown.iter().map(|(at, _)| *at).collect::<Vec<_>>(),
            [original + HOUR],
            "{:?}: no occurrence stays at the old time",
            self.kind()
        );
        calendar
            .remove_event(home, &made.id, None, Notify::Nobody)
            .await
            .expect("a delete");
    }
}
