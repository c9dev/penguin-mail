//! The CalDAV adapter over the real client against Radicale: the calendar
//! list, a first read, an edit that keeps the lines it did not touch, a
//! read after a token, and one occurrence of a series cancelled. Then the
//! same server as a calendar account has it: no mail behind it and no
//! address handed in.
//!
//! One test, because it points `SSL_CERT_FILE` at a root made for the
//! run, and the environment belongs to the whole process.

use std::sync::Arc;

use mailrs_dav::{DavApi, DavClient, Login};
use mailrs_discover::{Security, Server, UserName};
use mailrs_domain::Provider;
use mailrs_domain::calendar::{Event, Notify, occurrence_id};
use mailrs_imap::{ImapClient, SmtpClient};
use mailrs_store::Db;
use mailrs_sync::calendar_copy::CalendarCopy;
use mailrs_sync::passwords::MemoryPasswords;
use mailrs_sync::services::CalDav;
use mailrs_sync::services::finding::RealProbe;
use mailrs_sync::sign_in::{CalDavSignInError, NewCalDav, caldav_signed_in};
use mailrs_sync::{
    AccountServices, AnyMail, BackendError, CalendarService, EngineConfig, ImapSettings, NoMail,
    SyncEngine, SyncError, connect_caldav,
};
use mailrs_testmail::{Certs, Radicale};

const USER: &str = "me@example.test";

fn series(title: &str) -> String {
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//t//EN\r\nBEGIN:VEVENT\r\nUID:standup\r\nDTSTAMP:20261001T000000Z\r\n\
         DTSTART:20261005T093000Z\r\nDTEND:20261005T100000Z\r\nRRULE:FREQ=WEEKLY\r\nSUMMARY:{title}\r\nX-KEEP:yes\r\n\
         END:VEVENT\r\nEND:VCALENDAR\r\n"
    )
}

