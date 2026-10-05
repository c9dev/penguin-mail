//! The CalDAV adapter over the real client against Radicale: the calendar
//! list, a first read, an edit that keeps the lines it did not touch, a
//! read after a token, and one occurrence of a series cancelled.
//!
//! One test, because it points `SSL_CERT_FILE` at a root made for the
//! run, and the environment belongs to the whole process.

use std::sync::Arc;

use mailrs_dav::{DavApi, DavClient, Login};
use mailrs_discover::{Security, Server, UserName};
use mailrs_domain::calendar::{Notify, occurrence_id};
use mailrs_imap::{ImapClient, SmtpClient};
use mailrs_sync::services::{CalDav, Imap};
use mailrs_sync::{CalendarService, ImapSettings};
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
        let Some(radicale) = Radicale::start(&certs, USER, &password).await else { return };
        let path = radicale.make_calendar(USER, &password, "work").await;
        let href = format!("{path}standup.ics");
        radicale.put(USER, &password, &href, &series("Standup"), "text/calendar").await;
        let client = Arc::new(DavClient::new(&radicale.url(), Login::new(USER, &password)).expect("a client"));

        // The mail adapter is for replying to invitations, which no step
        // here does, so its clients never connect.
        let nowhere = Server { host: "localhost".into(), port: 1, security: Security::Tls, user_name: UserName::Address };
        let login = mailrs_imap::Login::new(USER, password.as_str());
        let mail = Imap::new(
            Arc::new(ImapClient::new(nowhere.clone(), login.clone())),
            Arc::new(SmtpClient::new(&nowhere, &login).expect("an SMTP client")),
            ImapSettings { address: USER.into(), provider_name: "Radicale".into(), files_sent_mail: false, window_days: 30 },
        );
        let caldav = CalDav::new(Arc::clone(&client), mail, vec![USER.into()]);

        let calendars = caldav.calendars().await.expect("the calendars");
        let work = calendars.iter().find(|c| c.id.ends_with(&path)).expect("the calendar is listed").id.clone();

        let first = caldav.event_changes(&work, None, None, 0).await.expect("a first read");
        assert_eq!(first.events.len(), 1, "{first:?}");
        assert_eq!(first.whole_series, ["standup"]);
        let token = first.next_sync.clone().expect("one page");

        let edited = mailrs_domain::calendar::Event { title: "Standup (short)".into(), ..first.events[0].clone() };
        let sent = caldav.put_event(&edited, Some(&edited.etag), false, Notify::Nobody).await.expect("the edit");
        assert_eq!(sent.title, "Standup (short)");

        let after = caldav.event_changes(&work, Some(&token), None, 0).await.expect("a read after the token");
        assert_eq!(after.events[0].title, "Standup (short)");

        let master = &after.events[0];
        let one = occurrence_id(master, master.start + 7 * 24 * 3_600_000);
        caldav.remove_event(&work, &one, Some(&master.etag), Notify::Nobody).await.expect("one occurrence cancelled");
        let body = client.get(&href).await.expect("the resource").body;
        assert!(body.contains("EXDATE") && body.contains("20261012T093000"), "{body}");
        assert!(body.contains("X-KEEP:yes"), "{body}");
    });
}
