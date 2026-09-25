//! The IMAP adapter over the real client, against Dovecot and an SMTP sink
//! in Docker, once for each server profile: everything on, CONDSTORE
//! without QRESYNC, and neither. doveadm inside the container plays the
//! other client that changes the mailbox, and it is also the witness for
//! what the server holds after each step.
//!
//! One test, because it points `SSL_CERT_FILE` at a root made for the run,
//! and the environment belongs to the whole process.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use mailrs_discover::{Security, Server, UserName};
use mailrs_domain::query::{Query, Term};
use mailrs_domain::{AccountId, ChangeEvent, Target};
use mailrs_imap::{ImapClient, Login, SmtpClient};
use mailrs_store::{Db, accounts};
use mailrs_sync::{
    AccountServices, AccountSync, DEFAULT_BODY_CACHE_BYTES, EngineConfig, ImapSettings, SyncEngine,
    TriageAction,
};
use mailrs_testmail::{Certs, Dovecot, Mailpit, Profile, Submission};
use rusqlite::OptionalExtension;

const ADDRESS: &str = "me@example.test";
const WINDOW_DAYS: i64 = 30;
/// 1 January 2024, 10:00 UTC, far outside the window.
const LONG_AGO: i64 = 1_704_103_200;
/// A poll interval no step waits out, so only IDLE can bring new mail in
/// time.
const NO_POLL: Duration = Duration::from_secs(600);

#[test]
fn the_imap_adapter_syncs_and_writes_against_dovecot_in_every_profile() {
    if mailrs_testmail::docker().is_none() {
        return;
    }
    let Some(certs) = Certs::make() else {
        return;
    };
    // SAFETY: this is the binary's only test, and no runtime or client has
    // started a thread yet, so nothing else reads the environment.
    unsafe { mailrs_testmail::trust(&certs.root()) };
    run(certs);
}

fn run(certs: Certs) {
    // The runtime the app and the CLI build, with the body spawned onto
    // its workers as the engine spawns an account's sync. `block_on` would
    // poll the body on this thread's stack instead, which is not what the
    // app does; spawned, a worker stack too small for a debug build fails
    // here first.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .thread_stack_size(mailrs_sync::WORKER_STACK)
        .enable_all()
        .build()
        .expect("a runtime");
    let body = runtime.spawn(async move {
        let password = mailrs_testmail::password();
        let Some(sink) = Mailpit::start(&certs, Submission::Tls, ADDRESS, &password).await else {
            return;
        };
        for profile in Profile::ALL {
            eprintln!("Dovecot with the {profile:?} profile");
            let Some(dovecot) = Dovecot::start(&certs, profile, &password).await else {
                return;
            };
            Run::new(profile, &dovecot, &sink, &password)
                .await
                .all()
                .await;
        }
    });
    runtime
        .block_on(body)
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic.into_panic()));
}

/// Where the store says a message sits on the server.
#[derive(Debug, PartialEq, Eq)]
struct Located {
    mailbox: String,
    uidvalidity: u32,
    uid: u32,
}

/// A message as the store keeps it.
#[derive(Debug, Clone)]
struct Stored {
    id: String,
    thread_id: String,
}

struct Run<'a> {
    profile: Profile,
    dovecot: &'a Dovecot,
    sink: &'a Mailpit,
    password: &'a str,
    db: Db,
    account_id: AccountId,
    sync: AccountSync,
    _events: async_channel::Receiver<ChangeEvent>,
    _dir: tempfile::TempDir,
}

fn services(dovecot: &Dovecot, sink: &Mailpit, password: &str) -> AccountServices {
    let at = |port| Server {
        host: "localhost".to_string(),
        port,
        security: Security::Tls,
        user_name: UserName::Address,
    };
    let login = Login::new(ADDRESS, password);
    AccountServices::imap(
        ImapClient::new(at(dovecot.imaps), login.clone()),
        SmtpClient::new(&at(sink.smtp), &login).expect("an SMTP client for the sink"),
        ImapSettings {
            address: ADDRESS.to_string(),
            provider_name: "Dovecot".to_string(),
            // Dovecot files nothing a client sends, so the engine appends
            // each sent message to Sent itself.
            files_sent_mail: false,
            window_days: WINDOW_DAYS,
        },
    )
}

