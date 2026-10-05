//! A POP3 account end to end: `connect_pop3` builds the client from the
//! servers the store keeps, the check downloads the Inbox over TLS into
//! the store with its raw copy, and Remove After Downloading empties the
//! server once QUIT goes through.
//!
//! One test, because it points `SSL_CERT_FILE` at a root made for the
//! run, and the environment belongs to the whole process.

use std::sync::Arc;

use mailrs_discover::{Security, Server, UserName};
use mailrs_domain::{MailSet, RemoveSetting, Role};
use mailrs_store::{Db, accounts, local_messages, messages};
use mailrs_sync::passwords::{MemoryPasswords, PasswordStore};
use mailrs_sync::{AccountSync, connect_pop3, pop3_servers_for};
use mailrs_testmail::{Certs, Dovecot, Profile};

const USER: &str = "me@example.test";

#[test]
fn a_pop3_account_downloads_from_dovecot() {
    if mailrs_testmail::docker().is_none() {
        return;
    }
    let Some(certs) = Certs::make() else { return };
    // SAFETY: the only test in this binary, before any thread starts.
    unsafe { mailrs_testmail::trust(&certs.root()) };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(mailrs_sync::WORKER_STACK)
        .build()
        .expect("a runtime");
    runtime.block_on(async {
        let password = mailrs_testmail::password();
        let Some(dovecot) = Dovecot::start(&certs, Profile::Full, &password).await else { return };
        let pop3s = dovecot.pop3s.expect("POP3 over TLS");
        dovecot
            .save(USER, "INBOX", b"From: a@example.test\r\nTo: me@example.test\r\nSubject: Hello\r\nMessage-ID: <h1@example.test>\r\n\r\nHi\r\n", None)
            .await;
        let dir = tempfile::tempdir().expect("a folder");
        let db = Db::open(&dir.path().join("mail.db")).expect("a store");
        let account = db
            .write(move |c| {
                let id = accounts::insert_pop3_account(c, USER, "Dovecot", RemoveSetting::Downloaded, 0)?.expect("a new account");
                let at = Server { host: "localhost".into(), port: pop3s, security: Security::Tls, user_name: UserName::Address };
                // Nothing sends mail in this test, so SMTP names the POP3 port.
                mailrs_store::servers::save_pop3(c, id, &pop3_servers_for(&at, USER, &at, USER))?;
                Ok(accounts::account(c, id)?.expect("just made"))
            })
            .await
            .expect("an account");
        let passwords = Arc::new(MemoryPasswords::default());
        passwords.save(account.id, &password).expect("a password");
        let services = connect_pop3(&db, passwords, &account).await.expect("services");
        let sync = AccountSync::new(account.id, services, db.clone(), async_channel::unbounded().0);
        sync.refresh_labels().await.expect("the role mailboxes");

        sync.pop3_check().await.expect("a check over TLS");
        let id = account.id;
        let inbox: Vec<String> = db
            .read(move |c| messages::held_by(c, id, &MailSet::Role(Role::Inbox)))
            .await
            .expect("the Inbox")
            .into_iter()
            .collect();
        assert_eq!(inbox.len(), 1);
        let first = inbox[0].clone();
        assert!(first.starts_with("pop3/"), "{first}");
        let raw = db.read(move |c| local_messages::get(c, id, &first)).await.expect("a read").expect("the raw copy");
        assert!(String::from_utf8_lossy(&raw).contains("Subject: Hello"));
        assert!(dovecot.messages(USER, "INBOX").await.is_empty(), "Remove After Downloading emptied the server at QUIT");

        sync.pop3_check().await.expect("a second check");
        let after = db.read(move |c| messages::held_by(c, id, &MailSet::Role(Role::Inbox))).await.expect("the Inbox");
        assert_eq!(after.len(), 1, "nothing new, nothing lost");
    });
}
