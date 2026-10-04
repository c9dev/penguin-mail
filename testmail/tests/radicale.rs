//! Radicale starts, takes the account's password, and makes a calendar
//! and an address book a DAV client can find.

use mailrs_testmail::{Certs, Radicale};

const USER: &str = "me@example.test";

#[test]
fn radicale_starts_and_makes_collections() {
    if mailrs_testmail::docker().is_none() {
        return;
    }
    let Some(certs) = Certs::make() else { return };
    // SAFETY: the only test in this binary, before any thread starts.
    unsafe { mailrs_testmail::trust(&certs.root()) };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    runtime.block_on(async {
        let password = mailrs_testmail::password();
        let Some(radicale) = Radicale::start(&certs, USER, &password).await else {
            return;
        };
        let calendar = radicale.make_calendar(USER, &password, "work").await;
        assert_eq!(calendar, format!("/{USER}/work/"));
        let book = radicale.make_address_book(USER, &password, "people").await;
        assert_eq!(book, format!("/{USER}/people/"));
        let refused = reqwest::Client::new()
            .request(
                reqwest::Method::from_bytes(b"PROPFIND").expect("a method"),
                radicale.url() + &calendar[1..],
            )
            .basic_auth(USER, Some("wrong"))
            .header("Depth", "0")
            .send()
            .await
            .expect("Radicale answers");
        assert_eq!(refused.status().as_u16(), 401);
    });
}