/// A message from Ada to the account, dated now unless `date` says
/// otherwise.
fn incoming(
    message_id: &str,
    subject: &str,
    body: &str,
    extra: &str,
    date: Option<&str>,
) -> Vec<u8> {
    let now = chrono::Utc::now().to_rfc2822();
    let date = date.unwrap_or(&now);
    format!(
        "From: Ada Lovelace <ada@example.test>\r\nTo: {ADDRESS}\r\nSubject: {subject}\r\n\
         Date: {date}\r\nMessage-ID: <{message_id}>\r\n{extra}MIME-Version: 1.0\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\r\n{body}\r\n"
    )
    .into_bytes()
}

/// A message from the account to Bo.
fn outgoing(message_id: &str, subject: &str, body: &str) -> Vec<u8> {
    let date = chrono::Utc::now().to_rfc2822();
    format!(
        "From: Me <{ADDRESS}>\r\nTo: Bo <bo@example.test>\r\nSubject: {subject}\r\n\
         Date: {date}\r\nMessage-ID: <{message_id}>\r\nMIME-Version: 1.0\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\r\n{body}\r\n"
    )
    .into_bytes()
}

/// A reply from the account to Ada, threaded to `in_reply_to` and filed
/// straight into Sent, as a client's own copy of a sent reply would sit.
fn outgoing_reply(message_id: &str, in_reply_to: &str, subject: &str, body: &str) -> Vec<u8> {
    let date = chrono::Utc::now().to_rfc2822();
    format!(
        "From: Me <{ADDRESS}>\r\nTo: Ada Lovelace <ada@example.test>\r\nSubject: {subject}\r\n\
         Date: {date}\r\nMessage-ID: <{message_id}>\r\nIn-Reply-To: <{in_reply_to}>\r\n\
         References: <{in_reply_to}>\r\nMIME-Version: 1.0\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\r\n{body}\r\n"
    )
    .into_bytes()
}

/// The stored message whose Message-ID is `message_id`, with or without
/// the angle brackets the store may keep.
async fn stored_in(db: &Db, account_id: AccountId, message_id: &str) -> Option<Stored> {
    let message_id = message_id.to_string();
    db.read(move |c| {
        Ok(c.query_row(
            "SELECT id, thread_id FROM messages
             WHERE account_id = ?1 AND rfc822_msgid IN (?2, '<' || ?2 || '>')",
            rusqlite::params![account_id, message_id],
            |row| {
                Ok(Stored {
                    id: row.get(0)?,
                    thread_id: row.get(1)?,
                })
            },
        )
        .optional()?)
    })
    .await
    .expect("the store answers")
}

