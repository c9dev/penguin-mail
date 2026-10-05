//! A POP3 account's mail service, over the store and the fakes: the role
//! mailboxes, moves and flags that need no server, Delete Forever, the
//! sent copy and drafts kept here, and the engine leaving old mail alone.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use mailrs_domain::mailbox::keyword;
use mailrs_domain::{AccountId, ChangeEvent, EpochMillis, MailSet, RemoveSetting, Role, Target};
use mailrs_store::services::{self, FoundService, ServiceKind};
use mailrs_store::{Db, accounts, local_messages, messages, pop3};
use tokio::time::Instant;

use super::Connected;
use crate::fake::{FakePop3, FakeSmtp, pop3_mail};
use crate::passwords::{MemoryPasswords, PasswordStore};
use crate::services::pop3::{keep_local, local_meta};
use crate::{
    AccountServices, AccountSync, AnyCalendar, AnyContacts, AnyRules, BackendError, EngineConfig, History,
    MailAction, MailActions, MailBackend, Pop3Settings, RulesPlace, TriageAction, connect_pop3,
    now_millis,
};

const DAY: EpochMillis = 24 * 60 * 60 * 1000;

pub(crate) struct Pop3Harness {
    pub fake: Arc<FakePop3>,
    pub smtp: Arc<FakeSmtp>,
    pub sync: Arc<AccountSync>,
    pub db: Db,
    pub events: async_channel::Receiver<ChangeEvent>,
    pub account_id: AccountId,
    sender: async_channel::Sender<ChangeEvent>,
    _dir: tempfile::TempDir,
}

/// A POP3 account with `remove`, over `fake`, its role mailboxes listed as
/// the engine's first tick lists them.
pub(crate) async fn pop3_harness(fake: FakePop3, remove: RemoveSetting) -> Pop3Harness {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("mail.db")).unwrap();
    let account_id = db
        .write(move |c| Ok(accounts::insert_pop3_account(c, "me@example.org", "example.org", remove, 0)?.expect("a new account")))
        .await
        .unwrap();
    let (fake, smtp) = (Arc::new(fake), Arc::new(FakeSmtp::default()));
    let (sender, events) = async_channel::unbounded();
    let settings = Pop3Settings { address: "me@example.org".into(), provider_name: "example.org".into() };
    let services = AccountServices::fake_pop3(db.clone(), account_id, Arc::clone(&fake), Arc::clone(&smtp), settings);
    let sync = Arc::new(AccountSync::new(account_id, services, db.clone(), sender.clone()));
    sync.refresh_labels().await.unwrap();
    Pop3Harness { fake, smtp, sync, db, events, account_id, sender, _dir: dir }
}

impl Pop3Harness {
    /// The store's database file.
    pub fn db_path(&self) -> std::path::PathBuf {
        self._dir.path().join("mail.db")
    }

    /// Another sync over the same store and fakes, as a restart makes.
    pub fn again(&self) -> AccountSync {
        self.with_server(Arc::clone(&self.fake))
    }

    /// A sync over the same store and SMTP sink, reaching the POP3 server
    /// `fake`.
    pub fn with_server(&self, fake: Arc<FakePop3>) -> AccountSync {
        let settings = Pop3Settings { address: "me@example.org".into(), provider_name: "example.org".into() };
        let services = AccountServices::fake_pop3(self.db.clone(), self.account_id, fake, Arc::clone(&self.smtp), settings);
        AccountSync::new(self.account_id, services, self.db.clone(), self.sender.clone())
    }

    /// Stores `raw` in `mailbox` as downloaded message `uidl`, received at
    /// `received`, and answers its id.
    pub async fn keep(&self, uidl: &str, raw: &[u8], mailbox: &str, received: EpochMillis) -> String {
        let account_id = self.account_id;
        let (raw, uidl) = (raw.to_vec(), uidl.to_string());
        let mailbox = mailbox.to_string();
        self.db
            .write(move |c| {
                let id = pop3::download_id(c, account_id, &uidl)?;
                let (meta, links) = local_meta(account_id, &id, &raw, &mailbox, &[], received, false);
                keep_local(c, account_id, meta, links, &raw)?;
                pop3::mark_downloaded(c, account_id, &uidl, &id, received)?;
                Ok(id)
            })
            .await
            .unwrap()
    }

