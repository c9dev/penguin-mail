//! An IMAP account's services, as connect_imap builds them from the found
//! servers, and the local copy, the address book and the settings over
//! them.

use std::sync::Arc;

use mailrs_dav::Kind;
use mailrs_dav::fake::FakeDav;
use mailrs_domain::Filter;
use mailrs_sieve::fake::FakeSieve;
use mailrs_store::services::{self, FoundService, ServiceKind};

use crate::calendar_copy::CalendarCopy;
use crate::passwords::{MemoryPasswords, PasswordStore};
use crate::services::{CalDav, CardDav, SieveRules};
use crate::tests::{ServedBy, imap_harness};
use crate::{
    AccountSettings, AnyAutoReply, AnyCalendar, AnyContacts, AnyRules, ContactBook,
    ContactsService, Permitted, Replaced, RulesPlace, connect_imap,
};

fn found(kind: ServiceKind, url: &str, confirmed: bool) -> FoundService {
    FoundService {
        kind,
        url: url.into(),
        user_name: "me@example.com".into(),
        confirmed,
        source: "table".into(),
    }
}

/// An IMAP account saved with servers and a password, as sign-in leaves it.
async fn saved(
    rows: Vec<FoundService>,
) -> (mailrs_store::Db, mailrs_domain::Account, Arc<MemoryPasswords>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db = mailrs_store::Db::open(&dir.path().join("mail.db")).unwrap();
    let account = db
        .write(move |c| {
            let id = mailrs_store::accounts::insert_imap_account(c, "me@example.com", "Example", 0)?
                .expect("a new account");
            let tls = |host: &str, port| mailrs_discover::Server {
                host: host.into(),
                port,
                security: mailrs_discover::Security::Tls,
                user_name: mailrs_discover::UserName::Address,
            };
            mailrs_store::servers::save(
                c,
                id,
                &crate::servers_for(
                    &tls("imap.example.com", 993),
                    "me@example.com",
                    &tls("smtp.example.com", 465),
                    "me@example.com",
                ),
            )?;
            for row in &rows {
                services::save(c, id, row)?;
            }
            Ok(mailrs_store::accounts::account(c, id)?.expect("just made"))
        })
        .await
        .unwrap();
    let passwords = Arc::new(MemoryPasswords::default());
    passwords.save(account.id, "pw").unwrap();
    (db, account, passwords, dir)
}

#[tokio::test]
async fn an_account_with_no_servers_found_keeps_rules_here_and_has_no_reply() {
    let (db, account, passwords, _dir) = saved(Vec::new()).await;
    let services = connect_imap(&db, passwords, &account, 30).await.unwrap();
    assert!(matches!(services.rules, Some(AnyRules::Local(_))));
    assert_eq!(services.rules.as_ref().map(AnyRules::place), Some(RulesPlace::ThisComputer));
    assert!(services.auto_reply.is_none() && services.calendar.is_none() && services.contacts.is_none());
    let offers = services.offers();
    assert!(offers.rules && !offers.auto_reply && !offers.calendar);
}

#[tokio::test]
async fn confirmed_servers_serve_the_calendar_contacts_rules_and_reply() {
    let rows = vec![
        found(ServiceKind::CalDav, "https://caldav.example.com/", true),
        found(ServiceKind::CardDav, "https://carddav.example.com/", true),
        found(ServiceKind::Sieve, "imap.example.com:4190", true),
    ];
    let (db, account, passwords, _dir) = saved(rows).await;
    let services = connect_imap(&db, passwords, &account, 30).await.unwrap();
    assert!(matches!(services.calendar, Some(AnyCalendar::Dav(_))));
    assert!(matches!(services.contacts, Some(AnyContacts::Dav(_))));
    assert!(matches!(services.rules, Some(AnyRules::Sieve(_))));
    assert!(matches!(services.auto_reply, Some(AnyAutoReply::Sieve(_))));
    assert_eq!(services.rules.as_ref().map(AnyRules::place), Some(RulesPlace::Server));
    let offers = services.offers();
    assert!(offers.calendar && !offers.event_files && !offers.moves_events && !offers.calendar_list);
}

#[tokio::test]
async fn a_server_nobody_confirmed_is_not_used() {
    let (db, account, passwords, _dir) =
        saved(vec![found(ServiceKind::CalDav, "https://dav.elsewhere.example/", false)]).await;
    let services = connect_imap(&db, passwords, &account, 30).await.unwrap();
    assert!(
        services.calendar.is_none(),
        "the password never goes to a host the person did not say yes to"
    );
}

