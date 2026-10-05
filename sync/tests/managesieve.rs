//! Server rules end to end. `connect_imap` builds the ManageSieve client
//! from the servers the store keeps, `SieveRules` writes the script over
//! the real STARTTLS path, Dovecot runs it on delivery, and the automatic
//! reply sits in the same script. A wrong password reaches the app as
//! `NeedsReauth`.
//!
//! One test, because it points `SSL_CERT_FILE` at a root made for the
//! run, and the environment belongs to the whole process.

use std::sync::Arc;

use mailrs_discover::{Security, Server, UserName};
use mailrs_domain::{Account, Filter, FilterAction, FilterCriteria, MailSet, Vacation};
use mailrs_store::services::{self, FoundService, ServiceKind};
use mailrs_store::{Db, accounts};
use mailrs_sync::passwords::{MemoryPasswords, PasswordStore};
use mailrs_sync::{AnyAutoReply, AnyRules, AutoReplyService, BackendError, RulesPlace, RulesService};
use mailrs_testmail::{Certs, Dovecot, Profile};

const USER: &str = "me@example.test";

/// An IMAP account saved as sign-in leaves it, with Dovecot's ports and a
/// ManageSieve server the person confirmed, whose `host:port` goes through
/// the same parsing a found server does.
async fn account_on(dovecot: &Dovecot, dir: &std::path::Path) -> (Db, Account) {
    let db = Db::open(&dir.join("mail.db")).expect("a store");
    let imaps = dovecot.imaps;
    let sieve = dovecot.sieve.expect("the Sieve profile maps ManageSieve");
    let account = db
        .write(move |c| {
            let id = accounts::insert_imap_account(c, USER, "Dovecot", 0)?.expect("a new account");
            let at = |port| Server {
                host: "localhost".into(),
                port,
                security: Security::Tls,
                user_name: UserName::Address,
            };
            // Nothing sends mail in this test, so SMTP names the IMAP port.
            mailrs_store::servers::save(c, id, &mailrs_sync::servers_for(&at(imaps), USER, &at(imaps), USER))?;
            services::save(
                c,
                id,
                &FoundService {
                    kind: ServiceKind::Sieve,
                    url: format!("localhost:{sieve}"),
                    user_name: USER.into(),
                    confirmed: true,
                    source: "test".into(),
                },
            )?;
            Ok(accounts::account(c, id)?.expect("just made"))
        })
        .await
        .expect("an account");
    (db, account)
}

#[test]
fn a_server_rule_files_arriving_mail() {
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
        let Some(dovecot) = Dovecot::start(&certs, Profile::Sieve, &password).await else { return };
        dovecot.create_mailbox(USER, "Receipts").await;
        let dir = tempfile::tempdir().expect("a folder");
        let (db, account) = account_on(&dovecot, dir.path()).await;
        let passwords = Arc::new(MemoryPasswords::default());
        passwords.save(account.id, &password).expect("a password");

        let services = mailrs_sync::connect_imap(&db, Arc::clone(&passwords), &account, 30).await.expect("services");
        let (Some(rules), Some(reply @ AnyAutoReply::Sieve(_))) = (services.rules.as_ref(), services.auto_reply.as_ref()) else {
            panic!("the confirmed ManageSieve server serves the rules and the reply");
        };
        assert!(matches!(rules, AnyRules::Sieve(_)));
        assert_eq!(rules.place(), RulesPlace::Server);

        rules
            .create_filter(&Filter {
                criteria: FilterCriteria { from: Some("shop@example.test".into()), ..FilterCriteria::default() },
                action: FilterAction { add: vec![MailSet::Mailbox("Receipts".into())], ..FilterAction::default() },
                ..Filter::default()
            })
            .await
            .expect("the rule is written");
        reply
            .set_vacation(&Vacation { enabled: true, subject: "Away".into(), body: "Back soon.".into(), ..Vacation::default() })
            .await
            .expect("the reply is written");

        dovecot
            .deliver(
                USER,
                b"From: shop@example.test\r\nTo: me@example.test\r\nSubject: Your receipt\r\nMessage-ID: <r1@example.test>\r\n\r\nThanks.\r\n",
            )
            .await;
        assert!(dovecot.find(USER, "Receipts", "r1@example.test").await.is_some(), "the server ran the rule");
        assert!(dovecot.find(USER, "INBOX", "r1@example.test").await.is_none());
        assert_eq!(rules.filters().await.expect("the rules").len(), 1);
        assert!(reply.vacation().await.expect("the reply").enabled);

        // The same account with a wrong password: the server refuses the
        // login, and the adapter tells the app the account needs to sign in.
        passwords.save(account.id, "wrong").expect("a password");
        let wrong = mailrs_sync::connect_imap(&db, passwords, &account, 30).await.expect("services");
        let refused = wrong.rules.as_ref().expect("rules").filters().await;
        assert!(matches!(refused, Err(BackendError::NeedsReauth)), "{refused:?}");
    });
}