#[test]
fn the_caldav_adapter_reads_and_writes_radicale() {
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
        let Some(radicale) = Radicale::start(&certs, USER, &password).await else {
            return;
        };
        let path = radicale.make_calendar(USER, &password, "work").await;
        let href = format!("{path}standup.ics");
        radicale
            .put(USER, &password, &href, &series("Standup"), "text/calendar")
            .await;
        let client = Arc::new(
            DavClient::new(
                &radicale.url(),
                mailrs_dav::Kind::Calendar,
                Login::new(USER, &password),
            )
            .expect("a client"),
        );

        // The mail adapter is for replying to invitations, which no step
        // here does, so its clients never connect.
        let nowhere = Server {
            host: "localhost".into(),
            port: 1,
            security: Security::Tls,
            user_name: UserName::Address,
        };
        let login = mailrs_imap::Login::new(USER, password.as_str());
        let mail = AccountServices::imap(
            ImapClient::new(nowhere.clone(), login.clone()),
            SmtpClient::new(&nowhere, &login).expect("an SMTP client"),
            ImapSettings {
                address: USER.into(),
                provider_name: "Radicale".into(),
                files_sent_mail: false,
                window_days: 30,
            },
        )
        .mail;
        let caldav = CalDav::new(Arc::clone(&client), mail, vec![USER.into()]);

        let calendars = caldav.calendars().await.expect("the calendars");
        let work = calendars
            .iter()
            .find(|c| c.id.ends_with(&path))
            .expect("the calendar is listed")
            .id
            .clone();

        let first = caldav
            .event_changes(&work, None, None, 0)
            .await
            .expect("a first read");
        assert_eq!(first.events.len(), 1, "{first:?}");
        assert_eq!(first.whole_series, ["standup"]);
        let token = first.next_sync.clone().expect("one page");

        let edited = mailrs_domain::calendar::Event {
            title: "Standup (short)".into(),
            ..first.events[0].clone()
        };
        let sent = caldav
            .put_event(&edited, Some(&edited.etag), false, Notify::Nobody)
            .await
            .expect("the edit");
        assert_eq!(sent.title, "Standup (short)");

        let after = caldav
            .event_changes(&work, Some(&token), None, 0)
            .await
            .expect("a read after the token");
        assert_eq!(after.events[0].title, "Standup (short)");

        let master = &after.events[0];
        let one = occurrence_id(master, master.start + 7 * 24 * 3_600_000);
        caldav
            .remove_event(&work, &one, Some(&master.etag), Notify::Nobody)
            .await
            .expect("one occurrence cancelled");
        let body = client.get(&href).await.expect("the resource").body;
        assert!(
            body.contains("EXDATE") && body.contains("20261012T093000"),
            "{body}"
        );
        assert!(body.contains("X-KEEP:yes"), "{body}");

        // A calendar account on the same server. Radicale names no address
        // for its principal and schedules nothing, so the account is
        // nobody's guest, and an answer from it would reach no organizer.
        let alone = CalDav::new(Arc::clone(&client), AnyMail::NoMail(NoMail), Vec::new());
        let listed = alone
            .calendars()
            .await
            .expect("the calendars, with no mail");
        assert!(listed.iter().any(|c| c.id == work), "{listed:?}");
        assert!(
            !alone.tells_organizer(),
            "no mail and no scheduling leave nobody to tell"
        );
        let read = alone
            .event_changes(&work, None, None, 0)
            .await
            .expect("a read with no mail");
        assert!(
            read.events.iter().any(|e| e.title == "Standup (short)"),
            "{read:?}"
        );
        assert!(
            read.events.iter().all(|e| e.guests.iter().all(|g| !g.me)),
            "nobody is the account"
        );
        let lunch = Event {
            calendar: work.clone(),
            id: "pm0123456789abcdefghijklmnopqrstuv".into(),
            start: 1_791_300_000_000,
            end: 1_791_303_600_000,
            zone: "UTC".into(),
            title: "Lunch".into(),
            busy: true,
            ..Event::default()
        };
        let made = alone
            .put_event(&lunch, None, true, Notify::Nobody)
            .await
            .expect("an event of its own");
        assert_eq!(made.title, "Lunch");

        // The whole way in, as Add Account goes: the login is tried at the
        // address, the account is kept under its user and host, connecting
        // builds its services, the engine keeps them, and the calendar
        // copy reads them into the store.
        let dir = tempfile::tempdir().expect("a folder for the store");
        let db = Db::open(&dir.path().join("mail.db")).expect("a store");
        let passwords = Arc::new(MemoryPasswords::default());
        let new = NewCalDav {
            url: radicale.url(),
            user_name: USER.into(),
            password: password.clone(),
        };
        let account = caldav_signed_in(&RealProbe, &db, Arc::clone(&passwords), new, 7)
            .await
            .expect("the sign-in");
        let host = mailrs_dav::host_of(&radicale.url()).expect("the server's host");
        assert_eq!(
            (account.email.as_str(), account.provider),
            (format!("{USER}@{host}").as_str(), Provider::CalDav)
        );
        let wrong = NewCalDav {
            url: radicale.url(),
            user_name: USER.into(),
            password: "not the password".into(),
        };
        let refused = caldav_signed_in(&RealProbe, &db, Arc::clone(&passwords), wrong, 8).await;
        assert!(
            matches!(
                refused,
                Err(CalDavSignInError::Server(SyncError::Backend(
                    BackendError::NeedsReauth
                )))
            ),
            "a wrong password is a refused login: {:?}",
            refused.map(|a| a.email)
        );

        let services = connect_caldav(&db, passwords, &account)
            .await
            .expect("the services");
        assert!(!services.serves_mail());
        let (engine, _events) = SyncEngine::new(db.clone(), EngineConfig::default());
        engine.start_account(account.id, services);
        let copy = CalendarCopy::new(Arc::new(engine), db.clone());
        copy.refresh(account.id, mailrs_sync::now_millis())
            .await
            .expect("a first read into the store");
        let id = account.id;
        let kept = db
            .read(move |c| mailrs_store::calendar::calendars(c, id))
            .await
            .expect("the stored calendars");
        assert!(kept.iter().any(|c| c.id == work), "{kept:?}");
        let held = db
            .read(move |c| mailrs_store::calendar::event(c, id, &work, &made.id))
            .await
            .expect("the store");
        assert_eq!(held.map(|e| e.title).as_deref(), Some("Lunch"));
    });
}