impl<'a> Run<'a> {
    async fn new(
        profile: Profile,
        dovecot: &'a Dovecot,
        sink: &'a Mailpit,
        password: &'a str,
    ) -> Run<'a> {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let db = Db::open(&dir.path().join("mail.db")).expect("the store opens");
        let account_id = db
            .write(|c| {
                let id = accounts::insert_imap_account(c, ADDRESS, "Dovecot", 0)?;
                Ok(id.expect("a fresh store holds no other account at this address"))
            })
            .await
            .expect("the account row");
        let (events, receiver) = async_channel::unbounded();
        let sync = AccountSync::new(
            account_id,
            services(dovecot, sink, password),
            db.clone(),
            events,
        )
        .with_limits(WINDOW_DAYS, DEFAULT_BODY_CACHE_BYTES);
        Run {
            profile,
            dovecot,
            sink,
            password,
            db,
            account_id,
            sync,
            _events: receiver,
            _dir: dir,
        }
    }

    /// A Message-ID for this profile's run. The sink serves every profile,
    /// so the ids it sees must differ between runs.
    fn id(&self, name: &str) -> String {
        format!("{name}.{:?}@example.test", self.profile)
    }

    async fn stored(&self, name: &str) -> Option<Stored> {
        stored_in(&self.db, self.account_id, &self.id(name)).await
    }

    /// One look at every synced mailbox, through a second `AccountSync`
    /// whose adapter has not looked yet. It shares the store and the sync
    /// state with `self.sync`.
    async fn look_everywhere(&self) {
        let (events, _receiver) = async_channel::unbounded();
        AccountSync::new(
            self.account_id,
            services(self.dovecot, self.sink, self.password),
            self.db.clone(),
            events,
        )
        .with_limits(WINDOW_DAYS, DEFAULT_BODY_CACHE_BYTES)
        .incremental()
        .await
        .expect("a look at every mailbox");
    }

    async fn must(&self, name: &str) -> Stored {
        self.stored(name)
            .await
            .unwrap_or_else(|| panic!("the store lacks {}", self.id(name)))
    }

    async fn on_server(&self, mailbox: &str, name: &str) -> Option<mailrs_testmail::Held> {
        self.dovecot.find(ADDRESS, mailbox, &self.id(name)).await
    }

    async fn located(&self, store_id: &str) -> Located {
        let (account_id, store_id) = (self.account_id, store_id.to_string());
        self.db
            .read(move |c| {
                Ok(c.query_row(
                    "SELECT mailbox, uidvalidity, uid FROM remote_refs
                     WHERE account_id = ?1 AND message_id = ?2",
                    rusqlite::params![account_id, store_id],
                    |row| {
                        Ok(Located {
                            mailbox: row.get(0)?,
                            uidvalidity: row.get(1)?,
                            uid: row.get(2)?,
                        })
                    },
                )?)
            })
            .await
            .expect("a remote ref for the message")
    }

    async fn where_server_has(&self, mailbox: &str, name: &str) -> Located {
        let held = self
            .on_server(mailbox, name)
            .await
            .unwrap_or_else(|| panic!("the server has no {} in {mailbox}", self.id(name)));
        Located {
            mailbox: mailbox.to_string(),
            uidvalidity: self.dovecot.uidvalidity(ADDRESS, mailbox).await,
            uid: held.uid,
        }
    }

    async fn keywords(&self, store_id: &str) -> Vec<String> {
        let (account_id, store_id) = (self.account_id, store_id.to_string());
        self.db
            .read(move |c| {
                let mut statement = c.prepare(
                    "SELECT keyword FROM message_keywords WHERE account_id = ?1 AND message_id = ?2",
                )?;
                let keywords = statement
                    .query_map(rusqlite::params![account_id, store_id], |row| row.get(0))?
                    .collect::<Result<Vec<String>, _>>()?;
                Ok(keywords)
            })
            .await
            .expect("the store answers")
    }

    async fn has_body(&self, store_id: &str) -> bool {
        let (account_id, store_id) = (self.account_id, store_id.to_string());
        self.db
            .read(move |c| {
                Ok(c.query_row(
                    "SELECT count(*) FROM bodies WHERE account_id = ?1 AND message_id = ?2",
                    rusqlite::params![account_id, store_id],
                    |row| row.get::<_, i64>(0),
                )? > 0)
            })
            .await
            .expect("the store answers")
    }

    async fn copies(&self, name: &str) -> i64 {
        let (account_id, message_id) = (self.account_id, self.id(name));
        self.db
            .read(move |c| {
                Ok(c.query_row(
                    "SELECT count(*) FROM messages
                     WHERE account_id = ?1 AND rfc822_msgid IN (?2, '<' || ?2 || '>')",
                    rusqlite::params![account_id, message_id],
                    |row| row.get(0),
                )?)
            })
            .await
            .expect("the store answers")
    }

    /// Role to mailbox id, for the mailboxes the server listed.
    async fn roles(&self) -> BTreeMap<String, String> {
        let account_id = self.account_id;
        self.db
            .read(move |c| {
                let mut statement = c.prepare(
                    "SELECT role, id FROM mailboxes
                     WHERE account_id = ?1 AND named = 1 AND role IS NOT NULL",
                )?;
                let roles = statement
                    .query_map([account_id], |row| Ok((row.get(0)?, row.get(1)?)))?
                    .collect::<Result<BTreeMap<String, String>, _>>()?;
                Ok(roles)
            })
            .await
            .expect("the store answers")
    }

    async fn mailbox_names(&self) -> Vec<String> {
        let account_id = self.account_id;
        self.db
            .read(move |c| {
                let mut statement =
                    c.prepare("SELECT name FROM mailboxes WHERE account_id = ?1 AND named = 1")?;
                let names = statement
                    .query_map([account_id], |row| row.get(0))?
                    .collect::<Result<Vec<String>, _>>()?;
                Ok(names)
            })
            .await
            .expect("the store answers")
    }

    async fn all(self) {
        self.seed().await;
        self.load().await;
        self.mailboxes_have_their_roles().await;
        self.the_window_is_in_the_store().await;
        self.a_body_opens().await;
        self.another_clients_changes_arrive().await;
        self.flags_reach_the_server().await;
        self.a_move_keeps_the_message_its_id().await;
        self.archive_files_the_thread().await;
        self.archiving_a_thread_leaves_its_sent_copy_in_sent().await;
        self.delete_forever_erases().await;
        self.drafts_save_replace_and_send().await;
        self.a_send_reaches_the_sink_and_sent().await;
        self.search_finds_mail_past_the_window().await;
        self.a_new_uidvalidity_keeps_ids_and_bodies().await;
        self.idle_brings_new_mail().await;
    }

    async fn seed(&self) {
        let d = self.dovecot;
        // Sent exists from the start so that old mail can sit in it. Drafts
        // does not: the drafts step watches its first APPEND.
        for name in ["Receipts", "Café", "Sent"] {
            d.create_mailbox(ADDRESS, name).await;
        }
        let welcome = self.id("welcome");
        d.save(
            ADDRESS,
            "INBOX",
            &incoming(&welcome, "Welcome", "Welcome body", "", None),
            None,
        )
        .await;
        let threading = format!("In-Reply-To: <{welcome}>\r\nReferences: <{welcome}>\r\n");
        d.save(
            ADDRESS,
            "INBOX",
            &incoming(
                &self.id("reply"),
                "Re: Welcome",
                "Reply body",
                &threading,
                None,
            ),
            None,
        )
        .await;
        for name in ["invoice", "erase", "vanish"] {
            let body = format!("The {name} body");
            d.save(
                ADDRESS,
                "INBOX",
                &incoming(&self.id(name), name, &body, "", None),
                None,
            )
            .await;
        }
        // Sent syncs only its window, so this stays on the server.
        d.save(
            ADDRESS,
            "Sent",
            &incoming(
                &self.id("old"),
                "Old plans",
                "Old body",
                "",
                Some("Mon, 01 Jan 2024 10:00:00 +0000"),
            ),
            Some(LONG_AGO),
        )
        .await;
        // A thread with one message in the Inbox and its reply filed
        // straight into Sent, so archiving the thread can be checked
        // against the rule that a move never takes a message out of Sent,
        // Drafts, Trash or Junk where it already sits (`drop_protected` in
        // sync/src/ops.rs).
        let note = self.id("note");
        d.save(
            ADDRESS,
            "INBOX",
            &incoming(&note, "Note", "Note body", "", None),
            None,
        )
        .await;
        d.save(
            ADDRESS,
            "Sent",
            &outgoing_reply(&self.id("note-reply"), &note, "Re: Note", "Note reply body"),
            None,
        )
        .await;
    }

    async fn load(&self) {
        self.sync.bootstrap().await.expect("bootstrap");
        while self.sync.backfill_step().await.expect("a backfill page") {}
    }

    async fn mailboxes_have_their_roles(&self) {
        let roles = self.roles().await;
        for (role, id) in [
            ("inbox", "INBOX"),
            ("sent", "Sent"),
            ("drafts", "Drafts"),
            ("trash", "Trash"),
            ("junk", "Junk"),
        ] {
            assert_eq!(
                roles.get(role).map(String::as_str),
                Some(id),
                "the {role} role in {roles:?}"
            );
        }
        assert_eq!(
            roles.get("archive").map(String::as_str),
            self.profile.has_archive().then_some("Archive"),
            "the archive role in {roles:?}"
        );
        let names = self.mailbox_names().await;
        assert!(
            names.iter().any(|name| name == "Café"),
            "the server's Caf&AOk- reads as Café: {names:?}"
        );
    }

    async fn the_window_is_in_the_store(&self) {
        let welcome = self.must("welcome").await;
        let reply = self.must("reply").await;
        assert_eq!(
            welcome.thread_id, reply.thread_id,
            "local threading puts a reply with its parent"
        );
        for name in ["invoice", "erase", "vanish"] {
            self.must(name).await;
        }
        assert!(
            self.stored("old").await.is_none(),
            "mail older than the window stays on the server"
        );
        let invoice = self.must("invoice").await;
        assert_eq!(
            self.located(&invoice.id).await,
            self.where_server_has("INBOX", "invoice").await
        );
    }

    async fn a_body_opens(&self) {
        let welcome = self.must("welcome").await;
        let body = self.sync.body(&welcome.id).await.expect("the body");
        assert!(
            body.text
                .as_deref()
                .unwrap_or_default()
                .contains("Welcome body"),
            "{body:?}"
        );
    }

    async fn another_clients_changes_arrive(&self) {
        let d = self.dovecot;
        let invoice = self
            .on_server("INBOX", "invoice")
            .await
            .expect("the invoice");
        d.add_flags(ADDRESS, "INBOX", invoice.uid, "\\Flagged")
            .await;
        let vanish = self
            .on_server("INBOX", "vanish")
            .await
            .expect("the message to expunge");
        d.expunge(ADDRESS, "INBOX", vanish.uid).await;
        d.save(
            ADDRESS,
            "INBOX",
            &incoming(&self.id("late"), "Late", "Late body", "", None),
            None,
        )
        .await;

        self.sync.incremental().await.expect("incremental");

        let stored = self.must("invoice").await;
        let keywords = self.keywords(&stored.id).await;
        assert!(
            keywords.iter().any(|k| k == "$flagged"),
            "a flag set elsewhere arrives: {keywords:?}"
        );
        assert!(
            self.stored("vanish").await.is_none(),
            "a message expunged elsewhere leaves the store"
        );
        self.must("late").await;
    }

    async fn flags_reach_the_server(&self) {
        let welcome = self.must("welcome").await;
        for action in [TriageAction::Star, TriageAction::MarkRead] {
            self.sync
                .triage_thread(&welcome.thread_id, &action)
                .await
                .expect("the flag goes out");
        }
        let held = self
            .on_server("INBOX", "welcome")
            .await
            .expect("still in the inbox");
        assert!(
            held.has_flag("\\Flagged") && held.has_flag("\\Seen"),
            "{held:?}"
        );
    }

    async fn a_move_keeps_the_message_its_id(&self) {
        let invoice = self.must("invoice").await;
        self.sync
            .triage_thread(
                &invoice.thread_id,
                &TriageAction::MoveTo("Receipts".to_string()),
            )
            .await
            .expect("the move");
        assert!(
            self.on_server("INBOX", "invoice").await.is_none(),
            "the inbox no longer holds it"
        );
        assert_eq!(
            self.located(&invoice.id).await,
            self.where_server_has("Receipts", "invoice").await,
            "the ref follows the message"
        );
        assert_eq!(
            self.must("invoice").await.id,
            invoice.id,
            "the store's id survives a move"
        );
        let body = self
            .sync
            .body(&invoice.id)
            .await
            .expect("the body after the move");
        assert!(
            body.text
                .as_deref()
                .unwrap_or_default()
                .contains("The invoice body"),
            "{body:?}"
        );
    }

    async fn archive_files_the_thread(&self) {
        let welcome = self.must("welcome").await;
        self.sync
            .triage_thread(&welcome.thread_id, &TriageAction::Archive)
            .await
            .expect("archive");
        let mailboxes = self.dovecot.mailboxes(ADDRESS).await;
        assert!(
            mailboxes.iter().any(|name| name == "Archive"),
            "an Archive mailbox exists, made by the client where the server had none: {mailboxes:?}"
        );
        for name in ["welcome", "reply"] {
            assert!(
                self.on_server("Archive", name).await.is_some(),
                "{name} is in Archive"
            );
            assert!(
                self.on_server("INBOX", name).await.is_none(),
                "{name} left the inbox"
            );
        }
    }

    /// A folder account's move never takes a copy out of Sent, Drafts,
    /// Trash or Junk where it already sits (`drop_protected` in
    /// sync/src/ops.rs). Archiving a thread whose reply lives only in Sent
    /// must move the Inbox message to Archive and leave the Sent copy
    /// alone.
    async fn archiving_a_thread_leaves_its_sent_copy_in_sent(&self) {
        let note = self.must("note").await;
        let reply = self.must("note-reply").await;
        assert_eq!(
            note.thread_id, reply.thread_id,
            "the Sent reply joins the inbox message's thread"
        );
        self.sync
            .triage_thread(&note.thread_id, &TriageAction::Archive)
            .await
            .expect("archive");
        assert!(
            self.on_server("Archive", "note").await.is_some(),
            "the inbox message moved to Archive"
        );
        assert!(
            self.on_server("INBOX", "note").await.is_none(),
            "note left the inbox"
        );
        assert!(
            self.on_server("Sent", "note-reply").await.is_some(),
            "the Sent copy of the reply stays in Sent"
        );
        assert!(
            self.on_server("Archive", "note-reply").await.is_none(),
            "the Sent copy does not also move to Archive"
        );
    }

    async fn delete_forever_erases(&self) {
        let erase = self.must("erase").await;
        let answers = self
            .sync
            .erase_all(&[Target::thread(self.account_id, erase.thread_id.clone())])
            .await
            .expect("erase");
        assert!(answers.iter().all(Result::is_ok), "{answers:?}");
        assert!(
            self.on_server("INBOX", "erase").await.is_none(),
            "gone from the inbox"
        );
        assert!(
            self.on_server("Trash", "erase").await.is_none(),
            "and not in the Trash"
        );
        assert!(
            self.stored("erase").await.is_none(),
            "and gone from the store"
        );
    }

    async fn drafts_save_replace_and_send(&self) {
        // If nothing has selected Drafts yet, the first APPEND makes it, and
        // Dovecot 2.4.5 answers that APPEND with a UIDVALIDITY one more than
        // the mailbox then reports. The ref must name the real one either way.
        let saved = self
            .sync
            .save_draft(
                outgoing(&self.id("draft-1"), "Plans", "First draft"),
                None,
                None,
            )
            .await
            .expect("save a draft");
        // Drafts is not the Inbox, so the look that brings the draft into
        // the store has to cover every mailbox.
        self.look_everywhere().await;
        let first = self.must("draft-1").await;
        let held = self
            .on_server("Drafts", "draft-1")
            .await
            .expect("the draft on the server");
        assert!(held.has_flag("\\Draft"), "{held:?}");
        assert_eq!(
            self.located(&first.id).await,
            self.where_server_has("Drafts", "draft-1").await
        );
        let body = self.sync.body(&first.id).await.expect("the draft's body");
        assert!(
            body.text
                .as_deref()
                .unwrap_or_default()
                .contains("First draft"),
            "{body:?}"
        );

        let replaced = self
            .sync
            .save_draft(
                outgoing(&self.id("draft-2"), "Plans", "Second draft"),
                None,
                Some(saved.draft_id.clone()),
            )
            .await
            .expect("replace the draft");
        let drafts: Vec<String> = self
            .dovecot
            .messages(ADDRESS, "Drafts")
            .await
            .into_iter()
            .map(|held| held.message_id)
            .collect();
        assert_eq!(
            drafts,
            vec![self.id("draft-2")],
            "the new copy replaced the old"
        );

        let sent = self
            .sync
            .send_draft(&replaced.draft_id)
            .await
            .expect("send the draft");
        assert!(sent.is_some(), "the draft was there to send");
        let caught = self
            .sink
            .find(&self.id("draft-2"))
            .await
            .expect("the sink has it");
        assert_eq!(caught.to, vec!["bo@example.test".to_string()]);
        assert!(
            self.dovecot.messages(ADDRESS, "Drafts").await.is_empty(),
            "a sent draft leaves Drafts"
        );
        let filed = self
            .on_server("Sent", "draft-2")
            .await
            .expect("a copy in Sent");
        assert!(filed.has_flag("\\Seen"), "{filed:?}");
    }

    async fn a_send_reaches_the_sink_and_sent(&self) {
        let message_id = self.id("sent");
        self.sync
            .send(outgoing(&message_id, "Hello", "Sent body"), None, None)
            .await
            .expect("send");
        let caught = self.sink.find(&message_id).await.expect("the sink has it");
        assert_eq!(caught.subject, "Hello");
        assert_eq!(caught.from, ADDRESS);
        let filed: Vec<_> = self
            .dovecot
            .messages(ADDRESS, "Sent")
            .await
            .into_iter()
            .filter(|held| held.message_id == message_id)
            .collect();
        assert_eq!(filed.len(), 1, "one copy in Sent: {filed:?}");
        assert!(filed[0].has_flag("\\Seen"), "{filed:?}");
    }

    async fn search_finds_mail_past_the_window(&self) {
        let query = Query::Term(Term::Subject("Old plans".to_string()));
        let searched = self
            .sync
            .search_tree(&query, 10)
            .await
            .expect("the server searches");
        assert!(!searched.store_only, "IMAP can say a subject search");
        assert_eq!(searched.refs.len(), 1, "{searched:?}");
        let metas = self
            .sync
            .metadata_of(&searched.refs)
            .await
            .expect("the hit's metadata");
        assert_eq!(
            metas
                .iter()
                .map(|meta| meta.subject.as_str())
                .collect::<Vec<_>>(),
            vec!["Old plans"]
        );
    }

    async fn a_new_uidvalidity_keeps_ids_and_bodies(&self) {
        let late = self.must("late").await;
        self.sync.body(&late.id).await.expect("the body");
        assert!(self.has_body(&late.id).await, "the body is cached");
        let before = self.dovecot.uidvalidity(ADDRESS, "INBOX").await;
        let after = before + 1000;
        self.dovecot.set_uidvalidity(ADDRESS, "INBOX", after).await;

        // Dovecot's own IMAP process keeps a mailbox's UIDVALIDITY as it
        // stood when the process first opened it, even across a fresh
        // SELECT on that same connection, once `doveadm mailbox update`
        // has changed it from outside; only a new connection reads what
        // doveadm wrote. `self.sync`'s pool already holds INBOX open from
        // earlier steps, so the look that is meant to notice this change
        // has to open a fresh connection, as a reconnect or an app
        // restart would.
        self.look_everywhere().await;

        assert_eq!(
            self.must("late").await.id,
            late.id,
            "the store's id survives"
        );
        assert_eq!(self.copies("late").await, 1, "and no second copy appears");
        assert!(self.has_body(&late.id).await, "the cached body survives");
        assert_eq!(
            self.located(&late.id).await,
            self.where_server_has("INBOX", "late").await,
            "the ref names the new UIDVALIDITY"
        );
    }

    async fn idle_brings_new_mail(self) {
        let Run {
            profile,
            dovecot,
            sink,
            password,
            db,
            account_id,
            sync,
            ..
        } = self;
        // Its connections close, so the count below is the engine's alone.
        // The engine's poll is NO_POLL, and the adapter's own cadence is
        // 5 minutes or more on a server with IDLE, so only IDLE brings the
        // message in time.
        drop(sync);
        let (engine, _events) = SyncEngine::new(
            db.clone(),
            EngineConfig {
                poll_interval: NO_POLL,
                ..EngineConfig::default()
            },
        );
        engine.start_account(account_id, services(dovecot, sink, password));
        // Time for the loop's first pass, and for the IDLE connection to sign
        // in and select the inbox.
        tokio::time::sleep(Duration::from_secs(3)).await;

        let message_id = format!("pushed.{profile:?}@example.test");
        dovecot
            .save(
                ADDRESS,
                "INBOX",
                &incoming(&message_id, "Pushed", "Pushed body", "", None),
                None,
            )
            .await;
        let saved_at = Instant::now();
        let arrived = loop {
            if stored_in(&db, account_id, &message_id).await.is_some() {
                break Some(saved_at.elapsed());
            }
            if saved_at.elapsed() > Duration::from_secs(20) {
                break None;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        let connections = dovecot.connections(ADDRESS).await;
        engine.shutdown();

        let arrived = arrived.unwrap_or_else(|| {
            panic!("new mail was not in the store 20 s after the server took it, with the poll {NO_POLL:?} away")
        });
        eprintln!("{profile:?}: new mail reached the store {arrived:?} after the server took it");
        assert!(
            connections <= 3,
            "{connections} connections, over the limit of 3"
        );
    }
}
