//! DavClient against Radicale: principal and homes, collections, a
//! sync-collection round, If-Match refusing a stale etag, a wrong
//! password, a vCard, and the DAV header that says whether the server
//! schedules invitations itself.
//!
//! One test, because it points `SSL_CERT_FILE` at a root made for the
//! run, and the environment belongs to the whole process.

use mailrs_dav::{DavApi, DavClient, DavError, Kind, Login, Precondition};
use mailrs_testmail::{Certs, Radicale};

const USER: &str = "me@example.test";
const EVENT: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//t//EN\r\nBEGIN:VEVENT\r\nUID:e1\r\nDTSTAMP:20261001T000000Z\r\nDTSTART:20261006T120000Z\r\nDTEND:20261006T130000Z\r\nSUMMARY:Lunch\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

#[test]
fn the_client_speaks_caldav_and_carddav_to_radicale() {
    if mailrs_testmail::docker().is_none() {
        return;
    }
    let Some(certs) = Certs::make() else { return };
    // SAFETY: the only test in this binary, before any thread starts.
    unsafe { mailrs_testmail::trust(&certs.root()) };
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("a runtime");
    runtime.block_on(async {
        let password = mailrs_testmail::password();
        let Some(radicale) = Radicale::start(&certs, USER, &password).await else { return };
        let calendar = radicale.make_calendar(USER, &password, "work").await;
        let book = radicale.make_address_book(USER, &password, "people").await;
        let client = DavClient::new(&radicale.url(), Login::new(USER, &password)).expect("a client");
        let homes = client.homes().await.expect("the homes");
        let home = homes.calendar.expect("a calendar home");
        let found = client.collections(&home, Kind::Calendar).await.expect("the calendars");
        let work = found.iter().find(|c| c.href.ends_with(&calendar)).unwrap_or_else(|| panic!("{found:?}"));
        assert!(work.sync, "Radicale answers sync-collection");

        // Radicale does no scheduling, so its DAV header lacks
        // calendar-auto-schedule and the app sends the reply itself.
        assert!(!client.auto_schedule().await.expect("the DAV header"));

        let first = client.sync(&calendar, "").await.expect("a first sync");
        let href = format!("{calendar}e1.ics");
        let etag = client.put(&href, EVENT, Kind::Calendar, Precondition::NoneMatch).await.expect("a put");
        let etag = match etag {
            Some(e) => e,
            None => client.get(&href).await.expect("a get").etag,
        };
        let next = client.sync(&calendar, &first.token).await.expect("a second sync");
        assert_eq!(next.changed.len(), 1, "{next:?}");
        radicale.put(USER, &password, &href, &EVENT.replace("Lunch", "Lunch, theirs"), "text/calendar").await;
        let stale = client.put(&href, EVENT, Kind::Calendar, Precondition::Match(etag)).await;
        assert!(matches!(stale, Err(DavError::Changed)), "{stale:?}");
        let fetched = client.fetch(&calendar, Kind::Calendar, &[next.changed[0].href.clone()]).await.expect("a fetch");
        assert!(fetched[0].body.contains("Lunch, theirs"));

        let card = format!("{book}a.vcf");
        client
            .put(&card, "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:a\r\nFN:Ana\r\nEND:VCARD\r\n", Kind::AddressBook, Precondition::NoneMatch)
            .await
            .expect("a vCard put");
        assert_eq!(client.members(&book, Kind::AddressBook, None).await.expect("the members").len(), 1);

        let wrong = DavClient::new(&radicale.url(), Login::new(USER, "wrong")).expect("a client");
        assert!(matches!(wrong.homes().await, Err(DavError::Unauthorized)));
    });
}