#[tokio::test]
async fn the_local_copy_reads_a_caldav_calendar_and_sends_an_edit_back() {
    let h = imap_harness().await;
    let dav = Arc::new(FakeDav::new());
    dav.add_collection("/cal/work/", Kind::Calendar, "Work", None);
    dav.put_resource(
        "/cal/work/lunch.ics",
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//t//EN\r\nBEGIN:VEVENT\r\nUID:lunch\r\nDTSTART:20261006T120000Z\r\n\
         DTEND:20261006T130000Z\r\nSUMMARY:Lunch\r\nX-KEEP:yes\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
    );
    let mail = h.sync.services().fake_imap_adapter().expect("an IMAP account");
    let services = h.sync.services().clone().with_calendar(AnyCalendar::FakeDav(CalDav::new(
        Arc::clone(&dav),
        mail,
        vec!["me@example.com".into()],
    )));
    let accounts = Arc::new(ServedBy::new(h.account_id, Arc::clone(&h.sync), services));
    let copy = CalendarCopy::new(accounts, h.db.clone());
    copy.refresh(h.account_id, crate::now_millis()).await.unwrap();
    let account_id = h.account_id;
    let stored = h
        .db
        .read(move |c| mailrs_store::calendar::find_event(c, account_id, "lunch"))
        .await
        .unwrap()
        .expect("read into the copy");
    assert_eq!(stored.title, "Lunch");
    copy.save(h.account_id, mailrs_domain::calendar::Event { title: "Lunch, moved".into(), ..stored })
        .await
        .unwrap();
    copy.send(h.account_id).await.unwrap();
    let body = dav.body("/cal/work/lunch.ics").unwrap();
    assert!(body.contains("SUMMARY:Lunch\\, moved") && body.contains("X-KEEP:yes"), "{body}");
}

#[tokio::test]
async fn a_lost_token_reads_the_calendar_whole_once_and_doubles_nothing() {
    let h = imap_harness().await;
    let dav = Arc::new(FakeDav::new());
    dav.add_collection("/cal/work/", Kind::Calendar, "Work", None);
    dav.put_resource(
        "/cal/work/a.ics",
        "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:a\r\nDTSTART:20261006T120000Z\r\nDTEND:20261006T130000Z\r\nSUMMARY:A\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
    );
    let mail = h.sync.services().fake_imap_adapter().expect("an IMAP account");
    let services = h
        .sync
        .services()
        .clone()
        .with_calendar(AnyCalendar::FakeDav(CalDav::new(Arc::clone(&dav), mail, vec![])));
    let copy = CalendarCopy::new(Arc::new(ServedBy::new(h.account_id, Arc::clone(&h.sync), services)), h.db.clone());
    let now = crate::now_millis();
    copy.refresh(h.account_id, now).await.unwrap();
    dav.remove_resource("/cal/work/a.ics");
    dav.forget_tokens();
    copy.permission_changed(h.account_id);
    copy.refresh(h.account_id, now + 120_000).await.unwrap();
    let account_id = h.account_id;
    assert!(
        h.db.read(move |c| mailrs_store::calendar::find_event(c, account_id, "a")).await.unwrap().is_none()
    );
}

#[tokio::test]
async fn an_address_book_that_came_reads_the_contacts_whole() {
    let h = imap_harness().await;
    let dav = Arc::new(FakeDav::new());
    dav.add_collection("/card/default/", Kind::AddressBook, "Contacts", None);
    dav.put_resource("/card/default/a.vcf", "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Ana\r\nEMAIL:ana@example.pt\r\nEND:VCARD\r\n");
    let services = h.sync.services().clone().with_contacts(AnyContacts::FakeDav(CardDav::new(Arc::clone(&dav))));
    let accounts = Arc::new(ServedBy::new(h.account_id, Arc::clone(&h.sync), services));
    let photos = tempfile::tempdir().unwrap();
    let book = ContactBook::new(accounts, h.db.clone(), photos.path().to_path_buf());
    book.refresh(h.account_id).await.unwrap();
    dav.add_collection("/card/work/", Kind::AddressBook, "Work", None);
    dav.put_resource("/card/work/b.vcf", "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Bruno\r\nEMAIL:bruno@example.org\r\nEND:VCARD\r\n");
    let Permitted::Done(refreshed) = book.refresh(h.account_id).await.unwrap() else {
        panic!("the contacts were refused");
    };
    assert_eq!(refreshed.contacts, 2);
}