    /// The stored ids in `set`, sorted.
    pub async fn ids_in(&self, set: MailSet) -> Vec<String> {
        let account_id = self.account_id;
        let mut ids: Vec<String> = self.db.read(move |c| messages::held_by(c, account_id, &set)).await.unwrap().into_iter().collect();
        ids.sort();
        ids
    }

    fn actions(&self) -> MailActions<Connected> {
        MailActions::new(
            Arc::new(Connected(HashMap::from([(self.account_id, Arc::clone(&self.sync))]))),
            self.db.clone(),
            crate::OneClick::Fake(Arc::default()),
        )
    }

    /// The thread the store put message `id` in.
    pub async fn thread_of(&self, id: &str) -> String {
        let (account_id, id) = (self.account_id, id.to_string());
        self.db.read(move |c| messages::thread_id_of(c, account_id, &id)).await.unwrap().expect("stored")
    }
}

#[tokio::test]
async fn a_pop3_account_lists_six_role_mailboxes_and_the_ones_made_here() {
    let h = pop3_harness(FakePop3::default(), RemoveSetting::Never).await;
    let mail = &h.sync.services().mail;
    for role in [Role::Inbox, Role::Sent, Role::Drafts, Role::Trash, Role::Junk, Role::Archive] {
        let id = mail.mailbox_for(role).expect("every role has a mailbox");
        assert_eq!(mail.set_of(&id), MailSet::Role(role));
        assert!(!mail.made_by_person(&id));
    }
    let made = h.sync.create_label("Receipts").await.unwrap();
    assert!(mail.made_by_person(&made.id));
    let listed = mail.mailboxes().await.unwrap();
    assert_eq!(listed.len(), 7);
    assert!(listed.iter().any(|m| m.id == made.id && m.name == "Receipts"));
    assert!(mail.rename_mailbox("inbox", "Mine").await.is_err(), "a role mailbox keeps its name");
    assert_eq!(h.fake.connects(), 0, "listing asks the POP3 server nothing");
    assert_eq!(h.again().services().mail.mailboxes().await.unwrap().len(), 7, "a restart lists the same");
}

#[tokio::test]
async fn moving_and_flagging_change_the_store_and_reach_no_server() {
    let h = pop3_harness(FakePop3::default(), RemoveSetting::Never).await;
    let id = h.keep("u1", &pop3_mail(1), "inbox", now_millis()).await;
    let target = Target::thread(h.account_id, h.thread_of(&id).await);
    for action in [TriageAction::Archive, TriageAction::Star] {
        let outcome = h.actions().run(std::slice::from_ref(&target), MailAction::Triage(action), History::Record).await;
        assert_eq!(outcome.done, std::slice::from_ref(&target));
    }
    assert!(h.ids_in(MailSet::Role(Role::Inbox)).await.is_empty());
    assert_eq!(h.ids_in(MailSet::Role(Role::Archive)).await, std::slice::from_ref(&id));
    assert_eq!(h.ids_in(MailSet::Keyword(keyword::FLAGGED.into())).await, [id]);
    assert_eq!(h.fake.connects(), 0);
    assert!(h.smtp.with(|s| s.sent.is_empty()));
}