#[tokio::test]
async fn the_settings_ask_before_replacing_a_script_and_go_ahead_after_a_yes() {
    let h = imap_harness().await;
    let sieve = Arc::new(FakeSieve::new("fileinto vacation imap4flags"));
    sieve.put_elsewhere("mine", "keep;\n", true);
    let imap = h.sync.services().fake_imap_adapter().expect("an IMAP account");
    let rules = SieveRules::new(Arc::clone(&sieve), imap, "me@example.com".into(), "Example".into());
    let services = h
        .sync
        .services()
        .clone()
        .with_rules(AnyRules::FakeSieve(rules.clone()))
        .with_auto_reply(AnyAutoReply::FakeSieve(rules));
    let settings =
        AccountSettings::new(Arc::new(ServedBy::new(h.account_id, Arc::clone(&h.sync), services)), h.db.clone());
    let refused = settings.add_rule(h.account_id, Filter::block("pest@example.com")).await.unwrap_err();
    assert!(matches!(refused, crate::SyncError::Backend(crate::BackendError::WouldReplace { .. })));
    settings.take_over_rules(h.account_id).await.unwrap();
    assert!(matches!(
        settings.add_rule(h.account_id, Filter::block("pest@example.com")).await.unwrap(),
        Permitted::Done(_)
    ));
}

#[tokio::test]
async fn an_edit_through_the_settings_keeps_its_place_on_a_sieve_server() {
    let h = imap_harness().await;
    let sieve = Arc::new(FakeSieve::new("fileinto vacation imap4flags"));
    let imap = h.sync.services().fake_imap_adapter().expect("an IMAP account");
    let rules = SieveRules::new(Arc::clone(&sieve), imap, "me@example.com".into(), "Example".into());
    let services = h.sync.services().clone().with_rules(AnyRules::FakeSieve(rules));
    let settings =
        AccountSettings::new(Arc::new(ServedBy::new(h.account_id, Arc::clone(&h.sync), services)), h.db.clone());
    let Permitted::Done(first) = settings.add_rule(h.account_id, Filter::block("a@example.com")).await.unwrap()
    else {
        panic!("refused")
    };
    settings.add_rule(h.account_id, Filter::block("b@example.com")).await.unwrap();
    let puts = sieve.puts();
    let Permitted::Done(Replaced::Swapped(_)) =
        settings.replace_rule(h.account_id, &first, Filter::block("c@example.com")).await.unwrap()
    else {
        panic!("not swapped")
    };
    assert_eq!(sieve.puts(), puts + 1, "one script write, not a create and a delete");
    let Permitted::Done(all) = settings.rules(h.account_id).await.unwrap() else { panic!("refused") };
    assert_eq!(all.len(), 2);
    assert!(format!("{:?}", all[0]).contains("c@example.com"), "{all:?}");
}

#[tokio::test]
async fn a_contacts_server_that_refuses_the_login_says_so() {
    let dav = Arc::new(FakeDav::new());
    dav.refuse_login(true);
    let contacts = AnyContacts::FakeDav(CardDav::new(Arc::clone(&dav)));
    assert!(contacts.login_refused().is_none());
    let _ = contacts.connections(None, None).await;
    assert!(contacts.login_refused().is_some());
}

#[tokio::test]
async fn an_edit_through_the_settings_keeps_its_place_among_local_rules() {
    let h = imap_harness().await;
    let rules = AnyRules::Local(crate::LocalRules::new(h.db.clone(), h.account_id));
    let services = h.sync.services().clone().with_rules(rules);
    let settings =
        AccountSettings::new(Arc::new(ServedBy::new(h.account_id, Arc::clone(&h.sync), services)), h.db.clone());
    let Permitted::Done(first) = settings.add_rule(h.account_id, Filter::block("a@example.com")).await.unwrap()
    else {
        panic!("refused")
    };
    settings.add_rule(h.account_id, Filter::block("b@example.com")).await.unwrap();
    settings.replace_rule(h.account_id, &first, Filter::block("c@example.com")).await.unwrap();
    let Permitted::Done(all) = settings.rules(h.account_id).await.unwrap() else { panic!("refused") };
    assert_eq!(all.len(), 2);
    assert!(format!("{:?}", all[0]).contains("c@example.com"), "{all:?}");
}

#[tokio::test]
async fn a_caldav_account_offers_quiet_changes() {
    let h = imap_harness().await;
    let dav = Arc::new(FakeDav::new());
    let mail = h.sync.services().fake_imap_adapter().expect("an IMAP account");
    let services = h
        .sync
        .services()
        .clone()
        .with_calendar(AnyCalendar::FakeDav(CalDav::new(dav, mail, vec!["me@example.com".into()])));
    assert!(services.offers().quiet_changes);
}