#[tokio::test]
async fn delete_forever_drops_the_copy_and_asks_the_server_only_when_the_account_removes_mail() {
    for (remove, wanted) in [(RemoveSetting::Never, false), (RemoveSetting::Days(30), true)] {
        let h = pop3_harness(FakePop3::default(), remove).await;
        let id = h.keep("u1", &pop3_mail(1), "trash", now_millis()).await;
        let thread = h.thread_of(&id).await;
        let erased = h.sync.erase_all(&[Target::thread(h.account_id, thread)]).await.unwrap();
        assert!(erased.iter().all(Result::is_ok));
        let account_id = h.account_id;
        let (raw, pending) = h
            .db
            .read(move |c| Ok((local_messages::get(c, account_id, "pop3/u1")?, pop3::pending_removal(c, account_id, None, pop3::PAGE)?)))
            .await
            .unwrap();
        assert_eq!(raw, None, "{remove:?}: the raw copy goes with the message");
        assert_eq!(pending == ["u1"], wanted, "{remove:?}");
    }
}

#[tokio::test]
async fn the_hourly_tick_keeps_archived_mail_older_than_the_window() {
    let h = pop3_harness(FakePop3::default(), RemoveSetting::Never).await;
    let old = h.keep("old", &pop3_mail(1), "archive", now_millis() - 730 * DAY).await;
    let (mut next_poll, mut next_prune, mut stagger) = (Instant::now(), Instant::now(), Duration::ZERO);
    let more = crate::engine::tick(&h.sync, &mut next_poll, &mut next_prune, &mut stagger, &EngineConfig::default()).await.unwrap();
    assert!(!more, "a POP3 account pages no window");
    assert_eq!(h.ids_in(MailSet::Role(Role::Archive)).await, [old]);
}

#[tokio::test]
async fn sending_files_a_read_copy_in_sent_on_this_computer() {
    let h = pop3_harness(FakePop3::default(), RemoveSetting::Never).await;
    let raw = b"From: me@example.org\r\nTo: ana@example.org\r\nSubject: Hi\r\nMessage-ID: <s1@example.org>\r\n\r\nHello\r\n".to_vec();
    let sent = h.sync.send(raw.clone(), None, None).await.unwrap();
    assert_eq!(h.smtp.with(|s| s.sent.len()), 1);
    assert_eq!(h.ids_in(MailSet::Role(Role::Sent)).await, std::slice::from_ref(&sent));
    assert!(h.ids_in(MailSet::Unseen).await.is_empty(), "the sent copy is read");
    let account_id = h.account_id;
    let kept = h.db.read(move |c| local_messages::get(c, account_id, &sent)).await.unwrap();
    assert_eq!(kept, Some(raw.clone()));
    assert!(h.sync.sent_copy(&raw).await.unwrap().is_some(), "the outbox finds what went out");
    let heard: Vec<ChangeEvent> = std::iter::from_fn(|| h.events.try_recv().ok()).collect();
    assert!(heard.iter().any(|e| matches!(e, ChangeEvent::ThreadsChanged { .. })), "the window hears of the copy");
}

#[tokio::test]
async fn a_saved_draft_lives_in_drafts_and_saving_again_leaves_one() {
    let h = pop3_harness(FakePop3::default(), RemoveSetting::Never).await;
    let draft = |body: &str| format!("From: me@example.org\r\nTo: ana@example.org\r\nSubject: Plan\r\n\r\n{body}\r\n").into_bytes();
    let first = h.sync.save_draft(draft("one"), None, None).await.unwrap();
    let second = h.sync.save_draft(draft("two"), None, Some(first.draft_id.clone())).await.unwrap();
    assert_eq!(h.ids_in(MailSet::Role(Role::Drafts)).await, std::slice::from_ref(&second.message_id));
    assert!(h.sync.send_draft(&second.draft_id).await.unwrap().is_some());
    assert!(h.ids_in(MailSet::Role(Role::Drafts)).await.is_empty());
    assert_eq!(h.ids_in(MailSet::Role(Role::Sent)).await.len(), 1);
}

#[tokio::test]
async fn connect_pop3_keeps_rules_here_and_takes_a_confirmed_calendar_and_contacts() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("mail.db")).unwrap();
    let found = |kind, url: &str| FoundService { kind, url: url.into(), user_name: "me@example.org".into(), confirmed: true, source: "table".into() };
    let rows = vec![
        found(ServiceKind::CalDav, "https://caldav.example.org/"),
        found(ServiceKind::CardDav, "https://carddav.example.org/"),
        found(ServiceKind::Sieve, "pop.example.org:4190"),
    ];
    let account = db
        .write(move |c| {
            let id = accounts::insert_pop3_account(c, "me@example.org", "example.org", RemoveSetting::Never, 0)?.expect("new");
            let tls = |host: &str, port| mailrs_discover::Server { host: host.into(), port, security: mailrs_discover::Security::Tls, user_name: mailrs_discover::UserName::Address };
            mailrs_store::servers::save_pop3(c, id, &crate::pop3_servers_for(&tls("pop.example.org", 995), "me@example.org", &tls("smtp.example.org", 465), "me@example.org"))?;
            for row in &rows {
                services::save(c, id, row)?;
            }
            Ok(accounts::account(c, id)?.expect("just made"))
        })
        .await
        .unwrap();
    let passwords = Arc::new(MemoryPasswords::default());
    passwords.save(account.id, "pw").unwrap();
    let services = connect_pop3(&db, passwords, &account).await.unwrap();
    assert!(matches!(services.calendar, Some(AnyCalendar::Pop3Dav(_))));
    assert!(matches!(services.contacts, Some(AnyContacts::Dav(_))));
    assert_eq!(services.rules.as_ref().map(AnyRules::place), Some(RulesPlace::ThisComputer), "a Sieve row never serves a POP3 account");
    assert!(services.auto_reply.is_none());
    let offers = services.offers();
    assert!(offers.rules && offers.calendar && offers.contacts && !offers.auto_reply && !offers.search);
    assert!(offers.delete_forever && !offers.labels && !offers.categories);
    assert!(services.capabilities().local_mailboxes);

    let none = Arc::new(MemoryPasswords::default());
    assert!(connect_pop3(&db, none, &account).await.is_err(), "no password needs a new sign-in");
}

#[tokio::test]
async fn renaming_a_folder_renames_the_ones_inside_it() {
    let h = pop3_harness(FakePop3::default(), RemoveSetting::Never).await;
    let parent = h.sync.create_label("Work").await.unwrap();
    let child = h.sync.create_label("Work/Clients").await.unwrap();
    h.sync.rename_label(&parent.id, "Jobs").await.unwrap();
    let listed = h.sync.services().mail.mailboxes().await.unwrap();
    let mut names: Vec<&str> = listed.iter().skip(6).map(|m| m.name.as_str()).collect();
    names.sort();
    assert_eq!(names, ["Jobs", "Jobs/Clients"]);
    assert!(listed.iter().any(|m| m.id == child.id), "a folder keeps its id through a rename");
}

#[tokio::test]
async fn deleting_a_folder_deletes_the_mail_in_it_as_the_dialog_says() {
    let h = pop3_harness(FakePop3::default(), RemoveSetting::Never).await;
    let folder = h.sync.create_label("Receipts").await.unwrap();
    let filed = h.keep("u1", &pop3_mail(1), &folder.id, now_millis()).await;
    let kept = h.keep("u2", &pop3_mail(2), "inbox", now_millis()).await;
    h.sync.delete_label(&folder.id).await.unwrap();
    let account_id = h.account_id;
    let (row, raw) = h
        .db
        .read(move |c| Ok((messages::thread_id_of(c, account_id, &filed)?, local_messages::get(c, account_id, "pop3/u1")?)))
        .await
        .unwrap();
    assert_eq!((row, raw), (None, None), "no message is left in no mailbox");
    assert_eq!(h.ids_in(MailSet::Role(Role::Inbox)).await, [kept]);
    assert!(h.sync.services().mail.delete_mailbox("inbox").await.is_err(), "a role mailbox stays");
}

/// A store-only account's folders change only through the person. A
/// listing read before they made or renamed one must not undo it when it
/// is stored, or the new folder goes with the mail moved into it.
#[tokio::test]
async fn a_listing_read_before_the_person_changed_a_folder_leaves_the_change_alone() {
    let h = pop3_harness(FakePop3::default(), RemoveSetting::Never).await;
    let mail = &h.sync.services().mail;
    let bills = h.sync.create_label("Bills").await.unwrap();
    let stale = mail.mailboxes().await.unwrap();
    let receipts = h.sync.create_label("Receipts").await.unwrap();
    let filed = h.keep("u1", &pop3_mail(1), &receipts.id, now_millis()).await;
    h.sync.rename_label(&bills.id, "Invoices").await.unwrap();
    h.sync.store_listing(stale).await.unwrap();
    let listed = mail.mailboxes().await.unwrap();
    assert!(listed.iter().any(|m| m.id == receipts.id), "the new folder stays");
    assert_eq!(h.ids_in(MailSet::Mailbox(receipts.id)).await, [filed], "with its mail");
    let renamed = listed.iter().find(|m| m.id == bills.id).map(|m| m.name.as_str());
    assert_eq!(renamed, Some("Invoices"), "the rename stays");
}

/// A file of its own, a forwarded message holding a file, and one sent
/// base64-encoded, whose parts lie only in its decoded copy.
const WITH_FILES: &[u8] = b"From: Ana <ana@example.org>\r\nSubject: Files\r\n\
Content-Type: multipart/mixed; boundary=f\r\n\r\n--f\r\nContent-Type: text/plain\r\n\r\nSee below.\r\n\
--f\r\nContent-Type: application/pdf; name=a.pdf\r\nContent-Transfer-Encoding: base64\r\n\r\nJVBERi0xLjcK\r\n\
--f\r\nContent-Type: message/rfc822\r\n\r\nSubject: inner\r\nContent-Type: multipart/mixed; boundary=g\r\n\r\n\
--g\r\nContent-Type: text/plain\r\n\r\nInner body.\r\n--g\r\nContent-Type: image/png; name=i.png\r\n\
Content-Transfer-Encoding: base64\r\n\r\niVBORw0KGgo=\r\n--g--\r\n\
--f\r\nContent-Type: message/rfc822\r\nContent-Transfer-Encoding: base64\r\n\r\n\
U3ViamVjdDogaW5uZXINCkNvbnRlbnQtVHlwZTogYXBwbGljYXRpb24vemlwOyBuYW1lPWEuemlwDQoNClBLAwQ=\r\n--f--\r\n";

/// Its parts come from the stored copy as a server's structure gives
/// them, and each file comes alone, whether the message opened first in
/// this run or not.
#[tokio::test]
async fn a_kept_message_opens_by_its_structure_and_each_file_comes_alone() {
    let h = pop3_harness(FakePop3::default(), RemoveSetting::Never).await;
    let id = h.keep("u1", WITH_FILES, "inbox", now_millis()).await;
    let expected = mailrs_mime::read(WITH_FILES);
    assert_eq!(expected.attachments.len(), 5, "the PDF, two forwarded messages and their files");
    let files: Vec<(String, Vec<u8>)> = expected
        .attachments
        .iter()
        .map(|a| a.part_id.clone())
        .zip(mailrs_mime::files(WITH_FILES))
        .collect();

    let mail = &h.sync.services().mail;
    let parts = mail.fetch_structure(&id).await.unwrap();
    assert_eq!(mailrs_mime::body(&parts), expected);
    for (path, bytes) in &files {
        assert_eq!(&mail.fetch_part(&id, path).await.unwrap(), bytes, "part {path} after opening");
    }

    let fresh = h.again();
    for (path, bytes) in &files {
        assert_eq!(&fresh.services().mail.fetch_part(&id, path).await.unwrap(), bytes, "part {path} unopened");
    }
    assert!(matches!(mail.fetch_part(&id, "9").await, Err(BackendError::NotFound)));
}
